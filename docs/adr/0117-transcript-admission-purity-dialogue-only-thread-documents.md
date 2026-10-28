---
id: ADR-0117
title: "Transcript Admission Purity: Dialogue-Only Thread Documents, Exhaustive Event Classification, and Ledger-Derived Operation Rows"
status: accepted
date: 2026-12-09
scope: protocol/nuo-protocol, presentation/nuo-view, host/nuo, tui/transcript, tui/event-loop, persistence/nuo-persistence
superseded_by: null
negative_knowledge: true
---

# 0117. Transcript Admission Purity: Dialogue-Only Thread Documents, Exhaustive Event Classification, and Ledger-Derived Operation Rows

- Status: Accepted
- Date: 2026-12-09
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime Kernel, Protocol, Persistence, Interface Architecture
- Amends: [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) — the command *invocation* remains durable and the *reply* remains ephemeral, now made structural rather than conventional; [ADR-0116](0116-content-residency-and-surface-layer-topology.md) — supplies the layer that non-dialogue content moves to

---

## Context and Problem Statement

The durable model already separates concerns correctly. `TranscriptEntry` stores immutable **facts** and
`ProjectionDirective` stores **decisions**, with every consumer window a pure derivation
(`nuo-protocol/src/transcript.rs`). Slash commands live in a durable ledger, not the message stream, and
the doctrine is written down verbatim: *"The message stream is pure dialogue; these records are the
operations that happened."* Round interrupts and retry resolutions are likewise *"projection record[s],
not conversation message[s]"*, stored beside the ledger and re-projected on resume.

The **live** path does not honour any of this. `RoundEvent` is a single union carrying dialogue
(`StreamDelta`, `Text`, `Reasoning*`), operations (`CommandResult`), telemetry (`ContextTokens`,
`TurnPerformance`), interaction bookkeeping (`QueueUpdated`, `SteerAdmitted`, `FollowUpStarted`), and
ephemeral notices. The envelope distinguishes only *which session* an event belongs to
(`AgentResponse::Round { session_id, event }`); nothing on the wire distinguishes *which class* it is.
The frontend therefore classifies ad hoc, per arm, and the shell method that renders a round is the
de-facto admission controller for the transcript:

- `RoundEvent::CommandResult` appends a command row into the rendered transcript.
- `RoundEvent::Notice` appends unless the producer set the `Toast` surface (see
  [ADR-0116](0116-content-residency-and-surface-layer-topology.md)).
- `RoundEvent::Compacted` / `RoundInterrupted` append projection cards.

This produces four defects:

1. **Admission is untyped.** A new `RoundEvent` variant lands in the transcript by whichever arm is
   written for it; the default behaviour for "content that is not dialogue" is residency in the time
   axis.
2. **Live and resume implement different models of the same concept.** Live pushes an optimistic command
   row and later *settles* it (`TranscriptEdit::SettleCommandResult`); resume discards the historical
   command-echo rows and re-joins the ledger by timestamp seam
   (`shell::transcript::merge_command_rows`). Two mechanisms for one rendering guarantee cannot stay
   congruent, and every future row type has to be implemented twice.
3. **Conversation semantics are damaged by non-conversation rows.** `round` and `turn` are properties of
   *saying something*. An operation row that occupies a position in the rendered list has no round,
   forcing round-stamping heuristics (`StampTurnPrompt`, `RebaseRounds`) to step around it.
4. **The transcript stops being answerable.** Given the rendered thread, a reader cannot decide whether
   "everything in here was said by someone, in this order" — the one property a transcript is for.

## Decision Drivers

- **`[INV-LAYER-01]` restated at the write boundary** — one home per content; the thread document is the
  home of dialogue and turn structure only.
- **Admission must be unreachable by accident** — the absence of a decision must not produce residency,
  and there must be no ambient `push` on the transcript.
- **Live and resume must be one function** — the rendered thread at attach time and the rendered thread
  after a live event must be produced by the same join.
- **Total order is load-bearing** — the seam merge that positions ledger rows among dialogue requires a
  single ordered stream; splitting the wire into per-class channels would lose the interleaving order.
- **Zero legacy burden** ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)) —
  delete the dual mechanisms and the projection workarounds; do not shim them.

## Architectural Decisions

### 1. `RoundEvent::class()`: one exhaustive classification, on the wire type

The wire keeps **one ordered enum** — the interleaving of dialogue and operations is a fact the renderer
needs — but every variant declares its class, adjacent to its definition:

```rust
// nuo-protocol/src/events.rs
pub enum EventClass {
    /// Someone said something. The only class admitted to L0.
    Dialogue,
    /// How a round ended (completed / interrupted / retry-resolved). Admitted as an *anchor*.
    TurnOutcome,
    /// An operation happened (command invocation, configuration mutation). Never admitted.
    Operation,
    /// Counters, timings, projections of context size. Never admitted.
    Telemetry,
    /// Interaction bookkeeping and one-shot notices. Never admitted (L1/L2/L4).
    Ephemera,
}

impl RoundEvent {
    pub fn class(&self) -> EventClass;   // exhaustive; no wildcard arm
}
```

