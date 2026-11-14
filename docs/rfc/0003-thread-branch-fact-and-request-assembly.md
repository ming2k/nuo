# RFC-0003: Thread, Branch, Fact — A Single Durable Conversation Model

- **Status:** Approved — implementation in progress ([ADR-0173](../adr/0173-thread-is-the-durable-ssot-branch-is-a-cursor.md))
- **Date:** 2026-11-13
- **Related:** RFC-0002 (request plan and wire projection),
  ADR-0172 (containers are regions and blocks are boundaries),
  ADR-0173 (thread is the durable SSOT), ADR-0174 (command receipts,
  fencing leases, and blob liveness),
  ADR-0155 (O(delta) persistence), ADR-0158 (derived indexes off the commit
  path), ADR-0160 (compaction as a durable fact)
- **Supersedes in part:** `Session` as the conversation model,
  transcript-as-SSOT, `SessionIR` as a parallel request compiler, and the
  undefined fork semantics

## 0. Recommendation (TL;DR)

> **Implementation note (2026-11-13):** The RFC core is implemented.
> The protocol-level `Thread` / `ThreadFact` / `BranchState` / `BlobRef` model
> is in `crates/nuo-protocol/src/thread.rs` with fork, branch-scoped
> compaction, live paths, GC roots, command envelopes, writer leases, clients,
> transcript pages, and schema envelopes.
> `ThreadWindow` / `RequestView` are protocol projections in
> `crates/nuo-protocol/src/thread_view.rs`; `RequestBuilder` is the single
> builder in `request_builder.rs`.
> `ThreadDelta`, `ThreadStore`, `CommandAck`, `BlobStore`, and the in-memory
> conformance store are in `crates/nuo-protocol/src/thread_store.rs`;
> `ThreadRuntime` is in `crates/nuo-engine/src/cognition/thread_runtime.rs`.
> The SQLite adapter in `crates/nuo-persistence/src/db/thread_store.rs`
> round-trips create/hydrate/append/fork/compaction, command receipts,
> fencing leases, blob CAS, and mark-and-sweep GC. Migrations 34–37 add
> branch cursors, command receipts, leases, blob liveness, thread facts as the
> conversation SSOT, and working-state metadata; migration 36 converts the
> legacy transcript and drops its tables.
> `SessionIR`, its compiler, `CausalNode`/`NodePayload`/`SessionDelta`, the
> durable transcript tables, and the kernel's old `SessionDelta` sink are
> deleted. `SessionStore` is renamed `ThreadSession`; the transcript is an
> in-memory per-branch projection rebuilt from facts.
> Server branch control is available through
> `ControlRequest::{ForkBranch, ListBranches, SwitchBranch, DeleteBranch}`.
> `/fork` now creates a branch cursor and switches the current view to it; the
> old child-thread copy path, `ForkPoint`, and `ThreadForkKind::Fork` are
> deleted. ADR-0174 records the command/lease/blob transaction contract and
> ADR-0175 records the SessionIR deletion and transcript projection decision.


1. `Thread` is the durable SSOT: `graph` + `state` + `policy`.
2. `ThreadFact` is one immutable durable fact: message, tool observation,
   compaction, termination, or system notice. It is **not** a ReAct turn.
3. `Branch` is a named head in the thread's fact DAG. **Fork creates a branch**,
   not a copy of the conversation.
4. A **subagent** is a new `Thread` with a parent link, not a branch. It has an
   independent lifecycle, policy, and execution context.
5. `ThreadStore` persists `Thread` as `ThreadDelta`s and owns the CAS
   `BlobStore`. It does not perform semantic folding.
6. `ThreadRuntime` is per `(thread, branch)`: a bounded `ThreadWindow` +
   `RequestView`. It never holds the full thread or blob bytes.
7. `TranscriptView` is per `Client`; it is derived from facts and has no
   separate durable store or resume path.
8. `RequestBuilder` is the only request builder. Live, resume, replay, and
   branch switching all use it.
9. `RequestContext` is request-scoped and passed to the builder; it is not a
   runtime field.
10. Compaction is a branch-scoped durable fact. Request-time folding is a
    `RequestView` transform and is never persisted.
11. `Session` is retired. A live client/server connection is a `Client`
    (or `AuthSession` only if an auth lifecycle exists).

The core rule:

> **Thread is the fact. Branch is a cursor. Runtime and request views are
> projections. Projections never write back into facts.**

---

## 1. Motivation

### 1.1 `Session` is overloaded

`Session` currently means the durable conversation, the runtime driver, the
working state/policy, and a client connection. The domain term `thread` already
exists (`thread_id`, `ThreadScope`). The conversation should be a `Thread`; a
live connection is a `Client`.

### 1.2 Transcript as SSOT forces a split-brain write model

The transcript is a UI-shaped message log. The graph is richer: observations,
termination, notices, lineage, compaction horizons. Making the transcript the
SSOT forces every graph fact to be mirrored, and the transcript cannot represent
the whole truth without becoming the graph.

### 1.3 `SessionIR` is a second request compiler

`compile_session_request` re-implements instruction assembly, cache-boundary
analysis, and request lowering. The live path has its own
`InstructionRegistry -> RequestPlan -> ModelRequest` pipeline. The two can
drift, and only one is exercised on the hot path.

### 1.4 Fork semantics were undefined

The DAG and `TimelineCursor` implied branching, but there was no explicit
contract for:

- whether a fork creates a thread, a branch, or a copied transcript;
- how facts are shared;
- how compaction interacts with branches;
- how a subagent differs from a fork.

This RFC makes the distinction first-class.

### 1.5 Large payloads need one physical home

Large tool outputs and media belong in a content-addressed blob store,
referenced by facts. UI and request views must resolve the same reference; only
their read policies differ.

---

## 2. Goals and Non-Goals

### Goals

- One durable conversation model: `Thread`.
- First-class branches and O(1) forks.
- One runtime authority per `(thread, branch)`.
- One request builder for live, resume, replay, and branch switching.
- Explicit, disposable request and UI projections.
- Large payloads stored once in CAS, referenced by facts.
- Compaction as a branch-scoped fact; request folding as a view.
- Clear separation of `Thread`, `Branch`, `Fact`, `Client`, `Round`,
  `Turn`.

### Non-Goals

- Redesigning the TUI or provider protocols.
- A general event-sourcing framework.
- Preserving `SessionIR`/`SessionStore`/transcript-SSOT APIs.
- Making projections durable.
- Distributed multi-writer consensus.

---

## 3. Principles

1. **One fact model.** `Thread` is the only durable conversation truth.
2. **Facts are immutable.** A correction is a new fact, not an edit.
3. **Branches are cursors, not copies.** Fork shares the immutable prefix.
4. **Projections are disposable.** `ThreadRuntime`, `RequestView`, and
   `TranscriptView` can be rebuilt.
5. **No write-back from projections.** Folding, truncation, and display policy
   never mutate facts.
6. **Compaction is a fact.** It creates a branch-scoped node and moves that
   branch's horizon.
7. **Physical bytes are content-addressed.** Facts hold `BlobRef`s.
8. **One builder.** `RequestBuilder` is the only path to `ModelRequest`.
9. **Clean breaks.** Old names and dual-write paths are deleted, not aliased.

### 3.1 Complexity budget

Every layer must prevent a concrete failure. If a layer cannot answer “what
breaks without me?”, it is deleted.

