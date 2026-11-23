---
id: ADR-0239
title: "The Journal Carries the Branch Graph; SQLite Branch Rows Are Derived"
status: accepted
date: 2026-11-22
scope: persistence/thread, persistence/journal, protocol/thread
superseded_by: null
amends:
  - "[ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md) §5 (the branch-cursor boundary is closed)"
negative_knowledge: true
---

# 0239. The Journal Carries the Branch Graph; SQLite Branch Rows Are Derived

- **Status**: Accepted
- **Date**: 2026-11-22
- **Amends**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md) §5
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md),
  [ADR-0237](0237-per-session-group-commit-lanes-keep-the-async-loop-off-the-storage-path.md)

---

## Context and Problem Statement

ADR-0236 left one explicit boundary: branch cursors lived in the SQLite
aggregate. `fork_branch` / `switch_branch` / `delete_branch` wrote
`thread_branches` directly, `thread_aggregate` read it back, and
`switch_branch` had to append a **journal-only** frame because a
transcript-shaped `FullRewrite` projection would erase the branch table.

That exception defeated the journal contract: a crash could leave the
transcript in the journal and the branch cursor only in SQLite, and deleting
the derived database lost every sibling branch.

The root cause is representational. `ThreadData.transcript` is the **active
branch's linear projection**; the fact graph is a DAG whose other branches are
not present in that projection.

## Architectural Decisions

### 1. `[INV-JOURNAL-BRANCH-01]` A branch mutation is one frame

`ThreadData` gains:

```rust
pub(crate) branches: BTreeMap<BranchId, BranchState>,
pub(crate) graph: Option<protocol::Thread>, // the fact DAG sidecar
```

`fork_branch`, `fork_at_branch`, `switch_branch`, `delete_branch`, and
`commit_thread` mutate the in-memory graph and persist **one full journal
frame** containing the graph, the branch map, the active cursor, and the
active transcript. There is no separate aggregate write and no
`persist_journal_only` exception.

### 2. `[INV-JOURNAL-BRANCH-02]` An ordinary delta never drops the graph

`clone_metadata_without_history` sets `graph: None`, so a turn delta carries
only its working state and new transcript entries. Replay's
`apply_journal_commit` carries the previously materialised graph forward when
the incoming frame omits it. A branch/full frame supplies its own graph and
replaces the old one.

`thread_from_session_data` starts from `data.graph`, merges facts from the
active transcript that the graph does not yet contain (renumbered above the
DAG's max seq), and sets the active branch's head to the transcript tail. A
fork's new turn therefore attaches to the fork cursor, not to the global tail.

### 3. `[INV-JOURNAL-BRANCH-03]` Branch reads never consult SQLite

`thread()`, `thread_aggregate()`, `branches()`, and `tree()` derive from
`ThreadData` (journal state). `thread_aggregate` no longer flushes the
projection or calls `load_thread`. The derived branch rows are still
materialised for clients, but they are written only by the projector and can
be deleted and rebuilt.

### 4. Older frames remain readable

Frames written before this ADR have no `branches`/`graph`; serde defaults make
them an empty map/`None`. `thread_from_session_data` falls back to the linear
single-cursor shape, so a lazy-imported legacy session opens unchanged. No
journal format version bump is needed.

## Consequences

- Transcript and cursor switch atomically: a crash cannot leave them on
  different branches.
- Deleting SQLite no longer loses sibling branches. A test asserts
  `main` (4 facts) and a forked `round-1` (2 facts) reload from the journal
  after `nuo.db*` is removed.
- Branch operations pay one full snapshot (the graph). They are cold control
  paths; ordinary turns stay on the small delta path.
- `save_thread_aggregate` / `save_thread_aggregate_with_generation`,
  `persist_journal_only`, and the SQLite read in `thread_aggregate` are
  retired from the session path.

## Alternatives Considered

| Alternative | Why rejected |
|---|---|
| Keep the aggregate authoritative for branches | The journal is then not the source of truth, and `rm nuo.db*` loses branches |
| Tag every transcript entry with its branch and derive the DAG | Correct end state, but it changes the protocol transcript surface and every append path; the graph sidecar reaches the same guarantee with a smaller blast radius |
| Store only `branch → head` and rebuild other branches from SQLite | Reintroduces the boundary this ADR closes |

## Negative Knowledge

- **Do not project a transcript-shaped frame without the graph.** A full
  projection owns every row; without the graph it erases sibling branches.
- **Do not trust the active branch's stored head after a delta.** The
  transcript tail is the authoritative head; derive it.
- **Do not put the graph in every delta.** It is a cold-path sidecar; cloning
  it per turn would multiply the write volume.