`[INV-TRANSCRIPT-03]` — classification completeness. Adding a variant fails the build until its class is
chosen, in exactly one place, next to the definition. This is the *audit surface*: the list of what may
enter the transcript is one function, not the union of every `match` in every frontend.

### 2. `ThreadDoc` owns L0 and admits only classified dialogue

```rust
// nuo-view/src/model/thread.rs
pub struct ThreadDoc { /* facts: Vec<TranscriptMessage> */ }

impl Deref for ThreadDoc { type Target = [TranscriptMessage]; }

impl ThreadDoc {
    /// The single admission gate. Dialogue becomes a fact, TurnOutcome becomes an
    /// anchor, every other class is refused and the row is dropped.
    pub fn admit(&mut self, class: EventClass, message: TranscriptMessage) -> Admission;
    pub fn reset_from(&mut self, facts: Vec<TranscriptMessage>);   // resume / fork / switch
    // presentation edits on already-admitted facts only:
    pub fn facts_mut(&mut self) -> &mut [TranscriptMessage];      // a slice: cannot grow
    pub fn find_last_mut(..) / retain(..) / remove_last(..) / clear(..)
}

pub enum Admission { Dialogue(u64), Anchor(u64), Refused(EventClass) }
```

`[INV-TRANSCRIPT-02]` — **one admission entry point.** There is no `push`, and the presentation mutators
hand out a slice or a single message by predicate, so they cannot grow the document. "How did this get
into the transcript?" is answerable by reading one function.

`[INV-TRANSCRIPT-03]` — **admission is gated on the declared class**, not on the caller's intent. The
gate lives at the app boundary in the transcript edit channel (`TranscriptEdit::Append { class, message }`
→ the applier admits only `Dialogue`/`TurnOutcome` and refuses the rest), so every producer of a
transcript row states its class at the point of emission, and `RoundEvent::class()` (exhaustive, no
wildcard) is what supplies it for event-driven rows.

### 2a. The rendering buffer is fed only through the gate

`App`'s message buffer remains the renderer's flat working set (layout caches, selection and focus are
keyed by message id and iterate it as a slice). It is no longer an open vector that any event arm may
append to: every append in the shell goes through the class-carrying edit, so the buffer's contents are
exactly the admitted rows. The model of record — the class-aware document — is `ThreadDoc`, and it is
where the gate's rules are stated and tested.

### 3. Operation rows are derived from the ledger, identically live and on resume

The rendered thread is a **join** of two sources, produced by one function:

```text
L0 facts (ordered, append-only)  ─┐
                                  ├─► join by timestamp seam ─► rendered rows
ledger (ordered, replace-only)   ─┘
```

- The ledger (`session.commands`) is the home of operation records. It is *replace-only*: a mutation
  announces a new ledger snapshot, and the derived operation rows are rebuilt from it.
- Because the ledger row must render *among* dialogue by timestamp, the joined view is still materialised
  into the same rendering buffer. The join is therefore a projection step over `(facts, ledger)`, not a
  second append path: an operation row is admitted with `EventClass::Operation` semantics recorded at its
  production site, and its body is owned by the ledger — the row is a presentation of a record, not a
  statement.
- The convergence still outstanding is mechanical: `TranscriptEdit::SettleCommandResult` and the resume
  path (`transcript_commands_from_ledger` + `merge_command_rows`) must be replaced by one join function so
  that a live thread and a resumed thread are produced by identical code. Until then the two paths agree
  by construction of their inputs (same ledger, same timestamps) but not by construction of their code —
  which is why `[INV-TRANSCRIPT-04]` is stated as the target and pinned by a test rather than claimed as
  already true.

`[INV-TRANSCRIPT-04]` — **live equals resume.** Any row that cannot be produced by the join does not
exist; a live-only row is a defect by construction.

### 4. Turn structure is admitted as an anchor, not as prose

Round outcomes — interrupted, retry-resolved, completed — are *projection records* by their own
documented design. They enter L0 as `TurnAnchor`: a timestamped, non-conversational marker carrying an
id, a short label, and a pointer to its durable record. The anchor is what preserves the round/turn
sequence; the record's body lives in the store (and, for anything a human needs to read at length, in the
surface log of [ADR-0116](0116-content-residency-and-surface-layer-topology.md)).

Compaction checkpoints keep their existing treatment: a checkpoint is an `EntryOrigin::Checkpoint`
fact standing in for an elided range, because it *does* change what the conversation is.

### 5. `model_window` is a projection of dialogue only

Nothing in this ADR changes the durable derivation; it *restores* it. `model_window` remains the pure
projection of transcript facts, and the invariant is now also true of the live rendered document.
Data-shape tolerance for pre-ledger rows (historical `CommandEcho` user messages) stays confined to the
persistence read boundary as a migration concern; it is explicitly **not** a live admission path, and no
live producer can create such a row.

