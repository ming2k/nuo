---
id: ADR-0209
title: "Read Pages Run Off-Thread and Respect a Scan Budget"
status: accepted
date: 2026-11-17
scope: tools/filesystem, runtime, cancellation
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0209. Read Pages Run Off-Thread and Respect a Scan Budget

- Status: Accepted
- Date: 2026-11-17

## Context and Problem Statement

`read` was `async fn` in signature only. Its body performed synchronous
`fs::metadata`, `File::open`, `read_to_end`, and `BufReader::read_until` calls
on a Tokio worker. Three consequences followed:

1. **Worker starvation in the session server.** Nuo hosts many sessions in one
   process. A slow filesystem (NFS/FUSE), a huge directory listing, or a
   far-offset scan could occupy several workers at once, delaying provider
   streams, timers, permission prompts, and other sessions.
2. **The dispatcher timeout did not fire.** `tokio::time::timeout` can only
   preempt at an await point. A synchronous scan ran to completion; a 30s
   deadline could be exceeded by minutes.
3. **Cancellation did not stop the work.** Dropping the future released the
   caller, but the blocking loop kept reading. A large file with a far `offset`
   could scan the whole file to return a 50 KiB page.

## Decision

### 1. The synchronous half runs on the blocking pool

`ReadTool::read_path` now splits into:

- an async wrapper that owns the `spawn_blocking` boundary;
- `read_path_sync`, which performs metadata, directory listing, magic-byte
  probing, text pagination, and image ingest with `std::fs`;
- a `BlockingRead::Pdf` marker for the one branch that needs the async
  document extractor.

PDF file loading also moved into `spawn_blocking`; extraction already did.

### 2. Cancellation is observable inside the blocking loop

The `ToolContext` passed to a dispatch carries a per-call child token. The
blocking scanner checks it at a bounded line cadence (512 lines), and the
directory reader at 512 entries. A timeout cancels that token before returning,
so the blocking task exits at its next check instead of continuing to completion.

### 3. Forward and tail scans have a byte budget

`MAX_READ_SCAN_BYTES = 64 MiB`. A forward `offset` that would require scanning
past the budget is refused with an actionable hint (`search_text` / `code_query`
to locate the line, or a negative offset for the tail). A tail read of a file
larger than the budget is refused before opening it.

The page output caps (2000 lines / 50 KiB) are unchanged; the budget bounds the
*work*, not just the result.

### 4. Repeated deep reads resume from sparse checkpoints

A large text read records a checkpoint roughly every 1 MiB: the byte offset and
the number of complete lines before it. The cache key is the same optimistic-concurrency version used by writes
(content hash for small files, metadata identity including inode/ctime for
large ones), so a changed file invalidates its checkpoints. A later positive-offset read seeks to the nearest checkpoint
strictly before the requested line instead of rescanning from byte zero.

The index is bounded (16 files, 128 checkpoints each) and is a latency
optimisation, not a source of truth. Tail reads still scan to EOF and seed the
index for later forward reads.

## Consequences

- **Positive**: slow or pathological reads no longer occupy Tokio workers; the
  30s read deadline and user cancellation now actually stop the scan; huge-file
  far-offset reads fail honestly instead of burning minutes of I/O.
- **Negative & Incurred Costs**: one blocking-pool handoff per read; a tail read
  of a >64 MiB file requires a different tool; the blocking task may run briefly
  after the caller has already received a cancellation result.
- **Neutral**: page format, line numbering, version tokens, and read pagination
  are unchanged.

## Discarded Alternatives

- **Keep the synchronous body and wrap the whole future in `timeout`**: the
  timeout still cannot preempt a synchronous section.
- **Use `tokio::fs` for every call**: line-oriented `read_until`/seek semantics
  and the magic-byte probe are simpler and faster on the blocking pool.
- **Build a full line index for every file**: useful for repeated far-offset
  reads, but a cache with its own invalidation/version policy; the scan budget
  plus `code_query` covers the common case first.
- **Silently return an empty page on budget exhaustion**: that would make a
  large file look empty; an explicit refusal is the honest answer.
