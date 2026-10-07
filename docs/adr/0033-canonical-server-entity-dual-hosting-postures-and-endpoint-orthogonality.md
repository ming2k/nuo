---
id: ADR-0033
title: "Canonical Server Entity, Dual Hosting Postures, and Orthogonal Endpoint Architecture"
status: accepted
date: 2026-10-11
scope: architecture/server, runtime/concentric, comm/ipc, security/isolation
superseded_by: null
negative_knowledge: true
---

# 0033. Canonical Server Entity, Dual Hosting Postures, and Orthogonal Endpoint Architecture

- Status: Accepted
- Date: 2026-10-11
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Security, Interface, Protocol, and CLI Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0029](0029-client-lifecycle-bound-daemon-foreground-headless-and-interface-takeover.md)

---

## Context and Problem Statement

Historically, Nuo operated under ambiguous terminology and conflated transport topologies:
1. **The "Daemon" Nomenclature Legacy**:
   Earlier iterations referred to the core session container as a "daemon" (`nuo start`, `daemon.json`, `daemon.sock`, `Mode::Daemon`). In modern software architecture, a daemon implies an opaque, detached Unix process with background forks and PID files—an anti-pattern in containerized (Docker, K8s) and supervisor-managed (systemd) environments. Concurrently, the codebase housed the server container crate as `nuo-server` (ADR-0011) and exposed `nuo serve`, creating terminological drift between user expectations and system architecture.
2. **Universal TCP Port Contention**:
   Under ADR-0029, every server instance (whether launched for an interactive TUI session or a persistent background service) attempted to bind both Unix Domain Sockets (UDS) and TCP port `9800`. This created severe hazards:
   - **Multi-Tenant Collisions**: On shared development servers, multiple concurrent engineers running `nuo` collided on TCP `9800`, triggering aggressive takeover wars that terminated peer processes.
   - **Expanded Attack Surface**: A local terminal session unnecessarily opened a TCP listener on loopback, exposing the control plane to browser-based drive-by attacks and DNS rebinding probes.
3. **Monolithic Hosting Assumptions**:
   Interactive terminal sessions require zero network exposure, sub-millisecond IPC latency, and strict user-bound file permissions (`0600`), whereas remote headless hosting requires deterministic TCP bindings, network exposure, and persistent container lifecycles.

We require a modern, unified architectural foundation that standardizes the **Server** as the canonical entity, stratifies hosting postures, and enforces orthogonal endpoint isolation.

---

## Decision Drivers

- **Domain Nomenclature Purity**: Eradicate "daemon" from domain entities; establish **Server** as the authoritative session and tool container aligned with `nuo-server`, MCP Server, and LSP ecosystems.
- **Zero Local Attack Surface (`[INV-SERVER-03]`)**: Interactive client-bound servers must expose zero TCP ports, restricting communication strictly to authenticated OS-level IPC (UDS / Named Pipes).
- **Multi-Tenant Frictionless Concurrency (`[INV-SERVER-04]`)**: Multiple users on a single host machine running interactive `nuo` sessions must never collide on TCP ports.
- **Hierarchical Fallback and Opportunistic Attachment (`[INV-SERVER-05]`)**: Interactive terminal clients must transparently attach to an existing standalone server if one is running, avoiding redundant process duplication.

---

## Considered Options

### Option 1: Universal Dual-Bind (Status Quo)
- Continue binding both UDS and TCP port 9800 on all server startups.
- *Assessment*: Rejected. Inevitably leads to TCP port collisions among multi-tenant users, expands local attack vectors, and causes aggressive takeover logic to erroneously terminate peer services.

### Option 2: Pure In-Process Engine (No IPC for TUI)
- Run the server directly inside the interactive TUI process without any socket or IPC abstraction.
- *Assessment*: Rejected. Violates ADR-0005 and ADR-0011; prevents concurrent aside monitoring (`nuo attach`), external tool review (`/review`), and headless multi-window orchestration.

### Option 3: Dual Hosting Postures with Orthogonal Endpoint Isolation (Chosen)
- Establish two clearly demarcated hosting postures:
  1. **Client-Bound Posture**: Binds strictly to user-scoped UDS (`server.sock`). Zero TCP port binding. Lifecycle coupled to connected interactive clients.
  2. **Standalone Hosted Posture**: Binds deterministic TCP port (`9800`) plus UDS. Always-on lifecycle suited for containers, remote development, and web frontends.
