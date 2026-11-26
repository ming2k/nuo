---
id: ADR-0260
title: "Server Lifecycle Contract, Session-Free Handshake, and Unified Readiness Projection"
status: accepted
date: 2026-11-27
scope: protocol/protocol, server/server, presentation/view, host/nuo
superseded_by: null
amends:
  - "[ADR-0034](0034-deterministic-lifecycle-governance-phased-draining-and-tiered-restart.md) (unifies server startup, reload, and drain into one ServerLifecycle state machine)"
  - "[ADR-0133](0133-attach-readiness-boundary-network-free-handshake-typed-session-phases-and-authoritative-late-hydration.md) (separates server readiness from thread readiness, removes ThreadPhase::Binding and dead ThreadPhase::Loading)"
  - "[ADR-0251](0251-multiplexed-thread-sessions-and-resident-per-thread-workspaces.md) (completes single-connection unification by decoupling Hello handshake from Subscribe)"
negative_knowledge: true
---

# 0260. Server Lifecycle Contract, Session-Free Handshake, and Unified Readiness Projection

- **Status**: Accepted
- **Date**: 2026-11-27
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0034](0034-deterministic-lifecycle-governance-phased-draining-and-tiered-restart.md), [ADR-0133](0133-attach-readiness-boundary-network-free-handshake-typed-session-phases-and-authoritative-late-hydration.md), [ADR-0251](0251-multiplexed-thread-sessions-and-resident-per-thread-workspaces.md)
- **Related**: [ADR-0123](0123-attach-failure-typing-stale-tab-retirement-and-identity-addressed-session-rebasing.md), [ADR-0136](0136-wire-drift-is-a-frame-not-a-disconnect.md), [ADR-0224](0224-bounded-reachability-failure-keepalive-and-owned-client-websocket.md), [ADR-0253](0253-one-owner-per-field-thread-instance-and-viewport.md)

---

## Context and Problem Statement

When a user launched `nuo`, two category errors collided into a jarring startup experience:

1. **The Server Had No Published Lifecycle Before Shutdown**: `server::runtime::host::run_inner` completed all storage, configuration, and listener initialization before writing the discovery record, and only exposed a terminal `MonitorEvent::ServerDraining` signal at shutdown. Clients had to print a blind stderr line (`nuo: connecting to the session server…`) and poll the discovery file without knowing what the server was doing.
2. **Handshake Coupled Connection Establishment to Thread Binding**: `Wire::Select -> Wire::Welcome { thread_id, phase }` forced every interactive connection to claim a thread before the client could even mount. Because [ADR-0133](0133-attach-readiness-boundary-network-free-handshake-typed-session-phases-and-authoritative-late-hydration.md) returned `Wire::Welcome` before local assembly began, the first frame carried `ThreadPhase::Binding`, which the TUI projected literally as `"Binding thread…"` immediately on startup — an internal protocol implementation term masquerading as server initialization.
3. **Dead Phase Variants and Dual-Connection Split**: `ThreadPhase::Loading` was defined in `protocol` and matched in `view`, yet never emitted anywhere in `server`. Meanwhile, the TUI maintained a separate background monitor connection hardcoded to local discovery (`spawn_monitor_translator`), leaving `--remote` clients without server observability and allowing `ServerDraining` to be silently ignored.

Applying Occam's Razor and KISS principles, we break clean at `PROTOCOL_VERSION = 36` to replace the coupled handshake and fragmented waiting states with three orthogonal, minimal state machines over a single unified connection.

---

## Architectural Decisions

### 1. First-Class `ServerPhase` and Single `ServerLifecycle` SSOT

We introduce `protocol::ServerPhase` and `protocol::ServerInitStage`:

- `ServerPhase::Initializing { stage: ServerInitStage }` (`Storage`, `Runtime`)
- `ServerPhase::Ready`
- `ServerPhase::Reloading`
- `ServerPhase::Draining { reason: Option<String> }`
- `ServerPhase::Failed { code: Option<String>, message: String }`

`ServerPhase` is strictly orthogonal to `PersistenceHealth` (subsystem durability degradation) and `ThreadPhase` (per-thread readiness). `MonitorEvent::ServerDraining` is retired and subsumed by `MonitorEvent::ServerPhaseChanged(ServerPhase)`. `MonitorSnapshot` carries both `server_id` and `server_phase`.

On the server, `ServerLifecycle` (unifying `ShutdownGate` with a `watch::Sender<ServerPhase>` and unique per-boot `server_id`) is the single source of truth for:

- `Wire::ServerHello` initial phase delivery,
- Reliable per-connection `MonitorEvent::ServerPhaseChanged` streaming (driven by `watch::Receiver`, immune to broadcast lag),
- Plain-HTTP `/healthz` (process liveness) and `/readyz` (200 only when `ServerPhase::Ready`, 503 otherwise).

### 2. Listen-First Startup Sequence and Explicit Admission

`server::runtime::host::run_inner` acquires the process instance lock, constructs `ThreadRegistry` in `ServerPhase::Initializing { stage: ServerInitStage::Storage }`, binds local IPC / TCP listeners, and publishes the discovery record **immediately** before running bounded local initialization (`Storage` → `Runtime` → `Ready`).

Admission rules are explicit with zero hidden server-side queuing:

- `Wire::Hello` is always served immediately regardless of `ServerPhase`.
- `Wire::Control(ControlRequest::Shutdown)` is always admitted.
- `Wire::Subscribe` is admitted in `Ready` and `Reloading`, and explicitly refused with `ERR_SERVER_NOT_READY`, `ERR_SERVER_DRAINING`, or `ERR_SERVER_FAILED` otherwise.

