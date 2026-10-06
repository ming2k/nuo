---
id: ADR-0029
title: "Client-Lifecycle-Bound Daemon, Explicit Foreground Headless Host, and Aggressive Interface Takeover"
status: accepted
date: 2026-10-10
scope: runtime/daemon-lifecycle, architecture/cli, server/nuo-server, client/discovery, comm/ipc
superseded_by: null
negative_knowledge: true
---

# 0029. Client-Lifecycle-Bound Daemon, Explicit Foreground Headless Host, and Aggressive Interface Takeover

- Status: Accepted
- Date: 2026-10-10
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Interface, CLI, and Protocol Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0021](0021-daemon-image-content-identity-and-idle-self-heal.md)

---

## Context and Problem Statement

Historical daemon and server orchestration in `nuo` operated on assumptions inherited from detached Unix system daemons:
1. **Unbounded Detached Lifetimes**:
   When launched interactively via TUI (`nuo`), `nuo-client` spawned a background detached daemon process via `detach_daemon()`. This daemon lingered in the background indefinitely (with `idle_exit` defaulting to 1440 minutes / 24 hours) long after all terminal interactive sessions had closed. Operators closing their terminal windows were unaware that background processes continued to consume RAM and hold file locks, leading to phantom/zombie processes and developer confusion.
2. **Hidden Headless Execution**:
   Commands intended to run headlessly or manage the service (such as `nuo start`) defaulted to background detachment, requiring an explicit `--fg` flag to stay in the foreground. In modern containerized (Docker, Kubernetes) and init-supervised (systemd) environments, background detachment is an anti-pattern that conceals stdout/stderr streams, breaks supervisor health monitoring, and impedes log aggregation.
3. **Fragile and Conservative Interface Collision Handling**:
   When launching a daemon, collisions on the Unix Domain Socket (UDS) path (`daemon.sock`) or TCP port (`9800`) were handled by defensive refusal: returning `AddrInUse`, falling back to non-deterministic random ports, or blocking on `daemon.lock` for 10 seconds before failing with an error directing the human operator to manually run `nuo stop`. If a rebuilt binary or mismatched process held the socket or port, automatic startup halted.

We require a modernized, uncompromising runtime architecture with zero legacy burden:
- Daemons spawned by interactive TUI sessions must be coupled to the lifecycle of active clients: when the last TUI closes, the daemon terminates cleanly.
- Headless runs and standalone service hosts must run explicitly in the foreground.
- Any conflict on configured endpoints (UDS or TCP port) between distinct daemon instances must trigger automatic, direct takeover and replacement of the stale predecessor without human intervention.

---

## Decision Drivers

- **Zero Zombie Footprint**: Exiting all interactive terminal windows must leave zero lingering background `nuo` processes.
- **Cloud-Native & Supervisor Purity**: Running a service or headless command must default to foreground execution with visible stdio streams.
- **Self-Healing Interface Takeover**: Interface collisions (UDS or TCP port) across differing daemon instances must resolve automatically via graceful drain followed by forceful termination and socket rebinding.
- **Zero Legacy Burden**: Eliminate manual `nuo stop` gating when a replacement daemon is launched.

---

## Considered Options

### Option 1: Status Quo with Shorter Idle Timeout
- Keep detached daemon execution, but reduce `idle_exit_minutes` from 1440 to 5 minutes.
- *Assessment*: Inadequate. Daemons still persist after TUI exit for up to 5 minutes, leaving residual locks and port conflicts. Does not fix headless foreground visibility or port collision refusal.

### Option 2: Pure In-Process Embedded Engine for TUI
- Run the server directly inside the TUI binary process, eliminating IPC and sockets for single-window interactive use.
- *Assessment*: Breaks multi-window attachment, aside/subagent multi-client monitoring, and client-server architectural decoupling codified in ADR-0005 and ADR-0011.

### Option 3: Client-Lifecycle-Bound Daemon, Foreground Headless, and Aggressive Takeover (Chosen)
- Track active interactive client sessions in the daemon connection table. When active interactive client count drops to zero, trigger graceful shutdown after a short debounce (1 second).
- Make standalone server / headless execution strictly foreground by default.
- On startup, if UDS or TCP port conflicts with an existing differing process, execute an aggressive takeover: signal graceful stop with a 500ms budget, forcibly kill the stale PID if lingering, unlink stale sockets, and bind the endpoint immediately.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: PID File Exclusivity Without Socket Reclaiming**
   - *Reason*: Storing PIDs in lockfiles without active socket and port takeover fails when orphaned processes hang or when PID wrap-around occurs. Reclaiming requires socket connectivity probing and direct signal delivery to the socket/port holder.
