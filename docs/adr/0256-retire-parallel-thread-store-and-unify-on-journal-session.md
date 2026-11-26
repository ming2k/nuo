---
id: ADR-0256
title: "Retire Parallel ThreadStore / SqliteThreadStore and Unify on Journal-First ThreadSession"
status: accepted
date: 2026-11-26
scope: protocol/thread, protocol/thread_store, persistence/db, persistence/thread
superseded_by: null
supersedes: [ADR-0174]
negative_knowledge: true
---

# 0256. Retire Parallel ThreadStore / SqliteThreadStore and Unify on Journal-First ThreadSession

- **Status**: Accepted
- **Date**: 2026-11-26
- **Deciders**: Nuo Architecture Working Group
- **Supersedes**: [ADR-0174](0174-thread-command-receipts-fencing-leases-and-blob-liveness-share-one-durable-door.md)
- **Amends**: [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md) (retains the pure domain graph types `Thread`, `ThreadGraph`, `ThreadFact`, `BranchState`, `ThreadWindow`, and `RequestView`; retires the unreferenced `ThreadStore` / `BlobStore` trait persistence surface)
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md), [ADR-0239](0239-the-journal-carries-the-branch-graph-and-sqlite-branch-rows-are-derived.md), [ADR-0242](0242-the-derived-index-is-split-into-a-catalog-plane-and-rebuildable-content-shards.md), [ADR-0255](0255-versioned-content-shards-pure-catalog-plane-and-durability-boundaries.md)

---

## Context and Problem Statement

ADR-0173 and ADR-0174 introduced a trait-based `ThreadStore` / `BlobStore` persistence abstraction (`protocol::thread_store`, `SqliteThreadStore`, `SqliteBlobStore`, `MemoryThreadStore`, `MemoryBlobStore`) backed by four dedicated SQLite tables (`thread_commands`, `thread_leases`, `thread_lease_counters`, `thread_blobs`) and two dedicated columns (`threads.spawned_from_fact_id`, `thread_policies.thread_policy_json`).

Subsequent architectural decisions (ADR-0236, ADR-0237, ADR-0239, ADR-0242, ADR-0248, ADR-0249) took production persistence in a fundamentally different and superior direction:

- Every session's durable truth is a per-session append-only journal with group-commit `fdatasync` lanes (`SessionJournal` + `ThreadSession`).
- SQLite is a derived projection split into a small catalog plane (`nuo.db`) and fixed-`K` rebuildable content shards (`shards/content-XX.db`), driven through a single supervised `PersistenceHandle` door.
- Large observation bodies live in the filesystem CAS (`persistence::blobs::BlobStore`), never inside SQLite `BLOB` rows (`thread_blobs`).
- Idempotency and optimistic concurrency live on `SessionJournal` + `commit_receipts` + `threads.revision`, not `thread_commands` / `thread_leases`.

As a result, the entire `ThreadStore` / `SqliteThreadStore` subsystem had **zero production callers** across `crates/server`, `crates/engine`, `crates/nuo`, `crates/host`, and `crates/acp`, while dragging ~1,800 lines of dead code, four dead SQLite tables, and two dead columns through every schema migration, structural check, and compile cycle.

---

## Architectural Decisions

### 1. `[INV-SESSSTORE-01]` One conversation persistence path: `ThreadSession` + `SessionJournal` + `PersistenceHandle`

- Delete `crates/protocol/src/thread_store.rs` (`ThreadStore`, `BlobStore`, `MemoryThreadStore`, `MemoryBlobStore`, `Ack`, `CommandAck`, `GcReport`, `ThreadStoreError`) and remove its exports from `crates/protocol/src/lib.rs`.
- Delete `ThreadStore`-only and unreferenced ADR-0173 scaffolding types/methods from `crates/protocol/src/thread.rs` (`ThreadCommand`, `CommandMeta`, `CommandEnvelope`, `CommandOutcome`, `WriterLease`, `ThreadDelta`, `ThreadStateUpdate`, `FactEnvelope`, `FACT_SCHEMA_VERSION`, `ThreadQuery`, `ByteRange`, `Client`, `ClientId`, `ClientState`, `InputQueue`, `QueuedInput`, `PermissionSnapshot`, `TurnState`, `TranscriptItem`, `TranscriptView`, `ThreadPolicy`, dead `ThreadError` variants, `Thread::apply_command`, `Thread::drain_delta`, `Thread::gc_eligible_facts`, `Thread::spawned_from_fact_id`).
- Delete `SqliteThreadStore`, `SqliteBlobStore`, and their unit test suite from `crates/persistence`; retain only the live content-shard projection functions (`persist_content_facts`, `load_thread_from_planes`) in a focused internal module `crates/persistence/src/db/content_projection.rs`.
- Drop the dead tables (`thread_commands`, `thread_leases`, `thread_lease_counters`, `thread_blobs`) and dead columns (`threads.spawned_from_fact_id`, `threads.status`, `thread_policies.thread_policy_json`) in Migration v9 (ADR-0255).

### 2. `[INV-SESSSTORE-02]` Retain the pure domain fact-graph and projection types

The pure, zero-I/O domain types introduced by ADR-0173 that have live production consumers remain canonical for in-memory branch DAG manipulation, content-shard projection, compaction, offstream reads, and `RequestView` / `ThreadWindow` compilation:

- `Thread`, `ThreadGraph`, `ThreadFact`, `FactPayload`, `FactKind`, `FactId`, `FactContext`
- `BranchId`, `BranchState`, `BranchPolicy`, `ThreadId`, `ThreadRevision`, `ThreadStatus`, `ThreadState`, `ThreadError`
- `ThreadWindow`, `RequestView`

They are pure domain values and projections, never coupled to a second persistence engine.

---

## Consequences & Trade-offs

- **Positive**: Eliminates split-brain persistence models, four dead database tables, two dead columns, and ~1,800 lines of unreferenced store/lease/command machinery.
- **Positive**: Prevents future contributors from mistakenly wiring features against `SqliteThreadStore` or assuming `thread_leases` / `thread_blobs` are active in production.
- **Negative & accepted**: If Nuo ever adds multi-tenant or remote-database persistence in the future, it will build a dedicated sibling crate over `protocol` domain types (per `crates/persistence/src/lib.rs` and ADR-0005) rather than reviving an unexercised local SQLite trait.

---

## Discarded Alternatives (Negative Knowledge)

- **Keep `SqliteThreadStore` "just in case we finish migrating to it later".** A persistence layer with zero production callers rots immediately: it already bypassed the journal, bypassed content shards, wrote blobs into SQLite instead of CAS, and conflicted with ADR-0236/0242/0248. Dead parallel storage engines are active architectural debt.
- **Rewrite production `ThreadSession` to sit on top of `SqliteThreadStore`.** That would revert ADR-0236 and ADR-0242 by putting synchronous SQLite transactions back onto the ReAct turn critical path and re-centralizing all sessions onto one writer mutex.
