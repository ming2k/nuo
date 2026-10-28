---
id: ADR-0122
title: "Provider Retry Is Chrome, Not Conversation: A Typed Retry State Machine, One Attempt Vocabulary, and Turn-Outcome Anchors"
status: accepted
date: 2026-12-12
scope: presentation/nuo-view, protocol/nuo-protocol, protocol/nuo-tool, harness/nuo-harness, host/nuo, tui/chrome, tui/event-loop
superseded_by: null
negative_knowledge: true
---

# 0122. Provider Retry Is Chrome, Not Conversation: A Typed Retry State Machine, One Attempt Vocabulary, and Turn-Outcome Anchors

- Status: Accepted
- Date: 2026-12-12
- Deciders: Nuo Architecture Working Group
- Consulted: Interface Architecture, Runtime Kernel, Protocol
- Amends: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) — the routing table's `ProviderRetry → Absorbed` row is deleted because the content it named is no longer a notice; [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) — the retry path becomes a conforming producer (its row is a `TurnOutcome` anchor admitted through the one gate, not a `push`); [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) — the retry receipt is the notice log's second writer

---

## Context and Problem Statement

A provider retry — the harness waiting out a 503/429/529 before re-sending a request — was represented **four times**, by four mechanisms that disagreed:

1. **The activity bar** rendered `ProviderRetryState::summary()` as
   `retry {attempt-1}/{max_attempts-1} (next in Xs)`.
2. **A transcript entry** (`MessageKind::ProviderRetry`) rendered
   `retry {attempt}/{max_attempts} in Xs`.
3. **A wire notice** (`NoticeKind::ProviderRetry`, emitted by the harness beside
   `RetryScheduled`) was routed by `layer_for` to `ContentLayer::Absorbed` and
   discarded.
4. **A settled notice**: on interrupt, `TranscriptEdit::Interrupted` called
   `settle_interrupted_provider_retry()`, rewriting the live entry in place into
   `Notice { topic: "retry", title: "Provider request failed (attempt N/M)",
   detail: <pretty-printed HTTP body> }`.

Both (1) and (2) were fed by the same `RoundEvent::RetryScheduled`, so for every
single retry the bar said `retry 5/29` while the transcript row said `retry 6/30`.
The same event also produced a `Notice` that the router threw away — the retry
was published twice, formatted twice, and displayed once and a half.

The consequences, in order of severity:

1. **Two disagreeing counts on one screen.** The vocabulary was re-derived at
   each paint site (`attempt-1` in one place, `attempt` in another). A user
   watching a struggling request saw the bar and the transcript contradict each
   other about how many attempts had been spent.
2. **A transport payload became a conversation message.** The settled entry's
   detail was `notice_text::normalize`'s pretty-printed JSON — the entire Google
   `503 / MODEL_CAPACITY_EXHAUSTED / OVERLOADED_TOO_MANY_RETRIES_PER_REQUEST`
   envelope, multi-line, in the scrollback. It entered the model window's
   neighbourhood, the export, and search, as if someone had said it.
3. **The write bypassed the admission gate.** `TranscriptEdit::Append` carries an
   `EventClass` and refuses non-dialogue/non-anchor rows. `TranscriptEdit::UpsertRetry`
   carried **no class** and called `messages.push(...)` directly, so the retry
   entry was the one L0 row in the process that could arrive without declaring
   what it was.
4. **The write had no name.** It was a second responsibility hidden inside a
   variant called `Interrupted`. Reading the mutation enum — the audit surface —
   told you "an interrupt cancels the prompt and appends an anchor"; it did not
   tell you "an interrupt also freezes a transient card into a permanent JSON
   notice".
5. **The success side was explicitly retired while the failure side persisted.**
   `transcript_retry_resolutions_from_records` returns an empty `Vec` with the
   comment *"Transient transport retries that recovered successfully are
   implementation details; they never project onto the transcript history"* —
   yet an interrupted retry left a full HTTP body behind. The same class of fact
   was ephemeral when it succeeded and permanent when it failed, for no reason
   anyone could state.