| Layer | Prevents | Cost |
| :--- | :--- | :--- |
| `Thread` aggregate | two competing conversation models | one durable shape |
| `Branch` cursor | copying a conversation on fork | branch state per fork |
| `ThreadFact` | transcript losing observations/lineage | polymorphic payload |
| `ThreadStore` | dual-write split-brain | storage adapter |
| `BlobStore` | inline bloat and duplicate bytes | CAS + ref ledger |
| `ThreadRuntime` | full-history memory blow-up | runtime manager, LRU |
| `ThreadWindow` | re-reading the whole thread per request | bounded fact cache |
| `RequestView` | provider-specific folding leaking into facts | per-request projection cache |
| `TranscriptView` | UI policy contaminating facts | per-client view cache |
| `Client` | per-client state duplicated into execution | connection manager |
| `RequestBuilder` | live/replay request drift | one shared builder |
| `ModelRequest` | providers depending on engine internals | provider-neutral contract |
| `WireRequest` / `EncodedRequest` | projection and encoding tangling | two thin transport types |

If a future addition cannot justify itself in this table, it does not land.

---

## 4. Terminology

| Term | Definition | Lifetime |
| :--- | :--- | :--- |
| `Thread` | Durable conversation aggregate: graph + state + policy | durable |
| `ThreadGraph` | Immutable fact DAG (parent/child, lineage) | durable |
| `FactId` | Stable identity of a fact (not a sequence or a turn id) | durable |
| `ThreadFact` | One durable fact (message, observation, compaction, termination, notice) | durable |
| `ThreadState` | Branches, active branch, counters, status | durable |
| `ThreadPolicy` | Thread-wide rules, budgets, model/route constraints | durable |
| `Branch` | Named head/fork point in the DAG | durable |
| `BranchState` | Head, fork origin, compaction horizon, policy overrides | durable |
| `ThreadDelta` | Incremental facts + state/policy update + watermark | durable write unit |
| `ThreadStore` | Persistence boundary for `Thread` and CAS blobs | durable |
| `BlobStore` | Content-addressed storage for large bytes | durable |
| `ThreadRuntime` | Bounded working set for one `(thread, branch)` | process |
| `ThreadWindow` | Bounded branch-scoped fact window | process / cached |
| `RequestView` | Branch-scoped message projection with budget folding | one request / cached |
| `TranscriptView` | Per-client UI/export projection | client |
| `RequestContext` | Identity, rules, tools, provider guidance | one request |
| `Round` | One user request/response cycle | execution |
| `Turn` | One model request inside a round | execution |
| `Client` | Live client/server connection | connection |

A `ThreadFact` is **not** a turn. A turn may produce multiple facts; a fact may
belong to no turn (compaction, termination, notice).

---

## 5. Invariants

- **INV-THREAD-01** — `Thread` is the only durable SSOT for conversation facts.
- **INV-THREAD-02** — `TranscriptView` and `RequestView` never write back into
  `Thread`.
- **INV-THREAD-03** — `ThreadRuntime` is rebuildable from `ThreadStore`; it is
  never a second SSOT.
- **INV-THREAD-04** — Exactly one runtime authority exists per
  `(thread, branch)`.
- **INV-THREAD-05** — `Client` never owns conversation facts.
- **INV-THREAD-06** — `TranscriptView` is per-client, derived from facts,
  and never a fact source.
- **INV-THREAD-07** — `TranscriptView` has no separate durable store; it is
  rebuilt from `ThreadStore` on attach.
- **INV-FACT-01** — Every fact has a stable id, a parent, a sequence, and a
  kind. A fact is immutable once committed.
- **INV-FACT-02** — `Round` and `Turn` are execution coordinates, optionally
  attached to facts; they are not fact kinds.
- **INV-BRANCH-01** — A branch is a cursor: `head`, `forked_from`,
  `compaction_horizon`, and policy overrides. It does not copy facts.
- **INV-BRANCH-02** — Fork is O(1): it appends a branch cursor, not messages.
- **INV-BRANCH-03** — A fact may be reachable from multiple branches; it remains
  immutable and is GC'd only when unreachable from every branch/thread.
- **INV-BRANCH-04** — Compaction is branch-scoped: it never hides facts from a
  sibling branch.
- **INV-BRANCH-05** — A subagent is a new `Thread`, not a branch.
- **INV-FOLD-01** — Request-time folding is a `RequestView` transform and is
  never persisted as a fact.
- **INV-FOLD-02** — Compaction is a `ThreadFact` and changes only its branch's
  horizon.
- **INV-BLOB-01** — Large payloads live in `BlobStore`; facts hold `BlobRef`s.
- **INV-BLOB-02** — UI and request views resolve the same `BlobRef`; they may
  apply different read policies.
- **INV-BLOB-03** — GC never deletes a referenced blob. Eviction is recorded as
  a fact before bytes are removed.
- **INV-REQUEST-01** — `RequestBuilder` is the only conversation-to-request
  path.
- **INV-REQUEST-02** — Tools and messages are moved into the request exactly
  once; they never enter prompt text.
- **INV-REQUEST-03** — `RequestContext` is runtime-only and never persisted.

---

## 6. Design

### 6.1 Thread and Branch

```rust
pub struct Thread {
    pub id: ThreadId,
    pub parent_thread_id: Option<ThreadId>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub graph: ThreadGraph,
    pub state: ThreadState,
    pub policy: ThreadPolicy,
}

pub struct ThreadState {
    pub active_branch: BranchId,
    pub branches: BTreeMap<BranchId, BranchState>,
    pub round_counter: u64,
    pub status: ExecutionStatus,
}

pub struct BranchState {
    pub id: BranchId,
    pub name: String,
    pub head: FactId,
    pub forked_from: Option<FactId>,
    pub compaction_horizon: Option<FactId>,
    pub policy_overrides: BranchPolicy,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
```

`Thread` replaces `SessionIR`. `history` becomes `graph`. `SessionState` becomes
`ThreadState`. `SessionPolicy` becomes `ThreadPolicy`.

`Branch` is a cursor, not a copy:

- `head` is the latest fact on this branch;
- `forked_from` is the fact where the branch diverged;
- `compaction_horizon` is branch-local;
- `policy_overrides` lets a branch use a different model/route without changing
  the thread default.

A thread always has at least one branch (`main`). The `active_branch` is the
default branch for new clients; a client may select another.

### 6.2 ThreadFact

```rust
pub struct ThreadFact {
    pub id: FactId,
    pub parent_id: Option<FactId>,
    pub seq: u64,
    pub timestamp_ms: u64,
    pub kind: FactKind,
    pub payload: FactPayload,
    pub context: FactContext,
}

pub enum FactKind {
    Message,
    Observation,
    Compaction,
    Termination,
    SystemNotice,
}

pub struct FactContext {
    pub round: Option<u64>,
    pub turn: Option<u64>,
}
```

`FactPayload` keeps the current `NodePayload` shape:

- `Message { message }`;
- `Observation { call_id, tool_name, output_ref, metrics, lifecycle }`;
- `Compaction { summary, first_kept_fact_id, tokens_before, files, ... }`;
- `Termination { reason, partial_output, duration_ms }`;
- `SystemNotice { source, notice_type, content }`.

A `Message` fact may carry inline text or a `BlobRef`. An `Observation` fact
always carries a `BlobRef` for the full output plus a preview.

`FactId` is an opaque, stable identity (for example UUIDv7 or ULID), distinct
from:

