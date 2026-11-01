---
id: ADR-0137
title: "Steering Admission Provenance and the Turn-Seam Interjection: Stop Staging Untrue Optimism in the Thread, Then Render One-Body"
status: accepted
date: 2026-12-16
scope: tui/nuo, presentation/nuo-view, protocol/nuo-protocol, harness/nuo-harness, persistence/nuo-persistence
superseded_by: null
negative_knowledge: true
---

# 0137. Steering Admission Provenance and the Turn-Seam Interjection

- Status: Accepted
- Date: 2026-12-16
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md), [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md), [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md)

---

## Context and Problem Statement

A user message issued while a ReAct round is running is admitted by the harness
atomically at the next safe turn boundary (`steering.rs`), and the durable
transcript therefore records it **at the seam between Turn N and Turn N+1**.
The TUI, however, staged the same message into its live working set
(`App::messages`) **at the instant the user pressed Enter** — in the middle of
Turn N's tool stream. The same dialogue therefore rendered two different causal
orders: live, the steer sat inside Turn N's tools; after resume, it sat between
the turns.

This is not an aesthetic gap. It is an **admission-provenance lie** that broke
the guarantees of ADR-0117 (`[INV-TRANSCRIPT-02]`: *one admission entry point,
refusing non-facts*). The optimistic staged entry was a type of untruth that
the document's gate permitted because the shell bypassed the gate by writing
directly into the buffer. The defect carried real costs:

- **Space**: each pending steer consumed the full round-level user-panel
  chrome — a header row, header/body gap, top and bottom solid transition rows
  — so a one-line steer demanded ~6 rows of transcript at the exact moment the
  streaming output needed that space (user complaint: "它直接追加到 transcript
  最下面，挤占空间").
- **Causal lies**: the live view placed the insert mid-turn while the persisted
  story placed it at the seam (deterministic divergence between live and
  resumed rendering of the same conversation).
- **Visual weight**: an in-round course correction that opens no new round was
  drawn with full round-opening panel weight, so readers could not distinguish
  "new topic" from "tactical correction".

One additional defect surfaced during the audit: `DeliveryStatus::HeldNextRound`
and its `TranscriptEdit::HoldInserted` machinery, and the "held for next round"
rendering branch, are **dead on every production path** — no producer exists.
`SteerUnavailable` already hands the content back to the outbox or composer;
no transcript entry is ever left in a "held" state. A third untruth was hiding
here: *dead state that implies a lifecycle that does not exist*.

## Decision Drivers

- **Honesty of the transcript**: a message must enter the thread only when it
  has actually been admitted, at its real causal seam (ADR-0117).
- **Zero drift between live and resumed rendering** of the same conversation.
- **A one-row ceiling** for the transient footer chrome above the composer
  (ADR-0062 `[INV-FOOTER-02]`), and **at most one body row cost** for an
  admitted in-round interjection.
- **Protocol-faithful cancellation**: `AgentRequest::CancelSteer` exists and is
  implemented server-side; the shell has never wired it. The pending-steer
  recall path is restored without inventing a new verb.
- **Negative knowledge**: dead lifecycle state (`HeldNextRound`,
  `HoldInserted`) must be deleted, not documented around.

## Architectural Decisions

### 1. Busy-Enter steering is staged out-of-thread until `SteerAdmitted`

- `handle_send_steer` no longer appends a `DeliveryStatus::Queued`
  `TranscriptMessage` to `App::messages`. Instead it stages the steer in the
  session's **outbox** (the existing `VecDeque<QueuedDispatch>` that already
  owns follow-up lifecycle) with a new `state = QueuedDispatchState::Steering`.
- Outbox items are never rendered in the transcript; they feed the existing
  consolidated `ActivityStrip` (count + preview) and the `Ctrl+Q` queue modal —
  both already owned surfaces, so the shell adds **no new footer row**
  (`[INV-FOOTER-02]`).
