---
id: ADR-0089
title: "nuo-client Retirement and Four-Way Responsibility Realignment"
status: accepted
date: 2026-11-25
scope: client/nuo-client, protocol/nuo-protocol, tui/nuo-tui, server/nuo-server, host/nuo-host
superseded_by: null
negative_knowledge: true
---

# 0089. nuo-client Retirement and Four-Way Responsibility Realignment

- Status: Accepted
- Date: 2026-11-25
- Deciders: Nuo Architecture Working Group
- Consulted: Core Runtime, Protocol Governance, TUI Engineering, Security
- Amends: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) (retires early `nuo-client` SDK role)
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md) (purifies Layer 3 presentation and Layer 1 protocol boundaries)
- Amends: [ADR-0078](0078-unified-remote-transport-topology-secure-endpoint-resolution-and-interactive-remote-attach-architecture.md) (relocates `ClientTarget` and `RemoteServer` to `nuo-tui::client`)

---

## Context and Problem Statement

When [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) dismantled `nuo-contracts`, `crates/nuo-client` was provisionally designated to hold client/daemon interaction DTOs and client transport. Subsequently, [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md) established the single public binary invariant (`nuo`) and created `nuo-wire` (later canonically named `nuo-protocol` in [ADR-0066](0066-canonical-protocol-nomenclature-and-codec-purity.md)) as the Layer-1 East-West IPC contract layer.

However, the migration remained incomplete, leaving `crates/nuo-client` in an ambiguous, unprincipled state with severe architectural defects:

1. **Duplicated Protocol DTOs & Framed Codecs**:
   `crates/nuo-client/src/wire.rs` duplicated verbatim the core wire DTOs (`Wire`, `AttachAction`, `ControlRequest`, `SessionInitOptions`, `NativeWireCodec`) that were already canonically maintained in `crates/nuo-protocol`.
2. **Duplicated Server Discovery Records**:
   `crates/nuo-client/src/discovery.rs` and `crates/nuo-server/src/serve_discovery.rs` maintained independent, near-identical copies of `struct Discovery` (`ServerInfo`), global path resolvers, and file leases.
3. **Severe Anti-Inversion Red-Line Violation**:
   `crates/nuo-server/Cargo.toml` directly depended on `nuo-client = { workspace = true }`, and multiple server modules imported `nuo_client::wire::*` and `nuo_client::compare_versions`. This directly violated the non-negotiable architectural invariant: *"nuo-server must NEVER depend directly or indirectly on nuo-client"*.
4. **Junk-Drawer TUI Domain Pollution**:
   `crates/nuo-client/src/lib.rs` accumulated over 1,000 lines of frontend-specific interactive slash command specifications (`command_catalog`), subcommand expansion algorithms (`complete_slash_items`), and file-system directory traversers (`complete_for_frontend_test` for `@file:` and `@skill:`). A headless client transport library was masquerading as a terminal autocomplete engine.
5. **Absence from Concentric Layer Governance**:
   In `docs/dev/standards/architecture.md`, `nuo-client` was never positioned in the four-layer topology diagram because in a single-binary architecture, an isolated in-tree client crate with zero external consumers is a superfluous structural layer.

Under a zero-legacy engineering standard, we make an uncompromising clean break by permanently retiring `crates/nuo-client` and executing a four-way responsibility realignment.

---

## Decision Drivers

- **Driver 1: Zero-Inversion Enforcement**: Guarantee that `nuo-server` strictly depends downward on Layer-1 contracts (`nuo-protocol`) and has zero dependencies on client or presentation code.
- **Driver 2: Single Source of Truth for IPC & Discovery**: All wire envelopes (`Wire`, `AttachAction`, `ControlRequest`) and server discovery formats (`Discovery`) must exist exclusively in `nuo-protocol`.
- **Driver 3: Single Public Binary Alignment**: The `nuo` executable and `nuo-tui` presentation substrate own the client transport drivers directly; no pseudo-SDK crate is maintained for hypothetical external callers.
- **Driver 4: UI/Presentation Boundary Purity**: Slash command catalogues, autocomplete state machines, and filesystem mention scanners belong strictly inside `nuo-tui`.

---

## Architectural Decisions

### 1. Retirement of `crates/nuo-client`

The crate `crates/nuo-client` is permanently removed from the workspace:
- Removed from workspace root `Cargo.toml`.
- Directory `crates/nuo-client` deleted.
- All references across `nuo`, `nuo-server`, and `nuo-tui` excised.

### 2. Relocate Protocol DTOs, Discovery, and Versioning to `nuo-protocol`

- **Discovery Substrate**: `crates/nuo-protocol/src/discovery.rs` is established as the canonical SSOT for `Discovery` (`ServerInfo`), `global_discovery_path()`, `instance_dir()`, `default_uds_path()`, `default_local_endpoint()`, `global_lock_path()`, and atomic discovery file I/O (`read()`, `write_to()`, `remove_if_matching_process()`).
- **Version & Skew Diagnostics**: `crates/nuo-protocol/src/version.rs` owns `compare_versions()`, `VersionRelation`, `protocol_accepts()`, `versions_compatible()`, `incompatibility_error()`, `version_mismatch()`, and `protocol_mismatch()`.
- **Elimination of Duplicated Codecs**: The duplicate `Wire`, `AttachAction`, `ControlRequest`, and `NativeWireCodec` in `nuo-client` are discarded in favor of the canonical implementations in `nuo-protocol`.

