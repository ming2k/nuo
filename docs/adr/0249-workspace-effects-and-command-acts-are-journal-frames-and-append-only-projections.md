---
id: ADR-0249
title: "Workspace Effects and Command Acts Are Journal Frames and Append-Only Projections"
status: accepted
date: 2026-11-24
scope: persistence/journal, persistence/db, persistence/workspace_effects, persistence/thread, server/runtime
superseded_by: null
negative_knowledge: true
---

# 0249. Workspace Effects and Command Acts Are Journal Frames and Append-Only Projections

- **Status**: Accepted
- **Date**: 2026-11-24
- **Amends**: [ADR-0125](0125-workspace-effect-journal-reversibility-as-a-declared-property.md) (workspace effects move into the session journal as durable truth; SQLite `workspace_effects` becomes a derived projection), [ADR-0140](0140-command-effect-classes-readings-are-receipts-not-records.md) (unbounded command acts are stored append-only instead of rewriting a monolithic JSON column)
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md), [ADR-0242](0242-the-derived-index-is-split-into-a-catalog-plane-and-rebuildable-content-shards.md), [ADR-0248](0248-storage-work-is-split-by-what-it-needs.md)

---

## Context and Problem Statement

ADR-0236 established that the per-session journal is the sole durable source of truth and SQLite is a disposable, rebuildable derived view running with `PRAGMA synchronous=NORMAL`. Two exceptions remained in the persistence layer, and both broke the architecture:

1. **`workspace_effects` lived only in SQLite.**
   - Every `/undo` pre-image record, completion stamp, and reversal stamp was written solely through the single-writer SQLite actor (`synchronous=NORMAL`).
   - On a power loss, a mutated file could survive on disk while its `workspace_effects` WAL commit was lost, leaving `/undo` blind.
   - Worse, `rm -rf <derived SQLite>` + journal replay (ADR-0242's core rebuildability invariant) permanently destroyed every `/undo` record.
   - Every file-mutating tool call also blocked on the shared SQLite single-writer actor instead of riding its own per-session journal lane.

2. **`thread_policies.commands_json` rewrote the entire command ledger on every metadata commit.**
   - ADR-0140 rightly rejected capping the audit ledger to the newest `N` entries, because audit history must not depend on session length.
   - However, storing an unbounded ledger as a single JSON array column caused `O(M * T)` write amplification: every turn serialized and rewrote all `M` prior command acts, violating ADR-0155's `O(delta)` persistence invariant.

---

## Architectural Decisions

### 1. `[INV-TRUTH-01]` Workspace effects are journal frames; SQLite `workspace_effects` is derived

The session journal (`FrameKind::WorkspaceEffect`) carries every workspace effect lifecycle event:

- `Started { effect_id, effect }` — write-ahead intent appended and `fdatasync`-ed **before** the tool mutates the workspace (after the pre-image blob is durably written to CAS).
- `RecordedCompleted { effect_id, effect, completed_at_ms }` — receipt for non-reversible effects (shell/network/process).
- `Completed { effect_id, after_hash, completed_at_ms }` — post-image confirmation.
- `Reversed { effect_id, reversed_at_ms, note }` — monotonic reversal stamp.
- `Expired { effect_id, expired_at_ms }` — retention expiry stamp.

`effect_id` is allocated monotonically per session from the journal's own counter (seeded from replay), so replaying the journal into a fresh SQLite shard reproduces the exact same `(thread_id, id)` rows.

Consequences:
- `/undo` achieves true power-loss durability via the journal's `fdatasync` without introducing a second `synchronous=FULL` SQLite database.
- `rm -rf` of any derived SQLite database or content shard rebuilds `workspace_effects` completely from the journals.
- SQLite remains `synchronous=NORMAL` everywhere with zero exceptions.

### 2. `[INV-TRUTH-02]` Command acts are append-only journal frames and normalized rows

`thread_policies.commands_json` is dropped (clean break, no compatibility shim).

- **Journal**: Each appended `CommandRecord` is written as a `FrameKind::CommandAct { seq, record }` frame (`O(1)` per act; `0` bytes on ordinary turns).
- **SQLite**: Command acts are projected into a normalized `STRICT` table `command_acts(thread_id, seq, payload_json, PRIMARY KEY (thread_id, seq))`.
- Incremental projection only `INSERT`s new acts (`O(delta)`). Full rebuild replays the journal's `CommandAct` frames (and snapshot acts) into `command_acts`.

---

## Discarded Alternatives (Negative Knowledge)

- **Separate SQLite DB with `synchronous=FULL` for `workspace_effects`.** Creates a third durable storage engine, duplicates backup/locking/recovery machinery, and still breaks the "journal is the sole source of truth" invariant.
- **Keep `commands_json` and accept full-array rewrites.** Treats a storage-layout flaw as an unavoidable cost of unbounded audit. Append-only frames + normalized table give unbounded audit at `O(1)` per act.