6. **A record ticked.** The entry painted a live countdown, so a transcript row
   re-rendered every frame. A record that changes on a timer is not a record, and
   it defeated the height cache.

The policy table said all of this was wrong. `layer.rs` documented
`NoticeKind::ProviderRetry → Absorbed` with the justification *"the retry card
owns its own disclosure"* — but the "card" was a transcript row, so the table was
documenting a hole, not a policy.

## Decision Drivers

- **One vocabulary, one owner.** An attempt count is a *fact about a request*,
  not a rendering choice. If two sites format it, they will eventually disagree —
  they already had.
- **A live state and a durable record are different things.** A countdown is
  chrome. A conversation is what was said. Content that re-renders on a clock
  cannot be a conversation message.
- **Every L0 write declares its class** ([ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md)). No exception, no
  "this one is special".
- **A transport detail must remain diagnosable.** Deleting the JSON from the
  timeline is only correct if the full payload survives somewhere addressable.
- **A recovered round is a real outcome.** A user who waited through four
  backoffs deserves to know the answer cost four retries — symmetrically with a
  round that was interrupted.
- **A subagent's setback is visible too.** The same rule must hold for a child,
  whose progress is the only thing the user is watching.

## Architectural Decisions

### 1. `RetryProgress`: one typed state machine owns the vocabulary

```rust
// nuo-view/src/retry.rs
pub struct RetryProgress { /* attempt, max_attempts, retry_at, faults */ }

impl RetryProgress {
    pub fn scheduled(attempt, max_attempts, retry_at, fault) -> Self;   // clamps into 1..=max
    pub fn with_next_attempt(self, attempt, max_attempts, retry_at, fault) -> Self;
    pub fn phase(&self, now: Instant) -> RetryPhase;                    // BackingOff | Reattempting
    pub fn label(&self, now: Instant) -> String;                        // "retry 6/30 (next in 8s)"
    pub fn compact_label(&self) -> String;                              // "(6/30)"
    pub fn retries(&self) -> usize;                                     // attempt - 1
    pub fn faults(&self) -> &[String];
}
```

`[INV-RETRY-01]` — **One attempt vocabulary.** The count is
`attempt/max_attempts`, composed in exactly one function, and every consumer
reads it: the bar's clause, its width-constrained compact form, the receipt-log
line, the recovered anchor, and the exhausted-error phrasing. The constructor
clamps, so `retry 0/0` and `retry 7/5` are unrepresentable.

The compact form is a **method**, not a substring of the full label. The old bar
extracted `(N/M)` by splitting the rendered sentence on `"retry "` — parsing a
phrase back out of its own rendering is precisely how the two vocabularies
drifted apart.

The *phase* (`BackingOff` / `Reattempting`) is **derived** from the clock against
the scheduled instant, never stored: a stored phase needs a timer to drive it,
and any missed tick leaves the bar lying about which state it is in.

### 2. The live retry owns no transcript row

`MessageKind::ProviderRetry` is deleted. A live retry is:

- **chrome** — `SessionChrome::transport_setback: Option<RetryProgress>`, rendered
  as a clause beside the activity label, retired by the next phase write that is
  not `AwaitingModel` (`nuo_view::phase` rule 4, unchanged); and
- **a receipt** — one `SurfaceId::NOTICES` line per scheduled attempt, carrying
  the clause, the route (`provider/model`) and the fault, openable with the notice
  viewer and copyable.

`[INV-RETRY-02]` — **No transcript row while a retry is in flight.** The
transcript contains what was said; a backoff is not something anyone said.

### 3. The retry is a turn outcome, admitted as an anchor

`TranscriptMessage::retry_recovered(&RetryProgress, round, at_ms)` mints the
success-side twin of `round_interrupted`: `topic: "recovered"`, title
`"Recovered after N provider retries (round R)"`, detail = the fault list, one
line each. It is admitted through `TranscriptEdit::Append { class: TurnOutcome }`
— the one gate.

