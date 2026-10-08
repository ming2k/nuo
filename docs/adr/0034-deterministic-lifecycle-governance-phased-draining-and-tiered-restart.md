---
id: ADR-0034
title: "Deterministic Server Lifecycle Governance: Single-Flight Startup, Phased Draining, and Tiered Restart"
status: accepted
date: 2026-10-11
scope: runtime/server-lifecycle, architecture/cli, server/nuo-server, host/nuo-host
superseded_by: null
negative_knowledge: true
---

# 0034. Deterministic Server Lifecycle Governance: Single-Flight Startup, Phased Draining, and Tiered Restart

- Status: Accepted
- Date: 2026-10-11
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Security, Interface, Persistence, and Protocol Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0021](0021-daemon-image-content-identity-and-idle-self-heal.md), [ADR-0029](0029-client-lifecycle-bound-daemon-foreground-headless-and-interface-takeover.md), [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md)

---

## Context and Problem Statement

Nuo Server operates as the single-writer coordinator for interactive AI sessions, agent round dispatch, and SQLite persistent state. Historical iterations struggled with three distinct lifecycle pathologies:
1. **Concurrent Spawning Contention**:
   When multiple terminal windows or automated scripts were launched simultaneously without an active server, each process evaluated `discover()` as empty and concurrently executed `spawn_daemon()`. Multiple emerging server instances competed for file locks and endpoint ownership, triggering race conditions and spurious process kills.
2. **Duplicated, Fragmented Process Takeover Logic**:
   The logic to inspect conflicting PIDs, request graceful termination, await process exit, and escalate to `SIGKILL` was duplicated across three separate layers (`nuo::supervisor`, `nuo-client::ensure_daemon`, and `nuo-server::host`), violating the DRY principle and introducing subtle timing discrepancies (300ms vs 500ms deadlines).
3. **Absence of Tiered Restart Contracts**:
   The runtime offered only binary extremes: either an implicit, disruptive kill-and-respawn during binary drift, or manual `nuo stop` followed by `nuo start`. There was no explicit CLI restart command, no configuration hot-reload capability without dropping client connections, and no formal safety tiers during process replacement.

We require a deterministic, multi-tiered lifecycle architecture that guarantees clean startup single-flight execution, unified takeover escalation, phased graceful draining, and transparent restart recovery.

---

## Decision Drivers

- **Single-Flight Spawning Concurrency (`[INV-LIFECYCLE-02]`)**: Concurrent client launches must never trigger competing server spawns.
- **Unified OS Takeover Escalation (`[INV-LIFECYCLE-03]`)**: All process replacement logic must be consolidated into a single host primitive with deterministic graceful-to-force escalation.
- **Three-Stage Drain Integrity (`[INV-LIFECYCLE-04]`)**: Server shutdown must follow a strict three-stage pipeline ensuring zero in-flight data loss and atomic SQLite WAL checkpoints.
- **Tiered Restart Capabilities (`[INV-LIFECYCLE-05]`)**: Provide explicit, granular restart primitives spanning in-process soft reload, graceful cold replacement, and emergency forced takeover.

---

## Considered Options

### Option 1: Ad-Hoc Process Management (Status Quo)
- Allow concurrent spawns and let internal socket binding errors or instance lock timeouts resolve winners.
- *Assessment*: Inadequate. Causes startup flakiness in automated test harnesses and multiplexed terminal workflows.

### Option 2: Complex Zero-Downtime Socket Handoff (SCM_RIGHTS FD Passing)
- Implement Unix domain socket file descriptor passing across parent and child processes (similar to Nginx/Envoy).
- *Assessment*: Rejected. Over-engineered for local AI developer tooling. Cross-platform implementation (Windows Named Pipes) is brittle, and session state serialization across live process boundaries introduces severe synchronization bugs.

### Option 3: Deterministic Single-Flight Startup, Host-Level Takeover, and 3-Tiered Restart (Chosen)
- Introduce a short-lived `server-spawning.lock` ensuring single-flight server initialization.
- Downstream process termination escalation into `nuo_host::process::takeover_pid`.
- Provide a three-tiered restart hierarchy: Level 1 Soft Reload, Level 2 Graceful Cold Restart with auto-reconnect, and Level 3 Force Takeover.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Blind SIGKILL on Conflict Without Graceful Drain**
   - *Reason*: Immediately killing a conflicting predecessor process corrupts in-flight SQLite transactions, truncates command execution ledgers, and drops pending tool call outputs.
2. **Rejected: Indefinite Graceful Drain Waiting**
   - *Reason*: Waiting without an enforced timeout allows a hung external subprocess or blocked network hook to pin the server open forever, blocking upgrades and automation pipelines. An enforced escalation budget (default 10s graceful, 500ms takeover) is mandatory.
