---
id: ADR-0078
title: "Unified Remote Transport Topology, Secure Endpoint Resolution, and Interactive Remote Attach Architecture"
status: accepted
date: 2026-11-18
scope: client/nuo-client, tui/nuo-tui, server/nuo-server, comm/transport
superseded_by: null
negative_knowledge: true
---

# 0078. Unified Remote Transport Topology, Secure Endpoint Resolution, and Interactive Remote Attach Architecture

- Status: Accepted
- Date: 2026-11-18
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Security, Protocol, Interface, and TUI Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0045](0045-dynamic-port-sharing-palette-orthogonal-control-plane-and-server-lifecycle-handoff.md)

---

## Context and Problem Statement

Nuo's client-server architecture decouples the execution engine (`nuo-server`) from presentation surfaces (`nuo-tui`, CLI) across typed protocol boundaries ([ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md)). Under [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md) and [ADR-0045](0045-dynamic-port-sharing-palette-orthogonal-control-plane-and-server-lifecycle-handoff.md), Nuo established dual hosting postures (Client-Bound UDS vs. Standalone Hosted TCP) and dynamic port sharing via the `C-x p` palette.

However, field operations revealed four critical architectural asymmetries and usability defects in remote collaboration:

1. **Interactive Remote Attach Blindspot**:
   While the CLI parser accepted `--remote <addr>` and `--token <token>`, and the TUI port palette (`C-x p`) displayed copyable connection commands (`nuo --remote 127.0.0.1:<port> --token <token>`), the interactive client runner (`nuo_tui::runner::run_attached`) unconditionally discarded `--remote` and forced local Unix Domain Socket discovery (`client::ensure_server_explicit`). Running `nuo --remote` in interactive mode failed or silently attached to the wrong local server. Remote network attachment was restricted only to non-interactive headless single-prompt runs.
2. **URL Scheme and Port Resolution Asymmetry**:
   `RemoteServer::parse` only supported plain `ws://host:port` or `host:port`. It lacked native TLS (`wss://`, `https://`) parsing, bracketed IPv6 parsing, and default scheme-derived port resolution (e.g. port 443 for `wss://`, port 80 for `ws://`). Attempting to connect through standard Zero-Trust tunnels (Cloudflare Tunnel, ngrok, HTTPS reverse proxies) failed with syntax errors like `'tunnel.domain.com' is not host:port`.
3. **Remote Working Directory Path Inversion**:
   During the handshake (`finish_handshake`), clients unilaterally serialized their local process working directory (`std::env::current_dir()`) in `Wire::Select`. When an external collaborator attached from their laptop, their irrelevant local filesystem path was sent to the remote host, causing the server to attempt scoping or creating sessions in non-existent local directory paths.
4. **Unbounded Attack Surface & Over-Engineering Dilemma**:
   When opening network listeners for pair programming and remote design collaboration, developers face a tension between unauthenticated RCE risk and heavy enterprise over-engineering (multi-tenant RBAC, OAuth, dynamic sandboxing). A clean, pragmatic, mathematically sound authentication boundary is needed: a high-entropy pre-shared Bearer token combined with mandatory TLS/WSS.

We require a decisive architectural resolution that unifies client connection targets, provides native WSS/TLS resolution, eliminates directory path pollution, and enables full interactive pair-programming across network transports.

---

## Decision Drivers