The transient-retry mutations are gone. `UpsertRetry` (which had no class and
pushed directly) and `RetainNotRetry` are replaced by a single
`TranscriptEdit::RetrySettled { recovered: Option<TranscriptMessage> }`, which
admits the anchor or does nothing.

`[INV-RETRY-03]` — **Every L0 retry row is a `TurnOutcome` anchor.** There is no
retry write path that skips the class declaration.

### 4. `Interrupted` stops having a second responsibility

`TranscriptEdit::Interrupted` no longer rewrites anything. If the round died
while backing off, the *producer* puts the last fault into the anchor's one-line
`detail` (via `notice_text::display_text`, which yields a single summary line for
a non-JSON fault and a lead + pretty detail for a JSON one — but the anchor's
`detail` is the lead, not a payload dump). The anchor is the single record of the
stop.

`[INV-RETRY-04]` — **A mutation does what its name says.** An interrupt records
an interrupt; it does not settle unrelated state.

### 5. `NoticeKind::ProviderRetry` is retired, not absorbed

The harness published `RetryScheduled` **and** a `Notice(ProviderRetry)` carrying
the same facts in prose. The notice was then routed to `Absorbed` and dropped —
so the duplicate cost a wire message and bought nothing. It is deleted, and
`RetryScheduled` is the single announcement.

The one legitimate user of that notice kind was not a retry at all: the dispatch
layer's *"Completed tool call not repeated"* receipt (a retried request replayed
an already-checkpointed tool call). It becomes `NoticeKind::CheckpointReplay`, a
first-class receipt that routes to the log, where a "why didn't that tool run?"
question can actually be answered later.

`[INV-RETRY-05]` — **`NoticeKind` names receipts, never live state.** The routing
table has no `Absorbed`-for-a-retry row because there is no retry notice.

`NoticeOrigin::Provider { provider_name, attempt }` collapses to
`NoticeOrigin::System { topic }`: with the live entry gone, `attempt` had no
producer and `provider_name` was always `None`. A variant whose every field is
dead is an invitation for a future producer to fill it with a second, disagreeing
count.

### 6. A subagent's setback gets the same typed treatment

`SubagentEvent::RetryScheduled { attempt, max_attempts, delay_ms, message }` and
`SubagentEvent::RetryResolved { attempts, faults }` replace the child's
`Notice(ProviderRetry)` + `Activity("waiting to retry (3s)")` pair. The parent
step holds one `Option<Box<SubagentRetry>>`, a single type carrying the live
progress **and** the recovered fault list — so a recovered list can never be
rendered beside a live countdown.

The peek row reads its rank order explicitly: a parked human-decision wait
outranks a live setback, which outranks derived activity. The setback line comes
from `RetryProgress::label`, never from parsing the activity string.

`[INV-RETRY-06]` — **A subagent's retry is typed state, not prose.** No renderer
parses `waiting to retry (Ns)` to recover a count.

### 7. The diagnostic header stops claiming a live attempt count

`DiagnosticRecord::attempt` is removed. A transcript entry cannot carry a live
attempt pair any more, so the field had no producer; the recovered anchor states
its count in its title. `kind=provider-retry` disappears with the entry kind.

## Normative Invariants Established

- `[INV-RETRY-01]` **One attempt vocabulary** — `RetryProgress` composes every
  count and every phrase; the constructor clamps into `1..=max_attempts`.
  Enforced by: the type's private fields plus unit tests over the boundaries.
- `[INV-RETRY-02]` **No live retry row** — `MessageKind::ProviderRetry` does not
  exist (compile-time); the bar clause and the notice log are the two homes.
- `[INV-RETRY-03]` **Retry rows are anchors** — the only retry admission is
  `Append { class: TurnOutcome }`. Enforced by: `UpsertRetry`'s deletion.
- `[INV-RETRY-04]` **Mutations are single-purpose** — `Interrupted` records a
  stop and nothing else.
- `[INV-RETRY-05]` **`NoticeKind` is a receipt vocabulary** — exhaustive
  `preferred_layer`, no absorb-for-live-state row.
