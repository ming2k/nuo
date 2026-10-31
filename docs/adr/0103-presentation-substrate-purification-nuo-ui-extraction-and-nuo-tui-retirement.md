---
id: ADR-0103
title: "Presentation Substrate Purification: Extraction of Pure Declarative nuo-ui, Relocation of Client Transport, and Retirement of nuo-tui"
status: accepted
date: 2026-11-30
scope: presentation/nuo-ui, presentation/nuo-tui, host/nuo, workspace/topology, transport/client
superseded_by: null
negative_knowledge: true
---

# 0103. Presentation Substrate Purification: Extraction of Pure Declarative nuo-ui, Relocation of Client Transport, and Retirement of nuo-tui

- Status: Accepted
- Date: 2026-11-30
- Deciders: Nuo Architecture Working Group
- Consulted: Interface Architecture, Terminal Rendering, Core Runtime, Protocol Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md), [ADR-0089](0089-nuo-client-retirement-and-four-way-responsibility-realignment.md)
- Amended by: [ADR-0105](0105-presentation-linkage-mechanism-and-dependency-firewall.md) — lean-link is an optional `interactive` feature; the dependency firewall is CI-gated

---

## Context and Problem Statement

The terminal user interface crate (`crates/nuo-tui`) was provisioned in [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md) by renaming the former `nuox` binary crate as part of the Single Public Binary unification mandated by [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md). Subsequently, when [ADR-0089](0089-nuo-client-retirement-and-four-way-responsibility-realignment.md) permanently retired `crates/nuo-client` to eradicate the unprincipled "junk drawer" SDK and fix illegal server-client dependency inversions, client-side network transport (`client.rs`, ~1,385 lines) was relocated into `nuo-tui::client` as an expedient migration measure. Concurrently, [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md) introduced the Elm Architecture (TEA) declarative component paradigm, but nested it inside `crates/nuo-tui/src/declarative`.

Over time, this accumulation of expedient compromises caused `crates/nuo-tui` to swell into a monolithic 133,000-line God crate suffering from severe architectural defects:

1. **Semantic Category Error of "TUI" as a Crate Name**:
   "TUI" denotes a physical presentation medium (Terminal User Interface), not a coherent system responsibility. Naming an entire interactive application layer `nuo-tui` forced daemon lifecycle supervision, IPC socket framing, headless execution runners, CLI argument parsing, interactive event loops, and declarative widget trees into an ambiguous, catch-all container.
2. **Severely Polluted Linkage for Headless and CLI Subcommands**:
   The `nuo` executable provides non-interactive commands (`nuo thread list`, `nuo status`, `nuo run --headless`). Because client transport resided inside `nuo-tui::client`, running a simple 10-millisecond IPC query forced the binary to link and initialize the entire terminal rendering engine (`nuotc`), Unicode segmentation engines, syntax highlighters, and the 130,000-line presentation tree.
3. **Impaired Presentation Purity and Test Hermeticity**:
   While [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md) and [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md) codified pure declarative components and an offline Storybook workbench, the component tree remained trapped inside a crate that directly depends on `tokio-tungstenite`, asynchronous network sockets, daemon process spawning, and clipboard libraries. This prevented testing the presentation substrate as a pure, zero-IO mathematical projection.
4. **Architectural Naming Asymmetry**:
   In Layer 2, `nuo-server` represents the headless runtime. In Layer 3, however, the codebase lacked a canonical counterpart for pure UI presentation, instead preserving a dated `nuo-tui` name alongside legacy workspace aliases (`nuox = { package = "nuo-tui" }`).

We require an uncompromising, clean-break architectural realignment that restores presentation purity, eliminates monolithic linkage overhead, and establishes unambiguous subsystem boundaries.

---

## Decision Drivers