- Admission is observed from the wire: `RoundEvent::SteerAdmitted(id)` is the
  only event that may materialize the message in `App::messages`, and it
  **appends** (`TranscriptEdit::InsertAdmittedSeam`):
  - the harness persists the steer at the seam, after the finished turn's last
    component and before the next turn's provider request;
  - `SteerAdmitted` arrives before the next `TurnStarted` on the same channel,
    so at that instant the buffer tail *is* the seam;
  - appending therefore reproduces the durable order exactly.

  Appending is load-bearing, not incidental: `App::messages` holds the whole
  thread, so head insertion would put a round-N steer above round 1 for any
  conversation with history. (That was the first implementation of this ADR, and
  it was wrong; `[INV-STEER-02]` now pins the seam-in-place rule with a
  multi-round fixture, because a single-message test cannot distinguish the
  two.)

  `RoundEvent::FollowUpStarted` uses the same edit: a follow-up opens a fresh
  round, so its admission is the same materialization point. The former
  `SettleInserted` — which settled an optimistic in-thread row that no longer
  exists — is deleted with the staging it served.

### 2. `Tags` field carries admission provenance for the renderer

- `UserMessageOrigin::Steer` stays (identity), and a new `origin_tags` field
  carries **admission provenance facts**:

```rust
pub enum SteerOriginTag {
    /// Admitted at the turn seam of a running round (SteerAdmitted).
    InRound,
    /// "Follow-up" content admitted as a fresh round's opening prompt
    /// (FollowUpStarted) — render at round weight.
    RoundOpening,
}
```

- `TranscriptMessage::with_origin_tags(Vec<SteerOriginTag>)` and
  `TranscriptMessage::has_origin_tag(SteerOriginTag)` builders are added; the
  field defaults empty and is **never serialized** (it is render metadata
  reconstructed by the restore engine from the durable `InjectionKind`).
- The renderer chooses weight by **`InRound` presence**, not by
  `UserMessageOrigin`:
  - `InRound` → turn-seam interjection (one body row, no header row, no solid
    transition rows);
  - `RoundOpening` / absent → full round-level user panel (unchanged existing
    behavior).

### 3. The turn-seam interjection rendering

For an `InRound` steer the shell renders a compact one-body-row block:

```text
› steer   不要改协议层，只改 nuo-view 布局      14:21
```

- The first row is a `MetaStrip` lead `› steer` (info-tone bold) + the text
  (user text color) + right-aligned `HH:MM` (muted); there is **no separate
  on-surface header row**, **no header/body gap row**, and **no solid
  transition (padding) rows**.
- Wrap continuation rows are indented under the text column and carry the same
  user-panel background band; the block therefore costs exactly
  `1 + wrapped_lines` rows.
- Selection remains character-level; the layout-map registration is unchanged
  except for the reduced prefix geometry.
- A user message always terminates a turn band. The `is_steer_insert`
  group-absorption rule is **deleted**: it existed only to paper over the
  optimistic mid-turn staging this ADR removes, and a rule that can absorb a
  seam-ordered steer into the band it now sits *after* is actively wrong. A
  restored legacy buffer arranges its steers with the same one rule as live
  output.

### 4. `FollowUpStarted` is the only opener of a round-level user panel for queue items

`FollowUpStarted` already means "a fresh round was started" (`handlers_chat`),
so the restored/rendered origin tags mark it `RoundOpening` — the existing
round-level panel is the correct weight for it.

### 5. Pending-steer recall and deletion flow through the canonical outbox verbs

