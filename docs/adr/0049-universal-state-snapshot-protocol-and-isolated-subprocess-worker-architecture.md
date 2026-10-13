---
id: ADR-0049
title: "Universal State Snapshot Protocol and Isolated Subprocess Worker Architecture"
status: draft
date: 2026-10-25
scope: wire/nuo-wire, server/nuo-server, runtime/isolation, substrate/workers
superseded_by: null
negative_knowledge: true
---

# 0049. Universal State Snapshot Protocol and Isolated Subprocess Worker Architecture

- Status: Draft
- Date: 2026-10-25
- Deciders: Nuo Architecture Working Group
- Consulted: Server Runtime, Wire Protocol, Security, and Agent Harness Maintainers
- Informed: System Architects, Release Engineering
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md)

---

## Context and Problem Statement

Nuo's session runtime (`nuo-server`) coordinates conversational threads, manages persistent storage in SQLite, and bridges external tools and model providers over asynchronous streams. As Nuo evolved to support complex multi-turn workflows, multi-client viewport attachment, dynamic tool execution slots (ADR-0046), and extensible MCP ecosystems, two foundational architectural vulnerabilities emerged:

1. **Brittle Attach-Time Synchronization (`AttachSyncBuffer` Fragility)**:
   When an interactive client attaches to an active thread (initial attach, reconnect after network hiccup, or secondary aside viewer), it requires full state hydration. Currently, `nuo-server` uses an ad-hoc `AttachSyncBuffer` that passively intercepts and caches an arbitrary whitelist of five specific event types:
   - `ProviderSwitched`
   - `ProviderKeys`
   - `ProviderPicker`
   - `ContextTokens`
   - `HarnessState`
   
   This event-filtering approach is fragile. As the domain model expands to incorporate dynamic tool execution slots ([ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md)), progressive DAG thread trees, dynamic permission grants, and subagent slots, `AttachSyncBuffer` suffers from synchronization drift. New state dimensions omitted from the hardcoded filter fail to reach re-attaching frontends, leaving the client in an inconsistent or partially blind state.

2. **Monolithic Shared-Fate Hazard in Single-Process Execution**:
   All user projects, hosted threads, model provider streaming decoders, and external Model Context Protocol (MCP) servers currently share the single process and Tokio runtime of `nuo-server`. This architectural coupling creates severe failure modes:
   - **Rogue MCP and External Tool Crashes**: If an external MCP server or sandboxed capability tool encounters an unhandled panic, memory corruption, or native segfault, it terminates the entire `nuo-server` process.
   - **Async Runtime Starvation**: A CPU-intensive syntax parsing task, unbounded AST traversal, or synchronous blocking file operation executed within an agent turn can starve the shared Tokio reactor thread pool, introducing latency spikes and frame drops across all other connected terminal viewports.
   - **Zero Fault Boundary**: An unexpected error in a background autonomous batch task running in project A will violently terminate an active, critical reasoning session being monitored by an interactive user in project B.

We require a modernized, uncompromising architecture that replaces ad-hoc event caching with a formal, universal **State Snapshot Protocol**, and establishes an **Isolated Subprocess Worker Architecture** to enforce strict crash and resource boundaries between the host coordinator and agent execution workloads.

---

## Decision Drivers

- **Deterministic State Hydration (`[INV-SNAPSHOT-01]`)**: Any client attaching at any point in a thread lifecycle must receive an authoritative, atomic snapshot of the complete thread and system state, eliminating event-whitelist synchronization drift.
- **Strict Subprocess Fault Isolation (`[INV-WORKER-01]`)**: Untrusted external MCP servers and intensive agent workloads must execute within isolated subprocess worker domains, guaranteeing that worker panics or memory faults cannot destabilize the host server.
- **Asynchronous Reactor Protection (`[INV-WORKER-02]`)**: CPU-heavy tool execution, code analysis, and external IPC parsing must run out-of-process, insulating the host server's core control plane and TUI event streams from reactor starvation.
- **Resilient Worker Self-Healing (`[INV-CRASH-01]`)**: A worker process crash must be trapped by the host supervisor, converted into an actionable diagnostic event on the wire, and isolated to the originating thread, allowing graceful worker respawning without thread loss.

---

## Considered Options

### Option 1: Incremental Whitelist Expansion in `AttachSyncBuffer` and Pure In-Process Harness (Status Quo)
- Retain the current `AttachSyncBuffer` implementation, adding new event variants as features are introduced. Keep all MCP servers and agent execution loops inside the core `nuo-server` process.
- *Assessment*: Rejected. Fails the scale threshold. Every new stateful feature requires updating attach buffer filters in tandem. Continues to expose all hosted sessions to unrecoverable crashes caused by errant external tool plugins or runtime panics.