- **Domain Precision and Layer Purity**: Presentation substrates must represent pure visual projections over terminal canvases, strictly decoupled from network I/O and process lifecycle mechanics.
- **Zero-IO Presentation Substrate (`[INV-ARCH-UI-ZERO-IO]`)**: UI components must be constructible, updatable, and renderable in pure memory without linking async sockets, IPC runtimes, or process supervisors.
- **Lean CLI and Headless Execution (`[INV-ARCH-CLI-LEAN-LINK]`)**: Non-interactive commands and headless runners must execute over pure client transport without dragging in terminal presentation substrates.
- **Abolition of the Catch-All God Crate**: Eradicate `nuo-tui` and its legacy aliases. Every subsystem must possess a single, unambiguous responsibility.
- **Single Public Binary Fidelity (`[INV-CLI-02]`)**: Preserve `nuo` as the sole public executable while avoiding the re-creation of pseudo-SDK junk drawers.

---

## Architectural Decisions

### 1. Extraction of `crates/nuo-ui` (Pure Declarative UI Substrate)

We establish **`crates/nuo-ui`** as the canonical Layer-3 presentation library in Nuo:
- **Core Abstraction**: Implements the TEA declarative component contract mandated by [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md) (`Component<Props, State, Msg, Cmd>`).
- **Scope & Contents**:
  - Declarative component definitions: `ToolCard`, `NoticeCard`, `AskUser`, `MarkdownTable`, `Footer`, `Modal`, `Dialogs`.
  - Design system tokens and styling: `theme.rs`, `design.rs`, color palettes, padding and elevation rules.
  - Offline Component Storybook and hermetic snapshot testing harness ([ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md)).
- **Substrate Dependency**:
  - `nuo-ui` depends downward strictly on `nuotc` ([ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md)) for terminal canvas differential rendering and layout geometry, and on Layer-1 contracts (`nuo-protocol`) for typed display schemas.
- **Zero-IO Enforcement**:
  - `nuo-ui` has **zero** dependencies on async network runtimes (`tokio-tungstenite`, `netune`), operating system socket primitives, or subprocess supervisors.

### 2. Internalization of Client Transport into Host Binary (`nuo::transport`)

We resolve the client transport placement without resurrecting an artificial SDK crate:
- **Internal Host Transport (`nuo::transport`)**:
  The client transport module (`crates/nuo-tui/src/client.rs`) is relocated directly into the `nuo` host binary crate as `nuo::transport`.
- **Responsibilities Retained**:
  - Endpoint resolution: Local Unix Domain Socket discovery (`discover`, `ServerInfo`) and remote WebSocket endpoints (`RemoteServer`).
  - Wire framing and splitting: UDS native framed split (`native_framed_split`) and WebSocket stream split (`websocket_split`).
  - Connection handshake and typed RPC dispatch: `connect()`, `control()`, `control_with_reply()`, `monitor_stream()`.
  - Background daemon supervision: `ensure_server()`, `stop_server()`, and health diagnostics (`diagnose_server()`).
- **Direct Consumption by CLI Subcommands**:
  Headless runs (`nuo run --headless`), session inspection (`nuo thread list`), and server management (`nuo server status`) consume `nuo::transport` directly, bypassing the UI presentation layer entirely.

### 3. Interactive Terminal Shell Consolidation (`nuo::shell`)

The interactive terminal application runtime is consolidated as `nuo::shell`:
- **Terminal Lifecycle**: Terminal raw mode initialization, signal traps, and exit guards.
- **Event Dispatch Loop**: Reads crossterm input events, drives `App` state transitions, translates user actions into `nuo::transport` RPCs, and projects active state into `nuo-ui` component trees for frame rendering.
- **Decoupled Architecture**: `nuo::shell` acts as the coordinator linking `nuo::transport` (data source) to `nuo-ui` (pure presentation sink).

### 4. Permanent Retirement of `crates/nuo-tui` and Legacy Aliases

- The directory `crates/nuo-tui` is deleted from the workspace.
- The entry `nuo-tui` is removed from workspace root `Cargo.toml`.
- The historical compatibility alias `nuox = { package = "nuo-tui", ... }` is permanently purged.

---

## Concentric Topology Realignment

Following this decision, the four-layer concentric architecture is purified:

