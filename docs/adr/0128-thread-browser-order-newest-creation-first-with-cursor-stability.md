---
id: ADR-0128
title: "Thread Browser Order: Newest Creation First, Cursor Stability Preserved, and the Dock Address Axis Left Alone"
status: accepted
date: 2026-10-29
scope: host/nuo, tui/overlays, cli/nuo
superseded_by: null
negative_knowledge: true
---

# 0128. Thread Browser Order: Newest Creation First, Cursor Stability Preserved, and the Dock Address Axis Left Alone

- Status: Accepted
- Date: 2026-10-29
- Deciders: Nuo Architecture Working Group
- Consulted: Interface Architecture, TUI Interaction, Persistence
- Amends: [ADR-0120](0120-thread-lineage-graph-persisted-fork-provenance-and-browser-grade-history-topology.md) — §5's ordering *direction*, and the direction named in `[INV-LINEAGE-03]`; the stability clause is unchanged
- Related: [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md), [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md)

---

## Context and Problem Statement

[ADR-0120](0120-thread-lineage-graph-persisted-fork-provenance-and-browser-grade-history-topology.md)
settled *what* the thread browser lists: every session, exactly once, geometry
derived from persisted lineage. It also settled the ordering **key** — `(parent_id,
created_at, id)`, never `updated_at` — and gave one reason for it, which this ADR
does not overturn: a liveness signal that changes on every model token cannot
reorder a row under a cursor.