- `seq`, which orders facts within the thread log;
- `Round` / `Turn`, which are execution coordinates;
- a content hash, which cannot distinguish two identical messages.

It is the key for parent links, fork points, idempotent replay, and GC
reachability. A `FactId` is never reused.

`ThreadFact` is the in-memory shape. Persisted facts use `FactEnvelope`
(§6.20) so the schema version travels with the payload.

Facts are immutable. A correction is a new fact with a parent pointing at the
corrected fact, not an edit.

### 6.3 Fork

Fork is a first-class command on a `Thread`:

```rust
pub enum ThreadCommand {
    Fork {
        from_fact_id: FactId,
        name: String,
        policy_overrides: BranchPolicy,
    },
    DeleteBranch { branch_id: BranchId },
    // ...
}
```

Semantics:

1. Validate `from_fact_id` is reachable in the thread.
2. Insert a new `BranchState` with `head = from_fact_id`,
   `forked_from = Some(from_fact_id)`, and the requested overrides.
3. Update `ThreadState` and append a `ThreadDelta` state update.
4. No facts are copied. The new branch shares the immutable prefix.

Consequences:

- Fork is O(1) in storage and message count.
- The parent and child branches can diverge independently.
- Compaction on one branch does not affect the other.
- Deleting a branch removes only the cursor; facts remain if reachable from
  another branch, and GC removes unreachable facts.

A branch can be exposed to the API as a thread-like view (`thread_id/branch_id`)
if the UI wants tabs. The durable aggregate remains one `Thread`.

### 6.4 Subagent vs Fork

| | Fork | Subagent |
| :--- | :--- | :--- |
| Aggregate | same `Thread` | new `Thread` |
| Facts | shares immutable prefix | independent facts |
| Policy | branch overrides | thread policy |
| Lifecycle | branch cursor | own thread lifecycle |
| Compaction | branch-scoped | thread/branch-scoped |
| Use case | “continue from here in another direction” | “delegate a scoped task” |

A subagent gets `parent_thread_id` and `spawned_from_fact_id` for lineage, but
it does not share the parent's facts. It receives a task prompt and builds its
own thread.

### 6.5 BlobRef and BlobStore

```rust
pub struct BlobRef {
    pub hash: String,
    pub size: u64,
    pub mime: &'static str,
    pub preview: Option<String>,
}
```

`ThreadStore` composes a `BlobStore`:

```rust
pub struct ThreadStore {
    db: ThreadDb,
    blobs: BlobStore,
}
```

- `append(delta)` writes facts, branch state, policy, and `blob_refs` in one
  transaction.
- `hydrate(thread, branch)` reads the branch path and state; blobs resolve
  lazily.
- `gc()` deletes only blobs absent from the durable `blob_refs` ledger.
- Retention eviction is recorded on the fact before bytes are removed.

### 6.6 ThreadStore

```rust
pub trait ThreadStore {
    fn hydrate(&self, thread: &ThreadId, branch: &BranchId) -> Result<HydratedThread, ThreadStoreError>;
    fn append(&self, thread: &ThreadId, delta: ThreadDelta) -> Result<Ack, ThreadStoreError>;
    fn snapshot(&self, thread: &ThreadId) -> Result<Option<ThreadSnapshot>, ThreadStoreError>;
}
```

`ThreadDelta` is the current `SessionDelta` renamed, extended with branch
updates:

```rust
pub struct ThreadDelta {
    pub thread_id: ThreadId,
    pub parent_thread_id: Option<ThreadId>,
    pub previous_watermark_seq: u64,
    pub new_watermark_seq: u64,
    pub new_facts: Vec<ThreadFact>,
    pub state_update: ThreadStateUpdate,
    pub policy_update: Option<ThreadPolicy>,
    pub updated_at_ms: u64,
}

pub struct ThreadStateUpdate {
    pub branches: Vec<BranchState>,
    pub removed_branches: Vec<BranchId>,
    pub active_branch: Option<BranchId>,
    pub round_counter: Option<u64>,
    pub status: Option<ExecutionStatus>,
}
```

`append` is idempotent at the watermark. `hydrate` is `snapshot + deltas` or a
full graph read. `ThreadStore` does not fold, truncate, or interpret facts for
the model.

### 6.7 ThreadRuntime

`ThreadRuntime` is scoped to one `(thread, branch)`:

```rust
pub struct ThreadRuntime {
    pub thread_id: ThreadId,
    pub branch_id: BranchId,
    pub revision: ThreadRevision,
    pub window: ThreadWindow,
    pub view: RequestView,
}
```

It is a bounded working set, not the full thread:

- created by hydrating the branch window from `ThreadStore`;
- updated by appending facts to the window and committing a `ThreadDelta`;
- rebuildable at any time;
- never written directly to storage except through `ThreadStore`.

It deliberately does not hold:

- the full `Thread` graph;
- sibling branches' facts;
- blob bytes;
- `TranscriptView`;
- `RequestContext`.

Multiple clients attached to the same branch share one runtime. Clients on
different branches get separate runtimes. `RequestContext` is passed to
`RequestBuilder` by the server/agent.

### 6.8 ThreadWindow

```rust
pub struct ThreadWindow {
    pub facts: Vec<ThreadFact>,        // branch path tail + compacted summary
    pub head: FactId,
    pub compaction_horizon: Option<FactId>,
    pub revision: ThreadRevision,
    pub oldest_loaded: Option<FactId>,
}
```

- Branch-scoped: follows `head` back to `compaction_horizon`.
- Bounded by fact count and byte budget.
- Older facts stay in `ThreadStore` and are paged in on demand.
- It is the raw material for `RequestView`, new-turn appends, and event fan-out.
- It is not a second fact model: every item is a `ThreadFact`.

### 6.9 RequestView

```rust
pub struct RequestView {
    pub messages: Vec<Message>,
    pub revision: ThreadRevision,
    pub folded: Vec<FoldRecord>,
}
```

Building it:

```text
ThreadWindow (branch-scoped)
  -> message facts in branch order
  -> budget pruning
  -> tool-result folding
  -> RequestView
```

Rules:

- Request-time folding never writes back to `Thread` or `ThreadWindow`.
- The view is cached by `(thread_id, branch_id, thread_revision)`.
- Any append, compaction, or policy change invalidates it.
- A role/model/provider switch does not invalidate the message view; it changes
  instruction assembly in `RequestBuilder`.
- A request may use a preview/stub for a large blob; the fact still owns the
  full `BlobRef`.

### 6.10 RequestBuilder and wire chain

```rust
pub struct RequestBuilder;

impl RequestBuilder {
    pub fn build(
        view: &RequestView,
        context: &RequestContext,
        tools: &[Arc<dyn Tool>],
    ) -> ModelRequest;
}
```

`RequestBuilder` is the only conversation-to-request path. It:

1. resolves `RequestContext` sections into `Block`s;
2. renders `fixed` / `transient` via `render_blocks`;
3. builds `ModelRequest` with tools, messages, and instruction text;
4. computes revisions and cache fingerprints.

Wire chain:

```text
ModelRequest
  -> Provider::project
  -> WireRequest { protocol, body }
  -> WireRequest::encode
  -> EncodedRequest { protocol, bytes, content_type }
  -> RequestBuilder::encoded
  -> transport
```

`Provider::project` is an explicit trait method. It never moves tools or
messages into text.

### 6.11 Compaction vs request folding