- Interactive clients opportunistically attach to an existing hosted server; when none exists, they spawn an isolated client-bound UDS server.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Random TCP Port Allocation for Interactive Sessions**
   - *Reason*: Assigning ephemeral random TCP ports for client-bound sessions evades port collisions but fails to eliminate the network attack surface, requires dynamic discovery negotiation, and leaves open loopback ports susceptible to browser discovery.
2. **Rejected: WebSockets Over TCP Loopback as the Exclusive Local Transport**
   - *Reason*: Deprecating UDS in favor of universal TCP loopback sacrifices kernel-level zero-copy performance (UDS offers 2x–3x throughput over loopback TCP) and relinquishes kernel `0600` file permission sandboxing.
3. **Rejected: Two Separate Executables (`nuo-server` vs `nuo-client`)**
   - *Reason*: Previously rejected under ADR-0005 and reaffirmed here. A single binary coordinator with flag-driven execution modes provides superior developer ergonomics.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Canonical Server Entity Alignment
- The central domain noun is ratified as **Server** across all crates (`nuo`, `nuo-server`, `nuo-client`, `nuo-host`).
- Top-level CLI verbs are canonicalized under `nuo server <start|stop|restart|status|token>`, with `nuo serve`, `nuo start`, `nuo stop`, and `nuo status` preserved as zero-cost ergonomic aliases.
- Configuration tables standardize on `[server]` in `config.toml`, with legacy `[daemon]` mapped via serde alias shims.

### 2. Dual Hosting Postures

```
                    nuo (Interactive TUI Client)
                               │
                               ▼
               [Probe 1: Active Standalone Server?]
               (Read discovery / connect TCP / UDS)
                      │                 │
                     YES                NO
                      │                 │
                      ▼                 ▼
             [Attach to Server]   [Probe 2: Client-Bound UDS?]
             (Shared instance)          │                 │
                                       YES                NO
                                        │                 │
                                        ▼                 ▼
                                  [Reuse UDS]     [Spawn Client-Bound Server]
                                                  (Strictly UDS, zero TCP)
```

#### A. Client-Bound Posture (`--client-driven`)
- **Transport**: Exclusively Unix Domain Socket (`$XDG_STATE_HOME/nuo/server.sock`) or platform Named Pipe.
- **Network Boundary**: **Zero TCP listeners**. `opts.port` is unset.
- **Security**: Kernel-enforced file mode `0600`. Completely invisible to browser runtimes and local port scanners.
- **Lifecycle**: Governed by active interactive client reference count; cascades termination 1 second after the last TUI disconnects (ADR-0029).

#### B. Standalone Hosted Posture (`nuo serve` / `nuo server start`)
- **Transport**: Deterministic TCP port (`9800` default) + local UDS.
- **Network Boundary**: Binds `127.0.0.1` (or `0.0.0.0` with `--public`).
- **Security**: Mandatory cryptographic bearer token authentication for TCP WebSocket/HTTP control plane endpoints.
- **Lifecycle**: Always-on foreground execution. Supervised by container runtimes or init systems.

---

## Invariants & Behavioral Boundaries

- **[INV-SERVER-03] Zero TCP on Client-Bound Posture**: An interactive server spawned under `client_driven` mode MUST NOT bind or listen on any TCP port unless an explicit `--port` flag is provided by the operator.
- **[INV-SERVER-04] User-Scoped Endpoint Isolation**: UDS endpoints MUST reside within the user's isolated instance directory (`$XDG_STATE_HOME/nuo/`), ensuring concurrent users on a shared OS host never collide.
- **[INV-SERVER-05] Opportunistic Server Attachment**: An interactive TUI invocation (`nuo`) MUST probe for a live compatible server in the current workspace/user context and attach to it before attempting to spawn a new server process.
- **[INV-SERVER-06] Canonical Server Nomenclature**: Public documentation, CLI help strings, log banners, and configuration schemas MUST designate the session host as "Server", deprecating "daemon" from domain vocabularies.

---

## Positive Consequences

- Completely eliminates TCP port 9800 collision wars on shared development servers.
- Hardens local security by cutting off browser drive-by scanning vectors against local TUI sessions.
- Enhances IPC communication speed with kernel-buffered UDS streams.
- Provides a clean mental model: lightweight invisible server for local terminal work; explicit network server for remote and container hosting.
