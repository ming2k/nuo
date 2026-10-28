---
id: ADR-0120
title: "Thread Lineage Graph: Persisted Fork Provenance, Browser-Grade History Topology, and the Retirement of Inferred Lineage"
status: accepted
date: 2026-12-10
scope: persistence/nuo-persistence, protocol/nuo-protocol, server/nuo-server, host/nuo, tui/overlays, tui/event-loop
superseded_by: null
negative_knowledge: true
---

# 0120. Thread Lineage Graph: Persisted Fork Provenance, Browser-Grade History Topology, and the Retirement of Inferred Lineage

- Status: Accepted
- Date: 2026-12-10
- Deciders: Nuo Architecture Working Group
- Consulted: Persistence, Protocol, Server, Interface Architecture
- Amends: [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md), [ADR-0104](0104-fork-client-projection-reconciliation-and-aside-retirement-completion.md)
- Related: [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md), [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md)

---

## Context and Problem Statement

ADR-0058 established causal session forking and ADR-0104 reconciled its client
projection with a single-attach terminal. Both decisions are about **producing**
branches. Neither ever decided what a *history browser* owes a user who has
branches: the Threads picker, the dashboard dock, and `nuo thread ls` each grew
their own ad-hoc answer, and all three are wrong in ways that destroy information
the engine already holds.

Three concrete defects were reproduced against the shipped tree:

1. **Forks beyond depth one disappear entirely.** `project_session_rows`
   (`crates/nuo/src/shell/overlays/session.rs`) collected trunks and then walked
   **one** level of children. A fork-of-a-fork is placed in
   `children_by_parent`, never emitted as a row, and never promoted to a root.
   A chain `T ← F1 ← F2 ← F3` renders as `T, F1`; `F2` and `F3` are invisible
   and unselectable. Forking a fork is supported by the server and is the normal
   way to explore two variations of one early decision.

2. **The agent's own branches are laundered into unrelated roots.** The client
   drops the active session from the overview (`sessions.retain(|s| !s.active)`,
   `crates/nuo/src/shell/event_loop/apply.rs`) so the picker's cursor lands on an
   *alternative* thread. The projection then requires a branch's parent to be
   present in the same snapshot (`all_ids.contains(pid)`) and, failing that,
   pushes the branch into `trunks`. The result: while you sit in a conversation,
   every branch you forked out of it — the exact relation you opened the picker
   to inspect — renders as a top-level conversation with no `⑂` marker at all.

3. **The fork point is not recorded anywhere.** `threads.fork_fact_id` exists as
   a column with **no writer in the repository**; `SessionOverview`,
   `SessionDetail`, and `SessionSummary` carry no round/turn; the child's
   `round_counter` is overwritten by its own subsequent rounds and therefore
   cannot be read back as a fork point. The only surviving trace of *where* a
   branch diverged was smuggled into the **title string** as `"{parent} (fork)"`
   — which is lossy (no round), non-unique (N forks share one name), and
   user-editable.

The lineage graph is the one thing a fork-based system is *for*. Inferring it
from a mutable display string, and then discarding rows the inference cannot
place, is a boundary error: the store knows the parent and the fork target at
fork time, and the client re-derives a weaker version of that fact on every
refresh.

## Decision Drivers

- **A branch without its origin is not a branch.** Viability of `/fork` depends
  on being able to see, and return to, the divergence point. A UI that flattens
  branches deletes the feature's value while leaving its cost.
- **Lineage is a fact, not a rendering.** It is established once, at fork time,
  by the writer that creates the child. Any consumer that *infers* it can only
  ever produce a subset of the truth, and must silently guess when its
  inference fails.
- **Positional stability over recency.** Rows ordered by `updated_at` jump
  under the cursor whenever any session emits a token; a browser ordered by
  recency cannot be navigated.
- **No dead columns, no unused promises.** A schema column no writer fills is a
  false statement about the system (the same discipline ADR-0059 applies to
  tables and ADR-0104 applied to the "concurrent multi-tab" promise).
- **Structured provenance, never string smuggling.** A fact that lives in a
  display string cannot be queried, sorted, or rendered without parsing.

## Architectural Decisions

### 1. Fork provenance is a persisted, typed field on the child

The child thread row records where it diverged from its parent:

```sql
threads.fork_round  INTEGER   -- parent's 1-indexed user round at the cut (NULL = trunk)
threads.fork_turn   INTEGER   -- 0-indexed turn inside that round (NULL = round boundary)
```