3. **Rejected: Client-Side Ephemeral In-Memory State Caching During Restart**
   - *Reason*: Relying on the client to hold in-flight round state during a server restart introduces split-brain state risks. Durable state must strictly reside in the server's SQLite store, enabling the client to simply re-hydrate upon reconnect.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Single-Flight Spawning Lock (`[INV-LIFECYCLE-02]`)
- When `nuo-client::ensure_server` detects no running server, it acquires an advisory `server-spawning.lock` (bounded to 6 seconds).
- The winner process spawns the server and polls for discovery publication.
- Concurrently competing clients block on the lock, and upon release, discover the freshly initialized server, proceeding without spawning redundant processes.

### 2. Consolidated Host Takeover Primitive (`[INV-LIFECYCLE-03]`)
- Process termination logic is centralized in `nuo_host::process::takeover_pid`:
  1. Verifies `ProcessIdentity` (PID + birth token) to prevent PID wraparound hazards.
  2. Issues `request_termination` (SIGTERM on Unix).
  3. Polls process liveness over a 500ms budget.
  4. If still alive after 500ms, escalates to `force_terminate` (SIGKILL).

### 3. Three-Stage Graceful Drain Pipeline (`[INV-LIFECYCLE-04]`)
Whenever shutdown is initiated (`ShutdownGate`), the server proceeds strictly in sequence:
- **Phase 1: Discovery Revocation**: Immediately deletes `server.json`, preventing new clients from routing to a terminating server.
- **Phase 2: Ingress Stop & In-Flight Task Drain**: Rejects new connections, broadcasts `AgentResponse::Exit`, and awaits completion of active tool executions and LLM streaming turns (up to configured grace budget).
- **Phase 3: Durable WAL Flush**: Executes final SQLite transaction commits and explicit WAL checkpoints, unlinks `server.sock`, and releases `server.lock`.

### 4. Three-Tiered Restart Architecture (`[INV-LIFECYCLE-05]`)

```
                      ┌─────────────────────────┐
                      │   nuo server restart    │
                      └────────────┬────────────┘
                                   │
             ┌─────────────────────┼─────────────────────┐
             ▼                     ▼                     ▼
       [Level 1]             [Level 2]             [Level 3]
    Soft Reload (RPC)     Graceful Restart      Forced Takeover
    nuo server reload     nuo server restart    nuo server restart --force
    Zero connection drop  Phased drain & spawn  SIGKILL stuck predecessor
```

- **Level 1 (Soft Reload)**:
  - Invocation: `nuo server reload`
  - Scope: Re-parses configuration files (`server.toml`, `agent.toml`), reloads provider credentials, re-syncs MCP servers and skills. Active client WebSocket connections remain undisturbed.
- **Level 2 (Graceful Cold Restart)**:
  - Invocation: `nuo server restart` (or `nuo restart`)
  - Scope: Requests graceful shutdown on the existing instance with a 3s drain deadline, unlinks stale discovery records, spawns the updated binary image, and allows connected clients to transparently auto-reconnect using session tokens.
- **Level 3 (Forced Takeover)**:
  - Invocation: `nuo server restart --force`
  - Scope: Bypasses graceful drain; immediately invokes `takeover_pid` to SIGKILL an unresponsive instance, forcefully rebinds endpoints, and starts fresh.

---

## Invariants & Behavioral Boundaries

- **[INV-LIFECYCLE-02] Single-Flight Spawning**: Client bootstrapping MUST gate server process creation behind a mutual exclusion lock, preventing concurrent spawn races.
- **[INV-LIFECYCLE-03] Unified Takeover Escalation**: All conflicting process terminations MUST funnel through `nuo_host::process::takeover_pid` with verified process birth tokens and automatic graceful-to-force escalation.
- **[INV-LIFECYCLE-04] Three-Stage Drain Invariant**: Server teardown MUST execute in strict order: (1) revoke discovery, (2) drain active tasks, (3) flush persistent stores. Stragglers MUST be aborted only when the total shutdown grace budget expires.
- **[INV-LIFECYCLE-05] Explicit Restart Contract**: The CLI coordinator MUST provide explicit `server restart` verbs supporting both graceful handover and forced takeover modes.
- **[INV-LIFECYCLE-06] Session Lease Protection**: Interactive clients engaged in session resolution or picker navigation MUST maintain a persistent active connection guard, preventing premature client-driven server termination.
- **[INV-LIFECYCLE-07] Pre-Bootstrap Takeover & Lock Ordering**: The server runtime MUST execute conflicting process takeover and acquire the global instance lock (`server.lock`) BEFORE initializing the session registry or any persistence handles. Database handle acquisition MUST poll with bounded backoff and never cache unrecoverable startup errors globally, preventing startup race conditions and handle poisoning.

---

## Positive Consequences

- Eliminates multi-terminal startup race conditions and spurious process kills.
- Unifies scattered process termination routines into a battle-tested, single host primitive.
- Provides developers with first-class `nuo restart` and `nuo server reload` commands.
- Guarantees data durability with zero truncated turns during scheduled server restarts.