## Normative Invariants Established

- `[INV-TRANSCRIPT-01]` **Dialogue-only residency.** L0 contains dialogue facts and turn anchors. Every
  other row class is refused at admission or lives in another layer. Enforced by: `ThreadDoc::admit`
  returning `Admission::Refused` for `Operation`/`Telemetry`/`Ephemera`, and the applier dropping those
  classes instead of pushing them.
- `[INV-TRANSCRIPT-02]` **Single admission entry point.** Enforced by: `Deref`-only reads on `ThreadDoc`,
  `facts_mut()` handing out a slice, and no `push` anywhere in the type.
- `[INV-TRANSCRIPT-03]` **Exhaustive classification.** Every `RoundEvent` variant declares an
  `EventClass`; the match has no wildcard arm. Enforced by: compiler exhaustiveness.
- `[INV-TRANSCRIPT-04]` **Live equals resume.** Operation rows and anchors are produced by the same join
  for both paths. Enforced by: the join being a single function fed by (facts, ledger) in both cases, plus
  the acceptance test that a resumed thread equals a live thread built from the same facts and ledger.

## Consequences

- **Positive**: "everything in the thread was said" becomes checkable. Round/turn stamping stops fighting
  foreign rows. Adding a new operation or telemetry event cannot pollute the transcript without deleting
  an invariant and a conversion function.
- **Positive**: the live and resumed renderings are the same code path, so the class of bug where a
  resumed session shows different rows than the live one is retired rather than patched.
- **Negative & trade-offs**: `ThreadDoc`'s narrow API forces the streaming engine, selection, and layout
  caches to address messages by id or predicate instead of by slice index. Cost: a mechanical refactor of
  the shell's transcript mutations. Benefit: the invariants are enforced by the compiler rather than by
  review.
- **Negative & trade-offs**: classification lives at the *consumption* side rather than in distinct types,
  so a reviewer must trust `class()` rather than the constructor. Mitigation: `class()` is one exhaustive
  function adjacent to the enum, and the gate is one function; both are test targets
  (`every_variant_is_classified`, `no_non_dialogue_class_admits_a_fact`).

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): keep the `surface`/layer hint on the payload

- **Why considered**: Nearly free — the field already exists, so the frontend could simply stop honouring
  it for the transcript.
- **Why rejected**: A hint the frontend must ignore is worse than no hint: it documents an intent nobody
  implements. Layer ownership belongs to the surface
  ([ADR-0116](0116-content-residency-and-surface-layer-topology.md)); the payload carries meaning only.

### Alternative B (Rejected): append-then-filter projections

- **Why considered**: This is what the codebase did — append the row, then project it out before the
  provider request (`is_command_echo`, the pre-ADR-0050 era).
- **Why rejected**: It converts an admission question into a *removal* question, so every consumer must
  re-implement the filter. It failed exactly as predicted: filters accumulated in
  `prepare_request_messages`, `state::enrich`, `causal_compactor`, the session projection, and the
  restore path, and the live path still showed the row. Admission purity is cheaper than universal
  removal.

### Alternative C (Rejected): split the wire into three independent channels

- **Why considered**: `DialogueEvent` / `OperationEvent` / `EphemeralEvent` as separate streams is the
  strongest possible type-level statement, and it was the first proposal.
- **Why rejected**: It destroys the total order. The seam merge positions ledger rows among dialogue
  using their timestamps from one stream; independent channels arrive interleaved arbitrarily, so the
  rendered order becomes a race. Classification keeps one ordered stream and moves the guarantee to the
  single admission gate, which is where it is actually consumed.

### Alternative D (Rejected): render operations in the transcript as collapsed rows

- **Why considered**: Users want to see what happened, in place and in order.
- **Why rejected**: An operation is not a thing that was said. Rendering it as a row makes it part of the
  conversation for every consumer (export, search, summarisation, review, attach replay), and it breaks
  the round sequence. The join gives the same reading experience — an operation appears at its moment —
  without granting it conversation membership.

## Links

- Related ADRs: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (where non-dialogue content lives), [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) (durable invocation / ephemeral reply), [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md) (zero legacy burden), [ADR-0104](0104-fork-client-projection-reconciliation-and-aside-retirement-completion.md) (projection reconciliation on resume)
- Successor: [ADR-0118](0118-dialog-conversation-sovereignty-ephemeral-scratch-and-explicit-promotion.md) (dialog conversations never hold a thread document)
- Code: `crates/nuo-protocol/src/events.rs` (`EventClass`, `RoundEvent::class`), `crates/nuo-view/src/model/thread.rs` (`ThreadDoc`, `Admission`), `crates/nuo/src/shell/event_loop/mutations.rs` (`TranscriptEdit::Append { class, .. }`), `crates/nuo/src/shell/transcript.rs` (ledger join)
