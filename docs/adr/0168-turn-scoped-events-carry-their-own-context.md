# ADR-0168: Turn-Scoped Events Carry Their Own Context

- **Status:** Accepted
- **Date:** 2026-11-10
- **Related:** [ADR-0167](0167-a-discarded-attempt-carries-its-own-position.md), [ADR-0162](0162-stream-restarts-are-a-separate-budget-and-visible-attempts-are-discarded.md), [ADR-0136](0136-wire-drift-is-a-frame-not-a-disconnect.md), [ADR-0133](0133-transcript-attachment-and-hydration-invariants.md)
- **Amends:** ADR-0167 (the discard keeps its position; the position moves from per-variant fields to the turn envelope)

## Context

ADR-0167 made `StreamDiscard` carry its own position, so a re-attached
client could sweep a failed attempt without first observing `TurnStarted`.
That closed the destructive path, but it deliberately left the translator's
per-session whiteboards in place:

```rust
let mut positions_by_session = HashMap::<String, (u64, u64)>::new();
let mut turn_sent_at_by_session = HashMap::<String, u64>::new();
```

`positions_by_session` is written only by the `TurnStarted` arm and read by
every other event that creates or updates a positioned row: text deltas,
reasoning deltas, tool announcements and results, steering admission, and
command-result fallbacks. `turn_sent_at_by_session` is the sibling clock for
the band header.

The invariant is again structural rather than local:

> Every positioned event for a turn arrives after that session's
> `TurnStarted`, and no client ever attaches mid-stream without replaying it.

The second half is false. `ThreadSnapshot` rebuilds the transcript but does
not replay `TurnStarted`. A late joiner therefore has an empty whiteboard: it
creates unpositioned rows and cannot translate a later discard by position.
The same state is also invisible to replay, tests, forks, and any future
multi-client or concurrent-turn topology.

ADR-0167's own consequences named the remaining whiteboard as a future
decision. This is that decision: **remove it, do not evolve it.**

## Decision Drivers

- **Self-describing events.** A consumer must not need to have observed a
  different event to interpret the one in front of it.
- **Re-attachment parity.** A client that attaches mid-stream must build the
  same rows as a client that watched from the start.
- **Replay and fork.** Event order alone cannot be the carrier of transcript
  identity; the event carries it.
- **One coordinate system.** Wire turns are 0-based; transcript turns are
  1-based. The conversion belongs in one typed helper, not in every consumer.
- **No compatibility lane.** The wire already moves in clean breaks
  (ADR-0136); a second encoding of the same position is how architectures
  rot.

## Decision

### 1. A turn context is a typed wire value

```rust
pub struct TurnContext {
    pub round: u64,      // 1-indexed enclosing round
    pub turn: u64,       // 0-indexed model request
    pub sent_at_ms: u64, // one stamp for every component of the turn
}
```

The timestamp is part of the context, not a second per-session map. A turn's
thinking trace, answer text, and tool steps read the same value, live or
resumed.

### 2. `Round` carries a scope, and the scope is mandatory

```rust
pub enum RoundScope {
    Turn { context: TurnContext, event: RoundEvent },
    Session { event: RoundEvent },
}
```

`AgentResponse::Round` carries `thread_id` plus `scope`. A turn-scoped event
cannot exist without a context; a session event cannot accidentally claim
one. `RoundScope::event()`/`into_event()` are the only generic accessors.

`RoundEvent::TurnStarted` and `RoundEvent::StreamDiscard` become unit
markers. Their meaning is the envelope: "this turn started" and "discard this
turn's provisional rows". The old per-variant fields are deleted, not kept
alongside the context.

`AgentEvent::AssistantDiscard` returns to a unit internal event. The relay
attaches the current turn context when it translates the event to
`RoundEvent::StreamDiscard`.

`AgentEvent::SteerAdmitted` carries the context of the turn that will consume
the message. The agent supplies it at the admission boundary; the frontend
never infers `turn + 1`.

### 3. Producers track only while producing; consumers track nothing

The harness's relay keeps one `TurnContext`, refreshed at
`ModelRequestStarted` (the only point a turn can begin), and stamps every
turn-scoped event with it. Direct emitters (final text, interrupted text,
discard on recovery) use the same context. This is producer state, not a
client-side reconstruction: the wire event is complete when it leaves the
process.

The shell deletes `positions_by_session` and `turn_sent_at_by_session`.
`TurnStarted` updates the visible round/turn from the context; `Text`,
deltas, reasoning, and tool events read it directly. `transcript_turn` is the
only 0-based → 1-based conversion, and `stream_discard_edit` is the only
wire-discard → transcript-edit translation. A turn-only arm uses
`require_turn!`; a mis-scoped event is dropped rather than silently written
unaddressed.