2. **Rejected: Zero-Second Immediate Teardown on Client Disconnect**
   - *Reason*: Instantaneous teardown without debounce causes race conditions during rapid terminal restarts, multiplexer pane switching, or network blips in remote scenarios. A 1-second debounce cleanly absorbs client reconnects without perceptible operator lag.
3. **Rejected: Silent Random TCP Port Fallback on Collision**
   - *Reason*: Binding a random port when 9800 is occupied hides the fact that a collision occurred and breaks fixed-endpoint configurations. Direct replacement of the conflicting process guarantees deterministic endpoint adherence.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Client-Lifecycle-Bound Daemon Termination (`[INV-DAEMON-01]`, `[INV-DAEMON-02]`)
- Connections are tracked in `ConnTable` by client category: **Interactive (TUI)** (`AttachAction::New`, `Attach`, `Picker`) versus **Passive (Monitor/Probe)** (`AttachAction::Monitor`, `Control`).
- When the daemon is launched by an interactive client (or in client-driven mode), the daemon monitors active interactive connections.
- When `active_tui_clients` drops to zero, a 1-second debounce timer arms. If no new interactive client connects before the timer fires, the daemon requests graceful shutdown (`ShutdownReason::AllClientsClosed`) via `ShutdownGate`.
- Persisted sessions, durable logs, and database transactions are cleanly flushed before exit.

### 2. Explicit Foreground Headless Host (`[INV-DAEMON-03]`)
- `nuo start` and `nuo serve` run explicitly in the foreground by default.
- Standard out and standard error remain attached to the controlling terminal, streaming real-time status banners, local endpoint URIs, and structured tracing events.
- Headless prompt runs (`nuo run`, `nuo -p`) execute attached to the foreground.

### 3. Aggressive Interface Conflict Takeover (`[INV-DAEMON-04]`, `[INV-DAEMON-05]`)
- During startup, the daemon checks both configured native endpoints: the Unix Domain Socket (`daemon.sock`) and the TCP port (`opts.port`, default 9800).
- If either endpoint is occupied:
  1. The initiating process identifies the target process PID (via `daemon.lock`, discovery record probe, or socket ownership).
  2. If the PID corresponds to a different process (`pid != current_pid`), the starter initiates **Takeover**:
     - Sends a graceful stop request to the existing daemon with a tight 500ms deadline.
     - If the process remains alive after 500ms, sends `SIGKILL` (or OS equivalent) to ensure unblocking.
     - Unlinks any stale UDS socket file and clears stale locks.
     - Binds the requested UDS and TCP port cleanly.
- The new daemon immediately takes ownership of the interfaces without prompting the user or aborting.

---

## Invariants & Behavioral Boundaries

- **[INV-DAEMON-01] Interactive Client Accounting**: The daemon MUST distinguish interactive TUI attachments from monitor/control streams and maintain an atomic count of live interactive clients.
- **[INV-DAEMON-02] Cascade Termination on TUI Disconnect**: When spawned in client-driven mode and the interactive client count reaches zero, the daemon MUST trigger graceful termination after a debounce window not exceeding 2 seconds.
- **[INV-DAEMON-03] Foreground Default**: Daemon hosting verbs (`nuo start`, `nuo serve`) MUST execute in the foreground by default, streaming stderr banners and responding directly to POSIX signals.
- **[INV-DAEMON-04] Aggressive Interface Takeover**: On UDS or TCP port collision with a differing PID, the daemon MUST proactively terminate the conflicting predecessor and claim the endpoints, never failing with `AddrInUse` or falling back to arbitrary ports.
- **[INV-DAEMON-05] Deterministic Bound Endpoints**: The daemon MUST serve on the canonical resolved endpoints (`daemon.sock` and configured TCP port) and never silently switch ports when started in replacement mode.

---

## Positive Consequences

- Eliminates background ghost processes when terminal windows close.
- Streamlines development iteration: running a new `nuo` immediately takes over endpoints from any stale daemon.
- Container, Docker, and systemd deployments work out-of-the-box in standard foreground supervision mode.
- Eliminates operator friction caused by "stop it with `nuo stop`" refusal messages.

---

## Negative Consequences & Trade-offs

- Running a second TUI in a different terminal window must connect to the same daemon before the 1-second debounce expires if the first window closes immediately. (Mitigated: 1-second debounce is sufficient, and if the daemon restarts, state is preserved in persistent SQLite store).
- Aggressive replacement kills existing conflicting processes holding the configured port. (Mitigated: restricted to Nuo instance directory sockets and configured Nuo ports, with graceful 500ms drain attempted first).
