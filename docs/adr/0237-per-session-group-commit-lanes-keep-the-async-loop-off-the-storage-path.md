---
id: ADR-0237
title: "Per-Session Group-Commit Lanes Keep the Async Loop Off the Storage Path"
status: accepted
date: 2026-11-21
scope: persistence/journal, persistence/thread, engine/cognition/agent
superseded_by: null
negative_knowledge: true
---

# 0237. Per-Session Group-Commit Lanes Keep the Async Loop Off the Storage Path

- **Status**: Accepted
- **Date**: 2026-11-21
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md)
  (what is written), [ADR-0238](0238-configuration-and-usage-are-process-snapshots-never-per-request-disk-reads.md)
  (what leaves the request path)

---

## Context and Problem Statement

Nuo hosts several sessions in one server process. Before this decision, every
session's commit went through one global writer actor; a slow fsync, a
checkpoint, or a 37 MiB delta in session A delayed session B's turn boundary.
The async loop also contained synchronous bridges (`block_in_place`,
`std::thread::spawn` + join) whose only job was to wait for that actor.

A single process writing several sessions is the easy case: there is no
cross-process coordination to preserve, so serialising every session behind one
queue is a pure cost.

## Architectural Decisions

### 1. `[INV-JOURNAL-LANE-01]` A session is pinned to one lane and stays FIFO

The journal runtime owns a fixed number of lanes:

```text
lanes = clamp(available_parallelism, 2, 16)   # amended by ADR-0248
lane  = crc32c(session_id) % lanes
```

The original count was `min(4, available_parallelism)`. Four lanes decoupled a
handful of sessions but capped concurrent `fdatasync` calls at four and made a
collision between two busy sessions likely. ADR-0248 §6 sizes the lane count by
the resource it contends for: the disk's parallel-fsync capacity, floored at two
and capped at sixteen so a pathological core count cannot spawn more lane
threads than the disk can serve.

Each lane is a dedicated OS thread fed by a bounded `tokio::mpsc` channel. A
session is hashed once, so all of its appends are FIFO by construction; two
different sessions land on different lanes whenever the hash allows it, and
never share an ack chain.

### 2. `[INV-JOURNAL-LANE-02]` One fsync per touched file per batch

A lane drains its queue (up to 256 jobs) and then:

1. writes every frame with `write_all` on a cached `O_APPEND` handle;
2. issues **one `fdatasync` per touched file**;
3. sends each job its individual ack.

A single append performs its own fsync; under load the batch amortises both
syscalls. Latency is bounded by `batch window + one fsync`, never by unrelated
work.

### 3. `[INV-JOURNAL-LANE-03]` A failed write leaves no partial frame

Before writing, the lane records the file's append offset. If `write_all`
fails, it truncates back to that offset and drops the cached handle. Every
subsequent job to the same path in that batch fails without writing. The next
append re-scans the segment and resumes at the last complete frame. The reader
therefore never observes a prefix that the writer did not durably acknowledge.

### 4. Back-pressure lives at the turn boundary, never inside the stream

The lane queue is bounded (`1024`). `submit` awaits the enqueue, so a full
lane stops the caller **before** the next model request starts. Nothing inside
the provider stream or the UI relay ever waits on storage.

### 5. Segment rotation and snapshots are cold-path work

Segments rotate at 64 MiB. A snapshot is written every 512 commits by a
`spawn_blocking` task, never inline in the commit path. A snapshot is written
to `*.json.tmp`, fsynced, then renamed; a torn snapshot is skipped and the
previous one is used.

## Consequences

- N sessions have N independent ack chains (modulo lane collisions); a slow
  session does not consume another session's budget.
- Per-commit latency is a write + one fsync. The 10–200 ms tails observed with
  the global actor are gone unless the disk itself stalls.
- The async runtime never parks a worker on `block_in_place` for a commit.
- A lane thread is a process resource: four threads, not one per session.
- If a lane is lost (panic), its sessions' pending jobs fail; the caller's turn
  returns an error and the journal's last complete frame remains the truth.

## Alternatives Considered

| Alternative | Why rejected |
|---|---|
| One global writer with group commit | Batching hides throughput, not head-of-line blocking; one large delta still delays every session in the batch |
| One thread per session | Correct but unbounded: dozens of hosted sessions means dozens of threads and fsyncs racing for the disk |
| `io_uring` per lane | Additional runtime/dependency complexity for a workload whose fsync, not syscall overhead, is the floor |
| `tokio::fs` / `spawn_blocking` for each append | Reintroduces a blocking-pool hop and makes per-session ordering depend on pool scheduling |

## Negative Knowledge

- **Do not put SQLite in the lane.** The lane has one job: append bytes and
  fsync. Mixing transactions or query work into it recreates the global actor.
- **Do not ack on `write_all`.** Without the per-file fsync, "durable" is a
  lie; the journal's whole contract is the fsync.
- **Do not share a lane's file handle across threads.** `O_APPEND` makes the
  write atomic, not the ordering; the lane is the ordering authority.
- **Do not derive the lane count from the session count.** Lanes are a
  process-wide fsync budget (ADR-0248); sizing them per hosted session turns a
  bounded resource into an unbounded one.
