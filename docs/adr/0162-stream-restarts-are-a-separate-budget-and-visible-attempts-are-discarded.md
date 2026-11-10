# ADR-0162: Stream Restarts Are a Separate Budget and Every Failed Attempt Is Discarded

- **Status:** Accepted
- **Date:** 2026-11-08
- **Related:** [ADR-0026](0026-streamed-tool-input-progress.md), [ADR-0122](0122-provider-retry-is-chrome-typed-state-machine-and-turn-outcome-anchors.md), [ADR-0163](0163-a-turns-tool-call-batch-is-a-bounded-contract-and-arguments-are-loop-guarded.md)
- **Supersedes in part:** ADR-0122 §"one attempt vocabulary" (the pair now describes the active budget, not one global attempt count)

## Context

One retry counter governed every retryable provider failure. A turn that had
already streamed visible output and then lost the stream (stall, truncation,
RST) replayed the **entire** request against the same `retry_max_attempts`
budget — 30 by default and in the live configuration. Each replay re-streamed
the same tool-call announcements, and the failed attempt's visible state was
not always discarded: `RoundEvent::StreamDiscard` only pops the trailing
assistant **text** row, and `ToolCallStarted` never set the `streamed_text`
flag that gated it. A fast model lane that announced a batch and died
mid-arguments therefore left a wall of `receiving input` rows, one per attempt,
and the round could spend thirty restore attempts on a stream that was never
going to finish. The live logs show attempts 2 through 30 at 10s backoff across
many threads.

The original reasoning — "a transient failure is a transient failure" — was
wrong for the mid-stream case. A silent failure (nothing was rendered) is cheap
and safe to repeat: the model never saw a partial answer and the user saw
nothing. A failure after visible output is a different event: the attempt's
partial state must be discarded, the user has already watched it, and an
upstream that breaks mid-response usually breaks the same way again. Sharing
one budget conflated the two.

## Decision

**Attempts are classified by whether they produced visible output.**

Visible output is any of: an assistant text delta, a reasoning delta, or a
tool-call announcement (`ToolCallStarted`). Announcements count: a provisional
tool row is already part of the transcript, so a retry that follows it is a
stream restart, not a silent request retry.

Two independent budgets apply:

- **Silent attempts** — `[resilience] retry_max_attempts` (`[resilience]
  retry_max_attempts`, default 30, clamped to 1..=60). The initial request is
  attempt 1; the remaining are retries.
- **Stream restarts** — `[resilience] stream_restart_max_attempts` (`[resilience]
  stream_restart_max_attempts`, default 3, clamped to 0..=10). This counts
  restarts after a visible attempt; `0` means any visible failure ends the
  round immediately. The initial visible attempt is not a restart, so a budget
  of 3 permits four visible attempts.

The classification is made **per failed attempt**, not per round: a round may
burn silent attempts first (a 429 before any output) and then switch to the
restart budget once output begins.

**Every failed attempt's visible state is discarded before the next attempt or
the terminal error:**

- streamed assistant text is dropped with `StreamDiscard` (including on the
  final failed attempt);
- announced tool steps are retracted by the harness with
  `ToolAnnouncementsRetracted` (ADR-0026 amendment), emitted on every abort
  path and on the settle-window interrupt;
- the aborted attempt is never committed to history.

`RetryScheduled` carries the **active** budget's `attempt` and `max_attempts`,
so every consumer (bar clause, notice receipt, headless record) shows the
budget the retry actually spends. There is no second counter on screen and no
synthetic "total" that mixes the two.

Recoveries that are not faults — context-overflow compaction and the
image-capability probe — consume neither budget.

Exhausting the restart budget is terminal for the round and says so:
`Provider stream failed after partial output N time(s); gave up after K
restart(s)…`. The user can resend, switch models, or raise the budget.

## Invariants

- `[INV-RETRY-01]` **Visibility is the classifier.** A failure is a stream
  restart iff the attempt emitted assistant text, reasoning, or a tool-call
  announcement.
