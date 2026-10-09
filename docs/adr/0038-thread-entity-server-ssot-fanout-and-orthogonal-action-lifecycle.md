---
id: ADR-0038
title: "Thread Entity Taxonomy, Server-Side SSOT Fan-Out, and Orthogonal Action Lifecycle"
status: accepted
date: 2026-10-18
scope: architecture/server, runtime/lifecycle, comm/wire, persistence/sqlite, domain/taxonomy
superseded_by: null
negative_knowledge: true
---

# 0038. Thread Entity Taxonomy, Server-Side SSOT Fan-Out, and Orthogonal Action Lifecycle

- Status: Accepted
- Date: 2026-10-18
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Security, Protocol, Interface, and Persistence Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0029](0029-client-lifecycle-bound-daemon-foreground-headless-and-interface-takeover.md), [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0034](0034-deterministic-lifecycle-governance-phased-draining-and-tiered-restart.md)

---

## Context and Problem Statement

Nuo evolved from a prototype into a multi-tiered architecture with a single background server coordinating agent workloads. However, the system accumulated critical conceptual collisions and lifecycle pathologies:

1. **Severe Session Abstraction Leakage**:
   The domain noun `Session` suffered from three-way vocabulary conflation:
   - *Transport layer*: The ephemeral WebSocket/IPC connection between a terminal client and the server (`Client Connection`).
   - *Interface layer*: An active interactive TUI terminal window (`UI Viewport`).
   - *Business entity layer*: The persistent agent dialogue with an LLM, tools, history, and workspace context.
   Because `Session` was used synonymously for all three, user operations like closing a terminal window raised ambiguous questions: *Does closing the UI kill the running AI task?*
2. **The "Client-Driven" Auto-Kill Contradiction**:
   Under ADR-0029, background servers spawned by interactive clients operated in `client_driven` mode. When `active_clients` dropped to zero, the server armed a 1.5-second debounce timer and then initiated full server shutdown (`ShutdownReason::AllClientsClosed`).
   This created a catastrophic cognitive trap:
   - When multiple terminals were open, closing one detached it cleanly while the other kept the task running.
   - When only one terminal was open, closing the window detached the client, but 1.5 seconds later the server terminated itself—killing the very background agent task the user expected to continue running.
3. **Multi-Client State Mutations and SQLite Writer Lock Collisions**:
   Subsystems like `Settings` and `Dashboard` were treated as client-local instances rather than server-side singletons. When multiple clients concurrently attempted to read or mutate settings, separate local handles collided on SQLite write locks (`nuo.db.owner.lock` / `os error 11`), triggering transient failures and split-brain configuration drift.

We require an uncompromising, modernized architectural taxonomy that formally establishes the persistent business entity as `Thread`, eradicates the fragile 1.5s client-driven auto-kill mechanism, establishes the Server as a true Single Source of Truth (SSOT) with reactive event fan-out, and strictly decouples client viewports from thread execution.

---

## Decision Drivers

- **Zero Vocabulary Conflation (`[INV-THREAD-01]`)**: Clearly isolate `Thread` (multi-instance persistent agent workload and dialogue) from `Client Connection` (ephemeral transport) and `Server Services` (singleton settings/dashboard).
- **Persistent Server Independence (`[INV-SERVER-07]`)**: Eradicate premature server self-termination on client disconnect. The server must remain active as a long-lived coordinator, allowing agent tasks to run autonomously without artificial 1.5s execution walls.
- **Single-Writer SSOT and Event Fan-Out (`[INV-SSOT-01]`)**: The server holds sole ownership of SQLite persistence and configuration state. Client mutations funnel as RPC requests to the server, which writes authoritatively and fans out update events to all connected clients.
- **Orthogonal Action Matrix (`[INV-ACTION-02]`)**: Client viewport actions (`Detach`) must NEVER terminate backend threads or servers. Thread lifecycle operations (`Interrupt`, `EndThread`, `KillThread`) must be explicit.

---

## Considered Options

### Option 1: Status Quo with Longer Client-Driven Debounce
- Keep the `client_driven` auto-kill mechanism, but increase the debounce from 1.5 seconds to 10 minutes.
- *Assessment*: Rejected. Simply widens the race window. A long-running compilation or deep reasoning task lasting 15 minutes would still be abruptly terminated when the operator disconnects.

### Option 2: Pure Local Monolith (Kill Everything on Terminal Exit)
- Make `nuo` operate purely inside the foreground TUI process, abandoning the background server coordinator for local terminal runs.
- *Assessment*: Rejected. Violates ADR-0005, ADR-0011, and ADR-0033. Destroys multi-window inspection, external CLI tools (`nuo status`, `nuo attach`), aside monitoring, and web frontend interoperability.

### Option 3: Thread Entity Taxonomy, Authoritative Server SSOT, and Decoupled Lifecycle (Chosen)
- Formally ratify `Thread` as the multi-instance persistent entity.
- Eliminate `client_driven` client-reference auto-kill; servers run persistently until explicit shutdown (`nuo server stop`) or wall-clock idle expiration (`idle_exit_minutes`).
- Establish Server as the sole authoritative state owner with reactive fan-out for singleton domains (`Settings`, `Dashboard`).
- Decouple client disconnection (`Detach`) from thread execution.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Treating Dashboard and Settings as Multi-Instance Sessions in SQLite**
   - *Reason*: Storing Settings and Dashboard as rows in `nuo.db` alongside threads creates severe database bloat and semantic corruption. Users running `nuo session list` or `nuo session delete` would see bizarre system entities. System configuration is a singleton service, not an archival dialogue.
