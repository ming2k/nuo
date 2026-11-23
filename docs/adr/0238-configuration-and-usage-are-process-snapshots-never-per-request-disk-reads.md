---
id: ADR-0238
title: "Configuration and Usage Are Process Snapshots, Never Per-Request Disk Reads"
status: accepted
date: 2026-11-21
scope: persistence/connections, persistence/model_providers, persistence/connection_usage, engine/cognition/agent
superseded_by: null
negative_knowledge: true
---

# 0238. Configuration and Usage Are Process Snapshots, Never Per-Request Disk Reads

- **Status**: Accepted
- **Date**: 2026-11-21
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md),
  [ADR-0237](0237-per-session-group-commit-lanes-keep-the-async-loop-off-the-storage-path.md),
  ADR-0065 (local spend circuit breaker)

---

## Context and Problem Statement

Request assembly consulted three program-managed stores on every attempt:

| Store | Read on every request | Write on every response |
|---|---|---|
| `connections.toml` (`Connections`) | file read + TOML parse + a second `model_providers.toml` read | — |
| `model_providers.toml` (`ModelProviders`) | file read + TOML parse | — |
| `state:connection_usage` (`ConnectionUsage`) | SQLite read + JSON decode | full JSON read-merge-write through the blocking bridge |

None of these values is a per-turn fact. They are process snapshots: the server
is the only writer, and a budget snapshot from the start of a turn is not
observable. Making every request pay for them fused configuration and
accounting into the ReAct loop for no correctness gain.

The token ledger had the same shape: `persist_request` awaited a SQLite
transaction **before** the provider handshake, and the TTFT clock started
afterwards, so the wait was invisible in telemetry.

## Architectural Decisions

### 1. `[INV-HOTPATH-01]` No request path reads configuration from disk

`Connections::load()` and `ModelProviders::load()` are path-keyed process
caches whose freshness is the file's metadata (modification time + length).
A cache hit costs one `stat`, never a `read_to_string` or TOML parse. `save()`
invalidates or refreshes the entry after its atomic write, so an external edit
is observed on the next load. `pruned()` may call `ModelProviders::load()`
freely: the second read is the cached snapshot.

### 2. `[INV-HOTPATH-02]` No per-response full-JSON read-modify-write

`ConnectionUsage` keeps one snapshot per database path in memory. Reads
(picker ordering, connection spend, budget checks) are `clone` from that
snapshot. The model-turn path calls:

```rust
ConnectionUsage::record_spend_deferred(connection, model, nano_usd);
```

which mutates the snapshot, sets a dirty flag, and returns. A single
process-wide worker flushes the dirty snapshot every 250 ms via the existing
atomic SQLite KV write. `flush_if_dirty()` is exposed for shutdown and tests.
`save()` / `save_exact()` remain the synchronous control-path verbs and keep
the cache coherent.

### 3. `[INV-HOTPATH-03]` Accounting never precedes the dispatch clock

`open_streaming_attempt` starts the provider clock first, then spawns the
in-flight usage-record write:

```rust
request_accounting.start_request();
tokio::spawn(async move { ledger.persist_request(&key).await });
```

The terminal record still reaches SQLite with the turn's usage upserts and with
crash recovery (`InFlight` → `Abandoned`). Only the pre-dispatch wait is gone;
TTFT now measures the real provider boundary rather than hiding harness work
behind it.

## Consequences

- A model request performs zero configuration-file reads and zero usage-store
  reads.
- A model response performs zero synchronous usage-store writes.
- Budget checks read the in-memory snapshot; a spend recorded by the current
  process is visible immediately, and a 250 ms flush window bounds loss on a
  hard crash.
- The picker's recency ordering and last-used model are always available,
  independent of SQLite health.
- `persist_request` is fire-and-forget; a crash between the spawn and the write
  loses that attempt's in-flight marker. The terminal record is still written
  by the turn commit, and crash recovery treats an absent marker as already
  settled — the ledger never invents a completion.

## Alternatives Considered

| Alternative | Why rejected |
|---|---|
| Keep synchronous config reads and rely on the page cache | A read + TOML parse per attempt is still CPU and a syscall on the hot path; it also makes latency depend on disk health |
| `ArcSwap` + filesystem watcher | Correct end state, but a watcher adds a thread and platform-specific semantics; the metadata-keyed cache reaches the same behaviour with less surface |
| Write usage spend through the journal | Viable, but spend is advisory telemetry for a circuit breaker; a debounced snapshot is simpler and does not couple money accounting to the transcript log |
| Make spend purely in-memory | Loses budget enforcement across restarts; the flusher keeps the durable total correct |

## Negative Knowledge

- **Do not cache without a freshness bound.** A process cache that never
  notices an external `connections.toml` edit is a configuration bug waiting
  to happen. The bound is the file's metadata (mtime + length): a hit costs one
  `stat`, so an external edit is observed on the next load, not after a fixed
  interval.
- **Do not move the dispatch clock after the accounting spawn.** That is how
  the pre-dispatch wait became invisible; the clock is the provider boundary,
  not the "after bookkeeping" boundary.
- **Do not flush spend on every response.** The full JSON merge is exactly the
  cost this ADR removes; batch it.