### Option 2: Heavyweight Containerization / MicroVM Isolation (Docker/Firecracker per Session)
- Spawn an isolated OCI container or microVM for every active thread.
- *Assessment*: Rejected. Introduces massive cold-start latency (several seconds per thread), exorbitant memory overhead, and severe friction in developer environments where local filesystem access, git status, and host tooling parity are essential.

### Option 3: Universal State Snapshot Protocol and Supervised Subprocess Worker Architecture (Chosen)
- Define a formal, declarative `SessionSnapshot` entity in `nuo-wire` that captures the complete, authoritative operational state of a thread.
- Introduce `nuo-worker`, a lightweight supervisor-managed worker subprocess architecture. The core `nuo-server` acts as the lean coordinator (owning SQLite persistence, authentication, and client multiplexing), while heavy agent execution and untrusted MCP runtimes are sandboxed in dedicated worker child processes communicating over low-latency native IPC.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Universal State Snapshot & Delta Protocol (`[INV-SNAPSHOT-01]`)

The ad-hoc `AttachSyncBuffer` is deprecated and eradicated. In its place, the wire contract (`nuo-wire`) introduces a first-class, declarative **`SessionSnapshot`** envelope:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub thread_status: SessionStatus,
    pub provider_state: ProviderStateSnapshot,
    pub token_accounting: ContextTokenAccounting,
    pub harness_state: HarnessStateSnapshot,
    pub active_tool_slots: Vec<ToolExecutionSlotSnapshot>,
    pub permission_rules: Vec<PermissionRuleSnapshot>,
    pub mcp_readiness: Vec<McpServerStatus>,
    pub watermark_seq: u64,
}
```

- **Atomic Hydration**: Upon client attachment (`AttachAction::Attach`), the server immediately transmits a single `AgentResponse::SessionSnapshot(Box<SessionSnapshot>)` frame, followed by the latest transcript history.
- **Incremental Fan-Out**: Once hydrated, ongoing state transitions are communicated via structured, typed state-delta events (`AgentResponse::StateDelta(StateDelta)`).
- **Zero Omission Guarantee**: New frontend state requirements (e.g. progressive tool slot inputs from ADR-0046 or subagent status) are added as fields to the declarative `SessionSnapshot` struct, ensuring that all attach pathways automatically hydrate the complete state without manual event filtering.

### 2. Isolated Subprocess Worker Architecture (`[INV-WORKER-01]`)

The runtime architecture is partitioned into two distinct physical process tiers:

```text
┌─────────────────────────────────────────────────────────────────────────────────┐
│                          nuo-server (Host Coordinator)                          │
│                                                                                 │
│   • Client Connection Gateway (UDS / TCP)      • Authoritative SQLite SSOT      │
│   • Global Worker Supervisor                   • Fast Event Broadcast Bus       │
└────────────────────────────────────────┬────────────────────────────────────────┘
                                         │
                         Native IPC (UDS / Anonymous Pipes)
                                         │
                 ┌───────────────────────┴───────────────────────┐
                 ▼                                               ▼
