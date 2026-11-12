---
id: ADR-0171
title: "Thread Removal Is Durable and Server-Owned: Stop Is Not Delete"
status: accepted
date: 2026-11-12
scope: protocol/control, server/registry, persistence/session-store, cli/thread
superseded_by: null
supersedes: [ADR-0161]
---

# 0171. Thread Removal Is Durable and Server-Owned: Stop Is Not Delete

- Status: Accepted
- Date: 2026-11-12
- Deciders: Nuo Architecture Working Group
- Supersedes: [ADR-0161](0161-thread-purge-is-a-client-side-orchestration-over-the-existing-kill-contract.md)
- Related: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0048](0048-cli-architecture-modernization-resource-symmetry-and-application-observability-topology.md), [ADR-0158](0158-derived-indexes-are-built-off-the-commit-path-from-durable-cursors.md), [ADR-0170](0170-role-scoped-thread-history-and-workspace-binding.md)

## Context

`nuo thread rm` did not remove a thread. It sent `ControlRequest::KillThread`,
which cancelled the driver, fired SessionEnd hooks, and dropped the in-memory
registry entry. The durable row, transcript, derived indexes, and snapshot
files survived. The dashboard's own help text said so: `/kill` means "tear the
session down (history kept on disk)".

ADR-0161 described `thread rm --all` as a purge and claimed the kill path
deleted from `nuo.db`. The implementation never did. The CLI enumerated only
the live monitor snapshot, so it could not even see suspended or post-restart
history. There was no durable delete primitive available to the command line:
the only permanent delete was the in-session picker's `DeleteThread`, which is
scoped to one active store and one user-facing row at a time.

This is a correctness problem, not a naming preference:

- `rm` promised removal and did not remove;
- `--all` was falsely global and destructive to other projects' runtime;
- a purge could race a live driver: deleting a row while a save is in flight
  either loses the delete or resurrects the thread;
- subagent runs had no deletion path and would be orphaned;
- the CLI could not list the exact set it was about to destroy.

## Decision

### 1. Stop and delete are different verbs

- **`nuo thread stop <id>` / `stop --all`** is runtime-only. It cancels and
  awaits the driver, fires SessionEnd hooks, and leaves every durable row and
  file intact. The next attach lazy-resumes it.
- **`nuo thread rm <id>` / `rm --all`** is a permanent durable delete. If the
  target is hosted, it is stopped first; then its SQLite row, transcript,
  derived indexes, and snapshot/log artifacts are deleted.

The old meaning of `rm` is not preserved. It was a naming lie; no compatibility
shim keeps it.

### 2. Removal is role-scoped by ADR-0170

`rm --all` deletes the current role's history in the current workspace. It is
not a global drop. The scope expands only through the explicit
`--all-workspaces` / `--all-roles` flags from ADR-0170.

A single-id removal uses the target's own stored role and workspace. An
optional `--role` acts as a guard: a mismatch is refused with the target's real
role, never silently deleted.

### 3. The exact id snapshot is the contract

The client:

1. calls `ListThreads { filter }` for the role-scoped view;
2. shows the role, workspace, thread count, hosted count, and dormant count;
3. asks for affirmative confirmation (default no) unless `--force`;
4. sends `PurgeThreads { filter, ids }` with the exact ids it confirmed.

Threads created after the snapshot survive. This is planning atomicity, not
transactional atomicity: the word "all" names the set the operator saw, not a
promise that concurrent work is impossible.

`StopThreads { filter, ids }` follows the same shape for runtime-only stops.

### 4. The server is the only deleter

The protocol gains two role-scoped operations:

- `ListThreads { filter }` returns `Vec<MonitoredThread>` (durable rows merged
  with resident drivers; dormant rows carry `ThreadHosting::Dormant`);
- `StopThreads { filter, ids }` and `PurgeThreads { filter, ids }` return a
  typed `ThreadRemovalReport { removed, subagents_removed, already_gone,
  skipped }`.

`ControlReply` gains a typed JSON `payload`; this replaces the old convention
of smuggling endpoint/usage/Archivist results through the `error` field.

The server serializes deletion through the existing single-writer door. The
client never opens SQLite.

### 5. Quiesce, then delete

A purge processes each target in this order:

1. validate that the stored `(persona, workspace_root)` still matches the
   requested filter (or, for a single id, use the stored scope);
2. cancel a building target and await its build task;
3. cancel a hosted driver and await `driver_done` — the driver task releases a
   permit only after it has fully unwound, including panic eviction;
4. call `PersistenceHandle::delete_session`, which purges the thread row and
   the derived FTS/causal/cursor rows in the writer;
5. remove the `<id>.json` snapshot and `<id>.jsonl` log from the project or
   workspace-free bucket;
6. publish `ThreadRemoved` for live observers.

The registry's purge gate holds the write side of a claim lock for the whole
run, so no attach can lazy-resume a target between the liveness probe and the
row delete. A target that cannot settle within the shutdown budget is reported
as skipped, never deleted underneath a live writer. The control call carries a dedicated
purge budget (600 s) rather than the generic 10 s verb budget; a large
legitimate purge must not look like a dead peer.

### 6. Subagents are deleted with their parent scope

Subagent runs are durable threads with `fork_kind = 'subagent'`. They are
internal, excluded from the picker and `thread ls`, and inherit their parent's
role and workspace. A purge expands the selected parent ids into their subagent
children and deletes those too. The confirmation reports the user-facing thread
count; the result reports subagent runs separately, so the operator can see the
whole storage effect without having internal runs inflate the browsing count.

### 7. Current active sessions

If a scope contains the session that issued an in-session delete, the existing
`SessionStore::delete` path repoints the store to a fresh session and emits
`ConversationCleared`. A control-plane purge kills the hosted target; attached
clients receive `Exit` / `ThreadRemoved` and retire the tab. Deleting history
never leaves a tab pointing at a resurrected identity.

### 8. What removal does not touch

`rm` deletes thread history only. It does not delete configuration, provider
credentials, permissions, workspace trust, input prompt history, usage records,
role memory, or the workspace effect journal. Dropping the database remains a
grossly different operation and is not a supported purge mechanism.

## Invariants

- **`[INV-REMOVE-01]` Stop Is Not Delete**: Runtime teardown MUST NOT be the
  implementation of a durable-delete command.
- **`[INV-REMOVE-02]` Role-Scoped Bulk**: A bulk removal MUST carry the
  role/workspace filter that produced its id snapshot; a client MUST NOT be
  able to delete outside that scope by supplying a stale id.
- **`[INV-REMOVE-03]` Exact Snapshot**: Bulk removal deletes the confirmed id
  set only. A concurrently created thread survives.
- **`[INV-REMOVE-04]` Quiesce Before Delete**: No durable row may be deleted
  while a driver for that thread may still write.
- **`[INV-REMOVE-05]` Subagent Closure**: A deleted parent's subagent runs are
  deleted in the same operation; no internal durable thread is orphaned.
- **`[INV-REMOVE-06]` Single Writer**: Every delete travels through the
  persistence actor; the CLI never opens the database.
- **`[INV-REMOVE-07]` No Configuration Loss**: Thread purge deletes thread
  history, not the surrounding instance state.

## Consequences

- `rm` now means what it says. `stop` is the non-destructive alternative.
- `rm --all` is safe by default: it no longer kills or deletes other projects'
  developer history.
- A bulk purge reports partial success honestly; one stuck driver does not
  block or corrupt the rest.
- The protocol gains two batch verbs and a typed control payload. This is
  justified: listing durable history and safely quiescing a resident driver
  cannot be composed from the live monitor and `KillThread`.
- The generic 10 s control budget is insufficient for a large purge; the purge
  path owns a larger budget and per-thread skip reporting.
- The old `thread rm --all` client-side `KillThread` loop is deleted, not
  retained as a compatibility path.

## Validation

- `cargo check --workspace --all-targets` is clean.
- Integration coverage:
  - a developer purge leaves a coder thread in the same workspace alive;
  - a coder-scoped purge deletes only the coder thread;
  - `stop --all` leaves the durable row and removes the driver, then a
    subsequent `rm --all` deletes it;
  - `rm --all` without a preview prompt aborts on a non-affirmative stdin;
  - the confirmation scope and counts are printed before deletion.
- Persistence coverage pins role + workspace filtering in summaries,
  switch-candidates, and `latest_session`.
- Migration 30 folds `general` into `developer` and indexes the composite
  scope.