| | Compaction | Request folding |
| :--- | :--- | :--- |
| Owner | compaction path | `RequestView` |
| Scope | branch | one request |
| Persistent | yes, `ThreadFact::Compaction` | no |
| Changes branch head | yes | no |
| Changes branch horizon | yes | no |
| Affects sibling branches | no | no |
| Visible to UI | yes, as a checkpoint | only as a truncated view |
| Survives restart | yes | no |

Compaction writes a `ThreadFact::Compaction`, advances the branch's
`compaction_horizon`, and leaves sibling branches untouched. Request folding
only changes the message window for one request.

### 6.12 TranscriptView

`TranscriptView` is a per-client read model, not a runtime field:

```text
Thread facts / ThreadWindow
  -> TranscriptProjector
  -> Client.transcript
  -> TUI / export / search
```

- Branch-scoped: a client selects a `branch_id`.
- Multiple clients may view different branches or different pages.
- On attach: hydrate the last N turns from `ThreadStore`, then subscribe to
  `ThreadDelta`.
- Older pages load from `ThreadStore` on scroll/search.
- On detach: drop the view.
- It has no durable store and no separate resume path; a reconnect runs the
  same hydrate path.
- Client-local UI state (scroll, expanded refs, filters) may be persisted
  separately, but it is not conversation truth.
- Display truncation is a view policy; it never writes back.

### 6.13 Client

```rust
pub struct Client {
    pub id: ClientId,
    pub thread_id: ThreadId,
    pub branch_id: BranchId,
    pub connection: ConnectionHandle,
    pub permissions: PermissionSnapshot,
    pub transcript: TranscriptView,
}
```

A client is a live client/server connection. It owns connection state,
permissions, UI state, and a transcript page. It does not own facts. Closing a
client releases its view; the thread and runtime remain.

### 6.14 Round, Turn, Fact

```text
Thread
  └── Branch 1..N
        └── Round 1..N
              └── Turn 0..N
                    └── ThreadFact 0..N
```

- **Round** — one user request/response cycle.
- **Turn** — one model request inside a round.
- **Fact** — one durable node.

A turn may produce several facts (assistant message, observation, notice). A
fact may belong to no turn (compaction, termination, system notice).
`FactContext { round, turn }` is optional metadata for grouping and analytics.

### 6.15 Memory budget and eviction

`ThreadRuntime` must not make every thread/branch resident.

- `ThreadWindow` is capped by maximum fact count and byte budget.
- `RequestView` is capped by the model context budget.
- Each attached `Client` keeps a bounded `TranscriptView` page.
- Blob bytes are loaded lazily; only previews stay resident.
- `RequestContext` is request-scoped and not stored on the runtime.
- Idle runtimes are evicted by LRU / idle timeout; the next attach hydrates from
  `ThreadStore`.
- Under pressure: drop `RequestView` first, then shrink `ThreadWindow`, then
  evict the runtime.
- Compaction rebuilds the window from the new branch horizon and releases old
  facts.

The invariant:

> **Runtime memory is proportional to active `(thread, branch)` runtimes plus
> per-client transcript pages, not to total history.**

### 6.16 Raw facts and terminal forms

There are three terminal forms, each with one raw source:

```text
Durable raw:
  ThreadFact + BlobRef

Write terminal:
  ThreadDelta -> ThreadStore (facts + blob refs)

Request terminal:
  ThreadWindow -> RequestView -> RequestBuilder
    -> ModelRequest -> Provider::project -> WireRequest
    -> encode -> EncodedRequest

UI terminal:
  ThreadStore / ThreadWindow -> TranscriptProjector
    -> TranscriptView -> TUI
```

- **Durable terminal**: `ThreadFact` + `BlobRef` in `ThreadStore`.
- **Transport terminal**: `EncodedRequest` (bytes).
- **UI terminal**: `TranscriptView` (rendered rows).

Intermediate forms:

- `ThreadWindow` — bounded fact cache;
- `RequestView` — request message projection;
- `ModelRequest` — provider-neutral prepared request;
- `WireRequest` — provider-shaped JSON.

Persistence is not a fourth pipeline: `ThreadDelta` is the write form of the
durable facts. `TranscriptView` is not a second store: it is rebuilt from the
durable terminal on attach and updated from deltas while attached.

### 6.17 Canonical representation of tool execution

`Observation` is the canonical durable fact for a tool execution. A tool-role
`Message` is a **projection**, not a second fact:

| Layer | Representation |
| :--- | :--- |
| Durable fact | `ThreadFact::Observation { call_id, tool_name, output_ref, metrics, lifecycle }` |
| Assistant call | `ThreadFact::Message` with `tool_calls` |
| RequestView | synthesizes provider tool messages from `Observation` |
| TranscriptView | renders `Observation` as a tool step |
| Legacy tool message | migrated to `Observation`; never written by new code |

This prevents double counting: a tool result exists once as an observation fact
and is materialized into provider-specific messages only at request projection.
If a provider requires a tool-role message in history, `RequestView` creates it;
it is not a durable fact.

### 6.18 Concurrency and command model

#### Single writer per thread

A thread has exactly one writer lease at a time. The lease is a row with:

```rust
pub struct WriterLease {
    pub thread_id: ThreadId,
    pub owner_id: ServerId,
    pub fencing_token: u64,
    pub expires_at_ms: u64,
}
```

Every `ThreadCommand` carries:

```rust
pub struct CommandMeta {
    pub command_id: CommandId,       // idempotency key
    pub expected_revision: ThreadRevision,
    pub fencing_token: u64,
}
```

The store rejects a command when:

- the fencing token is stale (`WriterLeaseLost`);
- the revision does not match (`RevisionConflict`);
- the command was already applied (`command_id` replay returns the prior ack).

This gives at-most-once effects and at-least-once retries.

#### One active turn per branch

A branch executes at most one model turn at a time. Clients attach to the
branch and submit input to a queue:

```rust
pub struct InputQueue {
    pub branch_id: BranchId,
    pub pending: Vec<QueuedInput>,
}
```

- Queue order is server-assigned sequence, not client wall clock.
- The active turn drains steering input only at safe boundaries (between model
  requests or before tool dispatch).
- A second prompt while a turn is active is either queued or rejected by policy;
  it never starts a concurrent turn on the same branch.
- Different branches may execute concurrently; they share immutable facts but
  have independent heads and horizons.

#### Turn ownership and permissions

A turn has an initiating `Client`. Permission is the intersection of:

1. `ThreadPolicy` ceiling;
2. the initiating client's grants;
3. the tool's own risk profile.

No client can elevate beyond the thread ceiling. Other clients can observe
events and the transcript, but cannot grant permission for a turn they do not
own. If the initiator disconnects, policy selects one of:

- `Pause` — wait for reattach;
- `Delegate` — hand ownership to another authorized client;
- `Abort` — terminate the turn with a recorded `Termination` fact.

The policy is explicit; there is no implicit “last client wins” behavior.

#### Multi-server

A server without the writer lease rejects `ThreadCommand`s. A server that loses
the lease must stop appending before its next command; the fencing token makes
a late append fail even if the lease row changed. Reads may be served by any
server from `ThreadStore`.

### 6.19 Compaction, reachability, and GC

The branch live path is:

```text
path(branch) = head -> parent -> ... -> compaction_horizon (inclusive)
```

The horizon is a hard cut: the parent of the horizon is not traversed for that
branch. Facts before the horizon may still be live for another branch.

#### Compaction

1. Create a `ThreadFact::Compaction` at the branch head with
   `parent_id = previous_head`.