- `CancelSteer` is now sent on queue-modal recall (and `D`, `J`/`K` reorder)
  for `Steering` items, exactly as the protocol's `exactly-one-of-admitted /
  cancelled` contract (ADR-0025-era linearization at `events.rs`) demands.
- On `RoundEvent::SteerCancelled`, the shell restores the item's text to the
  composer (draft) and removes the outbox row; no transcript mutation occurs.
- On `RoundEvent::SteerAdmitted`, the shell materializes the message and
  removes the `Steering` outbox row.
- `RoundEvent::SteerUnavailable` continues to (a) remove the outbox row and
  (b) restore the text to the composer — the same "never re-queue as
  follow-up, never leave a held state" rule the `handle_esc_interrupt` path
  already encodes.

### 5a. The retired standalone queue-bar renderer is deleted

`chrome/queue_bar.rs` carried a 189-line `draw_queue_bar` with no production
caller (ADR-0062 `[INV-FOOTER-02]` had already merged the row into
`ActivityStrip`). Keeping an uncalled second renderer of the same row is the
drift that invariant exists to prevent, so the renderer and its five tests are
deleted; the projection *types* (`QueueItemProps` / `QueueBarProps`) remain
because the live surfaces consume them as data.

### 6. Deletion of the dead third state

- `DeliveryStatus::HeldNextRound` is removed from `nuo-view` (and every match
  arm in the shell).
- `TranscriptEdit::HoldInserted` and `App::hold_pending_round()` are removed;
  no producer exists, and existence implies a lifecycle that does not exist.
- The `user_surface_queued` theme token and its queued-band branch are removed;
  the "queued" visual distinction no longer has a transcript consumer (queued
  items are outbox rows, not transcript rows).
- The `"held for next round"` label branch is deleted with it.

### 7. Admission is gated at the document, not the buffer

`App::messages` is now fed strictly through `ThreadDoc::admit`-style entry
points. The shell's optimistic buffer writes for steers are removed; the only
way a steer message enters the document is `RoundEvent::SteerAdmitted`
(append-at-head) or a restored projection (which only contains admitted
seam-ordered messages). This restores `[INV-TRANSCRIPT-02]` mechanical
enforcement for the live path.

## Normative Invariants Established

- `[INV-STEER-01] Pending steer keeps out of the thread`: no `Steer` request
  may append to `App::messages`; the only materialization is
  `SteerAdmitted`. Enforced by a test: after `handle_send_steer`, the
  message list is unchanged and the outbox holds a `Steering` item.
- `[INV-STEER-02] Admitted steer lands at the seam`: the admission edit appends
  at the buffer tail, which is the turn seam at admission time. Enforced by
  `admitted_steer_lands_at_the_seam_after_existing_history`, whose fixture is a
  three-round thread — it fails if the implementation inserts at the head, which
  a single-message fixture cannot detect.
- `[INV-STEER-05] One outbox population`: the queue modal's rendered list, the
  activity strip's preview, the `N queued` badge, the per-session count, the
  index-addressed mutators and the keymap context all derive from a single
  predicate (`App::is_outbox_visible`) via `outbox_projection` / `pending_count`
  / `outbox_total`. Enforced by
  `outbox_display_index_and_mutation_index_address_the_same_item`,
  `recall_index_matches_the_displayed_row` and
  `every_outbox_surface_reports_the_same_population`.
  - Before unification there were **three** populations under one name: the
    modal listed every row of the session, the addressable mutators filtered by
    `Waiting`, and the counters counted everything. With a mixed outbox, `D`
    deleted a different item than the highlighted one.
- `[INV-STEER-07] Deletion intent cannot outlive its item`: the flag recorded by
  a modal deletion is cleared on the single terminal path for the id
  (`DispatchRemoved`, plus the two arms that end an id directly). Enforced by
  `deletion_intent_is_cleared_when_the_item_leaves`.
  - The failure it prevents: a deletion followed by `SteerAdmitted` (the race
    the protocol permits) leaves the item gone but the flag set; a later recall
    reusing the id would then be silently swallowed.
- `[INV-STEER-03] Interjection is one body row`: an `InRound` steer accounts for
  exactly `1 + wrapped_lines` rows — no header row, no header/body gap row, and
  **no top or bottom transition row**. Asserted on the renderer's own height
  counter plus the painted grid, paired with a contrast test proving the
  untagged form still pays the panel rows (otherwise the assertion would hold
  vacuously). The bottom transition row was the defect this test caught: the
  first implementation gated only the top one, so every interjection silently
  cost one extra row.
- `[INV-STEER-04] No pending state in the document`: `DeliveryStatus` has no
  queued/held variant, `TranscriptMessage::queued` / `hold_pending_round` are
  gone, and `Theme::user_surface_queued` is gone. A row in the thread is never
  waiting for anything; pending items live in the outbox. Enforced by the
  compiler (deleted identifiers) plus the exhaustive-match build.
- `[INV-STEER-06] No second outbox renderer`: the outbox has exactly one live
  rendering surface family — the consolidated `ActivityStrip` row and the queue
  modal. The retired standalone `draw_queue_bar` is deleted rather than kept
  uncalled; a second renderer of the same row is the drift ADR-0062
  `[INV-FOOTER-02]` exists to prevent.

## Consequences

### Positive

- Live and resumed transcripts render the same causal order for every round:
  the steer sits at the seam between turns, matching the durable story.
- Pending steers cost the transcript **zero rows** and exist only in the
  always-on single-row activity strip plus the existing queue modal; the
  streaming output keeps the viewport.
- An admitted interjection renders at one body row, freeing ~5 rows per steer
  at peak streaming.
- The `CancelSteer` protocol path (client → server → `SteerCancelled`/
  `SteerAdmitted` linearization) is finally wired; no bespoke recall was
  invented.
- Dead lifecycle state is deleted, not documented; the transcript's
  `DeliveryStatus` now has exactly the states that can occur.

### Negative

- The full text of a pending steer is no longer visible in the transcript
  while waiting; visibility lives in the strip preview and the queue modal.
  This is accepted: the queue modal is the canonical outbox surface, and the
  strip preview preserves glanceability.
- Header-banded optimistic styling is gone; the composer no longer reflects
  a pending steer's full panel, only the outbox count.
- Restored legacy buffers that predate this ADR still contain appended steers;
  they render at round weight. The `is_steer_insert` absorption rule is
  retained as a legacy-compat path and documented as such.

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): Render pending steers as a compact in-transcript pending row

- **Why considered**: User-visible immediacy of the full text before admission.
- **Why rejected**: Any in-thread row — even a slim one — is a transcript fact
  that may be revoked, which re-imports the exact live/resume divergence with
  a smaller footprint. It also requires a new admission class in
  `EventClass`/`ThreadDoc` that is *pending by nature*, destroying the
  dialogue-only guarantee. The outbox surfaces already exist and are the
  correct home; the extra row was not worth re-breaching `[INV-TRANSCRIPT-02]`.

### Alternative B (Rejected): Reposition on admit by message id

- **Why considered**: Minimal change — keep optimistic staging, move to the
  seam on `SteerAdmitted`.
- **Why rejected**: Repositioning is a second write that resurrects a not-true
  intermediate state and leaves a window where the lie is rendered; it also
  requires layout-cache invalidation and selection bookkeeping for a row that
  should never have existed. Clean break: no optimistic row at all.

### Alternative C (Rejected): Add a third `QueuedDispatchState` permanent track for steers, keeping follow-ups separate

- **Why considered**: Keep follow-up outbox semantics unchanged.
- **Why rejected**: Unifies the outbox entry point (one `pending_dispatch`
  deque), which is simpler than two tracks and lets the existing queue modal
  and activity strip handle steers without new surfaces. The `Steering`
  variant is one field on the existing struct, not a parallel track.

## Links

- [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) — admission gate, `[INV-TRANSCRIPT-02]`
- [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md) — `[INV-FOOTER-02]` one-row activity strip
- [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md) — the concurrent turn-staging architecture this ADR preserves