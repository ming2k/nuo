---
id: ADR-0045
title: "Dynamic Port Sharing Palette, Orthogonal Control Plane, and Server Lifecycle Handoff"
status: accepted
date: 2026-10-22
scope: architecture/server, runtime/lifecycle, tui/nuo-tui, comm/wire, interface/cli
superseded_by: null
negative_knowledge: true
---

# 0045. Dynamic Port Sharing Palette, Orthogonal Control Plane, and Server Lifecycle Handoff

- Status: Accepted
- Date: 2026-10-22
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Interface, Protocol, and TUI Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md)

---

## Context and Problem Statement

Nuo previously stratified its execution postures under ADR-0033 into **Client-Bound Posture** (pure Unix Domain Socket / Named Pipe with zero TCP listeners) and **Standalone Hosted Posture** (deterministic TCP port `9800` + UDS). While this eliminated local TCP port collisions across multi-tenant developers, field operations identified critical friction points, architectural conflations, and diagnostic defects:

1. **Static Lifecycle Conflation on Explicit Port Invocations**:
   When an operator invokes `nuo --port 9800`, they explicitly express an intention to host network endpoints (for remote web panels, mobile companions, or peer collaboration). However, the engine treated this invocation as an ephemeral client session whose termination immediately cascades termination down to the server, abruptly killing active background jobs, remote watchers, and web attachments.
2. **Harness Command Pollution vs. Infrastructure Control**:
   Allowing infrastructure or server controls (such as `/server` or `/debug`) to exist as thread slash commands violates Thread Command Purity ([ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md)). Slash commands belong strictly to the cognitive and conversational domain (Prompt, Models, Memory, History, Fork, Skills, Tools). Server listening, port management, and trace debugging are client/infrastructure capabilities that must not pollute the conversational context or thread prompt history.
3. **Historical Port Drift and Diagnostic False-Positives**:
   A legacy default port (`9527`) remained hardcoded in several self-healing and diagnostic code paths in `nuo-client`, diverging from the canonical `DEFAULT_SERVE_PORT` (`9800`) and ignoring `NUO_PORT`. Concurrently, `nuo status --diagnostic` required `diag.tcp_listening == true` for a healthy verdict, causing completely healthy Client-Bound servers (which legitimately bind only UDS with TCP port `0`) to be falsely reported as `Server state is transitioning or unresponsive` with misleading `ws://127.0.0.1:0 (closed)` warnings.
4. **Inability to Dynamically Toggle Remote Sharing in Active Sessions**:
   Operators in the middle of a deep terminal reasoning session who wish to open a web view or share a link were forced to exit the TUI, manually start a detached server, and re-attach.

---

## Decision Drivers

- **Intent-Driven Lifecycle Handoff (`[INV-LIFECYCLE-03]`)**: Explicit network port declarations (`--port`) must decouple the server lifecycle from the interactive TUI, leaving the server active upon client detachment.
- **Strict Command Space Purity (`[INV-CMD-02]`)**: Infrastructure, server networking, and debug diagnostics must be completely segregated from thread slash commands into client-level workspace actions.
- **Dedicated Port & Network Sharing Affordance (`[INV-PALETTE-01]`)**: The client interface must provide an ergonomic, modal affordance (`C-x p`) to inspect, dynamically hot-plug, or disable TCP listeners without interrupting ongoing prompts.
- **Accurate Transport Capability Modeling (`[INV-PORT-01]`)**: Diagnostic tooling must model Local IPC as the authoritative health baseline and TCP listeners as an optional, explicitly enabled transport capability.

---

## Considered Options

### Option 1: Universal Always-Detached Server with Slash Commands
- Every `nuo` invocation spawns an always-on background server on TCP port 9800; use `/server listen` and `/server unlisten` as conversational slash commands.
- *Assessment*: Rejected. Re-introduces universal TCP port collisions on shared machines ([INV-SERVER-03]); pollutes thread prompt history with infrastructure tokens ([INV-CMD-01]).

### Option 2: Split Executable (`nuod` Daemon + `nuo` Client)
- Split Nuo into a separate daemon binary (`nuod`) and a thin CLI/TUI client (`nuo`).
- *Assessment*: Rejected per ADR-0005. Increases packaging and distribution complexity; introduces binary version desynchronization friction for end users.

### Option 3: Unified Engine with Orthogonal Control Plane, Dynamic Port Palette, and Lifecycle Handoff (Chosen)
- Retain the unified single-binary architecture.
- When `nuo --port <port>` is invoked, the spawned server upgrades to a detached hosted lifecycle upon TUI exit, displaying a clear handoff banner.
- Local IPC is treated as the permanent, authoritative control plane; TCP listeners can be dynamically enabled/disabled at runtime via wire control verbs.
- Introduce `C-x p` (Port & Remote Sharing Palette) in the TUI workspace namespace, reserving conversational slash commands strictly for thread cognition.
- Eliminate legacy port drift (`9527`) and align diagnostic logic with transport capabilities.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Conversational `/server` Slash Command**
   - *Reason*: Violates Thread Command Purity. If an operator types `/server listen` while reviewing code with an agent, the infrastructure directive leaks into LLM context builders and history transcripts. Infrastructure operations belong strictly to client viewport controls.