┌─────────────────────────────────┐             ┌─────────────────────────────────┐
│     Worker Process (Thread A)   │             │     Worker Process (Thread B)   │
│                                 │             │                                 │
│  • Agent Cognition Loop         │             │  • Agent Cognition Loop         │
│  • Tool Execution Sandbox       │             │  • Tool Execution Sandbox       │
│  • MCP Server Bridge            │             │  • MCP Server Bridge            │
└─────────────────────────────────┘             └─────────────────────────────────┘
```

1. **Host Coordinator (`nuo-server`)**:
   - Thin, resilient, rock-solid supervisor.
   - Holds exclusive write locks on `nuo.db` and configuration stores.
   - Manages client endpoints (UDS / optional TCP) and broadcasts telemetry.
   - Spawns, monitors, and recycles worker subprocesses via `WorkerSupervisor`.
2. **Worker Process (`nuo-worker` / Internal Worker Mode)**:
   - Spawned as an unprivileged child process: `nuo worker --thread-id <id> --ipc-fd <fd>`.
   - Executes the ReAct cognitive reasoning loop, model streaming token decoders, and capability tools (`nuo-tool-*`).
   - Owns external MCP server process lifecycles.
   - Communicates with the host coordinator strictly via framed asynchronous IPC over anonymous socket pairs (`SocketPair`) or platform named pipes.

### 3. Bounded Crash Containment & Self-Healing (`[INV-CRASH-01]`)

- If a worker process exits abruptly (due to a segfault in a native dependency, out-of-memory kill by OS, or unhandled panic), the `WorkerSupervisor` in `nuo-server` detects process termination via `SIGCHLD` / `waitpid`.
- **Zero Host Impact**: The host server remains 100% operational. Connected TUI and web clients do not disconnect. Peer worker processes in other threads remain entirely unaffected.
- **Actionable Crash Reporting**: The supervisor synthesizes an `AgentResponse::WorkerCrashed` event containing the exit status and captured standard error diagnostic residue, committing the failure to the thread transcript in SQLite.
- **Transparent Recovery**: The user is presented with a clear failure notice in the terminal, accompanied by an actionable prompt to resume or retry (`[r] Respawn worker and retry turn`), eliminating catastrophic data loss.

---

## Invariants & Behavioral Boundaries

- **`[INV-SNAPSHOT-01]` Deterministic Universal State Hydration**: Every client connecting or reconnecting to a hosted thread MUST receive an authoritative `SessionSnapshot` as the first post-handshake payload before streaming events commence. Ad-hoc whitelist filtering of transient event streams is strictly prohibited.
- **`[INV-WORKER-01]` Subprocess Worker Isolation**: Untrusted third-party MCP servers, long-running agent reasoning loops, and capability tool executions MUST execute within isolated worker subprocesses. They MUST NOT share memory or reactor thread pools with the core `nuo-server` host coordinator.
- **`[INV-WORKER-02]` Reactor Non-Blocking Guarantee**: The host coordinator's Tokio reactor MUST NOT execute CPU-intensive AST parsing, large payload serialization, or synchronous foreign code. All such operations MUST be offloaded to worker processes.
- **`[INV-CRASH-01]` Bounded Crash Containment**: A fatal crash, abnormal exit, or resource exhaustion in a worker process MUST NOT terminate the host server or disrupt peer workers. The host supervisor MUST trap the failure, preserve the persistent transcript in SQLite, and report the event cleanly to attached viewports.

---

## Positive Consequences

- Guarantees 100% consistent state hydration across all frontend viewports (TUI, Aside viewers, Web UI) regardless of when they attach.
- Shields core platform reliability from faulty, crashing, or resource-hogging third-party MCP plugins.
- Prevents event reactor starvation, ensuring buttery-smooth 60fps TUI scrolling and sub-millisecond IPC response times under heavy LLM generation workloads.
- Provides true fault isolation across multi-project and multi-thread workspaces.

---

## Negative Consequences & Trade-offs

- Spawning a dedicated subprocess per thread introduces minor memory overhead (~15–20 MB RSS per active worker process) and ~10–20ms initial spawn latency.
- *Mitigation*: Workers are pooled and spawned lazily upon the first turn of a thread; dormant or suspended threads release their worker processes while preserving complete serialized state in SQLite.

---

## Rejected Alternatives & Negative Knowledge

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

### 1. Rejected: Full Event-Stream Replay from Turn 0 on Every Client Reconnect
- *Why considered*: Seemed conceptually simple by reusing the existing append-only event log.
- *Why rejected*: Unviable for long-running sessions. Replaying thousands of raw historical token events, progress ticks, and tool telemetry frames causes massive CPU spikes, bandwidth exhaustion, and noticeable multi-second lag upon reconnect.

### 2. Rejected: In-Process Tokio Task Isolation (Without OS Process Boundaries)
- *Why considered*: Lower memory footprint and zero IPC overhead.
- *Why rejected*: Violates Subprocess Worker Isolation (`[INV-WORKER-01]`). Rust's standard library panics and memory-unsafe C/C++ libraries (commonly invoked by native MCP servers or Python bridges) can abort the entire operating system process or cause memory corruption that bypasses language-level catch-unwind guards.

### 3. Rejected: Standalone Heavyweight Container Virtualization (Docker Daemon)
- *Why considered*: Maximum theoretical isolation between execution tasks.
- *Why rejected*: Excessively heavy. Requires root/daemon privileges, introduces significant filesystem bind-mount latency, breaks local developer tooling integrations, and creates insurmountable adoption friction for developers running on standard desktop environments.

---

## Links

- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md)
- Amends: [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md)
- Amends: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md)
- Amends: [ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md)
