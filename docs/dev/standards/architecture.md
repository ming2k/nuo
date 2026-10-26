# System Architecture Governance

This document codifies the workspace topology, subsystem boundaries, and non-negotiable architectural red-lines of Nuo.

---

## 1. Concentric Workspace Topology

Nuo organizes its Cargo workspace into four concentric layers. Dependencies flow strictly from outer layers inward; backward or circular dependencies across layer boundaries are prohibited:

```text
┌──────────────────────────────────────────────────────────────────────┐
│  Layer 4: Host Binary (`crates/nuo/`)                                │
│  - Unified single executable (CLI & Terminal Host)                   │
│  - Internal Client Transport: `nuo::transport` (ADR-0103)            │
│  - Interactive Shell: `nuo::shell` behind `interactive`              │
├──────────────────────────────────────────────────────────────────────┤
│  Layer 3: Presentation & Capabilities                                │
│  - Generic UI Primitives (zero-IO): `crates/nuo-ui-primitives`       │
│  - Agent Views (zero-IO): `crates/nuo-view`                          │
│  - Terminal Canvas Engine: `nuotc` (ADR-0003)                        │
│  - Specialized Tools: `tools/nuo-tool-*`                             │
│  - Model Providers: `providers/nuo-provider-*`                       │
├──────────────────────────────────────────────────────────────────────┤
│  Layer 2: Core Runtime Substrate                                     │
│  - Execution Engine: `crates/nuo-harness`                            │
│  - Model Routing Pipeline: `crates/nuo-model-routing`                │
│  - Headless Server Container: `crates/nuo-server`                    │
│  - Persistent Storage: `crates/nuo-persistence`                      │
├──────────────────────────────────────────────────────────────────────┤
│  Layer 1: Pure Contracts & Codecs                                    │
│  - East-West IPC: `crates/nuo-protocol`                              │
│  - North-South Model Codec: `crates/nuo-inference-wire`              │
│  - Provider Substrate: `crates/nuo-provider` (Contracts, Transport, Auth; ADR-0108) │
│  - Tool Interface: `crates/nuo-tool`                                 │
└──────────────────────────────────────────────────────────────────────┘
```

**Physical layout (`[INV-ARCH-TIER-01]`, ADR-0109)**: All crates uniformly reside within one of
three flat tiers: `crates/*` (Layers 1–4, with `crates/nuo` as the sole public application
executable), `providers/nuo-provider-*` (Layer-3 providers), and `tools/nuo-tool-*` (Layer-3
tools). Zero crates reside directly at the workspace root. Gated mechanically by
`scripts/check_presentation_firewall.py` R5.

---

## 2. Architectural Red-Lines

### 2.1 Server-Client Anti-Inversion & Zero-Client Topology
- **Red-Line**: `nuo-server` must NEVER depend directly or indirectly on the presentation substrates (`nuo-ui-primitives`, `nuo-view`), the terminal canvas (`nuotc`), or retired client crates (`nuo-client`).
- **Enforcement**: Client presentation surfaces and Server interact strictly as peer processes across IPC boundaries. IPC messaging and discovery records are typed and serialized via `nuo-protocol` (ADR-0089). The obsolete intermediate crate `nuo-client` is permanently retired [INV-ARCH-CLIENT-01]; client transport belongs strictly to the host binary (`nuo::transport`, ADR-0103) and protocol contracts to `nuo-protocol`. Any attempt to introduce presentation or client dependencies into `nuo-server` is rejected at build time.

### 2.2 Headless Server Container Purity
- **Red-Line**: `nuo-server` must NEVER link terminal presentation libraries (`nuotc`, `crossterm`) or the presentation substrates (`nuo-ui-primitives`, `nuo-view`).
- **Enforcement**: `nuo-server` must compile and execute in purely headless, minimal containerized, or remote daemon environments with zero GUI/TUI overhead.

### 2.3 IPC Protocol vs. Model Codec Orthogonality
- **`nuo-protocol` (East-West IPC)**: Governs local inter-process communication between client and server, task dispatch, session snapshots, and event telemetry.
- **`nuo-inference-wire` (North-South Transport)**: Governs wire serialization to external model endpoints (HTTP SSE, JSON streaming). It carries only objective physical telemetry facts (`TokenClass`, `TokenBreakdown`) with zero financial accounting or local IPC logic.
- **Passthrough Prohibition**: Subsystems must not use intermediate crates as passthrough re-export conduits.

### 2.4 Non-Destructive Detach Lifecycle
- **Red-Line**: Exiting an interactive client (TUI or CLI) is strictly a **Detach** operation. It must NEVER tear down a running background session or terminate the hosted server daemon.
- **Enforcement**: The server maintains session authority (Single Source of Truth), allowing clients to attach, detach, and reattach without data loss.

---

## 3. Crate Addition and Workspace Integrity

1. **Single Public Binary**: `nuo` is the sole public executable binary produced by the workspace. No secondary binary entrypoints (e.g., deprecated `nuox`) are permitted.
2. **Crate Inclusion Threshold**: New crates are admitted to `crates/*` or `providers/*` only if their responsibilities cannot be cleanly modeled as internal modules of existing domain crates and their position in the concentric hierarchy is unambiguously ratified via an ADR.
