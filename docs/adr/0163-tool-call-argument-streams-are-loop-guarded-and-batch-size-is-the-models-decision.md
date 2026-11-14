---
id: ADR-0163
title: "Tool-Call Argument Streams Are Loop-Guarded; Batch Size Is the Model's Decision"
status: accepted
date: 2026-11-10
scope: [workspace]
superseded_by: null
negative_knowledge: true
---

# ADR-0163: Tool-Call Argument Streams Are Loop-Guarded; Batch Size Is the Model's Decision

- **Status:** Accepted
- **Date:** 2026-11-08
- **Related:** [ADR-0026](0026-streamed-tool-input-progress.md), [ADR-0099](0099-decoupled-streamguard-physical-flood-protection-and-cognitive-truncation-harmony.md), [ADR-0162](0162-stream-restarts-are-a-separate-budget-and-visible-attempts-are-discarded.md)
- **Supersedes:** the unused `nuo_tool::ToolPolicy` (deleted), and the text/reasoning-only scope of the in-flight stream loop detector

## Context

ADR-0026 announces a tool call as soon as its name arrives, before its
arguments finish streaming. That is the right UX, but it makes the model's
tool-call batch visible immediately. An incident produced a wall of
`receiving input` rows and looked like a runaway parallel-call loop.

Two different explanations were on the table:

1. the model legitimately emitted an oversized batch; or
2. a client-side accounting defect amplified a normal batch.

The evidence points to (2). The failed attempts were replayed against a single
retry budget (up to 30 attempts), while `RoundEvent::StreamDiscard` dropped only
the trailing assistant **text** row. Announced tool steps were never part of
that discard, and `ToolCallStarted` did not set the `streamed_text` flag that
gated it. A batch of a few calls per attempt therefore accumulated one wall of
provisional rows across attempts, and the round boundary then swept them into
`(cancelled …)`. The model did not need to emit thirty calls for the user to see
thirty rows.

That amplification is fixed independently by ADR-0162 (separate stream-restart
budget) and the ADR-0026 amendment (announcement ids + retraction on every abort
path). What remains is a genuine but narrower failure mode: a **degenerate
tool-argument stream** — a single call (or a few) whose argument payload keeps
cycling and never converges. Text and reasoning were already loop-guarded;
tool-call arguments were not, because they never entered the detector.

## Decision

**Batch size is the model's decision.** The harness does not impose a numeric
cap on concurrent tool calls and does not put a batch-size rule in the system
prompt. The remote decides how many calls fit its context and task; the harness
consumes what it emits, renders it, and gates execution through the existing
permission and trajectory machinery. A legitimate 10- or 15-file batch is not a
fault, and the UI must handle it.

**Tool-call argument streams are loop-guarded like text.** This is degeneration
detection, not a batch limit:

- `StreamLoopChannel` gains a `ToolCall` variant.
- `StreamLoopReviewInput` gains `tool_input`: a bounded (24 KB) tail of the
  argument fragments, carried as evidence.
- Argument fragments feed the same `StreamLoopDetector` and the same
  asynchronous Stream Sentinel as prose. The reviewer is told that repeated
  file content, generated data, or a long inline patch is legitimate task
  content, and confirms a loop only when the argument stream cycles without
  converging.
- On confirmation, the stream is cut, the partial batch is discarded (none of
  its calls is executable intent), announcements are retracted, and the turn
  follows the existing one-recovery-then-stop discipline. A tool-only response
  has no assistant text to commit, so the directive becomes the next turn's
  input; no empty assistant message is fabricated.
- The Sentinel verdict is settled **before** the strict tool-call truncation
  validation. A confirmed loop explains its own truncated arguments; judging
  them as a transport failure first would turn the intervention into a retry
  storm. When the verdict is `no`, truncation is still reported as a transport
  error.

**Dead policy is deleted, not repurposed.** `nuo_tool::ToolPolicy` had no call
sites and described limits that were never built; it is removed outright. No
replacement numeric cap is introduced.

**A latent detector bug is fixed.** The periodic-tail scan skipped a period
whose every offset matched (the strongest signal), so an exactly periodic
stream became undetectable once the 1024-char window filled whenever its period
did not divide the window. Path 2 now treats a zero-mismatch run as the
whole-window run, and `whole_window_period_survives_a_full_buffer` pins it.
Without this fix, the new `ToolCall` channel would have been inert on the very
payloads it exists to catch.

## Invariants

- `[INV-TOOLLOOP-01]` **No batch-size cap.** The harness prescribes no maximum
  number of tool calls per response and adds no prompt rule about one.
- `[INV-TOOLLOOP-02]` **A confirmed argument loop is not intent.** Its batch is
  cleared and its announcements retracted before any recovery or commit.
- `[INV-TOOLLOOP-03]` **A confirmed tool loop is not a transport truncation.**
  The Sentinel settles before strict argument validation.
- `[INV-TOOLLOOP-04]` **No fake assistant turn.** A tool-only cut does not
  commit an empty assistant message; the directive is the next turn's input.
- `[INV-TOOLLOOP-05]` **One recovery.** A tool-argument loop gets one guided
  retry per round; recurrence stops the round visibly.

## Consequences

- A model that emits a large but finite batch is obeyed. The transcript shows
  one row per call, as the model intended, and the scheduler fans them out.
- The `receiving input` wall is solved at its cause (retry/announcement
  accounting), not by truncating the model's legitimate output.
- A model that degenerates on a single argument payload is stopped in flight,
  with its batch discarded and one corrective retry.
- **Residual risk, explicitly accepted:** an unbounded stream of *distinct*
  tool calls is not caught by degeneration detection; it is bounded only by the
  provider's output limit. If that ever becomes a real incident, it deserves
  its own measured guard (and its own ADR) rather than an a-priori batch
  number chosen without evidence.

## Validation

- `tool_argument_loop_is_cut_and_recovered`: an 800-fragment periodic argument
  stream with a `yes` Sentinel verdict produces the loop notice, dispatches no
  call, and recovers on the next turn.
- `whole_window_period_survives_a_full_buffer`: the detector escalates on an
  exactly periodic stream after its window is full.
- The baseline system prompt remains empty: no batch-size instruction is added.