2. Set `first_kept_fact_id` to the first fact that remains visible.
3. Set `BranchState.head` to the compaction fact.
4. Set `BranchState.compaction_horizon` to `first_kept_fact_id`.
5. Append the delta atomically.

The request path is:

```text
compaction summary
  + facts from first_kept_fact_id .. head (excluding the compaction fact itself)
```

Sibling branches are untouched: their paths still traverse the original facts.

#### Fork after compaction

A fork point must be on the selected branch's live path
(`head .. compaction_horizon`). Forking from a fact that is before the horizon
or already GC'd returns `FactNotLive`. Forking from the compaction fact or a
kept fact is allowed.

#### GC

Live facts are the union of every branch's live path plus every branch head:

```text
live_facts = union(path(branch_a), path(branch_b), ...)
live_blobs = BlobRefs referenced by live facts
```

- A fact before a horizon is not live for that branch, but may be live for a
  sibling.
- A fact unreachable from every branch is GC-eligible.
- The compaction fact's `first_kept_fact_id` and audit metadata are not owning
  references; they never keep pre-horizon facts alive.
- GC is mark-and-sweep with a grace period:
  1. compute marks from branch heads and horizons;
  2. write a GC epoch/tombstone;
  3. delete unmarked facts and blobs after the grace window;
  4. a crash leaves marks/tombstones that the next pass can resume.
- Blob GC deletes only hashes absent from the live `blob_refs` ledger.

#### Branch deletion

Deleting a branch removes its cursor and head. Facts remain if reachable from
another branch; otherwise they become GC-eligible. A thread must keep at least
one branch (`main`).

#### Branch promotion

Promoting a branch to an independent thread is **out of scope for v1**. It
requires either copying facts or cross-thread reachability and GC. v1 returns
`UnsupportedPromotion` explicitly; it does not silently copy or share.

### 6.20 Schema evolution

Every persisted fact and thread record carries a schema version:

```rust
pub struct FactEnvelope {
    pub schema_version: u16,
    pub fact: ThreadFact,
}
```

Rules:

- **Additive changes** (new optional fields) keep the same fact kind and bump
  the schema version; old readers ignore unknown fields.
- **New fact kinds** are preserved by the store and skipped by projections that
  do not understand them; a warning is emitted.
- **Breaking changes** require a new fact kind or a migration. They never
  reinterpret existing bytes.
- `THREAD_SCHEMA_VERSION` and `FACT_SCHEMA_VERSION` move independently.
- The DB migration catalog and `PROTOCOL_VERSION` move together when the
  serialized shape changes.
- Rollback is by restoring the pre-migration backup; old binaries refuse a
  newer schema instead of half-reading it.

### 6.21 Commands, queries, and errors

Every command is wrapped in:

```rust
pub struct CommandEnvelope {
    pub meta: CommandMeta,
    pub command: ThreadCommand,
}

pub enum ThreadCommand {
    AppendFacts { facts: Vec<ThreadFact> },
    Fork { from: FactId, name: String, policy: BranchPolicy },
    DeleteBranch { branch: BranchId },
    Compact { branch: BranchId, summary: String, first_kept: FactId },
    Terminate { branch: BranchId, reason: TerminationReason },
    SetPolicy { policy: ThreadPolicy },
    DeleteThread,
}

pub enum ThreadQuery {
    Hydrate { thread: ThreadId, branch: BranchId },
    BranchList { thread: ThreadId },
    TranscriptPage { thread: ThreadId, branch: BranchId, before: Option<FactId>, limit: usize },
    Fact { thread: ThreadId, fact: FactId },
    Blob { hash: String, range: Option<ByteRange> },
}
```

Error taxonomy:

- `NotFound` — thread/branch/fact/blob absent.
- `RevisionConflict` — expected revision is stale.
- `WriterLeaseLost` — fencing token is stale.
- `FactNotLive` — fork point is outside the branch path.
- `InvalidFact` — payload violates the schema.
- `BlobTooLarge` / `BlobMissing`.
- `CompactionInProgress`.
- `Forbidden` — permission ceiling or client grant.
- `UnsupportedPromotion` — v1 branch promotion.
- `QueueFull` / `TurnActive` — input admission policy.

Errors are typed, never string-matched. A failed command leaves the revision
untouched.

### 6.22 Lifecycle and state machines

#### Thread

```rust
pub enum ThreadStatus {
    Active,
    Idle,
    Archived,
    Deleted, // tombstone; facts are GC-eligible
}
```

- `Active -> Idle` when no attached client and no active turn.
- `Idle -> Active` on attach or command.
- `Active/Idle -> Archived` only when no active turn; archived threads reject
  writes until explicitly unarchived.
- `Archived -> Deleted` is a tombstone command; the thread row remains until GC
  completes, then is removed.
- A `Deleted` thread cannot accept commands. Reads return `NotFound` after GC.

#### Turn

```rust
pub enum TurnState {
    Idle,
    Preparing,
    Streaming,
    ToolRunning,
    Paused,
    Completed,
    Aborted,
    Failed,
}
```

Allowed transitions:

```text
Idle -> Preparing -> Streaming -> ToolRunning -> Streaming -> ... -> Completed
                                      |-> Paused -> Streaming
                                      |-> Aborted
                                      |-> Failed
```

- Exactly one non-idle turn per branch.
- A second prompt is queued or rejected; it never starts a parallel turn.
- `Paused` is the human-decision/steering state; it still owns the branch.
- `Completed/Aborted/Failed` are terminal for that turn.

#### Client

```rust
pub enum ClientState {
    Attached,
    Detached,
}
```

- `Attached` may submit input, receive events, and hold a transcript page.
- `Detached` releases the transcript page; the thread runtime remains.
- A detached initiator triggers the turn-ownership policy (`Pause`, `Delegate`,
  `Abort`).

#### Branch

A branch has no separate lifecycle enum beyond presence in
`ThreadState.branches`. A branch is created by `Fork`, removed by
`DeleteBranch`, and cannot be removed if it is the last branch (`main`).

---

## 7. Security, Reliability, and Failure Modes

### 7.1 Threat model

| Boundary | Threat | Control |
| :--- | :--- | :--- |
| Client → server | Unauthorized thread access | Auth at attach; thread/workspace scoping |
| Client → turn | Permission elevation | Thread policy ceiling ∩ client grants; initiator ownership |
| Thread → thread | Cross-tenant data leak | Tenant/workspace partition on every store key |
| Fact → model | Prompt injection via tool/web output | Untrusted `BlobRef`/`Trust`, data envelopes, precedence |
| Blob → disk | Secret retention | Redaction policy, retention/eviction facts, encrypted-at-rest option |
| Tool → host | Destructive command | Existing `RiskProfile`, permission chain, sandbox |
| Fork → access | Reading a branch the client cannot access | Branch ACLs or thread-level ACLs; `Forbidden` |
| GC → audit | Premature fact deletion | Grace window, tombstones, immutable fact ids |

Trust is a property of the fact/BlobRef (`Trusted` / `Untrusted`), not of the
projection. An untrusted blob is never promoted to trusted policy text.

### 7.2 Permission model

- `ThreadPolicy` defines the ceiling (allowed tools, workspace, risk).
- A turn is owned by the initiating `Client`.
- Effective permission = ceiling ∩ initiator grants ∩ tool risk profile.
- Other clients may observe but cannot grant for a turn they do not own.
- On initiator disconnect: `Pause`, `Delegate`, or `Abort`, chosen by policy.
- Permission decisions are recorded as facts or audit events; they are not
  hidden in the runtime.

