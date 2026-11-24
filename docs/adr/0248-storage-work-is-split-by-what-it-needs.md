---
id: ADR-0248
title: "Storage Work Is Split by What It Needs, Not by Who Owns the Database"
status: accepted
date: 2026-11-24
scope: persistence/db, persistence/thread, persistence/journal, server/runtime
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0248. Storage Work Is Split by What It Needs, Not by Who Owns the Database

- **Status**: Accepted
- **Date**: 2026-11-24
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md)
  (what the derived view is), [ADR-0237](0237-per-session-group-commit-lanes-keep-the-async-loop-off-the-storage-path.md)
  (how the journal is written), [ADR-0242](0242-the-derived-index-is-split-into-a-catalog-plane-and-rebuildable-content-shards.md)
  (the not-yet-flipped shard layout), [ADR-0155](0155-turn-persistence-is-o-delta-and-storage-maintenance-never-blocks-the-writer.md)
  (storage maintenance never blocks the writer)

---

## Context and Problem Statement

ADR-0236 removed SQLite from the commit acknowledgement path and made the
journal per-session. That left one shared resource: the single-writer actor.

The actor is correct — WAL permits one writer, and the one-door rule
([ADR-0174](0174-thread-command-receipts-fencing-leases-and-blob-liveness-share-one-durable-door.md))
depends on there being exactly one. But "one writer" was being read as "all
storage work belongs on that one thread". Three kinds of work were queued behind
it that need no write lock at all:

1. **Filesystem work.** A projection offloaded oversized observation bodies to
   the CAS — `write_all` + `fdatasync` + rename — *inside* the writer. A large
   tool output therefore stalled every other session's projection for the
   duration of a file fsync.
2. **Whole-database read work.** The rolling backup ran `VACUUM INTO` on the
   writer connection. This is a read of the entire database; it needs a
   consistent snapshot, not the write lock. On a multi-gigabyte store it stopped
   all writes for its duration.
3. **Schema-wide maintenance.** `PRAGMA optimize` (which may run `ANALYZE` over
   every index) also ran on the writer.

The same confusion existed on the read side: a caller that could not get one of
16 reader leases received an **error**, and nearly every call site maps a reader
error to an empty result. Under many concurrent clients, contention therefore
appeared as *missing data* rather than latency.

Underneath all of this was a scaling limit: each session owned a projection OS
thread, so `N` hosted sessions meant `N` permanently parked threads, `N`
channels, and `N` stacks — for work that is idle almost all the time.

## Decision

### [INV-STORAGE-01] The writer executes SQL and nothing else

`DatabaseEngine` holds a connection and no blob store. A `SaveSession` command
carries a `PreparedSave`, not a `ThreadData`: the fact-graph projection, the CAS
offload, the JSON column encoding, the catalog summary, and the payload
fingerprint are all computed **before** the command crosses the door.

This is enforced by the type system rather than by review — the writer has no
blob store and no file path, so it cannot perform filesystem I/O even if a
future edit wanted it to.

The split has three properties the design depends on:

- a filesystem stall cannot queue a session's projection behind another
  session's large tool output;
- a preparation failure leaves the database untouched (no half-applied
  transaction to roll back);
- a blob written for a commit whose transaction later fails is simply
  unreferenced, and the collector reclaims it.

### [INV-STORAGE-02] A whole-database read does not take the write lock

`VACUUM INTO` opens its own read-only connection. `PRAGMA optimize` opens its own
short-lived write connection with a short busy timeout. Neither is dispatched to
the writer actor, and the actor no longer has a `CreateBackup` or `Optimize`
variant to dispatch to.

Consequence: a backup is concurrent with the writer by construction, and the
only way to reintroduce the stall is to add a writer command for it — which the
removed variants make impossible without a deliberate new command.

### [INV-STORAGE-03] A projection pool is bounded and fair, not per-session

Projections run on a fixed lane pool (`clamp(cores, 2, 8)`), not one thread per
session. The pool keeps every property the per-session worker had:

- **per-session FIFO** — each session has its own queue, appended under that
  session's commit gate;
- **fairness** — a lane's ready list holds a session at most once and rotates it
  to the back after each job, so a busy session cannot starve another on the
  same lane;
- **bounded memory** — the per-session queue cap is a *signal*: a full queue
  marks the session desynced and the next job repairs from the journal;
- **isolation** — a panic in one projection is caught by the lane, marks that
  session desynced, and leaves the lane serving everyone else.

### [INV-STORAGE-04] Reader contention is latency, never absence

A reader checkout waits up to a bounded budget (`750 ms`) for a free lease.
Only a genuinely saturated pool reports exhaustion, and the error names the wait.
A caller may still choose to degrade — but it is now degrading from a real error
after a real wait, not from an instantaneous refusal that looked like "no rows".

The pool holds 32 leases and 8 warm idle connections: `N` hosted sessions can
each read concurrently with headroom for the control plane.

### [INV-STORAGE-05] An in-memory mutation that did not reach the journal is undone

A commit mutates the session before its append because the frame is the delta
that mutation produces. If the append fails, the in-memory transcript is ahead
of the only copy that matters. Leaving it there is silent loss: the next commit
compares against the *in-memory* projection, treats the lost entries as durable,
and writes a metadata-only frame that never carries them.

Every mutating accessor therefore captures a `SessionUndo` under the lock and
the persist path restores it when the commit did not become durable. The
snapshot is O(1) for an appending commit (lengths, not clones) and O(history)
only where the commit already is.

### [INV-STORAGE-06] Lane counts are sized by the resource they contend for

- journal commit lanes: `clamp(cores, 2, 16)` — the floor is the disk's
  concurrent `fdatasync` capacity, and a session hashed onto a lane shares its
  ack chain, so too few lanes reintroduces cross-session head-of-line blocking;
- projection lanes: `clamp(cores, 2, 8)` — bounded above by the single SQLite
  writer, which cannot consume more concurrency than it has;
- reader leases: 32, with a bounded wait rather than an instant refusal.

## Consequences

- The writer's queue depth now reflects *SQL*, not filesystem latency. A
  session's projection is no longer delayed by another session's blob fsync or
  by a backup.
- A backup can run on a live server without stalling turns; its cost is a
  concurrent read, which WAL is designed for.
- Thread count is `O(1)` in the number of hosted sessions instead of `O(N)`.
- A failed journal append leaves the session exactly as the journal has it, so a
  retry cannot silently drop the failed turn.
- Reader bursts become latency instead of silent empty results.
- The `PreparedSave` split is the seam ADR-0242 needs: when the derived index
  becomes a catalog plus content shards, the preparation already has everything
  a shard needs and the writer already only executes SQL.

## Discarded Alternatives (Negative Knowledge)

- **Keep `VACUUM INTO` on the writer and call it "rare".** It is rare and it is
  also unbounded: one multi-gigabyte copy stops every session's projection for
  its duration. Rarity is not a bound.
- **Move the blob write to a background queue after the commit.** Then a crash
  between the commit and the blob write leaves a row that points at bytes which
  were never written — an unrecoverable dangling reference, not merely a lagging
  view.
- **Give each session a projection thread and cap the session count instead.**
  That trades a scheduling problem for a product limit, and the thread still
  costs a stack per session while idle.
- **Let `reader()` block indefinitely.** A wedged pool would then hang the
  caller with no diagnostic; a bounded wait preserves the failure while making
  the common case a success.
- **Reuse the writer connection for the backup.** That is exactly the coupling
  this ADR removes; WAL gives the read-only connection its own consistent
  snapshot for free.