`CommandResult` and pre-turn `Text` remain session-scoped: a command reply or
a prompt-blocked notice is not a model turn and cannot claim a position. Their
rows are addressed by their own invocation identity, not by a fabricated
turn.

### 4. Clean wire break

- `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` move to 26.
- `Round` changes shape; there is no old-field fallback and no dual path.
- The wire census records the new `AgentResponse`/`RoundEvent`/`RoundScope`
  shapes and the `TurnContext` struct. Its SHA-256 shape digest (ADR-0136
  amendment) makes this class of change impossible to land without the bump,
  including a field change inside the nested context.
- A v25 client cannot decode the envelope; the directional update/restart
  disclosure already covers the skew.

### Invariants & Behavioral Boundaries

- **`[INV-EVENT-01]` Self-Describing Turn Events**: every turn-scoped round
  event carries a `TurnContext`; no client-side per-session position or
  timestamp map may reconstruct it. `TurnStarted` ordering is
  not a correctness dependency. Enforced by the `RoundScope` type and
  `test_inv_event_01_turn_context_is_the_only_position_source`.

## Consequences

### Positive

- Re-attachment, replay, fork, and a late-joining second client are the same
  code path as the original client.
- Translator correctness is local: an event carries its identity; no hidden
  "current turn" lookup can miss.
- One timestamp source removes the second whiteboard and the class of
  "thinking and answer disagree about HH:MM" bugs.
- The shell loses two session-keyed mutable maps and their cleanup paths on
  thread switches.
- A new turn-scoped event cannot be added without choosing a scope; the
  compiler forces the question.

### Negative

- Every turn-scoped event carries `round`, `turn`, and `sent_at_ms`; streamed
  deltas grow slightly. The alternative is a client that must be lucky about
  event order. If framing cost ever matters, the right fix is a more compact
  framing, not a return to hidden state.
- `RoundScope` makes the `Round` envelope one level deeper. That is a
  deliberate, reviewable wire shape rather than a compatibility layer.
- One more hard protocol break (26). ADR-0136 accepts this cost and already
  provides the directional skew disclosure.

## Validation

- `crates/nuo-protocol/src/events.rs::
  turn_context_is_part_of_the_wire_shape` pins the envelope JSON, the context
  fields, and a decode round-trip.
- `crates/nuo-harness/tests/it/orchestration.rs::
  truncated_stream_discards_before_retrying_and_never_closes_reasoning`
  collects `(TurnContext, RoundEvent)`, asserts both attempts expose the same
  context, and asserts the discard's context is the failed attempt's.
- `crates/nuo/src/shell/tests/stream_discard.rs::
  wire_turn_zero_maps_to_the_first_transcript_turn` pins the one 0-based →
  1-based conversion and the wire-discard translation.
- `crates/nuo-server/src/serve.rs` tests pin that only same-context deltas
  coalesce; a turn change displaces the pending run.
- `test_inv_wire_census_matches_protocol_version` fails unless the protocol
  moves with the envelope shape; the checker includes `RoundScope` and
  `TurnContext` as explicit surface entries.
- `test_inv_event_01_turn_context_is_the_only_position_source` rejects a
  reintroduced `positions_by_session`/`turn_sent_at_by_session` in the shell
  translator.

## Rejected Alternatives

### Seed the whiteboard on re-attachment (rejected)

Adding a `TurnStarted` replay to `ThreadSnapshot` would fix one symptom while
keeping the hidden state. It still requires ordering, still cannot serve
replay or a second concurrent client, and still leaves every new event at risk
of forgetting the lookup. It is a patch on the architecture this ADR removes.

### Per-variant `round`/`turn` fields (rejected)

Sprinkling fields on each variant duplicates the coordinate, makes
`TurnStarted` and `StreamDiscard` carry redundant data, and invites drift
between "has a position", "has sent_at", and "has neither". The scope
envelope states the property once.

### Optional context on the envelope (rejected)

`Option<TurnContext>` would make "this event might own a turn" a runtime
question again. The two scopes make the answer structural: a `Turn` event has
a context by construction; a `Session` event is not a turn event.

### Keep the side channel for compatibility (rejected)

There is nothing to be compatible with: the wire is versioned, the client and
server are deployed together, and the protocol already treats an in-window
shape change as a clean break. Keeping both encodings would make the old
whiteboard authoritative in some paths and the event in others — the exact
rot this decision removes.

## Links

- [ADR-0167](0167-a-discarded-attempt-carries-its-own-position.md) — the
  destructive-path invariant this ADR generalizes.
- [ADR-0162](0162-stream-restarts-are-a-separate-budget-and-visible-attempts-are-discarded.md)
  — the sweep contract.
- [ADR-0136](0136-wire-drift-is-a-frame-not-a-disconnect.md) — the protocol
  number and shape census.
- [ADR-0133](0133-transcript-attachment-and-hydration-invariants.md) — the
  hydration payload that does not replay `TurnStarted`.