```text
┌──────────────────────────────────────────────────────────────┐
│  Layer 4: Host Binary (`nuo`)                                │
│  - Unified single executable                                 │
│  - Internal Transport & Supervisor (`nuo::transport`)        │
│  - Interactive Terminal Shell (`nuo::shell`)                 │
├──────────────────────────────────────────────────────────────┤
│  Layer 3: Presentation & Capabilities                        │
│  - Pure Declarative UI Substrate: `crates/nuo-ui`             │
│  - Terminal Canvas Engine: `nuotc` (ADR-0003)                │
│  - Specialized Tools: `tools/*` (`fs`, `exec`, `web`, `ast`) │
│  - Model Providers: `providers/*`                            │
├──────────────────────────────────────────────────────────────┤
│  Layer 2: Core Runtime Substrate                             │
│  - Execution Engine: `crates/nuo-harness`                    │
│  - Model Routing Pipeline: `crates/nuo-model-routing`        │
│  - Headless Server Container: `crates/nuo-server`            │
│  - Persistent Storage: `crates/nuo-persistence`              │
├──────────────────────────────────────────────────────────────┤
│  Layer 1: Pure Contracts & Codecs                            │
│  - East-West IPC Protocol: `crates/nuo-protocol`             │
│  - North-South Model Codec: `crates/nuo-inference-wire`      │
│  - Provider Abstraction: `crates/nuo-provider`               │
│  - Tool Interface: `crates/nuo-tool`                         │
└──────────────────────────────────────────────────────────────┘
```

---

## Normative Invariants Established

- **`[INV-ARCH-UI-ZERO-IO] Zero-IO UI Substrate Invariant`**:
  `crates/nuo-ui` must have zero compile-time dependencies on network libraries (`tokio-tungstenite`, `netune`), daemon process spawning, or filesystem IPC sockets. Every component rendering projection must execute in pure memory as $f(\text{Props}, \text{State}, \text{Theme}) \to \text{Frame}$.
- **`[INV-ARCH-CLI-LEAN-LINK] Lean CLI Linkage Invariant`**:
  Non-interactive CLI subcommands (`nuo thread`, `nuo status`, `nuo run --headless`) and server lifecycle management commands must execute via `nuo::transport` without initializing, allocating, or rendering `nuo-ui` component trees or linking terminal raw-mode drivers.

---

## Consequences

### Positive Consequences

- **100% Precision in Nomenclature**:
  - `nuotc`: The terminal canvas engine.
  - `nuo-ui`: The pure declarative UI component library.
  - `nuo`: The unified application binary owning the interactive shell and client transport.
- **Sub-Millisecond CLI Execution**:
  Headless queries and subcommands execute without linking 130,000+ lines of terminal presentation code, reducing memory overhead and binary symbol bloat.
- **Isolated, Hermetic UI Testing**:
  UI components can be constructed and tested via Storybook snapshot regressions in pure memory without launching daemons, network sockets, or background runtimes.
- **Eradication of God Crate Anti-Pattern**:
  Deconstructs `crates/nuo-tui`, resolving the long-standing conflation of network transport, application loops, and drawing trees.

### Negative Consequences & Trade-offs

- **Extensive Import Refactoring**:
  References to `nuo_tui::*` across the host binary, integration tests, and benchmarks must be migrated to `nuo_ui::*` or `crate::transport::*`.
- **Progressive Component Migration**:
  Legacy immediate-mode drawing modules in the old crate must be systematically adapted to the `Component` contract in `nuo-ui`.

---

## Rejected Alternatives & Negative Knowledge

### 1. Renaming `nuo-tui` to `nuo-frontend` (Rejected)
- *Why considered*: To acknowledge that the crate contained both client network transport and terminal presentation.
- *Why rejected*: "Frontend" is a web/browser paradigm that introduces a jarring category error into system-level terminal applications. In client-server architectures, the canonical counterpart to Server is Client, not Frontend. Furthermore, renaming without decomposing would perpetuate the monolithic entanglement of networking and rendering under a superficial label.

### 2. Reviving `crates/nuo-client` as an Independent Crate (Rejected)
- *Why considered*: To provide a symmetrical `nuo-server` / `nuo-client` crate pairing in Cargo workspace members.
- *Why rejected*: Reaffirming [ADR-0089](0089-nuo-client-retirement-and-four-way-responsibility-realignment.md): Nuo ships a single unified binary (`nuo`), not a multi-crate public SDK ecosystem. Maintaining a separate `nuo-client` crate invites the exact scope creep, reverse dependencies from server tests, and "junk drawer" entropy that led to its retirement. Internalizing transport into `nuo::transport` satisfies `[INV-CLI-02]` with zero crate-proliferation overhead.

### 3. Creating a Generic `crates/nuo-transport` Crate (Rejected)
- *Why considered*: Isolating IPC connection logic into a dedicated Layer-2 crate.
- *Why rejected*: Reaffirming [ADR-0085](0085-universal-outbound-egress-substrate-and-provider-decoupling.md): In Nuo, Client-Server IPC, ACP agent collaboration, and MCP subprocess bridges all feature transport mechanisms. A bare `nuo-transport` crate violates `[INV-ARCH-01]` (Catch-All Junk Drawer Ban) by introducing namespace ambiguity. Client-server IPC transport is exclusively a concern of the `nuo` host binary.

### 4. In-Process Direct Memory Invocation Without Transport (Rejected)
- *Why considered*: Bypassing socket IPC entirely during interactive terminal runs by instantiating `nuo-server` directly in the same memory process.
- *Why rejected*: Directly violates `[INV-ARCH-DETACH-01]` (Non-Destructive Detach Lifecycle, [ADR-0029](0029-client-lifecycle-bound-daemon-foreground-headless-and-interface-takeover.md), [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md)). An in-process server would abort long-running agent tasks whenever the user detaches or the terminal window is closed. It would also preclude concurrent multi-client attachments (`nuo thread list` running alongside an active interactive session).

---

## Migration Roadmap & Milestones

1. **Milestone 1 (Governance & Invariant Ratification)**:
   Ratify ADR-0103, register it in the ADR catalog, and update the architectural blueprint (`docs/dev/subsystems.md`).
2. **Milestone 2 (Transport Relocation)**:
   Extract `crates/nuo-tui/src/client.rs` into `nuo::transport`. Update `nuo` CLI subcommands (`thread`, `status`, `headless`) to bypass `nuo-tui`.
3. **Milestone 3 (nuo-ui Extraction)**:
   Create `crates/nuo-ui` containing `declarative/`, `components/`, `theme.rs`, `design.rs`, and Storybook snapshot infrastructure. Enforce `[INV-ARCH-UI-ZERO-IO]`.
4. **Milestone 4 (nuo-tui Decommissioning)**:
   Relocate remaining interactive terminal event loops and state into `nuo::shell`. Delete `crates/nuo-tui` and the legacy `nuox` workspace alias.

---

## Links

- [ADR-0003: Autonomous Terminal Canvas Substrate (Nuotc)](0003-autonomous-terminal-canvas-substrate-nuotc.md)
- [ADR-0005: Unified Binary and Concentric Runtime Architecture](0005-unified-binary-and-concentric-runtime-architecture.md)
- [ADR-0011: Nuo-Tui Presentation Decoupling, Nuo-Server Extraction, and Unified Host Architecture](0011-nuo-tui-presentation-and-nuo-server-container.md)
- [ADR-0029: Client-Lifecycle-Bound Daemon and Interface Takeover](0029-client-lifecycle-bound-daemon-foreground-headless-and-interface-takeover.md)
- [ADR-0033: Canonical Server Entity, Dual Hosting Postures, and Endpoint Orthogonality](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md)
- [ADR-0072: Pure Declarative Component Paradigm and TEA Single-Direction Dataflow Topology](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md)
- [ADR-0073: Offline Component Storybook and Hermetic Visual Regression Pipeline](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md)
- [ADR-0085: Universal Outbound Egress Substrate and Provider Decoupling](0085-universal-outbound-egress-substrate-and-provider-decoupling.md)
- [ADR-0089: nuo-client Retirement and Four-Way Responsibility Realignment](0089-nuo-client-retirement-and-four-way-responsibility-realignment.md)
