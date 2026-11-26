---
id: ADR-0255
title: "Versioned Content Shards, Pure Catalog Plane, and Honest Durability Boundaries"
status: accepted
date: 2026-11-26
scope: persistence/db, persistence/migrations, persistence/shards, persistence/journal, persistence/blobs, persistence/workspace_effects
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0255. Versioned Content Shards, Pure Catalog Plane, and Honest Durability Boundaries

- **Status**: Accepted
- **Date**: 2026-11-26
- **Deciders**: Nuo Architecture Working Group
- **Amends**:
  [ADR-0189](0189-immutable-baseline-plus-numbered-migrations.md) (extends the immutable baseline + numbered migration + structural verification engine to content shards),
  [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md) (clarifies the split between authoritative local tables and disposable derived views in `nuo.db`),
  [ADR-0242](0242-the-derived-index-is-split-into-a-catalog-plane-and-rebuildable-content-shards.md) (completes the clean-break cutover by dropping retired content tables from `nuo.db` and versioning content shards),
  [ADR-0249](0249-workspace-effects-and-command-acts-are-journal-frames-and-append-only-projections.md) (restores `INV-EFFECT-04` and cross-thread effect-id collision safety on production content shards)
- **Related**: [ADR-0192](0192-every-durable-sqlite-file-has-a-versioned-schema.md), [ADR-0248](0248-storage-work-is-split-by-what-it-needs.md), [ADR-0254](0254-orthogonal-thread-role-and-workspace-primitives.md), [ADR-0256](0256-retire-parallel-thread-store-and-unify-on-journal-session.md)

---

## Context and Problem Statement

ADR-0236, ADR-0242, and ADR-0249 moved conversation truth into per-session append-only journals and split the derived SQLite index into a small catalog (`nuo.db`) plus fixed-`K` content shards (`shards/content-XX.db`). A post-cutover audit of `crates/persistence` exposed six concrete structural fractures left behind by that transition:

1. **Unversioned, drifted content-shard DDL.** `db/shards.rs` embedded a hand-copied `CREATE TABLE IF NOT EXISTS` script outside the `db/migrations` runner. That copy had already silently drifted from the authoritative baseline:
   - `trg_workspace_effects_no_early_reversal` (`INV-EFFECT-04`) was omitted, leaving production shards unprotected while unit tests still ran against single-DB `nuo.db` and stayed green.
   - `fts_thread_facts` lost `tokenize = 'porter unicode61'`, silently splitting production search behavior from test behavior.
   - Because shards lacked a `schema_migrations` ledger and `sqlite_master` verification, any future column addition would break existing shards without detection or upgrade.
2. **Zombie tables in `nuo.db`.** Even after ADR-0242 moved content tables to shards and ADR-0256 retired `SqliteThreadStore`, `nuo.db` still carried empty content-plane tables (`causal_nodes`, `thread_branches`, `thread_blob_refs`, `command_acts`, `workspace_effects`, `fts_thread_facts`) and dead `ThreadStore` tables (`thread_commands`, `thread_leases`, `thread_lease_counters`, `thread_blobs`). Single-DB unit tests continued targeting those ghost tables.
3. **Lossy empty-string encoding for unbound workspaces.** `thread_policies.workspace_root` was `TEXT NOT NULL`, forcing `workspace: None` to serialize as `""`. Readers (`lookup_session_workspace`, `load_session_full_from_planes`) decoded `Some("")` back into `Some(WorkspaceBinding { root: "" })`, turning unbound threads into bogus empty-path bindings on lazy resume.
4. **Silent `commands` loss on SQLite fallback hydration.** `load_session_full_from_planes` selected a literal `13` as `schema_version` (`CURRENT_SCHEMA_VERSION = 17`), causing `migrate_session_data` to clear `commands` freshly loaded from the normalized v7 `command_acts` table.
5. **Unbounded journal snapshots & CAS GC race.**
   - `SessionJournal::compact_at` pruned covered `seg-*` segments but never deleted superseded `snap-*.json` snapshots, causing `O(N^2)` disk growth on long sessions.
   - `BlobStore::retain_only` ran concurrently with live `prepare_save` blob offloads without a modification-time grace window and unconditionally deleted `.tmp.*` staging files, risking deletion of freshly offloaded blobs before their `thread_blob_refs` row committed.
6. **Ambiguous durability taxonomy.** Prose described `nuo.db` as wholly disposable, yet `nuo.db` holds four authoritative local-state subsystems that cannot be rebuilt from `journal/` (`permission_rules`, `usage_*`, `input_history`, and `kv_store` carrying `state:route_settings`, `state:workspace_security`, `state:connection_usage`).