2. **Rejected: Client-Side Direct SQLite Writes with Distributed Mutexes**
   - *Reason*: Allowing individual client processes to directly acquire SQLite write locks and commit to `nuo.db` bypasses the server actor, reintroduces file lock contention (`os error 11`), and prevents real-time reactive event broadcasts across peer clients.
3. **Rejected: Implicit Thread Destruction on Client Exit (`/exit` killing background tasks)**
   - *Reason*: Equating "closing the UI" with "destroying the workload" is an anti-pattern for autonomous agents. If an operator needs to terminate a task, they issue an explicit business command (`/kill` or `/cancel`), not a navigation command.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Three-Tier Architectural Taxonomy (`[INV-THREAD-01]`)

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        1. Client Tier (UI/IPC)                         │
│   Terminal 1 (TUI)       Terminal 2 (TUI)       Headless CLI (nuo run) │
└───────────────────┬──────────────────┬──────────────────┬──────────────┘
                    │ (Attach/Detach)  │ (Attach/Detach)  │
                    ▼                  ▼                  ▼
┌────────────────────────────────────────────────────────────────────────┐
│                        2. Thread Tier (Logical)                        │
│   Thread A (Multi-instance)              Thread B (Multi-instance)     │
│   - Persistent UUID & Transcript         - Background autonomous task  │
│   - Context window & tool ledger         - Role, bounds, and artifacts │
└───────────────────┬─────────────────────────────────────┬──────────────┘
                    │                                     │
                    ▼                                     ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      3. Server Tier (Coordinator)                      │
│   - Sole SQLite Writer (nuo.db)        - Singleton Settings Service    │
│   - Native Local IPC (server.sock)     - Singleton Dashboard Hub       │
└────────────────────────────────────────────────────────────────────────┘
```

1. **`Client`**: A presentation viewport or CLI caller. Ephemeral, disposable, and interchangeable.
2. **`Thread`**: The multi-instance persistent business entity. Has an immutable UUID, role profile, transcript history, and LLM context window. Persists authoritatively in SQLite `nuo.db`.
3. **`Server`**: The local daemon coordinator. Owns exclusive file locks, coordinates execution drivers, and manages singleton services.

### 2. Server SSOT and Reactive Event Fan-Out (`[INV-SSOT-01]`)
- **Settings Singleton**: Server maintains authoritative in-memory configuration (`Arc<RwLock<Config>>`). When any client submits a configuration mutation, the server writes atomically to disk/database and broadcasts `Event::ConfigUpdated` to all attached clients.
- **Dashboard Hub**: Server maintains a single `broadcast::channel<MonitorEvent>`. All thread completions, tool failures, and system diagnostics publish to this bus; all connected dashboard views receive identical real-time updates.
- **Zero Client Lock Contention**: Clients NEVER directly open or hold writer handles on `nuo.db` or configuration locks.

### 3. Orthogonal Action Matrix (`[INV-ACTION-02]`)

| Action | Target | Semantics & State Transitions | Lifecycle Side-Effect |
| :--- | :--- | :--- | :--- |
| **`Attach`** | Client → Thread | Binds client IPC streams to target thread. | Upgrades human channel to Interactive; syncs transcript. |
| **`Detach`** | Client ⇸ Thread | Unbinds client connection (window closed, SIGHUP). | **Thread continues in Autonomous Mode**. Server remains running. |
| **`Interrupt`** | Thread (Turn) | Cancels in-flight tool or LLM stream via token. | Current turn marked `Interrupted`; thread stays live. |
| **`EndThread`** | Thread | Explicit operator completion command (`/exit`, `/quit`). | Unhosts driver; archives transcript in `nuo.db`. |
| **`KillThread`** | Thread | Administrative removal (`nuo thread delete` / `/kill`). | Halts execution; permanently removes records from `nuo.db`. |
| **`Shutdown`** | Server | Operator shutdown request (`nuo server stop`). | Executes 3-stage graceful drain; closes connections; exits with 0. |
| **`Reload`** | Server (Config) | Hot config reload (`nuo server reload`). | Re-reads configs without dropping client connections. |
| **`Restart`** | Server | Tiered server restart (`nuo server restart [--force]`). | Level 2 graceful drain & auto-reconnect, or Level 3 force SIGKILL. |
| **`Takeover`** | Server | Resolves occupied endpoints during server boot. | Terminates conflicting PIDs gracefully or forcibly before lock acquisition. |

---

## Invariants & Behavioral Boundaries

- **[INV-THREAD-01] Distinct Thread Identity**: The multi-instance persistent dialogue workload MUST be formally designated as `Thread`. Code, APIs, and wire envelopes MUST isolate thread identity from transport connection handles.
- **[INV-SERVER-07] Persistent Server Integrity**: A running server MUST NOT terminate simply because active interactive client count drops to zero. Servers MUST remain alive to execute autonomous background threads until explicitly stopped or when continuous global idle expiration (`idle_exit_minutes`) trips.
- **[INV-SSOT-01] Single-Writer Server Exclusivity**: All SQLite writes and persistent configuration changes MUST funnel exclusively through the server process. Client applications MUST NOT acquire standalone writer locks or modify authoritative storage out-of-band.
- **[INV-ACTION-02] Non-Destructive Detach**: Disconnection of a client viewport (whether intentional, accidental, or via terminal closure) MUST be treated strictly as `Detach`. It MUST NEVER cancel in-flight thread turns or trigger server shutdown.

---

## Positive Consequences

- Eliminates confusing background task cancellations when users close terminal windows.
- Prevents SQLite write lock collisions and handle poisoning across multiple clients.
- Provides a clean mental model: Client is a window; Thread is a task/execution line; Server is the host.
- Lays the architectural prerequisite for multi-tab client workspaces (ADR-0039).