- `[INV-RETRY-06]` **Subagent setbacks are typed** — `SubagentRetry` is the only
  carrier; no string parsing at the render site.

## Consequences

- **Positive**: the bar, the receipt and the anchor cannot disagree about an
  attempt count — they read one value. The conversation no longer contains HTTP
  envelopes. The recovered round is now reported, so the asymmetry between "it
  recovered silently" and "it failed loudly" is gone. The last retry write path
  without an `EventClass` is closed. A transcript row never ticks, so the height
  cache works for notices again. `MessageKind` shrank (280 → 264 bytes at the
  largest variant) despite gaining subagent retry state, because the new state
  sits behind one boxed episode type.
- **Negative & trade-offs**: the full HTTP payload is now **ephemeral** — it
  lives in the notice log (capped, dies with the process) and the rolling client
  log file (named by every diagnostic record), not in the scrollback or the
  export. Mitigation: this is exactly ADR-0116's residency rule ("if it must
  survive, it is a fact first and a presentation second"), the notice viewer
  scrolls and copies, and the client log retains it. A user who wants the body
  in a bug report copies the receipt, not the transcript.
- **Negative & trade-offs**: a live retry is no longer visible *in the
  conversation's scroll position*. A user who scrolled up during a backoff sees
  the clause on the bar but not in the transcript. This is the intended trade:
  the bar is always on screen, the receipt is always retained, and neither
  pretends to be dialogue.
- **Neutral**: `RetryResolution` (the durable record type) keeps its shape; only
  its projection function stays retired. The live anchor is minted from
  `RetryProgress`, which carries the same facts.

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): keep the transcript card, fix the count and strip the JSON

- **Why considered**: the smallest possible change — one shared formatter, and
  `detail: None` on settle. It would have removed the visible contradiction and
  the payload in an afternoon.
- **Why rejected**: it leaves a ticking transcript row, a write path without an
  `EventClass`, and a mutation carrying a hidden second responsibility. It also
  preserves the asymmetry where a recovered retry leaves no trace at all. The
  defect was structural, so a cosmetic fix would have to be re-fixed the moment
  the next field was added.

### Alternative B (Rejected): store the retry phase on `RetryProgress`

- **Why considered**: `BackingOff` vs `Reattempting` looks like state, and a
  stored field would make the transition explicit.
- **Why rejected**: a stored phase needs a writer — a timer — and any tick it
  misses leaves the bar asserting the wrong state. Deriving it from the clock
  against `retry_at` makes the displayed state unable to drift from the scheduled
  one, and removes a whole class of "the countdown says next in 0s forever" bug.

### Alternative C (Rejected): keep `NoticeKind::ProviderRetry` and route it to the log

- **Why considered**: the notice already carried a human-readable summary; the
  log is the right home for receipts; only the `Absorbed` row looked wrong.
- **Why rejected**: it would leave the harness publishing the same retry twice
  (once typed, once as prose), and the prose copy would be a second formatter for
  the same count — the exact defect. The receipt is now written by the *client*
  from the typed event, so there is one producer and one vocabulary.

### Alternative D (Rejected): render the subagent setback from its `Activity` string

- **Why considered**: the child already emits `Activity("waiting to retry (3s)")`,
  so the parent could keep parsing it and skip a protocol change.
- **Why rejected**: parsing a count back out of a rendered sentence is how the
  principal round's two vocabularies diverged in the first place. `[INV-RETRY-06]`
  exists to forbid repeating it at the child level.

## Links

- Related ADRs: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (content residency), [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) (admission purity), [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) (notice log residency), [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md) (the notice renderer contract this keeps)
- Code: `crates/nuo-view/src/retry.rs`, `crates/nuo-view/src/model/document.rs` (`retry_recovered`), `crates/nuo-view/src/model/layer.rs`, `crates/nuo/src/shell/event_loop/mutations.rs` (`RetrySettled`), `crates/nuo/src/shell/mod.rs` (`RetryScheduled` / `RoundInterrupted` / `RoundCompleted` arms)
