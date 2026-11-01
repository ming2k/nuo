---
id: ADR-0144
title: "Outbox Ownership Is Split by Row State: Waiting Rows Are the Driver's Projection, Steering and Dispatching Rows Are the Client's"
status: accepted
date: 2026-11-02
scope: tui/nuo, protocol/nuo-protocol, server/nuo-server, presentation/nuo-view, governance/invariants
superseded_by: null
negative_knowledge: true
---

# 0144. Outbox Ownership Is Split by Row State

- Status: Accepted
- Date: 2026-11-02
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0137](0137-steering-admission-provenance-and-the-turn-seam-interjection.md) — that ADR unified the outbox into one population and made the client a *renderer* of server state, but never said which rows the server could overwrite. This ADR names the ownership split that its own snapshot handling silently violated.
- Applies: [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md) (action permissibility — a verb must be legal in the state it is offered in), [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (a status is chrome; addressable content is a dialog).

---

## Context and Problem Statement

ADR-0137 collapsed the old split between "transcript-owned inserts" and "next-round
outbox items" into **one** `VecDeque<QueuedDispatch>` with three states
(`Waiting`, `Dispatching`, `Steering`), and made the server the authority over when
a follow-up ships (M4: the driver owns the `FollowUpQueue`). It also kept, unchanged,
two client-side steering paths: `CancelSteer` on recall/delete, and the
`SteerUnavailable`/`SteerCancelled` restore-to-composer.

That left an unstated question in the middle of the design: **when the server sends
its authoritative queue snapshot, which rows is it authoritative over?** The
implementation answered "all of them, except `Dispatching`". Every consequence of
that answer was a bug:

1. **The snapshot deleted data the server had never received.** `Steering` rows
   exist only in the client (the steer is held in the running round's admission
   window; the driver's queue never sees it). The full-replace merge erased them.
   The driver emits a snapshot after *every* queue change — including the
   `QueuePaused(true)` echo of merely opening the queue modal — so a pending steer
   was routinely deleted a frame after it was staged.
2. **Deleting a steer that way destroyed the message.** The row *is* the payload
   (text + images + large pastes). `SteerUnavailable` restored the text by reading
   the row; with the row gone, the restore found nothing and the prompt vanished
   with no transcript row, no composer draft, and no receipt.
3. **Local mutations of server rows were not mutations at all.** `D` (delete) and
   `Enter` (recall) edited only the local projection, so the driver kept the item
   and shipped it at the next round boundary: the "deleted" prompt still ran, and
   the next snapshot put the row back. The protocol verbs (`QueueRemove`,
   `QueueClear`) existed and were idempotent server-side, with zero client call
   sites.
4. **The pause was one-directional.** Opening the modal sent `QueuePaused(true)`;
   closing it cleared only the local latch. The driver stayed paused forever while
   the chrome said "not paused". Even a correctly-sent resume would not have
   shipped anything, because shipping is driven by round-boundary *wakes* and
   `queue_paused` never woke the loop.
5. **`/queue block` was a pure local illusion**, and `/queue clear` was a global
   projection wipe that ignored both the session boundary and the server.

The root cause is not five bugs; it is one **missing ownership rule**. A single
row type spanning two authorities with no stated precedence makes every merge,
every mutation, and every dismissal a per-call-site judgement. Each of the five
defects is a different call site answering the same question differently.

## Decision Drivers

- **One authority per fact.** A row must have exactly one writer that can destroy
  it. Where two processes hold the same logical item, the *transport* decides who
  may delete the local copy (ADR-0116 `[INV-LAYER-01]`, applied to state instead of
  content).
- **A local mutation of a remote fact must either be mirrored or refused.** There is
  no third option that is honest. A delete that only repaints is worse than a
  refused delete: it reports success for an action that did not happen.
- **The row is the payload.** Any path that removes a `Steering` row must have
  already extracted its content, or it is destroying user input.
- **Negative knowledge over silent tolerance.** A merge rule expressed as "keep the
  rows we happened to think of" cannot be reviewed. It must be a total match on the
  state, so adding a fourth state is a compile error rather than a silent data loss.
- **Status is chrome; addressable content is a dialog.** Paused-ness is a property
  of the session (one line above the composer); the item list is addressable and
  cursor-bearing (a dialog). ADR-0062 `[INV-FOOTER-02]` and ADR-0116 §1 already
  forbid the other arrangement.

## Considered Options

- **Option 1 (Chosen): ownership is a total function of `QueuedDispatchState`.**
  - `Waiting` — **server-owned projection**. The driver's `FollowUpQueue` is the
    record; `QueueUpdated` replaces these rows wholesale. Every local mutation must
    be mirrored with the matching verb (`QueueRemove`, `QueueReorder`,
    `QueueClear`) *before* any synthetic close releases the pause.
  - `Dispatching` — **client-owned** (a `FollowUp` whose ack is in flight). The
    snapshot cannot know it, so it is preserved.
  - `Steering` — **client-owned** (held in the running round, never in the
    driver's queue). Preserved by every snapshot; ended only by the protocol's
    linearized `SteerAdmitted` / `SteerCancelled` / `SteerUnavailable`, and its
    content is taken *from the row* before the row leaves.
  - The snapshot merge is a `match` on the state, so the set is exhaustive and a
    new state cannot inherit a default.
- **Option 2: make the server own steers too** (add steers to the `FollowUpQueue`
  snapshot). Changes the meaning of a steer — it is an in-round interjection, not a
  queued round — and forces the driver to model client-visible admission windows.
  Rejected: it duplicates the round's authority in the queue.
- **Option 3: keep the local mirror optimistic and reconcile on the next snapshot**
  ("eventual" deletes). Rejected: a delete that may resurrect is indistinguishable
  from a bug at the user's terminal, and the item can ship in the window.
- **Option 4: drop the local projection and render the snapshot directly.** Rejected:
  the client must render `Dispatching` and `Steering` rows, which by definition do
  not exist server-side.

## Decision Outcome

### 0. The five red lines

- **[INV-OUTBOX-01] One authority per row**: a `QueueUpdated` snapshot replaces `Waiting` rows and preserves `Dispatching` and `Steering` rows whose id it does not carry; ownership is a total `match` on `QueuedDispatchState`, never a filter list.
- **[INV-OUTBOX-02] No silent loss of user input**: a `Steering` row is taken (payload intact) before it leaves, and a steer that ends with no row to restore from records a retained `ClientNotice::SteerContentLost` rather than disappearing.
- **[INV-OUTBOX-03] Mirror or refuse**: a local mutation of a server-owned (`Waiting`) row sends its verb (`QueueRemove` / `QueueReorder` / `QueueClear`) before any synthetic close releases the pause; a client-owned (`Steering`) row is never addressed by a queue-position verb.
- **[INV-OUTBOX-04] Pause has a wake**: every writer of `queue_blocked_sessions` is paired with `QueuePaused`, and the server wakes the ship loop on resume (and only on resume).
- **[INV-OUTBOX-05] Residency**: the outbox item list renders in the queue modal (`L3`); the paused state renders as one clause on the consolidated activity strip (`L1`); neither surface grows into the other.
- **[INV-OUTBOX-06] Arrival re-reads the queue**: settling on a thread asks the driver for its current queue (`QueueSnapshot`), because `QueueUpdated` is an event and a snapshot that preceded the attach was never delivered; the projection must never start empty over a queue that holds items pointing at the next boundary.
- **[INV-OUTBOX-07] Shipment is a per-iteration invariant, never a consequence of a wake**: the driver's ship pass is evaluated on every loop iteration from checkable state (`is_running`, `is_paused`, queue depth). A wake is merely a hint that a boundary may have opened — `Notify` stores one permit and a boundary watcher drops its send when the channel is full, so a queue whose shipment depended on receiving one parked forever and the only symptom was a prompt that never ran.
- **[INV-OUTBOX-08] Every row has one admission authority, and unknown time is unknown**: `Select.protocol` is required, so the protocol window is the sole gate (there is no pre-protocol peer to judge by product version); and a row whose stamp the peer did not report carries `None` rather than a default, because a defaulted epoch renders as a real wall-clock.

### 1. The snapshot merge is total over the state

A `QueueUpdated` snapshot replaces
`Waiting` rows and **preserves** `Dispatching` and `Steering` rows whose id is not
in the snapshot. The merge is expressed as an exhaustive `match` on
`QueuedDispatchState`, not as a filter list, so the ownership of a new state is a
deliberate decision. Enforced by
`queue_snapshot_preserves_client_owned_rows` (`shell/tests/steering_admission.rs`),
which stages all three states against a one-item snapshot.

### 2. The row is the payload, so it is taken, not dropped

`App::take_steer_row` is the only way a `Steering` row leaves for a protocol
ending. It is **state-scoped** (a `Waiting` row can never be consumed by a
steer-ending path) and it returns the full payload, so the restore arms put back
text *and* attachments. Enforced by `steer_text_survives_a_snapshot_then_a_missed_round`
and `steer_ending_paths_consume_only_the_steer_row`.

### 3. An unrecoverable steer is disclosed, never silent

If a steer ends with no row to restore from (`[INV-OUTBOX-02]`), the client
records `ClientNotice::SteerContentLost` through the catalog funnel
([INV-NOTICE-05], ADR-0138). A prompt must never disappear with no transcript
row, no draft, and no receipt. Enforced by
`an_unrecoverable_steer_is_disclosed_in_the_notice_log`.

### 4. A local mutation of a server row is mirrored, in safety order

`D`, `Enter`, `J`/`K` and `/queue clear` (`[INV-OUTBOX-03]`) project locally
**and** send the matching verb; a row the server does not own (`Steering`) is
never addressed by a queue-position verb at all (it has no position to change, so
a `QueueReorder` for it would silently no-op server-side while the local list
moved). The verb is sent **before** any synthetic close releases the pause, so the
driver cannot ship the item being recalled in the window between "resumed" and
"removed". Enforced by `queue_modal_delete_and_recall_are_mirrored_to_the_server`
and `clear_queue_command_is_session_scoped_and_mirrored`.

### 5. Pause is a server flag with a symmetric client projection, and its ownership is recorded

Every writer of `queue_blocked_sessions` is paired with `QueuePaused`
(`[INV-OUTBOX-04]`), including both exit hooks (dialog close and session switch)
and the `/queue block` command. On the server, `queue_paused` **wakes the ship
loop when resuming** and only then: shipping is driven by round-boundary wakes, so
clearing the flag alone leaves a queue whose round already ended parked with no
event left to start it. Enforced by `resuming_a_paused_queue_wakes_the_ship_loop`
(`nuo-server`).

Symmetric means **exactly what this open set**, not "resume on the way out". The
exit hook used to resume unconditionally, which turned the panel's close into a
forced-resume button: pause with `Ctrl+P`, close, and the pause is silently undone
(the inverse of the original defect, and equally a lie). `queue_exit_session`
therefore carries a `QueuePauseOrigin` — `ModalAutoPause` while the auto-pause is
still the pause in effect, `UserOwned` once the user has toggled it or entered an
already-paused session. Enforced by
`closing_the_queue_panel_does_not_undo_an_explicit_pause`.

### 6. The paused state is chrome; the item list is a dialog

The item list renders in the queue modal (`[INV-OUTBOX-05]`): `L3` is the only
layer that can express selection and scroll (ADR-0116 §1); the paused state
renders as one clause on the consolidated activity strip (`L1` status, ADR-0062
`[INV-FOOTER-02]`). The strip must not grow a list and the modal must not become a
permanent row. `QueueBarProps::paused` was removed with this ADR: it was `true`
whenever any item merely existed, so it carried no information and had no reader —
the state it pretended to carry is now `ActivityStripProps::queue_blocked`.

### 7. Arrival re-reads the queue

`[INV-OUTBOX-06]` — `QueueUpdated` is an *event*, and an event that fired before
the client attached is gone. A client settling on a live thread (first attach, or
re-attach after a lost link) therefore has an empty projection while the driver
holds items that will ship at the next boundary: the user is shown "nothing
pending" and then receives prompts they never saw coming, which is the same class
of untruth as reporting a delete that did not happen. `AgentRequest::QueueSnapshot`
(the read twin of `QueueUpdated`) is sent on the same `Ready` transition that
flushes pre-`Ready` prompts, and the driver answers with the same snapshot
function every mutation uses. Enforced by `ready_re_reads_the_authoritative_queue`.

### 8. The deletion intent is scoped to the item's life, not the race

`[INV-STEER-07]` restated: `SteerCancelFailed` must **not** clear
`suppress_steer_restore` while the row is still staged. Doing so made "the cancel
lost" indistinguishable from "no cancel was requested", so a later
`SteerCancelled` for the same id would find no flag and restore text the user had
deliberately deleted. The flag is cleared by the event that actually ends the item
(`DispatchRemoved` / `SteerMissed` / `SteerCancelledRestored`), and eagerly only
when the row is already gone — the one case where nothing will ever end the id
again. Enforced by `a_lost_cancel_keeps_the_deletion_intent_until_the_item_leaves`.

### 9. Dead local-drain machinery is deleted

`App::begin_next_round_dispatch` and `App::requeue_dispatch` had zero production
callers (tests only) and described a client-side drain that has not existed since
the driver took the queue. They are removed rather than documented, following the
ADR-0137 §6 precedent: a method that implies a lifecycle which does not exist is a
third kind of untruth (after optimistic thread entries and dead states).

### 10. The scheduler is not the owner of the invariant

`[INV-OUTBOX-07]` — `ship_queued_follow_ups` is called once per driver iteration,
not from inside the wake arm. The extracted function reads only checkable state,
so the question "may anything ship right now?" has one answer that does not depend
on whether a hint arrived. The wake arm is reduced to its one real duty (park the
queue after an interrupted round). This removed a whole class of stalls: lost
`Notify` permits, an interrupted-then-resumed round, a wake that arrived while the
channel was full, and a resume that a queued request overtook. Enforced by
`an_idle_target_ships_without_any_wake_hint`, which drains the wake channel and
asserts the item still ships.

### 11. One admission authority; unknown time stays unknown

`[INV-OUTBOX-08]` — two defaults masquerading as compatibility were deleted:

- `Select.protocol` became required (`Option<u32>` → `u32`), deleting the
  "pre-protocol client is judged by exact product-version equality" lane. That
  fallback was a second admission rule for peers that no longer exist; an
  omitting handshake now fails to decode and is answered with the protocol
  taxonomy. The product version survives only to make a refusal name both builds.
- A queued row whose stamp the peer did not report carries `None`
  (`QueuedDispatch::queued_at_ms`, `QueueItemProps::queued_at_ms`). The projection
  had written `sent_at_ms.unwrap_or(0)`, and `sent_time_label(0)` is the Unix
  epoch in local time (`08:00` in UTC+8) — a fabricated wall-clock rendered beside
  the user's own queued message. Enforced by
  `queue_modal_does_not_invent_a_time_for_a_row_without_one`.

## Consequences

### Positive

- Every queue verb now has one authority and one documented direction of effect.
- A pending steer survives the snapshot storm that any queue activity produces.
- The three "reports success but did nothing" paths (delete, recall, pause, clear)
  are gone; each is now visible on the protocol.
- The strip can say *why* the queue is not moving, which is the one fact a
  count-only badge could never express.
- Attaching to a live session no longer renders an empty queue over pending items.
- One new protocol variant (`QueueSnapshot`, protocol 20 → 21).

### Negative

- The client now sends more intents (one per mutation). This is the cost of the
  mirror rule, and it is bounded: the verbs are idempotent and session-scoped.
- Ordering between "remove" and "resume" is load-bearing, so a future caller that
  releases the pause directly (rather than through the exit hook) can reintroduce
  the race. The exit hooks are the only release sites, and `[INV-OUTBOX-04]` pins
  that the projection is written through them.
- The snapshot merge is now asymmetric (replace one class, preserve two), which is
  more code than `retain` + rebuild. The exhaustiveness of the `match` is the
  compensation: a silent default is no longer representable.

### Neutral

- `Steering` rows are preserved at the tail of the rebuilt block. All addressing is
  session-filtered and the strip previews the head, so the global slot is not
  observable; a steer is never "the next item to ship" anyway (it waits on a turn
  boundary, not a round boundary).

## Links

- [ADR-0137](0137-steering-admission-provenance-and-the-turn-seam-interjection.md) — the outbox unification this ADR completes
- [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md) — action permissibility
- [ADR-0116](0116-content-residency-and-surface-layer-topology.md) — residency (`L1` status vs `L3` dialog)
- [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md) — `[INV-FOOTER-02]` single-row activity strip
- [ADR-0138](0138-notice-catalog-typed-registry-of-every-client-disclosure.md) — the disclosure funnel used by `[INV-OUTBOX-02]`