### 7.3 Crash consistency matrix

| Crash point | Durable state | Recovery |
| :--- | :--- | :--- |
| Before blob put | no fact, no ref | retry command |
| After blob put, before DB commit | orphan blob | GC removes; command retries |
| During DB transaction | rollback | command retries with same `command_id` |
| After DB commit, before ack | fact committed | replay returns prior ack |
| During snapshot write | old snapshot intact | rebuild snapshot from deltas |
| During GC mark | marks/tombstones partial | next pass resumes; grace window protects |
| During compaction | old head/horizon intact or new delta committed | revision check rejects half-state |

The store is the authority; the runtime never acknowledges a fact before the
delta is durable.

### 7.4 Failure modes

- **Missing blob** — a `BlobRef` whose bytes are gone is a degraded fact; the
  request uses a stub and emits a notice, the transcript shows “content
  unavailable”. It is never treated as empty.
- **Eviction** — retention records the eviction on the fact and removes its
  `blob_refs` entry in one transaction before GC deletes bytes.
- **Delta append failure** — the runtime watermark does not advance; the next
  append retries the same delta. Facts are keyed by `(thread_id, fact_id)`, so
  replay is idempotent.
- **Concurrent clients** — one runtime per `(thread, branch)`; clients
  share it. Steering and permission decisions route through clients, not
  through facts.
- **Fork race** — a fork appends a branch cursor; it never mutates the parent
  branch head. Two forks from the same fact are independent.
- **Compaction race** — a request uses a `ThreadRevision` snapshot. A compaction
  landing mid-turn invalidates the view; the next request uses the new horizon.
- **Branch deletion** — only the cursor is removed. Facts remain if reachable
  from another branch; GC removes unreachable facts.
- **Projection drift** — `RequestView` and `TranscriptView` both derive from
  facts; neither can mutate them.
- **Oversized facts** — facts above the CAS threshold must carry a `BlobRef`;
  the store rejects inline payloads above the hard cap.

### 7.5 Consistency guarantees

- **Writes** — linearizable per `Thread` via the single writer lease and
  `expected_revision`. A command either commits at exactly that revision or
  returns `RevisionConflict`.
- **Read-your-writes** — the command issuer receives an ack with the new
  revision and can hydrate at that revision.
- **Other clients** — monotonic reads via `ThreadDelta` sequence/watermark.
  They may lag, but never observe a revision going backwards.
- **Fork/compaction** — atomic with the head/revision update; a request uses a
  `ThreadRevision` snapshot and never sees a half-applied branch change.
- **Blobs** — immutable and content-addressed; a `BlobRef` either resolves to
  the same bytes or is explicitly missing/evicted.
- **TranscriptView** — eventually consistent per client; stale views are
  refreshed by revision. It is not a consistency boundary.
- **Cross-thread** — subagent threads are independent. There is no cross-thread
  transactional guarantee in v1.
- **Deletion** — a deleted thread is a tombstone; reads return `NotFound` after
  GC. In-flight requests finish against their revision or fail explicitly.

---

## 8. Migration and Compatibility

This is a clean break. No runtime aliases for old names.

### Phase 0 — naming and dead code

- Rename `SessionIR` → `Thread`; `history` → `graph`.
- Rename `SessionState` → `ThreadState`; `SessionPolicy` → `ThreadPolicy`.
- Rename `SessionDelta` → `ThreadDelta`; `SessionStore` → `ThreadStore`.
- Rename `CausalNode` → `ThreadFact`; `NodeKind` → `FactKind`; `NodePayload` →
  `FactPayload`.
- Rename `TimelineCursor` → `BranchState`; `timelines` → `branches`.
- Delete `Session`; use `Client` for live connections.

### Phase 1 — ThreadStore as the write path

- `ThreadStore::append(ThreadDelta)` becomes the primary durable write.
- The current `ir_mirror` becomes the store's fact writer.
- Transcript writes become a `TranscriptView` projection.
- Delete the transcript-as-SSOT commit path.

### Phase 2 — ThreadRuntime and RequestView

- Server owns `ThreadRuntime` per `(thread, branch)`.
- Remove the agent's independent `Vec<Message>` authority.
- `RequestView` becomes the only request input.
- Cache invalidation is keyed by thread/branch revision.

### Phase 3 — one RequestBuilder

- Move live instruction composition and IR compile pass3/pass4 into
  `RequestBuilder`.
- Delete `RequestPlan`.
- `compile_session_request` becomes a replay helper that projects a
  `RequestView` and calls `RequestBuilder`.

### Phase 4 — BlobRef upgrade

- Upgrade `content_blob`/`blob_hash` to `BlobRef` with size/mime/preview.
- Enforce CAS threshold and blob-ref recording in the same transaction.
- Add eviction facts and GC tests.

### Phase 5 — Fork and branches

- Add `ThreadCommand::Fork` and branch state.
- Map existing timelines/active leaves to branches.
- Add fork/branch tests and UI branch selection.

### Data migration

The one-time migration maps the old durable shapes to the new aggregate:

| Old | New |
| :--- | :--- |
| `sessions` row | `Thread` metadata + default `main` branch |
| transcript `Message` entries | `ThreadFact::Message` |
| transcript tool-result entries | `ThreadFact::Observation` (canonical) |
| `causal_nodes` | `ThreadFact` with the same ids where possible |
| `timelines` / `TimelineCursor` | `BranchState` |
| `active_leaf` | branch `head` |
| compaction directive | `ThreadFact::Compaction` + branch horizon |
| `content_blob` / `blob_hash` | `BlobRef` + `blob_refs` ledger |

Procedure:

1. Take a consistent DB + blob-store backup.
2. Run the migration against a shadow database.
3. Validate thread/fact/branch counts, blob-ref coverage, sample transcript
   round trips, and request-build equivalence.
4. Freeze writers; run the migration on the live database.
5. Start the new binary; old binaries refuse the new schema.
6. Keep the backup until the first successful compaction + GC cycle.

Rollback:

- restore the backup and the old binary; there is no dual-write fallback.
- a failed migration never leaves a half-converted database: the migration runs
  one transaction per thread, and the schema version is written last.

### Compatibility

- `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` move together if the serialized
  thread/fact/branch shape changes.
- DB schema version moves with a one-time migration from transcript +
  `causal_nodes` + `timelines` to `Thread` + `BranchState` + `ThreadFact`.
- Old names are deleted, not aliased.
- Code using `Session` for the conversation migrates to `Thread`; code using it
  for a live connection migrates to `Client`.

---

## 9. Performance, SLOs, and Capacity

### 9.1 Initial SLO targets

These are **validation targets**, not guarantees. They must be measured on the
reference hardware and revised with evidence.

| Operation | Target |
| :--- | :--- |
| Append `ThreadDelta` (1–10 facts) | p95 < 25 ms |
| Hydrate 10k facts, warm cache | p95 < 200 ms |
| Hydrate 100k facts, cold | p95 < 2 s |
| Build `RequestView` for 128k-token context | p95 < 100 ms |
| Build `ModelRequest` | p95 < 50 ms |
| Fork (branch cursor) | p99 < 20 ms |
| Compact 100k-fact branch | p95 < 5 s |
| Transcript page (100 entries) | p95 < 100 ms |
| Runtime memory per 128k-token branch | < 256 MB |
| GC pause / interruption | < 100 ms per batch |
| Concurrent clients per branch | 8 without request-build contention |