- `[INV-RETRY-02]` **No visible residue survives a failed attempt.** Before a
  retry or a terminal error, every provisional row the attempt streamed — its
  text row, its thinking trace (streaming *or* already finalized), and its
  provisional tool rows — is discarded. See the 2026-11-08 amendment.
- `[INV-RETRY-03]` **Budgets are independent.** Silent failures never consume
  the restart budget and vice versa.
- `[INV-RETRY-04]` **Recoveries are free.** Overflow compaction and the image
  probe consume neither budget.
- `[INV-RETRY-05]` **One truth on screen.** `RetryScheduled` reports the budget
  the retry belongs to, with the initial attempt counted as attempt 1.

## Consequences

- A provider that dies mid-response can no longer produce a thirty-attempt
  replay storm; it gets at most `stream_restart_max_attempts` restarts.
- The same budget split removes the `receiving input` wall: every failed
  attempt's rows are discarded, and the final failure does not leave a
  half-streamed answer behind.
- Operators who genuinely need more mid-stream restarts can raise the separate
  knob without weakening the silent-retry policy (and vice versa).
- The displayed attempt pair changes meaning in one narrow case: a mid-stream
  failure now shows `retry 1 of K` against the restart budget instead of
  against the global attempt count. ADR-0122's display machinery is unchanged;
  only the source of the pair moved.
- `[INV-RETRY-03]` means a round can make at most
  `retry_max_attempts + stream_restart_max_attempts + 1` provider calls.

## Validation

- `mid_stream_failures_consume_only_the_restart_budget` drives a provider that
  emits a delta and then fails on every attempt with
  `retry_max_attempts = 30, stream_restart_max_attempts = 2`: exactly three
  attempts, two `RetryScheduled` events (`2 of 3`, `3 of 3`), three
  `StreamDiscard`s, and a terminal error naming the restart budget.
- `turn_exhaustion_message_explains_retry_budget` pins the silent budget: a
  pre-output 429 still yields `retry_max_attempts` attempts.
- `turn_retries_transient_provider_failure_before_tool_activity` pins that a
  visible failure is still recovered while budget remains.

## 2026-11-08 Amendment: A Discard Sweeps the Whole Attempt

`StreamDiscard` popped **one** row: the trailing assistant text row. That was
already narrower than `[INV-RETRY-01]`, which classifies an attempt as visible
on reasoning deltas too, and narrower than `[INV-RETRY-02]`, which promises no
visible residue survives. An attempt can leave several provisional rows behind,
and the discard missed all but the last:

- **A thinking trace that outlived its answer text.** Reasoning streams, the
  model starts answering, and the transport dies mid-answer. The transcript
  tail is the text row, so the discard took that and orphaned the thinking.
- **A thinking trace that was already finalized.** The reasoning block closes
  (a duration is stamped) before the answer text begins. The row is no longer
  *streaming*, so neither the discard nor the `FinalizeOrphanedReasoning` safety
  net can reach it — that net only matches `is_reasoning_streaming()`.

The first shape is reachable on the ordinary failure path, and it is the one the
contract test below pins. The second needs a finalized trace to be sitting above
a still-uncommitted row; the reasoning block is closed by the agent only on the
paths that go on to commit (loop-cut, and natural completion), so treat it as a
defensive case rather than a demonstrated one. The sweep covers both, because
matching by *position and ownership* costs nothing extra.

The residue is not merely cosmetic. A retried attempt reuses the same
`(round, turn)` — verified: both attempts in the contract test open with
`TurnStarted { round: 1, turn: 0 }` — and `append_stream_text_delta` /
`append_reasoning_delta` resolve their target by position alone, so the next
attempt appends **into** the failed attempt's row. The user sees two attempts
fused into one uninterrupted block — `"attempt one reasoningattempt two
reasoning"` — with no separator marking where one ended and the other began. On
the finalized-trace path nothing ever corrects it, because `ReasoningFinalize`
overwrites only rows that are still streaming.

