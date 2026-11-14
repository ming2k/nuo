---
id: ADR-0174
title: "Thread Command Receipts, Fencing Leases, and Blob Liveness Share One Durable Door"
status: accepted
date: 2026-11-13
scope: protocol/thread, protocol/thread-store, persistence/thread-store, engine/runtime, protocol/monitor
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0174. Thread Command Receipts, Fencing Leases, and Blob Liveness Share One Durable Door

- Status: Accepted
- Date: 2026-11-13
- Related: [RFC-0003](../rfc/0003-thread-branch-fact-and-request-assembly.md), [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md)

## Context and Problem Statement

RFC-0003 makes `Thread` the durable conversation aggregate. The first
vertical slice exposed four concrete contract questions the RFC states as
requirements but leaves to the implementation:

1. `ThreadCommand` carries a `command_id` but no thread id. A store may host
   many threads (the in-memory conformance store does), while a production
   writer is normally scoped to one thread.
2. A retried command must return its original acknowledgement. Recording the
   receipt *after* the fact delta would leave a crash window in which the
   command applied but a replay applies it again or fails on a duplicate.
3. A single-writer lease is only useful if its fencing token is monotonic and
   checked inside the same transaction that writes facts.
4. Blob GC must never delete bytes another thread still references. A
   per-thread “live hash” set is not enough.
5. The repository already had a public `ThreadStatus` for the monitor
   (running/idle/needs-approval), while RFC-0003 names the aggregate lifecycle
   `ThreadStatus` (active/idle/archived/deleted). Two public types with one name
   cannot both be reachable at the crate root.

## Architectural Decisions

### 1. Commands execute against an explicit thread door

`ThreadStore::execute(thread: &ThreadId, command: CommandEnvelope)` names the
thread at the call site. The envelope stays exactly as RFC-0003 defines it
(`meta` + `command`); no thread id is duplicated into `CommandMeta`. The
in-memory store can therefore host many threads without inventing a second
envelope shape.

### 2. Receipt, delta, lease check, and blob refs are one transaction

A command is applied only when the following all hold inside the same SQLite
transaction:

- `thread_commands(thread_id, command_id)` has no receipt;
- `threads.revision == meta.expected_revision`;
- the current `thread_leases.fencing_token` equals `meta.fencing_token` (or
  the lease has expired / was never acquired);
- the thread is writable (not archived/deleted).

The receipt is inserted after the delta in the same transaction. A crash
before commit leaves neither; a crash after commit leaves both. Replay returns
the stored `CommandAck` with `replayed: true` and never touches facts.

### 3. Fencing tokens are monotonic per thread

`thread_lease_counters.last_token` is incremented under the same transaction
that writes `thread_leases`. Releasing a lease checks the token. A stale
writer cannot append after a new owner took over, even if its in-memory lease
value still looks valid.

### 4. Blob liveness is a global ledger question

`thread_blob_refs` records `(thread_id, fact_id) -> BlobRef`. GC removes rows
for unreachable facts, then deletes a blob only when **no** `thread_blob_refs`
row anywhere still names its hash. A per-thread difference would let thread A
sweep bytes thread B references.

### 5. Aggregate lifecycle is `ThreadStatus`; monitor activity is `ThreadActivity`

The conversation aggregate owns `ThreadStatus` (`Active`, `Idle`, `Archived`,
`Deleted`). The monitor's live execution badge is renamed `ThreadActivity`
(`Running`, `NeedsApproval`, `NeedsInput`, `Failed`, `Interrupted`, …). The
retired `SessionStatus` alias is deleted, not kept.

### 6. Fact graph validation is part of insertion

`ThreadGraph::insert` rejects a duplicate fact id, a duplicate `seq`, a missing
parent, and any parent-chain cycle before the fact becomes visible. Sequence
uniqueness is checked per thread; an invalid fact can never become a GC root.

## Normative Invariants Established

- **INV-THREAD-08** — A command receipt and its fact/state delta are committed
  in one transaction. Enforced by the SQLite command-replay test.
- **INV-THREAD-09** — A fencing token is checked in the write transaction, not
  only at lease acquisition. Enforced by the stale-token test.
- **INV-BLOB-04** — A blob is deleted only when no `thread_blob_refs` row names
  its hash. Enforced by the GC tests.
- **INV-FACT-03** — A fact with a duplicate sequence, missing parent, or cycle
  is rejected before insertion. Enforced by `ThreadGraph` tests.

## Consequences & Trade-offs

- **Positive**: A retried command is exactly-once; a lost lease cannot write;
  shared blob bytes survive another thread's GC; two lifecycle vocabularies no
  longer collide.
- **Negative & Incurred Costs**: Commands and lease bookkeeping add two tables
  and one transaction per command. The in-memory conformance store must mirror
  the same receipt/lease rules; it does.
- **Migration**: migration 35 adds `thread_commands`, `thread_leases`,
  `thread_lease_counters`, `thread_blobs`, and `thread_blob_refs`, plus the
  thread spawn-lineage column. Existing databases retain their facts; no
  transcript table is reinterpreted.

## Discarded Alternatives (Negative Knowledge)

### Thread id inside `CommandMeta` (Rejected)

- **Why considered**: It would let one trait method be called without a second
  argument.
- **Why rejected**: It duplicates addressing into an idempotency record whose
  identity is already `(thread, command_id)`. The RFC's envelope is the stable
  wire contract; the thread door belongs to the store call.

### Receipt outside the fact transaction (Rejected)

- **Why considered**: Simpler code in the adapter.
- **Why rejected**: A crash between commit and receipt leaves a command that
  applied but cannot be replayed safely — the exact at-most-once guarantee the
  RFC requires.

### Per-thread blob sweep (Rejected)

- **Why considered**: It matches the `ThreadStore` view.
- **Why rejected**: Content-addressed bytes are shared. Liveness is a property
  of the whole ledger, not of the thread performing the sweep.

### Keep `SessionStatus` as an alias (Rejected)

- **Why considered**: Fewer call-site edits.
- **Why rejected**: It reintroduces the retired `Session` noun and leaves two
  names for one monitor concept.

## Links

- [RFC-0003](../rfc/0003-thread-branch-fact-and-request-assembly.md)
- [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md)
- [ADR-0158](0158-derived-indexes-are-built-off-the-commit-path-from-durable-cursors.md)