If a target is missed, the design is revised; the target is not silently
relaxed.

### 9.2 Benchmarks

- Criterion benchmarks for hydrate, append, fork, compact, request build,
  transcript page, and GC.
- Synthetic threads: 1k / 10k / 100k facts; 1 / 8 / 64 branches; 1 MB / 10 MB /
  100 MB blobs.
- Mixed workload: active turn + 4 readers + 1 compaction + GC.
- Memory profiling per runtime and per client transcript page.
- Cold vs warm blob cache.
- SQLite WAL growth and checkpoint behavior.

### 9.3 Capacity and backpressure

- Global runtime budget (bytes + count) with LRU eviction.
- Per-thread branch runtime cap; additional clients wait or attach read-only.
- Input queue cap per branch; overflow returns `QueueFull`.
- Blob streaming with range reads; reject inline payloads above the hard cap.
- Request build concurrency limit; model calls are the bottleneck, not the
  builder.
- Transcript page size cap; UI requests pages, never the whole thread.
- Disk quota and retention policy for blobs; GC watermark.

### 9.4 Observability

Metrics:

- `thread_append_duration_ms`, `thread_hydrate_duration_ms`;
- `request_view_build_duration_ms`, `request_build_duration_ms`;
- `fork_duration_ms`, `compact_duration_ms`, `gc_duration_ms`;
- `runtime_active`, `runtime_memory_bytes`, `runtime_evictions`;
- `blob_cache_hit_ratio`, `blob_bytes_live`, `blob_orphan_bytes`;
- `input_queue_depth`, `turn_duration_ms`, `writer_lease_changes`.

Traces:

- span per command with `thread_id`, `branch_id`, `fact_id`, `command_id`;
- span per turn with `round`, `turn`, `client_id`;
- span per request build with token counts and projection revisions.

Logs:

- structured, no prompt/tool content by default;
- secret-redaction hook before persistence;
- errors carry typed codes, not string matches.

Health:

- store health, blob store health, writer lease age, GC lag, queue depth.

### 9.5 Risk register

| Risk | Impact | Likelihood | Mitigation |
| :--- | :--- | :--- | :--- |
| DAG/GC bug deletes live facts | data loss | medium | property tests, grace window, backup, dry-run GC |
| Migration corrupts old sessions | data loss | medium | shadow DB, per-thread transactions, backup/rollback |
| Concurrent clients split a turn | inconsistent state | medium | single writer, turn ownership, queue, revision |
| Hydrate too slow on long threads | latency SLO miss | high | windowing, snapshots, indexes, benchmarks |
| Runtime memory blow-up | OOM | medium | bounded windows, LRU, client page caps |
| Blob store grows unbounded | disk exhaustion | medium | retention, eviction facts, GC, quotas |
| Schema evolution breaks old facts | read failures | medium | fact schema version, unknown-kind preservation |
| Over-complexity slows delivery | missed roadmap | high | complexity budget, vertical slice, delete unjustified layers |
| Permission elevation via shared runtime | security | low/medium | ceiling ∩ client grants, initiator ownership |

### 9.6 Operational playbook

- **Writer lease lost** — stop appending, reject commands, re-acquire or hand
  off; clients retry with the new fencing token.
- **Blob store unavailable** — reads use previews/stubs; writes fail closed;
  the thread remains consistent.
- **GC lag** — alert on unreclaimed bytes and fact count; GC is resumable and
  never blocks the writer beyond a bounded batch.
- **Migration failure** — restore backup, run shadow validation, fix, retry;
  old binary refuses the new schema.
- **Compaction failure** — keep the old head/horizon; surface
  `CompactionInProgress`/`Failed`; never partially apply.
- **Queue saturation** — reject with `QueueFull`; do not drop input silently.
- **Runtime eviction** — rehydrate on next request; no fact loss.

---

## 10. Work Plan

1. RFC review: settle `Thread`/`Branch`/`Fact` naming, `FactContext`, `BlobRef`.
2. Phase 0: mechanical renames + delete old aliases.
3. Phase 1: ThreadStore write path; transcript projection.
4. Phase 2: ThreadRuntime + RequestView; remove live `Vec<Message>`.
5. Phase 3: RequestBuilder unification; delete `RequestPlan`.
6. Phase 4: BlobRef metadata, eviction, GC.
7. Phase 5: fork/branch commands and UI selection.
8. Docs/ADRs: supersede ADR-0160; update RFC-0002 references.

---

## 11. Test Plan

**Thread and store**

- hydrate(append(delta)) == Thread.
- Delta replay is idempotent.
- Snapshot + deltas equals full hydrate.
- Watermark never advances on failed append.
- Parent/child, branches, and active leaf survive round trips.
- Concurrent clients share one runtime; no duplicate facts.

**Fork and branches**

- Fork from a fact is O(1): no fact/message copy.
- Parent and child branches diverge independently.
- A fact shared by two branches remains immutable.
- Compaction on one branch does not affect the sibling.
- Deleting a branch keeps facts reachable from other branches.
- GC removes only facts unreachable from every branch/thread.
- Subagent is a new thread, not a branch.

**Blob and eviction**

- Large payloads are stored in CAS and referenced by `BlobRef`.
- `blob_refs` ledger covers every reference.
- GC deletes only unreferenced blobs.
- Eviction records a fact before bytes are removed.
- Missing blob produces an explicit degraded view, not an empty string.

**Projections**

- `RequestView` folding never changes `Thread`/`ThreadWindow`.
- `TranscriptView` display truncation never changes facts.
- Both views resolve the same `BlobRef`.
- Request-time folding does not survive restart.
- Compaction changes only the branch horizon and survives restart.
- `TranscriptView` has no separate durable store; reconnect hydrates from
  `ThreadStore`.

**Request building**

- Live and replay paths produce identical `ModelRequest` for the same
  `(thread, branch, revision)` and `RequestContext`.
- `RequestBuilder` is the only builder; no second instruction pipeline.
- Tools and messages never enter prompt text.
- Golden wire tests per provider.

**Execution**

- Round/Turn counters match fact context.
- A turn may produce multiple facts; a fact may have no turn.
- Compaction mid-turn invalidates the request view and the next request uses
  the new horizon.
- Clients on different branches share facts but not views.

### 11.1 Property-based invariants

Use `proptest` / model-based tests for:

- fact DAG is acyclic; every parent exists; `seq` is monotonic per thread;
- `FactId` is unique and never reused;
- a branch head is reachable and its path ends at the horizon;
- a fork point is on the selected branch's live path;
- deleting a branch never deletes a fact reachable from another branch;
- GC marks exactly the union of branch live paths;
- delta replay is idempotent and watermark never regresses;
- compaction on one branch never changes a sibling's request view.

### 11.2 Model-based state machine

Drive a reference model through random command sequences:

```text
AppendFacts | Fork | DeleteBranch | Compact | Terminate | DeleteThread
```

Compare the reference model's branch heads, horizons, live facts, and
projections with the implementation after every step.

### 11.3 Fault injection and crash tests

- kill between CAS put and DB commit;
- kill during DB transaction;
- kill after commit before ack;
- kill during snapshot write;
- kill during GC mark/sweep;
- stale writer lease / fencing token;
- revision conflict;
- blob missing/evicted mid-request;
- queue full / turn active.

Each case asserts the crash matrix in §7.3.

### 11.4 Concurrency tests