- **Complete Transport Symmetry (`[INV-REMOTE-02]`)**: Interactive terminal clients and headless runners must use an identical, unified target abstraction (`ClientTarget::Local` vs. `ClientTarget::Remote`) to connect to servers over local UDS or network WebSocket transports.
- **Universal Endpoint Resolution (`[INV-REMOTE-01]`)**: `RemoteServer` must natively resolve explicit TLS schemes (`wss://`, `https://`) with default port 443, cleartext schemes (`ws://`, `http://`) with default port 80, socket addresses (`host:port`), and bracketed IPv6 addresses (`[::1]:port`). Ambiguous bare hostnames without a port or explicit scheme must be rejected.
- **Sovereign Remote Detach (`[INV-REMOTE-03]`)**: Detaching an interactive client from a remote server must never cascade termination to the server or attempt local lifecycle signaling. It must display an actionable remote reconnect affordance.
- **Clean Epistemic Project Scoping**: Remote clients must not project foreign local filesystem paths into remote server registries. The target session on a remote server must be determined by explicit session ID or server-held workspace SSOT.
- **Pragmatic Transport Security Floor**: Require cryptographically random Bearer tokens (via `uuid::Uuid::new_v4`) paired with TLS/WSS transport encryption, eliminating unauthorized public exposure without premature RBAC over-engineering.

---

## Architectural Decisions

### 1. Unified Client Target Typestate (`ClientTarget`)

We establish `ClientTarget` in `nuo-client` as the canonical target abstraction for both interactive and headless runtimes:

```rust
#[derive(Debug, Clone)]
pub enum ClientTarget {
    Local(ServerInfo),
    Remote(RemoteServer),
}

impl ClientTarget {
    pub async fn connect(&self, action: AttachAction) -> Result<Handshake, String>;
    pub fn endpoint_display(&self) -> String;
}
```

- When `--remote <addr>` is provided, `run_attached` resolves `ClientTarget::Remote(RemoteServer::parse(&addr, token)?)`. Zero local UDS probes or server spawns occur.
- When `--remote` is absent, `run_attached` resolves `ClientTarget::Local(ensure_server_explicit(...))`.
- Interactive session switching and picker loops operate transparently over `target_server.connect(action)` regardless of physical transport.

### 2. Universal Endpoint Resolution Engine (`[INV-REMOTE-01]`)

`RemoteServer::parse` is modernized into a standard RFC 3986 URI and authority resolver:
- **Secure URIs**: `wss://` and `https://` strip the scheme, set `secure: true`, and default port to `443`.
- **Cleartext URIs**: `ws://` and `http://` strip the scheme, set `secure: false`, and default port to `80`.
- **Socket Addresses**: `host:port` and `[ipv6]:port` parse explicit ports and set `secure: false`.
- **Loopback Abbreviation**: `:port` resolves `host: "127.0.0.1"`, port from string, and `secure: false`.
- **Ambiguity Rejection**: Bare hostnames without an explicit scheme or port (e.g. `box.lan`) are rejected to prevent accidental misrouting.
- **Credential Fallback**: Token is resolved from CLI `--token <token>` or ambient `NUO_TOKEN` environment variable.

### 3. Native TLS/WSS Support via Rustls Native Roots

`tokio-tungstenite` workspace dependency is configured with `features = ["rustls-tls-native-roots"]`. When `RemoteServer.secure` is true, the client establishes an encrypted TLS channel verified against the host system's root certificates, natively supporting Cloudflare Tunnels, Let's Encrypt, and corporate reverse proxies.

### 4. Non-Polluting Remote Handshake Scoping

`finish_handshake_with_project` decouples client process working directory from wire session resolution:
- **Local Attach**: Passes `std::env::current_dir().ok()` to allow same-machine project matching.
- **Remote Attach**: Passes `project: None` (or explicit `--project` override). The remote server attaches to the active hosted session or presents the remote session picker, preventing foreign client filesystem paths from polluting the server's session registry.

### 5. Sovereign Remote Detachment Lifecycle (`[INV-REMOTE-03]`)

Upon interactive TUI exit (`None => outcome`):
- For `ClientTarget::Local`: retains the local detachment message with PID and port.
- For `ClientTarget::Remote`: displays the actionable remote reconnect command:
  ```text
  nuo: detached from remote server (wss://tunnel.example.com).
    • Reconnect:   nuo --remote wss://tunnel.example.com --token <token>
  ```
- No server termination signals (`kill`, `SIGTERM`) are emitted across network boundaries.

---