2. **Rejected: Hardcoded Killing of Server Process on All TUI Exits**
   - *Reason*: Defeats the purpose of remote collaboration. When a user opens a port, they expect the service to remain alive for external web/mobile clients even if they temporarily close their local terminal window.
3. **Rejected: Polling Random Ephemeral Ports via Browser Discovery**
   - *Reason*: Web clients in modern browsers cannot sweep local port ranges without triggering security blocks, CORS warnings, or firewall alarms. Explicit deterministic port allocation or clipboard token handoff is mandatory.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Static Lifecycle Handoff (`[INV-LIFECYCLE-03]`)
- When `nuo` is launched with an explicit `--port` (or `NUO_PORT` in interactive posture), the background server is designated as an autonomous host.
- When the interactive client TUI exits, the server process **continues running in the background**.
- Upon exit, the client terminal prints an actionable handoff banner:
  ```text
  nuo: detached from hosted server (PID <pid>, port <port>).
    • Reconnect:   nuo
    • Terminate:   nuo stop
  ```
- Conversely, when `nuo` is launched without `--port` (pure UDS posture), the server cascades termination upon client disconnection, preserving zero-waste resource invariants.

### 2. Client Workspace Navigation & `C-x p` Port Palette (`[INV-PALETTE-01]`)
- In the `C-x` two-stroke workspace namespace (ADR-0043):
  - **`C-x p`** is dedicated to **Port & Remote Sharing Palette** (`SceneVerb::PortShare`).
  - **`C-x b`** remains the universal **Command Palette / Buffer Switcher** (`SceneVerb::Switcher`).
  - **`C-x d`** remains the **Dashboard** surface (`SceneVerb::Dashboard`).
  - **`C-x ,`** remains **Settings** (`SceneVerb::Settings`).
- Pressing `C-x p` summons an ephemeral modal displaying:
  - Current TCP Listener Status (Active / Disabled / Port number).
  - Bearer Token and one-key clipboard URL copy (`[y]`).
  - Spacebar toggle to hot-enable or hot-disable TCP listening on the live server via control plane wire messages (`OpenTcpListener` / `CloseTcpListener`).

### 3. Wire Protocol Control Plane Extensions
- Extend `ControlRequest` with:
  - `OpenTcpListener { port: Option<u16> }`: Dynamically bind a TCP listener and update `server.json`.
  - `CloseTcpListener`: Terminate active TCP accept loops while retaining local UDS connections.
  - `GetServerEndpoints`: Query real-time listener states.

### 4. Canonical Default Port Realignment & Diagnostic Accuracy (`[INV-PORT-01]`)
- Deprecate and eradicate all hardcoded occurrences of `9527`. All client diagnostics and recovery fallbacks unify on `env_default_port()` (respecting `NUO_PORT`, defaulting to `9800`).
- Update `nuo status --diagnostic`:
  - When `diag.discovery_valid && diag.local_endpoint_connectable` is satisfied:
    - If TCP is disabled (`tcp_port == 0` or not listening), format TCP endpoint as `disabled (zero-surface local posture)` and declare the diagnosis as `Server is running and healthy (client-bound local IPC)`.
    - If TCP is listening, format TCP endpoint as `ws://127.0.0.1:<port> (listening)` and declare the diagnosis as `Server is running and healthy (hosted standalone posture)`.

### 5. Purge of `/debug` from Thread Slash Commands & Typed Client Initialization
- Thread slash commands MUST ONLY express conversational and agent cognition operations. `/debug` (and any infrastructure telemetry mutations) is eradicated from `BuiltinCmd` and `CommandCatalog`.
- The interactive client passes explicit port declarations directly through typed API boundaries (`client::ensure_server_explicit(&project_root, port)`), strictly eliminating runtime mutation of global environment variables (`std::env::set_var`).
- The internal server flag `--client-driven` is modernized to `--zero-tcp`, explicitly denoting the absence of TCP socket bindings.

---

## Invariants & Behavioral Boundaries

- **[INV-PORT-01] Canonical Baseline Port**: The system-wide default TCP control plane port is `9800`. Ambient `NUO_PORT` environment variables take precedence, followed by CLI `--port` flags. No subcomponent may define divergent fallback ports.
- **[INV-LIFECYCLE-03] Intent-Driven Server Handoff**: An interactive TUI launched with an explicit network port (`--port` or `NUO_PORT`) MUST NOT terminate the session server upon client exit, and MUST display an explicit detach handoff message.
- **[INV-CMD-02] Infrastructure & Cognition Orthogonality**: Server transport operations (listen/unlisten), port manipulation, and system diagnostics MUST NOT be exposed as conversational slash commands. They belong to Client Workspace Palettes (`C-x p`, `C-x d`).
- **[INV-PALETTE-01] Ephemeral Remote Sharing Palette**: `C-x p` MUST provide non-destructive inspection and hot-plugging of server network endpoints without interrupting the active thread composer or message history.

---

## Operational Consequences & Migration Path

- Operators running `nuo --port 9800` can safely disconnect and reconnect without killing running Agent tasks.
- Shared-machine multi-tenant users experience zero accidental port collisions and accurate, non-alarmist diagnostics under `nuo status --diagnostic`.
- TUI operators can dynamically share their live session over local network or web interfaces with a single keystroke (`C-x p Space`).