Both are written by `SessionStore::fork_at` — the single fork primitive
(ADR-0104 §1) — at the same moment it stamps `parent_id` and
`fork_kind = fork`. They are the **user-facing address** the `/fork` grammar
already accepts (`/fork <round>.<turn>`), so the recorded value round-trips
through the command that produced it:

| command | `fork_round` | `fork_turn` |
| :--- | :--- | :--- |
| `/fork` (head) | parent's `round_counter` | `NULL` |
| `/fork 2` | `2` | `NULL` |
| `/fork 2.1` | `2` | `1` |
| trunk (`/new`) | `NULL` | `NULL` |

`threads.fork_fact_id` is **dropped**: nothing writes it, nothing reads it, and a
round/turn address is what a user can act on. The causal anchor at full node
resolution remains where it belongs — the `SessionIR` child's `active_leaf`,
which `fork_at` already sets precisely (ADR-0104 §3).

`SessionData` gains the matching pair, so the value survives a session reload and
is covered by the row checksum. `CURRENT_SCHEMA_VERSION` becomes 16.

### 2. The picker is a browser: it projects a forest, it never drops a row

`project_session_rows` becomes a **recursive, deterministic forest projection**:

- **Roots** are rows with no parent *in the snapshot*; children are nested under
  their parent, in `created_at` ascending order. Every row is emitted exactly
  once.
- **Every row carries its lineage geometry**, not just its indentation: `depth`,
  `is_last`, and the ancestor continuation vector that decides whether a column
  paints `│`, `├─`, or `└─`. Rendering is a pure function of that geometry.
- **An unplaceable row is not a trunk.** A row whose `parent_id` names a thread
  absent from the snapshot keeps its `fork_kind`, renders with a `⑂ from <parent>`
  marker, and is placed at depth 0 as an *anchorless branch*. The projection may
  not silently reclassify it as a root conversation.
- **Cycles cannot hang it.** A visited set makes a corrupt `a → b → a` chain
  terminate with each row emitted once, instead of recursing forever.
- **The current thread stays in the list.** The picker's snapshot no longer
  filters `active` out. Switching is a browser verb and "where am I" is a browser
  question; the row is marked current and selecting it is the idempotent
  focus-recovery path that already exists.

`SessionPickerItem` collapses to a single shape carrying
`session, depth, is_last, ancestry, child_count, expanded, parent_missing`. The
two-variant `Trunk`/`Branch` split encoded the depth-1 assumption in the type and
is retired with it.

The lane glyph grammar (`⑂`), extended to a graph:

```text
Auth Refactor                         2h ago      ← trunk (no marker: there is exactly one main line)
├─ ⑂ r1 Regex Branch                  2h ago
│  ├─ ⑂ r1.0 Lookahead Probe          1h ago
│  └─ ⑂ r2 Manual Scan                1h ago
└─ ⑂ r3 Alternate Plan                40m ago
Other Conversation                    1d ago
⑂ from 9f3c1e2 Orphaned Branch        5m ago      ← parent not in this snapshot
```

### 3. Fork point is rendered, not titled — and the smuggled titles are purged

`fork_at` stops minting `"{parent} (fork)"`. Two forks of one parent are not
distinguishable by that string and it permanently pollutes a user-editable
title with machine data. The child is created with `title: None` and is
identified in the browser by its fork address (`⑂ r2.1`) and its first user
prompt — the same way every other thread is identified (ADR-0050).

Migration 25 also **purges the residue** on branches created before this ADR.
It strips a title only when the string is byte-for-byte what the retired
primitive would have generated from the parent's *current* title
(`"<parent> (fork)"`, or bare `"fork"` when the parent had no title), because
that is the only case in which the string is provably machine data. A user who
typed `fork`, or named something `my (fork) experiment`, keeps it. This is the
same zero-legacy discipline ADR-0059 applies to tables and ADR-0104 applied to
the aside substrate: a retired concept does not get to leave its output in a
user-visible field.

### 4. The dock and the CLI state the same relation in their own medium

- **Dashboard dock** (a card grid — nesting has no meaning in a grid): a branch
  card carries a `⇂r2.1#N` back-reference to its parent's dock sequence, so a card
  reads as derived from a specific card. The false doc comment claiming the dock
  "groups by trunk … branches nested beneath" is corrected to describe what it
  paints. Dock ordering and `#N` addressing stay on `creation_order` — the
  console's addresses are user muscle memory and must not renumber.
- **`nuo thread ls`** (headless, a table): gains a `LINEAGE` column painting the
  same `├─/└─/⑂ r2.1` geometry, so a terminal without the TUI still sees the tree.

Both consume `fork_point` from the wire; neither re-derives it.

### 5. Ordering is a pure function of lineage and creation time

