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
  retry or a terminal error, the attempt's text row and provisional tool rows
  are discarded.
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
