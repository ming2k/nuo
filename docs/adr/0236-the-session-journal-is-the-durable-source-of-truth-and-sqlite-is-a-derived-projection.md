---
id: ADR-0236
title: "The Session Journal Is the Durable Source of Truth; SQLite Is a Derived Projection"
status: accepted
date: 2026-11-21
scope: persistence/journal, persistence/thread, persistence/db, engine/cognition/agent, server/runtime
superseded_by: null
negative_knowledge: true
---

# 0236. The Session Journal Is the Durable Source of Truth; SQLite Is a Derived Projection

- **Status**: Accepted
- **Date**: 2026-11-21
- **Related**: [ADR-0237](0237-per-session-group-commit-lanes-keep-the-async-loop-off-the-storage-path.md)
  (how the journal is written), [ADR-0238](0238-configuration-and-usage-are-process-snapshots-never-per-request-disk-reads.md)
  (what is removed from the request path), ADR-0048 (mid-round save point)
- **Amended by**: [ADR-0241](0241-session-identity-path-aliases-and-deletion-live-in-the-journal-plane.md)
  — deletion is a terminal journal frame plus a `DELETED` marker, not a
  directory removal

---

## Context and Problem Statement

A ReAct turn boundary is a durability boundary: a tool may already have
changed the filesystem, so the transcript that describes it must reach stable
storage before the next model request. The store satisfied that requirement by
awaiting one SQLite transaction (`ThreadSession::commit_turn` →
`PersistenceHandle::save_session` → the single writer actor).

That made SQLite a **synchronous participant in every turn**, even though it is
also the queryable view (thread list, FTS, usage reports). The two roles have
opposite requirements:

- the commit needs a small, append-only write with one fsync;
- the view needs a globally consistent, indexable, queryable store that may be
  milliseconds behind without anyone noticing.

Fusing them produced the observed costs: a global writer queue shared by every
session, `block_in_place` bridges on the async runtime, page-level write
amplification from checkpoints, and read paths that could not distinguish
"durable" from "indexed".

## Architectural Decisions

### 1. `[INV-JOURNAL-01]` A session is acknowledged only after its journal fsync

Every session owns a journal directory under `<db parent>/journal/<session>/`.
A commit is one length-prefixed, CRC32C-protected frame:

```text
magic "NJ01" | version u16 | kind u8 | flags u8 | seq u64 | len u32 | crc u32 | payload
```

`kind` mirrors the persistence mode exactly:

| Frame kind | Payload semantics |
|---|---|
| `Metadata` | Working state only; keep the transcript |
| `Delta` | Working state plus new transcript entries/directives |
| `Full` | A complete `ThreadData` snapshot |

The caller's `commit_turn` returns only after the frame's `fdatasync`
acknowledges. A frame is therefore the durable receipt. SQLite's
`commit_receipts` table is an idempotency ledger for the derived writer; it is
not a second durability record.

### 2. `[INV-JOURNAL-02]` Replay stops at the first invalid frame

Recovery loads the newest parsable `snap-<seq>.json`, then replays every
segment frame above that seq. A frame that fails structure, CRC, or JSON
decoding ends the replay. Bytes after that point are a torn or corrupt tail,
never a commit. The writer truncates the tail to the last valid offset on open,
so a crash cannot poison the next append.

### 3. `[INV-JOURNAL-03]` No SQLite write is on the commit acknowledgement path

The journal ack is the commit. The same `ThreadData` is then handed to the
derived-view projection (originally one OS thread per session; now a bounded,
fair lane pool — see [ADR-0248](0248-storage-work-is-split-by-what-it-needs.md)
§3) which applies it to SQLite. The projection:

- never runs on the tokio runtime and never blocks the ReAct loop;
- preserves per-session order with a FIFO channel;
- marks the session **desynced** when the queue is full or a write fails,
  rather than dropping the commit;