What it did not settle is the key's **direction**. It chose `created_at`
ascending, and in the same breath rejected "most recent thread first" as an
alternative
([Alternative D](0120-thread-lineage-graph-persisted-fork-provenance-and-browser-grade-history-topology.md#alternative-d-order-the-picker-by-updated_at-rejected)).
That rejection was correct about the key and wrong about the direction, because
the two independent properties were argued as one:

1. **Stability** — a visible row never moves while the list is being navigated.
2. **Direction** — which end of a stable order the eye starts from.

Only the first is a correctness property. The second is a usability choice that
stability does not constrain: a list sorted by `created_at` descending is just as
immovable under a streaming token as one sorted ascending, because `created_at`
is written once, at fork, and never revised. Rejecting recency *directions* on
the grounds that recency *keys* are unstable borrowed the force of an argument
that only applied to `updated_at`.

The cost of that conflation is paid on every launch. A browser that leads with
the oldest thread answers "what did I start first in this repo?" — a question
nobody opens a browser to ask. The thread a user is most likely looking for is
the one they began most recently, and in the ascending order it is at the
**bottom** of its sibling set, behind everything it descended from.

## Decision Drivers

- **The browser's first row should answer the first question.** Opened cold, a
  thread list should lead with the thread most likely being sought.
- **Stability is not negotiable, and is not what is being changed.** No liveness
  signal may reorder a visible row (`[INV-LINEAGE-03]`). This ADR moves the
  direction of a key that was already fixed, not the key.
- **Hierarchy outranks recency.** A newer branch must still paint *beneath* its
  trunk. Recency orders a sibling set from the inside; it never promotes a child
  to a root or reorders the forest globally.
- **One rule, two consumers.** `project_session_rows` (the TUI picker) and
  `thread_rows` (`nuo thread ls`) already share the ordering key. Two surfaces
  that disagreed on what a branch is would be a bug; two surfaces that disagreed
  on what "first" means would be a second one.

## Architectural Decisions

### 1. `created_at` descending, id ascending, per sibling set

The key becomes `created_at` **descending**, `id` **ascending**, applied
independently to the root list and to each `children_by_parent` set:

```text
order(a, b) = b.created_at.cmp(&a.created_at)      // newest first
                .then(a.id.cmp(b.id))             // total order on ties
```

The id direction stays ascending so equal timestamps remain a *total* order with
a fixed tiebreak, rather than becoming an arbitrary-but-stable one.

### 2. Descending is per sibling set, not global

Recency never crosses a lineage edge. Roots (threads with no parent in the
snapshot included, per `[INV-LINEAGE-02]`) sort newest-first among themselves; a
branch created *after* every root still renders under the trunk it came from,
with the trunk's position. This is the property that keeps the ordering a pure
function of lineage plus creation time rather than a flat sort that happens to
indent.

### 3. A cycle's stranded tail is ordered like every other sibling set

The rows unreachable from any root — a corrupt `parent_id` cycle, handled by
`[INV-LINEAGE-02]`'s "corrupt lineage terminates" rule — were emitted in
**snapshot arrival order**, which is the one place the browser's order depended
on whatever the wire happened to serialize. They are now sorted with the same
comparator before emission.

### 4. Both consumers move together, and so does the elbow they imply

`project_session_rows` (`crates/nuo/src/shell/overlays/session.rs`) and
`thread_rows` (`crates/nuo/src/commands/thread.rs`) apply the same comparator,
so the picker and `nuo thread ls` agree on what "first" means as well as on what
a branch is.

The visible consequence is in the **lane geometry**, which is computed from
position, not the reverse: the *last* row of a sibling set is now the *oldest*
one, so it is the oldest child that paints the closing `└─` elbow and the newer
siblings that keep the lane open with `├─`. The renderer is unchanged; the
geometry it reads simply describes the new order.

### 5. The dashboard dock keeps the ascending axis

`creation_order` (`crates/nuo/src/shell/overlays/dashboard.rs`) is **not**
changed. It serves two masters this ADR does not touch: the `#N` console address
a user types to switch threads, and the dock card grid, whose rows carry that
address rather than a browsing order. Renumbering `#1` to mean "the newest
thread" would invalidate every address a user has read off the dock, and the
dock's cards are scanned in parallel rather than navigated with a cursor. The
`#N` axis is stable *identity*, not recency — which is exactly why it is allowed
to keep a different direction from the browser's.

### Normative Invariants Established

- `[INV-LINEAGE-03] Position is deterministic` — **amended in direction only**.
  Row order remains a pure function of `(parent_id, created_at, id)` and no
  liveness signal may reorder a visible list; the key's direction is
  `created_at` descending, id ascending. Enforced by
  `shell::overlays::session::tests::projection_is_stable_across_updates`
  (unchanged: `updated_at` must not move a row) and
  `shell::overlays::session::tests::siblings_are_ordered_newest_creation_first`
  (roots newest-first; a newer child does not jump the queue).
- `[INV-LINEAGE-02] The picker never drops a row` — **unaffected, and reaffirmed**.
  Reversing a sort cannot drop or duplicate a row, and the stranded-tail sort in
  §3 is applied to exactly the set that was already emitted. Enforced by
  `projection_emits_every_row_exactly_once`, `deep_fork_chain_remains_visible`,
  and `orphaned_branch_is_marked_not_promoted`.

---

## Consequences

- **Positive**: The browser leads with the thread a user most likely wants, at
  every level of the forest, without weakening the guarantee that made the
  ordering worth specifying in the first place. Cursor navigation is unchanged
  in kind: a list that does not move is navigable regardless of which end the
  eye enters from.
- **Positive**: The `#N` console addressing keeps the ascending axis, so no
  address a user has already read becomes wrong. The two axes now have stated
  reasons rather than an accident.
- **Negative & Trade-offs**: Two orderings now coexist in the tree — descending
  creation for browsing, ascending creation for addressing — and telling them
  apart requires knowing which surface you are reading. This is mitigated by
  keeping each direction named at its single definition site (`project_session_rows`,
  `thread_rows`, `creation_order`) rather than at call sites.
- **Negative & Trade-offs**: The elbow assertions in the picker and `thread ls`
  tests invert (the *oldest* sibling now closes the lane). That inversion is the
  change, so the tests were rewritten to assert the new geometry explicitly
  rather than relaxed.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Sort by `updated_at` descending (Rejected)

- **Why considered**: True MRU, the semantics a tab switcher uses.
- **Why rejected**: Unchanged from
  [ADR-0120](0120-thread-lineage-graph-persisted-fork-provenance-and-browser-grade-history-topology.md):
  a row set that reorders on every model token cannot be navigated with a
  cursor, and this is the property a browser exists to provide. Recency of
  *touch* is unstable; recency of *creation* is not, which is why this ADR takes
  the latter and leaves the former rejected.

### Alternative B: Keep ascending order and add a "newest first" toggle (Rejected)

- **Why considered**: Satisfies both preferences and lets the user decide.
- **Why rejected**: It relocates the decision to the worst possible moment — the
  user must know which order they want *while already looking for a thread*, and
  a persisted toggle makes the browser's geometry depend on stored UI state,
  which is one more thing `project_session_rows` would need to be given and one
  more way two clients could disagree about a pure function. The default would
  still have to be chosen, and choosing descending here is choosing this ADR with
  extra machinery.

### Alternative C: Reverse globally, then indent (Rejected)

- **Why considered**: One `sort_by` instead of a per-sibling-set comparator.
- **Why rejected**: A global sort has no notion of sibling sets, so a branch
  created after every root would be hoisted to the top and re-parented visually —
  it would stop painting beneath the trunk it came from, contradicting the
  lineage the surface exists to show (`[INV-LINEAGE-01]`).

### Alternative D: Emit the closing elbow from the newest sibling anyway (Rejected)

- **Why considered**: Keeps the *rendered* corner on the row users are most
  likely looking at, which sounds friendlier.
- **Why rejected**: The elbow is derived from `is_last`, and `is_last` is
  geometry, not taste. Painting a closing `└─` on a row that is not last makes
  the lane contradict the order, so a user reading the tree would conclude the
  newest child is the only child. The geometry describes the order; it does not
  get to interpret it.

## Links

- Amends: [ADR-0120](0120-thread-lineage-graph-persisted-fork-provenance-and-browser-grade-history-topology.md) (ordering direction in §5 and `[INV-LINEAGE-03]`; stability clause unchanged)
- Related: [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md) (thread-scoped naming and the `#N` address axis)
- Related: [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md) (causal session forking)