### 3. Server-Client Anti-Inversion Remediation in `nuo-server`

- `nuo-client = { workspace = true }` is removed from `crates/nuo-server/Cargo.toml`.
- `nuo-server` imports wire envelopes, control frames, and protocol versions strictly from `nuo_wire::*`.
- `nuo-server::serve_discovery` re-exports and consumes `nuo_wire::discovery::Discovery`.
- `nuo-server` has zero dependencies on presentation or client crates.

### 4. Interactive Command Catalog and Completion Relocated to `nuo-tui`

- `command_catalog()`: Static slash command definitions (`/models`, `/connections`, `/tools`, `/mcp`, `/compact`, `/new`, `/threads`, etc.) and command aliases move to `crates/nuo-tui/src/completion.rs`.
- `complete_slash_items()`, `complete_subcommand_items()`, and `complete_for_frontend_test()` move directly to `crates/nuo-tui/src/completion.rs`.

### 5. Client Transport and Lifecycle Driver Relocated to `nuo-tui::client`

- `crates/nuo-tui/src/client.rs` is established as the internal client transport module for presentation and CLI commands:
  - `RemoteServer` (URI parsing, RFC 3986 endpoint resolution, WSS/TLS handshake).
  - `ClientTarget` (`Local(ServerInfo)` vs `Remote(RemoteServer)`).
  - `Handshake` (`Attached` vs `Pick`).
  - Native UDS and WebSocket stream splitting (`native_framed_split`, `websocket_split`).
  - High-level async drivers: `connect()`, `control()`, `control_with_reply()`, `monitor_stream()`.
  - Process daemon management: `ensure_server()`, `ensure_server_explicit()`, `diagnose_server()`, `ServerDiagnostics`.
- The `nuo` application binary (`main.rs`, `commands/thread.rs`, `status.rs`, `supervisor.rs`) consumes `nuo_tui::client::*`.

---

## Normative Invariants Established

- **`[INV-ARCH-CLIENT-01] Zero-Client Server Topology`**: `nuo-server` must have zero compile-time dependencies (direct or indirect) on `nuo-client`, `nuo-tui`, or any presentation library.
- **`[INV-PROTO-DISCOVERY-01] Protocol Singularity for Discovery & Versioning`**: All discovery record formats (`Discovery`) and wire compatibility checks (`compare_versions`, `protocol_accepts`) must originate exclusively in `nuo-protocol`.
- **`[INV-TUI-SURFACE-01] Frontend Interactive Completion Purity`**: Interactive composer autocompletion, slash item generators, and filesystem mention scanners must never reside in headless IPC or protocol crates; they belong strictly in `nuo-tui`.

---

## Consequences

### Positive Consequences
- **Eradication of Circularity & Inversion**: `nuo-server` is 100% headless and unburdened by client crates.
- **Elimination of Duplication**: Removed hundreds of lines of duplicate DTOs, versioning logic, and discovery code.
- **Cognitive Simplicity**: The Cargo workspace topology aligns exactly with the concentric 4-layer architecture:
  - Layer 4: `nuo`
  - Layer 3: `nuo-tui`, `tools/*`, `providers/*`
  - Layer 2: `nuo-server`, `nuo-agent`, `nuo-harness`, `nuo-catalog`, `nuo-persistence`
  - Layer 1: `nuo-protocol`, `nuo-inference-wire`, `nuo-provider`, `nuo-tool`
- **Faster Compile Times**: Eliminates an intermediate crate and redundant link boundaries.

### Negative Consequences & Mitigations
- *In-tree client logic bundled in `nuo-tui`*: Headless CLI commands in `nuo` import `nuo_tui::client`.
  - *Mitigation*: `nuo` already links `nuo-tui` as the single executable binary; `nuo_tui::client` has zero terminal rendering dependencies and encapsulates pure network transport.

---

## Rejected Alternatives & Negative Knowledge

### 1. Retaining `nuo-client` as an Independent Crate
- *Why considered*: Preservation of a separate "Client SDK" crate for hypothetical external developers.
- *Why rejected*: Violates the Single Public Binary design principle. Nuo publishes a unified binary (`nuo`), not an ecosystem of multi-crate SDKs. Maintaining a pseudo-SDK crate created a junk drawer, caused illegal reverse dependencies from `nuo-server`, and fragmented discovery schemas.

### 2. Moving Client Transport into `nuo-protocol`
- *Why considered*: Placing `connect()` and WebSocket client code into `nuo-protocol`.
- *Why rejected*: Violates ADR-0005 `[INV-WIRE-01]` and ADR-0066 `[INV-PROTO-01]` (pure zero-I/O contract layer). `nuo-protocol` must compile with zero async network sockets or tungstenite runtime overhead.