- repairs a desynced view with one `FullRewrite` read from the journal head.

Cold control paths that read the derived row (delete, and the
request-projection archive) call `flush_projection()` first, so a view that is
allowed to lag never becomes a correctness hole. Branch aggregate and rename
read the journal state directly and need no flush.

### 4. `[INV-JOURNAL-04]` A derived view is rebuildable, never authoritative

The journal is the only thing whose loss loses data. SQLite may be deleted and
rebuilt from the journals. The journal loader never takes content from SQLite;
it may read the derived row only to decide whether that row needs a catch-up
rewrite (`catch_up_projections_for`). Legacy sessions are materialised once,
lazily, as a seq-0 snapshot (`snap-00000000000000000000.json`) the first time
they are opened. There is no dual-write window: the frame is durable before the
projection is applied, and a missing projection is rebuilt from the frame.

### 5. Branch cursors moved into the journal (ADR-0239)

This section originally recorded a boundary: branch cursors lived in the SQLite
aggregate, and `switch_branch` appended a journal-only frame to avoid a
transcript-shaped projection erasing the branch table. [ADR-0239](0239-the-journal-carries-the-branch-graph-and-sqlite-branch-rows-are-derived.md)
closes that boundary: the journal carries the branch map and a fact-graph
sidecar, a branch switch is one full frame, and `thread_aggregate` derives from
the journal state. SQLite's branch rows are now purely derived.

## Consequences

- A turn commit costs one append + one fsync on its own lane; it does not wait
  for another session, a checkpoint, or a `VACUUM`.
- The thread list, FTS, and usage reports may trail the journal by the
  projection queue's depth. Tests and control paths that need the view call
  `flush_projection()`; the UI sees the journal-derived live state for hosted
  sessions.
- A projection failure is self-healing: the journal already has the commit, and
  the next drained job (or a cold flush) rebuilds from it.
- Session deletion is a terminal journal frame: the segment and snapshot bodies
  are removed and a `DELETED` marker is left in the directory, so a concurrent
  reader or a queued projection cannot resurrect the session (ADR-0241).
- The journal adds ~1 fsync per turn on the critical path, replacing a SQL
  transaction that did the same plus page/index work.

## Alternatives Considered

| Alternative | Why rejected |
|---|---|
| Keep one global SQLite writer and batch commits | Still one lane for every session; a checkpoint or large delta delays unrelated sessions, and the async loop still awaits the actor |
| Shard SQLite by workspace | Removes cross-workspace contention but keeps page-level write amplification, schema migrations on the commit path, and a query engine whose read model is fused to the write model |
| Replace SQLite with RocksDB/LMDB/redb | Every embedded engine has the same single-writer model; the commit would still be a B-tree/LSM write plus an fsync, and the query/FTS surface would have to be rebuilt |
| Keep SQLite authoritative and add a journal as a cache | Two sources of truth; a crash can leave them disagreeing with no deterministic winner |

## Negative Knowledge

- **A journal without a snapshot bound is not a design.** Replay must be
  bounded by a snapshot; the store compacts every 512 commits on a blocking
  task.
- **Do not project a journal-only frame.** A `FullRewrite` projection owns every
  row; using it for a branch switch silently erases branch cursors.
- **Do not make the projection synchronous "just for safety".** That
  reintroduces exactly the coupling this ADR removes; cold reads use
  `flush_projection()` instead.
- **Do not give the projection a thread per session.** Per-session threads are
  `O(sessions)` resident stacks for work that is idle almost always; the pool is
  bounded and still keeps per-session FIFO (ADR-0248).
- **Do not let the writer touch the filesystem.** A projection's CAS offload is
  prepared before the command crosses the writer door, so a large tool output's
  `fdatasync` cannot stall another session's projection (ADR-0248
  `[INV-STORAGE-01]`).
- **Do not use the journal as a query engine.** It is an append-only log with
  no index; every list/search/report reads the derived view.