### 3. Session-Free Unified Handshake (`Wire::Hello` → `Wire::ServerHello`)

`Wire::Select`, `Wire::Welcome`, and `AttachAction` (along with its custom serde shim) are deleted. Every peer (TUI, headless CLI, `nuo status`, `nuo server stop`, `--remote`) opens one connection and speaks:

1. `Wire::Hello { protocol, version, posture, monitor }` → `Wire::ServerHello { server_id, protocol, version, started_at, phase, persistence_health, monitor }`
2. Over the exact same connection:
   - `Wire::Subscribe { request_id, target: ThreadTarget, project }` → `Wire::Subscribed` / `Wire::Pick` / `Wire::Error`
   - `Wire::Unsubscribe { thread_id }`
   - `Wire::Request` / `Wire::Response`
   - `Wire::Control { request_id, request }` → `Wire::ControlReply { request_id, ok, thread_id, error, code, payload }`
   - `Wire::Monitor { event }` (always delivering `ServerPhaseChanged`, plus full monitor diffs when `monitor.watch == true`)

The TUI deletes its redundant second monitor connection (`spawn_monitor_translator`), making local and `--remote` clients 100% isomorphic.

### 4. Purified `ThreadPhase`

`ThreadPhase::Binding` and the unproduced `ThreadPhase::Loading` are deleted. `ThreadPhase` has exactly three variants, every one with a real server producer:

- `ThreadPhase::Preparing { stage: PrepareStage }` (beginning immediately at `PrepareStage::Thread` when a pending thread is claimed)
- `ThreadPhase::Ready`
- `ThreadPhase::Failed { code: Option<String>, message: String }`

### 5. Unified Client Projection Ladder with Anti-Flicker Hysteresis

Client UI waiting states project through a single strict priority ladder:

1. Client transport link state (`Connecting` / `SpawningServer` / `Reconnecting` / `LinkDown` / `Failed`)
2. Server lifecycle state (`ServerPhase::Initializing` / `Draining` / `Failed`; `Reloading` surfaces non-blockingly in the status bar)
3. Explicit user thread switch (`ResumingThread`)
4. Foreground thread readiness (`ThreadPhase::Preparing` / `Failed`)
5. Ready workspace (or thread-free Dashboard/Picker)

Transient non-error waiting states apply a 120 ms display hysteresis so fast warm connections transition seamlessly without flashing intermediate labels.

### 6. Invariants

- **`[INV-SRVPHASE-01]` Single Server Lifecycle Source of Truth**: Every server-level phase projection (`Wire::ServerHello`, `MonitorEvent::ServerPhaseChanged`, `MonitorSnapshot.server_phase`, `/healthz`, `/readyz`) MUST read from one `ServerLifecycle` watch channel; every `ServerInitStage` and `ThreadPhase` variant MUST have a live production producer.
- **`[INV-SRVPHASE-02]` Session-Free Handshake**: Connection establishment (`Wire::Hello` → `Wire::ServerHello`) MUST NOT require claiming or resolving a thread and MUST return immediately across all `ServerPhase` values once protocol version compatibility is verified.
- **`[INV-SRVPHASE-03]` Explicit Non-Ready Admission**: While `ServerPhase` is not `Ready` (or `Reloading`), the server MUST explicitly refuse `Wire::Subscribe` with typed error codes (`ERR_SERVER_NOT_READY`, `ERR_SERVER_DRAINING`, `ERR_SERVER_FAILED`) rather than silently queuing requests behind opaque timeouts.
- **`[INV-SRVPHASE-04]` Single-Connection Local/Remote Isomorphism**: Interactive clients MUST receive server lifecycle events, monitor snapshots/diffs, and multiplexed thread streams over one unified wire connection without spawning a separate local-discovery monitor socket.
- **`[INV-SRVPHASE-05]` Layered Waiting Projection**: Presentation layers MUST project transport, server, and thread readiness strictly in that precedence order and MUST NOT surface internal thread-claim mechanics in place of server startup readiness.

---

## Consequences & Trade-offs

- **Positive**: Eliminates `"Binding thread…"` on cold boot, removes dead `ThreadPhase::Loading`, deletes `AttachAction`'s custom serializer and duplicate monitor socket, enables `--remote` server monitoring and thread-free Dashboard startup, and makes slow server initialization or startup failure immediately observable to every client.
- **Negative & Incurred Costs**: Clean wire break advancing `PROTOCOL_VERSION` from `35` to `36` with a renewed wire-surface census; clients and servers must run on matching protocol `36`.

---

## Negative Knowledge & Rejected Alternatives

### Rejected Alternative A: Implicit Server-Side Queuing of `Subscribe` During `Initializing`
Holding client `Subscribe` requests in a server-side queue until `ServerPhase::Ready` was rejected. Implicit queuing hides initialization failures behind opaque client deadlines, complicates cancellation when a client disconnects mid-boot, and forces clients to guess why nothing is responding. Explicit `ERR_SERVER_NOT_READY` combined with `ServerPhaseChanged` subscription keeps both sides stateless and deterministic.

### Rejected Alternative B: Retaining `Wire::Select`/`Welcome` Alongside a Separate Monitor Socket
Keeping dual connections and merely renaming `"Binding thread…"` to `"Starting server…"` was rejected. Relabeling a thread-claim phase as a server phase is a lie that breaks whenever a user opens a new tab on an already-running server, and leaving a separate local-only monitor socket preserves local/remote asymmetry and duplicate reconnect failure modes.