---

## Architectural Decisions

### 1. `[INV-SHARD-01]` Content shards use the versioned migration + structural verification engine

Content shards (`shards/content-XX.db`) own an immutable baseline (`migrations/content_shard/0001_baseline.sql`) and catalog (`db/migrations/content_shard.rs`, `CONTENT_SHARD_SCHEMA`) driven by the same schema-agnostic runner as `nuo.db` and `role_memory.db` (ADR-0192).

- Zero `CREATE TABLE IF NOT EXISTS` DDL exists in `db/shards.rs`.
- `migrations/content_shard/0001_baseline.sql` is the sole DDL source for `thread_branches`, `causal_nodes` (with `trg_causal_nodes_immutable`), `thread_blob_refs`, `command_acts`, `workspace_effects` (with all six triggers including `trg_workspace_effects_no_early_reversal` for `INV-EFFECT-04`), `fts_thread_facts` (with `tokenize = 'porter unicode61'`), and `projection_watermarks`.
- **Self-healing disposable open and runtime read repair**: Because content shards are 100% rebuildable from `journal/`, if an existing shard file fails ledger validation, structural verification, or integrity checks (including pre-ADR-0255 unledgered shards), the shard opener removes the stale `content-XX.db` (plus `-wal`/`-shm`) and initializes a fresh verified shard; if a shard file is unlinked at runtime, the shard writer detects `!path.exists()` and reopens a fresh file, and single-thread content reads (`load_thread`, `load_session_full`, `list_workspace_effects`, `undoable_workspace_effects`) replay that session's journal on demand rather than returning `None` until the next restart.
- Content-plane unit tests (`workspace_effects`, `content_projection`, `shards`) open `CONTENT_SHARD_SCHEMA` directly (`open_content_shard_in_memory_for_tests`), never `nuo.db`.

### 2. `[INV-SHARD-02]` Pure catalog plane and truly nullable `workspace_root` (Migration v9)

Migration `0009_pure_catalog_and_nullable_workspace.sql` makes `nuo.db` structurally pure:

- Drops retired content-plane tables: `fts_thread_facts`, `command_acts`, `workspace_effects`, `thread_blob_refs`, `causal_nodes`, `thread_branches`.
- Drops retired `ThreadStore` tables (ADR-0256): `thread_commands`, `thread_leases`, `thread_lease_counters`, `thread_blobs`.
- Drops dead/zombie columns `threads.spawned_from_fact_id` and `threads.status` (live thread activity is ephemeral `ThreadActivity` owned by `MonitorTracker`, never persisted in SQLite).
- Rebuilds `thread_policies` without dead `thread_policy_json` and with:
  ```sql
  workspace_root TEXT CHECK (workspace_root IS NULL OR workspace_root <> '')
  ```
  converting existing `''` rows to `NULL` via `NULLIF(workspace_root, '')`, and recreates `thread_list_view` and all three `thread_policies` indexes.
- Every read and write path maps `Option<WorkspaceBinding>` directly to SQL `NULL` / non-empty `TEXT`, making empty-path workspace bindings unrepresentable at both the SQL and Rust boundaries.

### 3. `[INV-SHARD-03]` Explicit durability taxonomy, rolling backups, and private permissions

Every durable artifact under the data/state directories belongs to exactly one durability class:

| Artifact | Class | Source of truth / Recovery |
|---|---|---|
| `journal/<session>/` | **Authoritative** | Sole truth for conversations, branch DAGs, command acts, workspace effects, path aliases, and deletion tombstones; protected by rolling point-in-time snapshots (`BackupManager::create_journal_snapshot`) |
| `blobs/` | **Authoritative** | Content-addressed observation bodies, oversized message bodies, base64 image payloads, and workspace effect pre/post-images |
| `nuo.db` (`permission_rules`, `usage_*`, `input_history`, `kv_store`) | **Authoritative** | Local user/policy/telemetry state; protected by online rolling `VACUUM INTO` backups |
| `role_memory.db` | **Authoritative** | Long-term role dialogue memory; protected by online rolling `VACUUM INTO` backups |
| Config / auth TOML files | **Authoritative** | User configuration and provider credentials |
| `shards/content-XX.db` | **Disposable derived view** | 100% rebuildable from `journal/` |
| `nuo.db` (`threads`, `thread_policies`, `projection_watermarks`, `commit_receipts`) | **Disposable derived view** | Rebuildable from `journal/` |

