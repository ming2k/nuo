---
id: ADR-0173
title: "Thread Is the Durable SSOT; Branch Is a Cursor; Runtime and Request Are Projections"
status: accepted
date: 2026-11-13
scope: protocol/thread, persistence/thread-store, engine/runtime, server/attachment
superseded_by: null
supersedes: [ADR-0160]
negative_knowledge: true
---

# 0173. Thread Is the Durable SSOT; Branch Is a Cursor; Runtime and Request Are Projections

- Status: Accepted
- Date: 2026-11-13
- Deciders: Nuo Architecture Working Group
- Supersedes: [ADR-0160](0160-compaction-is-a-transcript-fact-and-the-causal-node-is-a-derive.md) (compaction is a transcript fact)
- Related: [RFC-0003](../rfc/0003-thread-branch-fact-and-request-assembly.md), [ADR-0172](0172-request-plan-containers-are-regions-and-blocks-are-boundaries.md), [ADR-0174](0174-thread-command-receipts-fencing-leases-and-blob-liveness-share-one-durable-door.md), [ADR-0175](0175-session-ir-is-deleted-and-the-transcript-is-a-projection.md), [ADR-0155](0155-turn-persistence-is-o-delta-and-storage-maintenance-never-blocks-the-writer.md), [ADR-0158](0158-derived-indexes-are-built-off-the-commit-path-from-durable-cursors.md)

## Context

The conversation model has four competing shapes:

- a transcript that acts as the message SSOT;
- a derived causal graph (`SessionIR`);
- a live `Vec<Message>` owned by the agent;
- a request plan rendered from instructions and messages.

Every durable operation must keep the transcript and the graph convergent, and
every request is built by one of two pipelines (live or `compile_session_request`).
Fork existed only as an implied `TimelineCursor`; there was no contract for
whether a fork copies messages, shares facts, or creates a new thread. Large
tool outputs were partly inlined and partly in CAS, with truncation policies
spread across storage, request, and UI.

The result is a split-brain write model and an undefined branch model.

## Decision Drivers

- **One durable truth.** Conversation facts must have one owner.
- **Fork must be cheap and safe.** A fork must not copy a long conversation.
- **Projections must be disposable.** Request and UI views must never write
  back into facts.
- **Compaction must be branch-local.** A fold on one branch must not hide facts
  from a sibling.
- **Large bytes have one home.** CAS, referenced by facts.
- **One request builder.** Live, resume, replay, and branch switching must not
  drift.
- **No historical baggage.** Old names and dual-write paths are deleted.

## Decision

### 1. Thread is the durable aggregate

`Thread` owns:

- `graph: ThreadGraph` — immutable facts and parent links;
- `state: ThreadState` — branches, active branch, counters, status;
- `policy: ThreadPolicy` — rules, budgets, route constraints.

`Thread` replaces `SessionIR`. The transcript is a `TranscriptView`, not a
store.

### 2. ThreadFact is a durable fact, not a turn

`ThreadFact` kinds are `Message`, `Observation`, `Compaction`, `Termination`,
and `SystemNotice`. A ReAct turn may produce multiple facts; a fact may belong
to no turn. `Round` and `Turn` are execution coordinates in `FactContext`, not
fact kinds.

`Observation` is canonical for tool execution. Tool-role messages are
synthesized by `RequestView`; they are not durable facts.

### 3. Branch is a cursor; fork is O(1)

`BranchState` holds:

- `head: FactId`;
- `forked_from: Option<FactId>`;
- `compaction_horizon: Option<FactId>`;
- optional model/route/effort overrides.

A fork inserts a branch cursor at an existing fact. It copies no facts and no
messages. A subagent is a new `Thread`, not a branch.

### 4. Compaction is branch-scoped

A compaction appends a `Compaction` fact at the branch head and moves that
branch's horizon to `first_kept_fact_id`. Sibling branches continue to traverse
the original facts.

The branch live path is `head .. compaction_horizon`; GC roots are the union of
all branch live paths.

### 5. ThreadStore is the persistence boundary

`ThreadStore` writes `ThreadDelta`s and owns the CAS `BlobStore`. It does not
fold, truncate, or interpret facts for the model. Large payloads are `BlobRef`s;
`blob_refs` and GC decide liveness.

### 6. ThreadRuntime is a bounded working set

`ThreadRuntime` is per `(thread, branch)`:

- `ThreadWindow` — bounded branch facts;
- `RequestView` — branch-scoped message projection.

It does not hold the full thread, sibling branches' facts, blob bytes,
`TranscriptView`, or `RequestContext`.

### 7. Client is the live connection

`Client` replaces `Session` for a live client/server connection. It owns
connection state, permissions, and a per-client `TranscriptView`. It never owns
conversation facts. `Session` is deleted unless a separate auth lifecycle needs
`AuthSession`.

### 8. One request path

`RequestBuilder` is the only conversation-to-request path:

```text
RequestView -> RequestBuilder -> ModelRequest
  -> Provider::project -> WireRequest -> encode -> EncodedRequest
```

`RequestPlan` is deleted. `compile_session_request` is demoted to a replay
helper that projects a `RequestView`.

### 9. Concurrency

- One writer lease per thread with a fencing token.
- Commands carry `command_id` and `expected_revision`.
- One active turn per branch; input is queued FIFO.
- Permission is the intersection of thread ceiling, initiator grants, and tool
  risk.
- Multi-server reads are allowed; only the lease holder writes.

## Invariants

- **INV-THREAD-01** — `Thread` is the only durable conversation SSOT.
- **INV-THREAD-02** — Projections never write back into facts.
- **INV-THREAD-03** — `ThreadRuntime` is rebuildable from `ThreadStore`.
- **INV-THREAD-04** — One runtime authority per `(thread, branch)`.
- **INV-THREAD-05** — `Client` never owns facts.
- **INV-THREAD-06** — `TranscriptView` has no durable store or separate resume
  path.
- **INV-FACT-01** — Facts are immutable; corrections are new facts.
- **INV-FACT-02** — Turns are execution coordinates, not fact kinds.
- **INV-BRANCH-01** — Branch is a cursor, not a copy.
- **INV-BRANCH-02** — Fork is O(1).
- **INV-BRANCH-03** — Compaction is branch-scoped.
- **INV-BRANCH-04** — A fact is GC-eligible only when unreachable from every
  branch live path.
- **INV-FOLD-01** — Request folding is a view transform, never a fact.
- **INV-BLOB-01** — Large payloads live in CAS; facts hold `BlobRef`s.
- **INV-BLOB-02** — GC never deletes a referenced blob.
- **INV-REQUEST-01** — `RequestBuilder` is the only request path.
- **INV-REQUEST-02** — Tools and messages never enter prompt text.

## Negative Knowledge

- **Do not make the transcript the SSOT.** It cannot represent observations,
  termination, lineage, or branch horizons.
- **Do not copy a conversation on fork.** A branch cursor is O(1) and preserves
  shared immutable facts.
- **Do not make a subagent a branch.** Subagents need independent policy and
  lifecycle.
- **Do not compact globally.** One branch must not hide facts from a sibling.
- **Do not let storage fold for the model.** Request folding belongs to
  `RequestView`.
- **Do not let a projection write back.** That turns a cache into a fact.
- **Do not keep `SessionIR`/`SessionStore`/`SessionDelta` aliases.** Clean break.
- **Do not persist `TranscriptView`.** It creates a second resume path.

## Consequences

### Positive

- One durable fact model and one request path.
- O(1) forks with shared lineage.
- Branch-local compaction and GC by reachability.
- Bounded runtime memory via `ThreadWindow`.
- CAS deduplication for large payloads.
- Clear separation of execution (`ThreadRuntime`), presentation (`Client`), and
  durability (`ThreadStore`).

### Cost

- Storage and migration are substantial: transcript + `causal_nodes` +
  `timelines` become `Thread` + `ThreadFact` + `BranchState`.
- GC, compaction, fork, and writer leases require property/fault tests.
- `PROTOCOL_VERSION`, `MIN_PROTOCOL_VERSION`, and the DB schema move together.
- Old names are deleted; embedders must migrate.

## Compliance and Verification

The RFC-0003 Definition of Done is the acceptance gate:

- invariants encoded as property/model-based tests;
- crash matrix covered by fault injection;
- SLO benchmarks pass;
- fork/branch/compaction/GC tests pass;
- migration dry-run and rollback pass;
- threat model and permission model reviewed;
- no dual-write path remains.

`cargo check --workspace` and `cargo test --workspace` are necessary but not
sufficient. The design is not “industrial-grade” until the RFC-0003 readiness
checklist is complete.
