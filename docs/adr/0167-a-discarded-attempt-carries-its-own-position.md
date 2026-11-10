# ADR-0167: A Discarded Attempt Carries Its Own Position

- **Status:** Accepted
- **Date:** 2026-11-08
- **Related:** [ADR-0162](0162-stream-restarts-are-a-separate-budget-and-visible-attempts-are-discarded.md), [ADR-0166](0166-a-tool-call-has-one-identity-and-the-tool-step-reducer-is-idempotent.md), [ADR-0136](0136-wire-drift-is-a-frame-not-a-disconnect.md), [ADR-0133](0133-transcript-attachment-and-hydration-invariants.md)
- **Amends:** ADR-0162 §"2026-11-08 Amendment: The Discard Sweeps Every Provisional Row"

## Context

ADR-0162's amendment made `StreamDiscard` sweep *every* provisional row an
attempt left behind, not just the trailing assistant row. It was written against
the shell's `TranscriptEdit::StreamDiscard { round: Option<u64>, turn:
Option<u64> }` and had to add a guard:

```rust
// A discard that names no position owns nothing.
if round.is_some() || turn.is_some() { /* sweep */ }
```

Without that guard a `(None, None)` discard would match every unaddressed row in
the transcript. With it, an unaddressed discard becomes inert.

Both readings are wrong, and the reason is structural rather than local.

### The position is looked up, not carried

`RoundEvent::StreamDiscard` is a unit variant. The harness knows the failed
attempt's position — it is the attempt that just returned, and
`StreamingRoundState` holds `turn_index` while `Agent::round_count()` holds the
round — but the event discards it. The shell therefore reconstructs it from a
**side channel**:

```rust
let mut positions_by_session = HashMap::<String, (u64, u64)>::new();
```

`positions_by_session` is written in exactly one place: the `TurnStarted`
handler. Every position-carrying edit — `StreamTextDelta`, `StreamTextFinalize`,
`ReasoningDelta`, `ToolStepUpsert`, `StreamDiscard` — then reads it back with
`.get(&session_id).copied()`.

That makes the shell's correctness depend on an invariant no type expresses:
*"`TurnStarted` for this session has been observed before any of this attempt's
edits arrive."* Nothing enforces it, and it is not true.

### The invariant breaks on re-attachment

`ThreadSnapshot` (`[INV-ATTACH-04]`, ADR-0133) rebuilds the visible transcript
from the hydration payload. It does **not** repopulate
`positions_by_session` — that map is only ever written by `TurnStarted`. So a
client that attaches to a session which is *already streaming* has a
fully-rebuilt transcript and an empty position map.

From there:

- `StreamTextDelta` falls back to its `created` template with `round`/`turn`
  unset, so the next attempt's row is unaddressed;
- `StreamDiscard` resolves to `(None, None)`, and the guard added by ADR-0162
  makes the sweep a **no-op**.

The failed attempt's provisional rows therefore survive re-attachment, and the
retry — which does receive a `TurnStarted` and so *does* have a position —
appends into them. This is exactly the fused-attempt defect ADR-0162 set out to
remove, reachable again through a different door.

The `Option` did not protect against this; it **encoded** it. A `None` that
silently disables a destructive operation is the failure mode, not a safety net.
The guard only changed silent data loss into silent staleness.

### The position is a property of the event

There is no reason for a discard to be addressable only through a lookup. "Discard
the visible output of the attempt that just failed" is a statement *about a
specific attempt*; that attempt has a round and a turn. The harness is the only
party that knows which attempt failed, and it is the party emitting the event.

This is already the house style elsewhere: `subagent_tool.rs` captures
`(round, turn)` from `ModelRequestStarted` and threads it through the events it
emits, rather than making the consumer re-derive it.

## Decision

**A discard carries the position of the attempt it discards.**

1. `RoundEvent::StreamDiscard` becomes `StreamDiscard { round: u64, turn: u64 }`.
   The harness fills it from `agent.round_count()` and
   `streaming_round.committed_turns()` at the emit site. `turn_index` is only
   incremented *after* a turn commits, so on the failure path it is still the
   failed turn's index.

2. `TranscriptEdit::StreamDiscard` becomes `{ round: u64, turn: u64 }` — not
   `Option`. A discard without a position is not a value the program can
   construct, so the `if round.is_some() || turn.is_some()` guard is deleted
   along with the `(None, None)` inertness rule it existed to enforce.

3. The shell's `StreamDiscard` arm uses the carried position directly and no
   longer consults `positions_by_session`. The wire ordinal is 0-based
   (`TurnStarted` uses the same coordinate); the transcript row is 1-based, so
   one shared `transcript_turn` helper performs that conversion. A discard that
   skipped it would address no row and silently no-op.

4. The harness's internal `AgentEvent::AssistantDiscard` carries the same
   `{ round, turn }`. The text-fallback tool-call path is translated one-for-one
   into a `RoundEvent::StreamDiscard`; without the position on the agent event,
   that translation would have to invent one or re-run the lookup.

The remaining position-carrying edits keep their lookups for now. Their `None`
fallback degrades to an *unaddressed* row rather than a silently-skipped
destructive operation, which is a visible-but-harmless failure. Carrying the
position on every event is the consistent end state; doing it in one step would
mean re-plumbing the whole translator, and this ADR deliberately scopes itself to
the variant where `None` is unsafe rather than merely untidy.

## Consequences

### Positive

- The re-attachment hole closes: a discard now sweeps correctly regardless of
  whether `TurnStarted` was observed, because it no longer depends on that.
- A whole class of "silently disabled because a lookup missed" defects is
  removed from the destructive path. The type says a discard is addressed.
- The shell stops maintaining a second source of truth for the position of the
  attempt it is destroying.

### Negative

- `positions_by_session` remains for the other edits, so the shell still holds a
  side channel — now with a narrower blast radius. A future ADR should retire it
  by threading positions through every event; this one only removes the
  dangerous dependency.
- Every `RoundEvent::StreamDiscard` construction site must supply the position.
  There are three in the orchestration retry loop — the overflow-recovery
  discard, the image-probe discard, and the ordinary failure discard — plus the
  `AgentEvent::AssistantDiscard` translation. They sit after the failed attempt
  returns, so the value is mechanical, but the compiler now enforces that a new
  site cannot omit it.

### Neutral

- `RoundEvent` is wire-only (it is never persisted), so widening the variant is a
  clean break with no migration and no compatibility shim. Verified: no
  `RoundEvent` serialization to disk exists.
- The change is a wire break: the unit variant encoded as the string
  `"StreamDiscard"`, while the struct variant encodes as
  `{"StreamDiscard":{"round":…,"turn":…}}`. `PROTOCOL_VERSION` and
  `MIN_PROTOCOL_VERSION` move to 25; there is no compatibility lane. The wire
  census records the shape digest as well as the variant names (ADR-0136
  amendment), so this class of change cannot pass unnoticed again.

## Validation

`crates/nuo-harness/tests/it/orchestration.rs`:

- `truncated_stream_discards_before_retrying_and_never_closes_reasoning` — a mock
  provider streams reasoning + text and then fails the transport. Asserts the
  emitted `StreamDiscard` carries exactly the `(round, turn)` of the attempt's
  `TurnStarted`, that both attempts open at that same position, that the discard
  precedes `RetryScheduled`, that the reasoning delta actually reached the
  client, and that the failure path emits no `ReasoningEnd`.

`crates/nuo-protocol/src/events.rs`:

- `stream_discard_position_is_part_of_the_wire_shape` — pins the externally
  tagged JSON shape (including both fields) and a decode round-trip.

`crates/nuo/src/dev/wire_census.rs`:

- `a_shape_change_without_a_bump_is_rejected` — the same variant names with a
  changed field list fail the protocol gate.
- `a_bump_with_a_shape_change_is_accepted` — a moved protocol plus a changed
  shape is a valid wire move.
- `shape_ignores_layout_and_comments_but_not_fields` — rustfmt/comment churn is
  invisible to the digest, while a field type or serde tag is not.

`crates/nuo/src/shell/tests/stream_discard.rs`:

- `wire_turn_zero_maps_to_the_first_transcript_turn` — the 0-based wire ordinal
  and 1-based transcript ordinal share one conversion helper; the discard cannot
  silently address no row.
- The sweep's shapes (multiple provisional rows, the `Ingesting` tool step as a
  provisional floor, a dispatched step as a committed floor, cross-turn
  isolation).
- `discard_without_a_position_owns_nothing` is **deleted**: the state it
  described is no longer representable.