## Normative Invariants Established

- **`[INV-REMOTE-01] Universal Endpoint Resolution`**:
  `RemoteServer::parse` MUST accept explicit secure schemes (`wss://`, `https://`) with default port 443, cleartext schemes (`ws://`, `http://`) with default port 80, socket addresses (`host:port`), bracketed IPv6 addresses, and `:port` loopback notation. Bare hostnames without a scheme or port MUST be rejected.
  *Enforcement*: Mechanically verified in `test_inv_remote_01_endpoint_resolution`.

- **`[INV-REMOTE-02] Interactive Remote Symmetry`**:
  The interactive client runtime (`run_attached`) MUST support connecting to a `RemoteServer` across network transports with full terminal parity, without probing or spawning a local UDS server process.
  *Enforcement*: Mechanically verified in `test_inv_remote_02_remote_symmetry_and_detach`.

- **`[INV-REMOTE-03] Sovereign Remote Detach`**:
  Detaching an interactive client from a remote server MUST NOT cascade termination to the remote server, and subsequent clients MUST be able to re-attach to the server.
  *Enforcement*: Mechanically verified in `test_inv_remote_02_remote_symmetry_and_detach`.

---

## Rejected Alternatives & Negative Knowledge

1. **Rejected: Bundling NAT Traversal / STUN / TURN / DERP into `nuo` Core**
   - *Reason*: Violates Concentric Workspace Topology (ADR-0005) and Server Headless Purity (ADR-0033). NAT hole-punching and relay infrastructure require dedicated daemons, protocol churn, and ongoing cloud relay maintenance. External mature tooling (Cloudflare Tunnel, Tailscale, ngrok, frp) handles network tunneling with zero codebase bloat.
2. **Rejected: Heavy Multi-Role RBAC / OAuth for Initial Remote Pairing**
   - *Reason*: Over-engineering for developer pair programming. A cryptographically strong Bearer token (128-bit entropy via UUIDv4) transmitted over TLS (WSS) provides impenetrable protection against internet scanning. Full multi-tenant RBAC belongs to future enterprise gateways, not the microkernel core.
3. **Rejected: Auto-Defaulting Ambiguous Hostnames to Port 9800**
   - *Reason*: If `nuo --remote my-server` silently defaulted to `9800`, a typo in a domain name could silently attempt loopback or unintended ports, creating false-positive diagnostic puzzles. Requiring either `:port` or an explicit scheme (e.g. `wss://my-server`) ensures unambiguous operator intent.
4. **Rejected: Silent No-Op Legacy Flags (`--detach` / `--bg` / `-d` / `--client-driven` on `nuo server start`)**
   - *Why considered*: Backwards compatibility with scripts authored before detachment became the default posture and before `--zero-tcp` was ratified (ADR-0045).
   - *Why rejected*: A flag that explicitly requests the default posture is pure cognitive noise — it reads as if it changes behavior while changing nothing; a superseded alias teaches users a spelling that no longer exists in documentation. Both spellings are now refused with guidance (`--detach was removed — nuo server start already runs detached; use --fg for foreground`, `--client-driven was renamed — use --zero-tcp`), converting silent dead syntax into an actionable modernization signal.

---

## Consequences

### Positive
- Fully enables remote pair programming and external collaborator assistance via `nuo --remote <tunnel> --token <token>`.
- Interactive TUI and headless runs share a clean, unified `ClientTarget` abstraction.
- Zero-Trust tunnels (Cloudflare Tunnel, Tailscale, reverse proxies) work out of the box with `wss://` and `https://` URLs.
- Eliminates foreign filesystem path pollution in remote session registries.

### Negative & Trade-offs
- Compiling `tokio-tungstenite` with `rustls-tls-native-roots` introduces `rustls-native-certs` dependency into `nuo-client`, slightly increasing client crate compile time. Mitigated by `rustls` being purely in-tree without C-compiler (`OpenSSL`) dependencies.