- All durable local storage directories (`nuo.db` parent, `shards/`, `journal/`, `blobs/`, `backups/`) and newly created durable files are restricted to owner-only permissions (`0o700` / `0o600` on Unix) via `fs_privacy`.

### 4. `[INV-SHARD-04]` Bounded journal snapshots, CAS offloading, and race-free CAS garbage collection

- **Snapshot pruning**: After `SessionJournal::compact_at` fsyncs and atomically renames `snap-<seq>.json` (and fsyncs the session directory), it deletes all strictly older `snap-<old>.json` files (`old < seq`). A session directory retains at most one published snapshot, keeping disk usage `O(session)` instead of `O(N^2)`.
- **CAS offloading for oversized text and inline base64 images**: Before writing journal frames or compaction snapshots, `offload_session_blobs` moves oversized message bodies (`content_blob`) and inline base64 images (`image_blobs`) into `blobs/` off the tokio runtime, registers every hash in `thread_blob_refs` so CAS GC preserves them, and rehydrates them losslessly in `load_session_blobs`.
- **CAS GC grace window**: `BlobStore::retain_only_with_grace(live, grace)` preserves both unreferenced blobs and `.tmp.*` staging files whose modification time is newer than `now - grace`. Maintenance sweeps pass `DEFAULT_BLOB_GC_GRACE` (10 minutes), closing the race between off-writer `prepare_save` blob writes (`ADR-0248`) and concurrent daily GC.

### 5. `[INV-SHARD-05]` Honest fallback hydration, topological cold rebuild, and effect-id safety

- `load_session_full_from_planes` stamps `CURRENT_SCHEMA_VERSION` rather than a hard-coded legacy `13`, so normalized `command_acts` loaded from content shards are never wiped on SQLite fallback.
- `catch_up_projections_for` replays sessions in cycle-safe recursive ancestor depth order (supporting arbitrarily deep subagent hierarchies), and `save_session_inner` / `upsert_session` detach `parent_thread_id` to `NULL` when a referenced parent no longer exists (matching `ON DELETE SET NULL` when a parent was terminally deleted).
- `workspace_effects` on content shards uses composite `PRIMARY KEY (thread_id, id)` with thread-scoped completion/reversal updates (`complete_effect_for_thread`, `mark_reversed_for_thread`), structurally eliminating cross-thread `effect_id` collisions.
- Dead no-op maintenance stubs (`collect_entry_garbage`, `StorageMaintenanceReport::reclaimed_entries`, `UsageStatsStore::prune_old_days`) are removed.

---

## Consequences & Trade-offs

- **Positive**: Production content shards enforce every SQLite invariant (`STRICT`, `json_valid`, `INV-EFFECT-04`, `porter unicode61` FTS) and are structurally verified on every open.
- **Positive**: Tests exercise the real content-shard schema instead of ghost tables in `nuo.db`.
- **Positive**: Unbound (`workspace: None`) threads round-trip losslessly across persistence and lazy resume.
- **Positive**: Long sessions no longer leak historical full-state snapshots, and live blob offloads cannot be swept mid-commit.
- **Negative & accepted**: Historical pre-v9 `nuo.db` upgrades drop their local `causal_nodes` / `workspace_effects` tables (after v6 has already backfilled `msg_count` and `last_user_prompt`); live content lives in `journal/` and rebuilds into `shards/`. This clean break is intentional per ADR-0242 and ADR-0249.

---

## Discarded Alternatives (Negative Knowledge)

- **Keep `CREATE TABLE IF NOT EXISTS` on content shards and manually patch missing triggers.** That is the exact drift generator that dropped `INV-EFFECT-04` and `porter unicode61` in the first place. Reusing the schema-agnostic `Schema` runner eliminates drift by construction.
- **Keep empty content tables in `nuo.db` "so single-DB unit tests keep compiling".** Violates ADR-0242's "no single-DB test mode" invariant and actively hides schema divergence between tests and production.
- **Lock `BlobStore::put` against GC with a global filesystem mutex.** Reintroduces cross-session I/O serialization that ADR-0248 deliberately eliminated. An `mtime` grace window closes the race with zero lock contention.
- **Keep `workspace_root TEXT NOT NULL DEFAULT ''` and sprinkle `.filter(|s| !s.is_empty())` across callers.** Any reader that forgets the filter silently turns `None` into `Some(PathBuf::from(""))`. Enforcing `NULL` + `CHECK (workspace_root IS NULL OR workspace_root <> '')` in SQL makes the bug unrepresentable.