- two clients attach to the same branch and submit prompts;
- clients attach to different branches and fork concurrently;
- a compaction races a request build;
- a fork races a branch delete;
- a writer lease changes while a command is in flight;
- permission decisions from different clients cannot elevate.

### 11.5 Performance and capacity

- Criterion benchmarks at 1k / 10k / 100k facts, 1/8/64 branches;
- memory profiling per runtime and transcript page;
- SLO gates in CI for append, hydrate, request build, fork, compact, GC;
- blob cache hit/miss and range-read latency;
- SQLite WAL growth and checkpoint under mixed workload.

### 11.6 Security tests

- untrusted blob cannot become policy text;
- cross-tenant thread/fact/blob access is denied;
- prompt-injection canaries in tool/web output;
- secret redaction before persistence/logging;
- permission ceiling and initiator ownership.

### 11.7 Migration tests

- shadow-DB migration with representative data;
- count/hash validation for threads, facts, branches, blob refs;
- request-build equivalence before/after migration;
- rollback from backup;
- old binary refuses the new schema.

---

## 12. Alternatives Rejected

- **Make a fork a new thread with copied messages.** It duplicates storage and
  makes shared lineage impossible; a branch cursor is O(1).
- **Transcript + linear compaction checkpoints only.** It is simpler and may be
  correct for a single-user CLI, but it cannot represent fork lineage,
  branch-scoped compaction, or shared immutable facts. This RFC targets the
  multi-client/server/fork product; if those requirements are dropped, use the
  simpler model.
- **Make a fork a new thread with shared facts.** It requires cross-thread fact
  resolution and GC; a branch inside the same thread is simpler and matches
  lineage.
- **Make a subagent a branch.** Subagents have independent policy, lifecycle,
  and context; a separate thread is the correct isolation.
- **Keep `SessionIR` and transcript dual-write.** It preserves the split-brain.
- **Keep transcript as durable SSOT.** The graph is richer than messages.
- **Let `ThreadStore` do semantic folding.** Storage would need model budget
  policy and would create a hidden request view.
- **Let `RequestView` write back.** A request-local fold would become a fact and
  lose data.
- **Make `ThreadNode` a `Turn`.** Facts include compaction, termination, and
  notices; turns are execution coordinates.
- **Keep two request builders.** Live and replay drift.
- **Keep `RequestPlan` as a separate type.** It is a one-call intermediate and
  duplicates `ModelRequest`.
- **Store large payloads inline.** It bloats the graph and prevents CAS
  deduplication.
- **Persist `TranscriptView` separately.** It creates a second resume path and
  a split-brain risk.

---

## 13. Decisions and Remaining Questions

### 13.1 Resolved decisions

1. **Branch API** — branches are addressable as `(thread_id, branch_id)`.
   `main` is the default and may be omitted by single-branch clients.
2. **Branch promotion** — out of scope for v1. `UnsupportedPromotion` is an
   explicit error; a future RFC may add copy-on-write promotion.
3. **BranchPolicy** — may override model, route, and effort only. It cannot
   widen tools, permissions, or workspace scope.
4. **Runtime scope** — one `ThreadRuntime` per `(thread, branch)`.
   Multiple clients on the same branch share it; different branches get
   different runtimes.
5. **FactContext** — `round` and `turn` are required for message/observation
   facts and optional for compaction/termination/notice.
6. **Naming** — `ThreadFact` is the durable type; `CausalNode` is not retained
   as a public synonym. `Thread` is the type; “conversation” is a UI label.
7. **Snapshot** — optional, derived optimization. Hydration is snapshot +
   deltas; a snapshot never overrides the delta log.
8. **BlobRef** — carries hash, size, mime, and preview. Trust is a separate
   attribute; it is not inferred from mime.
9. **Retention** — thread policy sets the default; server config may tighten
   it. Eviction is recorded as a fact before GC deletes bytes.
10. **Steering** — one active turn per branch; input is queued FIFO; turn
    ownership belongs to the initiating client. On disconnect the policy is
    `Pause`, `Delegate`, or `Abort`.
11. **Migration** — one-time shadow-DB migration, backup, validation, cutover;
    no dual-write runtime.
12. **Client UI state** — scroll/expanded/filter state is client-local, may be
    persisted separately, and is never conversation truth.
13. **Tool execution** — `Observation` is canonical; tool-role messages are
    synthesized by `RequestView`, not stored as facts.

### 13.2 Remaining questions

These are product/operations decisions, not architecture blockers:

1. ACL granularity: thread-level only, or per-branch access control?
2. Cross-thread fact sharing for future branch promotion or subagent lineage:
   copy-on-write or reference?
3. How `BranchPolicy` conflicts with `ThreadPolicy` are surfaced to the user.
4. Default retention/eviction policy per deployment (desktop vs server).
5. Whether encryption-at-rest and secret redaction are mandatory defaults.
6. Whether the TUI exposes a branch tree or only a branch picker.
7. Long-term naming review: `Thread` vs `Conversation` in user-facing docs.

Everything else in this RFC is a decision, not an open question.

---

## 14. Definition of Done (Industrial Readiness)

The RFC is not “done” when it is accepted. It is done when all of the following
are true:

**Design**

- [ ] Every invariant in §5 has a property test or a model-based test.
- [ ] Concurrency model (§6.18) is ratified, including writer lease, revision,
      turn ownership, steering queue, and permission intersection.
- [ ] Compaction/GC semantics (§6.19) are ratified, including fork-after-
      compaction, branch deletion, and unreachable-fact GC.
- [ ] Canonical tool representation (§6.17) is ratified; no double counting.
- [ ] Schema evolution (§6.20) and migration/rollback (§8) are ratified.
- [ ] Threat model (§7.1) and permission model (§7.2) are reviewed.
- [ ] All remaining questions are either decided or explicitly deferred with
      an owner.

**Implementation**

- [ ] A thin vertical slice runs: append → hydrate → RequestView →
      RequestBuilder → EncodedRequest → transport.
- [ ] Fork and branch switching work end-to-end.
- [ ] Branch-scoped compaction is isolated from sibling branches.
- [ ] Blob CAS, `BlobRef`, `blob_refs`, eviction, and GC work under crash tests.
- [ ] No dual-write path remains; transcript is a projection.
- [ ] Old names (`SessionIR`, `SessionStore`, `SessionDelta`, `SessionView`,
      `CausalNode`, `NodeKind`, `NodePayload`) are deleted, not aliased.
- [ ] One `RequestBuilder`; `RequestPlan` is deleted.

**Verification**

- [ ] SLO benchmarks (§9.1) pass on reference hardware.
- [ ] Crash matrix (§7.3) is covered by fault injection.
- [ ] Property/model-based tests pass with a seeded corpus.
- [ ] Concurrency tests pass: multi-client, multi-branch, fork/compact races.
- [ ] Migration dry-run and rollback are tested on representative data.
- [ ] Security tests pass: cross-tenant, prompt injection, redaction,
      permission ceiling.
- [ ] Golden wire tests pass per provider.

**Operations**

- [ ] Metrics, traces, and logs from §9.4 are emitted.
- [ ] Runbook covers writer lease loss, GC lag, blob store failure, migration,
      and rollback.
- [ ] Backup/restore and retention/eviction are exercised.
- [ ] Capacity plan documents active runtime limits, disk growth, and blob
      quotas.

Only then is the design “industrial-grade”. Until then it is an architecture
proposal with a clear verification plan.
