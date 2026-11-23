---
id: ADR-0242
title: "The Derived Index Is Split Into a Catalog Plane and Rebuildable Content Shards"
status: draft
date: 2026-11-22
scope: persistence/db, persistence/thread, persistence/reader, server/runtime, server/handlers
superseded_by: null
negative_knowledge: true
---

# 0242. The Derived Index Is Split Into a Catalog Plane and Rebuildable Content Shards

- **Status**: Draft (target design; the storage-layout cutover is not yet
  flipped. The journal plane it depends on is accepted in ADR-0236/0239.)
- **Date**: 2026-11-22
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md),
  [ADR-0237](0237-per-session-group-commit-lanes-keep-the-async-loop-off-the-storage-path.md),
  [ADR-0239](0239-the-journal-carries-the-branch-graph-and-sqlite-branch-rows-are-derived.md)

---

## Context and Problem Statement

The journal commit path is per-session and parallel. The derived SQLite view
is still one database with one writer: every session's facts, FTS rows, branch
rows, and workspace effects queue behind the same actor. The actor is off the
acknowledgement path, so it cannot stall a turn, but it is the last shared
bottleneck of the persistence architecture. It also forces every content reader
that bypasses `ThreadSession` (`offstream`, `monitor`, `search_history`) onto
the same database.

## Progress

Step 1 is landed:

- migration `0005_projection_watermarks.sql` adds `projection_watermarks`;
- `save_session_inner` advances the watermark **inside the projection
  transaction**, so a commit is never marked absorbed before its facts are;
- `ThreadSession::delete` removes the watermark with the row;
- `catch_up_projections_for` scans a journal root, compares each head with its
  watermark (and with row presence), and replays a missing/behind session as a
  full rewrite;
- `catch_up_projections_once` runs once from the server bootstrap;
- tests cover a deleted derived row rebuilt from the journal.

The catalog summary has also landed: migration `0006_catalog_summary.sql`
re-materialises `msg_count` / `last_user_prompt` on `threads` (amending
ADR-0059 and migration `0002_drop_derived_thread_columns.sql` via
[ADR-0244](0244-the-catalog-materializes-a-fact-free-thread-summary.md)),
backfills from the still-co-located facts, and points `thread_list_view` at the
columns. The picker no longer reads content facts.

Remaining before the split can flip: the `SaveScope` split of
`save_session_inner`, a `ProjectionSink` with catalog/content handles, content
shard routing for `load_session_full` / offstream / monitor, and a fan-out
`search_all`.

## Target Architecture

```text
                    ┌──────────────────────────────┐
  journal (truth) → │  Catalog plane               │  thread id / workspace / role /
                    │  (small, one writer)         │  title / status / updated_at /
                    └──────────────────────────────┘  round / last prompt
                    ┌──────────────────────────────┐
                    │  Content shard 0..N          │  causal_nodes / FTS / branch rows /
                    │  (one projector per shard)   │  workspace_effects
                    └──────────────────────────────┘
```

### 1. `[INV-INDEX-01]` The catalog is small and lifecycle-only

`threads`, `thread_policies`, and usage aggregates stay in the catalog
database. The catalog is written on create/title/status/provider-pin changes
and a small `updated_at`/round touch per turn; it never stores transcript
bodies. One writer is acceptable because the rows are tiny and the volume is
lifecycle-shaped, not token-shaped.

### 2. `[INV-INDEX-02]` Content is sharded and every shard is rebuildable

Facts, FTS, branch rows, and workspace effects move to content shards keyed by
`workspace_root` (natural isolation) or `hash(session_id) % N`. Each shard has:

- its own `PersistenceHandle`/writer thread;
- its own `projection_watermarks(session_id, journal_seq)` table;
- a projector that consumes the same journal frames as today.

A shard's derived state is disposable: if `watermark < journal.durable_seq` or
the shard is missing/corrupt, the projector replays from the journal and writes
a fresh full rewrite.

### 3. `[INV-INDEX-03]` No cross-shard transaction

Catalog and content are written as separate transactions. The journal is the
only atomic unit; the derived planes are allowed to disagree for the duration
of a projection and are repaired from the journal. A crash between the two
writes is invisible to correctness: the next projection or the startup
catch-up converges.

### 4. `[INV-INDEX-04]` Reads route by plane

| Read | Plane |
|---|---|
| thread list, picker detail, title/status | catalog |
| `load_session_full`, `load_thread`, branches, offstream content | content shard for the thread's workspace |
| session-scoped FTS | content shard |
| global FTS (Archivist) | fan-out to every shard, merge by rank/time |
| usage report | catalog (or a usage shard) |

`ThreadSession` already knows its workspace; it gains a catalog handle (global)
and a content handle (workspace). The direct server readers get a
`content_reader_for(session_id)` helper that resolves the workspace from the
catalog and opens that shard. Global search becomes a fan-out with a bounded
result merge.

## Implementation Plan (not yet executed)

1. Add `projection_watermarks` to the catalog (migration 5) and write it after
   every successful content projection; on failure it stays behind.
2. Add `content_shard_path(workspace_root)` and a `PersistenceHandle` per
   workspace; keep the current single-DB mode for `for_path`/tests.
3. Split `save_session_inner` into `save_catalog` (`threads` +
   `thread_policies` + usage) and `save_content` (`causal_nodes`, branches,
   blobs, effects, FTS). The content save upserts a minimal `threads` stub so
   the existing foreign keys remain valid.
4. Route `load_session_full`/`load_thread`/search through the content handle.
5. Add startup catch-up: scan `journal/*`, compare each `durable_seq` with the
   shard watermark, and replay the gap. A missing shard is created.
6. Convert `search_history` and `offstream`/`monitor` to the routed readers;
   global search fans out.
7. Delete the single-writer session projection from the catalog writer.

## Acceptance Criteria

- `rm -rf <content shards>` with journals intact rebuilds every shard.
- N concurrent sessions on N workspaces show commit p99 independent of N and
  projection lag bounded by the shard queue, not by other shards.
- A catalog row missing or behind is repaired from the journal; a content
  shard behind is repaired from its watermark.
- Global search returns the same hits as the single-DB version (order may
  differ only by the documented merge rule).

## Why This Is Staged

The journal plane is already the source of truth (ADR-0236/0239). The index
split changes the on-disk layout, every direct reader in the server, and the
migration/verification path. Doing it without the watermark and routed readers
would either break global search or silently drop a workspace from the picker.
The journal does not change when this lands; only the projector and read
routing do.

## Negative Knowledge

- **Do not share a writer between shards.** That recreates the bottleneck with
  more files.
- **Do not make catalog and content one transaction again.** They are separate
  planes with separate failure domains.
- **Do not route a content read through the catalog.** The catalog has no
  transcript bodies; a fallback there is a silent data loss.