Note also that the failure path emits **no** `ReasoningEnd` at all: the agent
returns the transport error straight from its `Err(error)` arm (`rounds.rs`),
which retracts announced calls and returns. So a thinking trace that streamed
before the failure is never closed, and the shell has no finalize event to lean
on — it must discard by position. The contract test asserts this absence
explicitly, so a future change that starts emitting `ReasoningEnd` on the
failure path will fail the test and force this reasoning to be revisited.

`StreamDiscard` therefore truncates the **trailing run** of rows owned by that
`(round, turn)` that are this attempt's own provisional output — `Role::Assistant`,
any reasoning row, or a tool step still `Ingesting`. The sweep stops at the
first row that is none of those: a dispatched tool step, a command result, or a
notice is committed dialogue, and truncating beneath one would reorder history.
A discard that owns nothing is a no-op. It always names a position: ADR-0167
removes the positionless variant, which was the state that made the old guard
necessary.

The `Ingesting` tool step belongs in the provisional set because it is exactly
the row that would otherwise shield the answer text. On a mid-stream failure the
orchestration emits `StreamDiscard` (line 1588) *before* the retry, while
`RetractPendingToolSteps` rides the `RetryScheduled` frame that follows it. So at
discard time the announced step is still in the transcript, and a tail-only
sweep that treats any tool row as a floor would stop on it and leave the answer
text underneath. That step is already retracted by the harness at that point
(`[INV-TOOL-ID-05]`), so it is provisional by definition.

This ordering is now pinned end-to-end by
`truncated_stream_discards_before_retrying_and_never_closes_reasoning` in
`crates/nuo-harness/tests/it/orchestration.rs`, which drives a mock provider that
emits reasoning + text and then fails the stream, and asserts the emitted event
order. Verified to bite: reordering the discard after the retry fails the test
with `StreamDiscard@11, RetryScheduled@10`.

### Validation

`crates/nuo-harness/tests/it/orchestration.rs` pins the **ordering contract** the
sweep depends on, end-to-end through the real orchestration loop:

- `truncated_stream_discards_before_retrying_and_never_closes_reasoning` — a mock
  provider streams reasoning + text and then fails the transport. Asserts the
  emitted order is `StreamDiscard` then `RetryScheduled`, that the reasoning
  delta actually reached the client (so the negative below is not vacuous), that
  **no** `StreamReasoningEnd` is emitted on the failure path, and that both
  attempts open at the same `(round, turn)`.

`crates/nuo/src/shell/tests/stream_discard.rs` pins the **sweep** itself, driving
the real `apply_transcript`:

- `discard_removes_a_lone_thinking_trace` — reasoning-only attempt.
- `discard_removes_thinking_that_outlived_its_answer_text` — trace orphaned by
  the text row's pop (fails against the single-pop implementation).
- `retried_attempt_never_fuses_into_the_failed_attempt` — the user-visible
  fusion, through a full finalize cycle (fails against the single-pop
  implementation).
- `discard_keeps_committed_dialogue_below_the_attempt` — the sweep's floor.
- `discard_is_a_no_op_when_the_attempt_streamed_nothing` — never truncates
  another turn's dialogue.
- `discard_without_a_position_owns_nothing` — **deleted by ADR-0167**: a
  positionless discard is no longer representable.
- `discard_reaches_text_beneath_a_still_pending_tool_step` — an `Ingesting` step
  must not shield the text (fails if the sweep treats every tool row as a
  floor).
- `discard_never_truncates_beneath_a_dispatched_tool_step` — the mirror: a
  dispatched step is committed and stops the sweep.

ADR-0167 carries the position on `RoundEvent::StreamDiscard` itself and moves
the protocol floor to 25, so a client that re-attaches mid-stream does not need
`TurnStarted` to have been observed before the discard.