Roots and siblings sort by `created_at` ascending (id tiebreak), never by
`updated_at`. A refresh therefore repaints identical positions while statuses and
token counts change beneath them. This deliberately gives up "most recent thread
first" in the picker: a browser is entered to *locate* a thread, and a list that
reorders while you arrow through it cannot be located in.

## Normative Invariants

- **`[INV-LINEAGE-01]` Lineage is persisted, never inferred.** A thread's parent
  and fork point are written by the fork primitive and read verbatim by every
  consumer. No layer reconstructs lineage from display strings, titles, or a
  filtered snapshot. *Enforced by* `fork_at_records_the_fork_point_and_view_surfaces_it`
  (persistence) and `overview_carries_the_persisted_fork_point` (server).
- **`[INV-LINEAGE-02]` The picker never drops a row.** Every session in the
  snapshot is emitted exactly once, including the current thread and rows whose
  parent is absent. *Enforced by* `projection_emits_every_row_exactly_once`,
  `deep_fork_chain_remains_visible`, and `orphaned_branch_is_marked_not_promoted`.
- **`[INV-LINEAGE-03]` Position is deterministic.** Row order is a pure function
  of `(parent_id, created_at, id)`; no liveness signal may reorder a visible list.
  *Enforced by* `projection_is_stable_across_updates`.
- **`[INV-LINEAGE-04]` Provenance is structured, never smuggled.** No layer writes
  fork metadata into a title, a note, or an untyped string.
  *Enforced by* `fork_does_not_mint_a_title`.

## Consequences

- **Positive**: A fork is finally legible as a fork at any depth, in the TUI and
  in headless listings. Returning to a divergence point is a visible, addressable
  act rather than a memory test. The three surfaces cannot disagree, because they
  read one persisted field instead of re-deriving it. Deleting `fork_fact_id`
  removes a column that documented a capability the system never had.
- **Negative & Trade-offs**: The picker no longer leads with the most recently
  touched thread, and the current thread appears in its own list; both are
  deliberate browser semantics and both are cursor-stable. The projection is now
  recursive and must stay cheap — it operates on the already-materialized
  overview (`O(n)` build, `O(1)` per painted row, windowing unchanged). Lineage
  fields are additive on the wire with `serde(default)`, so a peer predating
  ADR-0120 degrades to "no fork point" rather than failing to decode.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Keep inferring lineage in the client (Rejected)

- **Why considered**: No schema change, no wire change, no migration.
- **Why rejected**: It is the current defect. Any inference is defeated by the
  very filter the picker needs (the active session is absent), by depth, and by
  a title the user may rewrite. Inference can only produce a subset of a fact the
  store already holds, and silently guesses when it cannot.

### Alternative B: Keep `threads.fork_fact_id` and record the node id (Rejected)

- **Why considered**: Node-resolution provenance would allow an exact causal diff
  between a branch and its origin.
- **Why rejected**: Nothing can interpret it today, the full-resolution anchor is
  already carried by the child `SessionIR` (`active_leaf`), and a user cannot act
  on a fact id — while `/fork <round>.<turn>` is the gesture that created the
  branch and therefore the address a browser can show.

### Alternative C: Encode the fork point in the child's title (Rejected)

- **Why considered**: Zero-cost, visible everywhere, including in surfaces that
  know nothing about lineage.
- **Why rejected**: It is string smuggling (`[INV-LINEAGE-04]`). Titles are
  user-editable, AI-generated, and length-limited; a fact stored there is
  destroyed by a rename and cannot be sorted or rendered without parsing.

### Alternative D: Order the picker by `updated_at` (Rejected)

- **Why considered**: Matches the switcher's MRU semantics users already know.
- **Why rejected**: A row set that reorders on every model token cannot be
  navigated with a cursor. Locating a thread requires stable positions; recency
  is a *switching* heuristic, not a *browsing* one.

### Alternative E: Nest branch cards inside the dashboard grid (Rejected)

- **Why considered**: Literal symmetry with the picker.
- **Why rejected**: A tiled card grid has no second axis to indent along. Nesting
  in a grid is not a hierarchy, it is a ragged column. A back-reference marker
  (`⇂r2.1#N`) states the same relation in the medium the surface actually has.

## Links

- Amends: [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md),
  [ADR-0104](0104-fork-client-projection-reconciliation-and-aside-retirement-completion.md)
- Related: [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)
  (zero-legacy clean break), [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)
  (thread identity), [ADR-0047](0047-autonomous-surface-interaction-architecture-and-spatial-event-delegation.md)
  (dialog-owned sub-layers)
