---
id: ADR-0242
title: "The Derived Index Is Split Into a Catalog Plane and Rebuildable Content Shards"
status: accepted
date: 2026-11-24
scope: persistence/db, persistence/thread, persistence/reader, server/runtime, server/handlers
superseded_by: null
negative_knowledge: true
---

# 0242. The Derived Index Is Split Into a Catalog Plane and Rebuildable Content Shards

- **Status**: Accepted (clean-break storage layout cutover)
- **Date**: 2026-11-24
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md),
  [ADR-0237](0237-per-session-group-commit-lanes-keep-the-async-loop-off-the-storage-path.md),
  [ADR-0239](0239-the-journal-carries-the-branch-graph-and-sqlite-branch-rows-are-derived.md),
  [ADR-0248](0248-storage-work-is-split-by-what-it-needs.md),
  [ADR-0249](0249-workspace-effects-and-command-acts-are-journal-frames-and-append-only-projections.md)

---

## Context and Problem Statement

The journal commit path is per-session and parallel. Keeping the derived SQLite view in a single database with one writer forced every session's facts, FTS rows, branch rows, command acts, and workspace effects to queue behind the same actor and forced every content reader (`offstream`, `monitor`, `search_history`) onto the same database.

---

## Architecture

```text
                    ┌──────────────────────────────┐
  journal (truth) → │  Catalog plane (nuo.db)      │  threads / thread_policies /
                    │  (small, one writer)         │  usage / permissions / kv /
                    └──────────────────────────────┘  input_history / catalog watermarks
                    ┌──────────────────────────────┐
                    │  Content shards 0..K-1       │  causal_nodes / thread_branches /
                    │  shards/content-XX.db        │  thread_blob_refs / command_acts /
                    │  key = hash(thread_id) % K   │  workspace_effects / FTS /
                    │  (one writer per shard)      │  shard watermarks
                    └──────────────────────────────┘
```

### 1. `[INV-INDEX-01]` The catalog is small and lifecycle-only

`threads`, `thread_policies`, `usage_*`, `permission_rules`, `input_history`, and `kv_store` stay in the catalog database (`nuo.db`). The catalog never stores transcript bodies, FTS rows, command act ledgers, or workspace effects. One writer is acceptable because the rows are tiny and lifecycle-shaped, not token-shaped.

### 2. `[INV-INDEX-02]` Content is sharded by fixed-K `hash(thread_id) % K` and every shard is rebuildable

All content-plane tables (`causal_nodes`, `thread_branches`, `thread_blob_refs`, `command_acts`, `workspace_effects`, `fts_thread_facts`, `projection_watermarks`) live in fixed-`K` content shards (`K = 16` by default) addressed by `crc32c(thread_id) % K`.

Why `hash(thread_id) % K` instead of `workspace_root`:
- **No monorepo hot-spot skew**: 64–128 concurrent sessions inside the same monorepo distribute uniformly across all `K` writers instead of collapsing onto one shard.
- **Strictly `O(1)` resources**: open SQLite files, WAL handles, and writer threads are bounded by `K`, never by workspace count.
- **Immutable routing**: a thread's shard never changes even if a workspace is unbound or moved.

Every shard has its own dedicated `STRICT` schema with **no fake `threads` stub rows** and no cross-plane foreign keys. A shard's state is 100% disposable: deleting `shards/` with journals intact rebuilds every shard via startup catch-up.

### 3. `[INV-INDEX-03]` No cross-shard transaction and no legacy single-DB mode

- Catalog and content shard writes are separate transactions; the journal is the only atomic unit. A crash between the two planes is repaired from the journal on next projection or startup catch-up.
- **One topology everywhere**: tests, `for_path`, and production all run the same catalog + content-shard topology. There is no legacy single-DB fallback mode.

### 4. `[INV-INDEX-04]` Reads route by plane

| Read | Plane |
|---|---|
| thread list, picker detail, title/status, existence check | catalog |
| `load_session_full` | catalog (metadata) + owning content shard (facts, branches, command acts) |
| `load_thread`, `list_workspace_effects`, `undoable_workspace_effects` | owning content shard |
| session-scoped FTS | owning content shard |
| global FTS (`search_history`, `search_history_relaxed`) | fan-out across all `K` shards, merge by score + catalog metadata join |
| usage report | catalog |

---

## Acceptance Criteria

- `rm -rf <shards>` with journals intact rebuilds every content shard (including facts, branches, FTS, `command_acts`, and `workspace_effects`).
- `N=64/128` concurrent sessions on the same or different workspaces show flat journal commit ack p99 and bounded projection lag.
- Global search fan-out merges hits across all shards with deterministic ordering.

## Negative Knowledge

- **Do not shard by `workspace_root`.** Recreates the single-writer bottleneck whenever a swarm runs concurrent sessions in one monorepo, and makes thread/FD count unbounded.
- **Do not upsert fake `threads` stub rows into content shards.** Pure legacy FK baggage; content shards key directly on `thread_id`.
- **Do not maintain a single-DB mode for tests.** Hides routing, watermark, and fan-out bugs behind a test-only topology.
- **Do not share a writer between shards.** Recreates the bottleneck with more files.
