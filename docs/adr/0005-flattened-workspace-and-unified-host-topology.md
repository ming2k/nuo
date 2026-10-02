---
id: ADR-0005
title: "Flattened Workspace Architecture and Unified Host Topology"
status: accepted
date: 2026-10-01
scope: workspace/topology, app/nuo, client/sdk, harness/naming
superseded_by: null
negative_knowledge: true
---

# 0005. Flattened Workspace Architecture and Unified Host Topology

- Status: Accepted
- Date: 2026-10-01
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Runtime Team
- Informed: System Architects

---

## Context and Problem Statement

As Nuo evolved from a monolithic repository into an ecosystem powered by the `nous` cognitive substrate, artificial structural divisions and historical naming artifacts began accumulating cognitive overhead and architectural debt:

1. **Hierarchical Indirection (`apps/` vs `crates/`)**:
   Dividing the repository into `apps/` and `crates/` added artificial directory depth without engineering benefit. In particular, the web frontend was relegated to `apps/web` rather than treated as a first-class top-level project.
2. **Cognitive Naming Ambiguity (`nuo-agent` vs `nous-agent`)**:
   Retaining `nuo-agent` alongside the `nous-agent` substrate created immediate confusion over ownership. In reality, `nuo-agent` does not implement generic cognitive loops; it implements host execution harness policies, human approval barriers, sandboxing, tool scheduling, and causal session compaction.
3. **The Runtime "Junk Drawer" (`nuo-runtime`)**:
   `nuo-runtime` grew into an uncurated junk drawer bundling server gateways (UDS/WebSocket), session dispatchers, slash command routers, and background task schedulers. Most critically, the real client communication implementation (`client.rs`) was housed inside `nuo-runtime`, reducing `nuo-client` to a hollow facade that leaked server internals into client dependencies.
4. **Platform Terminology Drift (`nuo-platform`)**:
   The label "platform" introduced severe ambiguity with developer APIs, cloud platforms, or third-party integrations, when the crate exclusively encapsulates OS-level system calls (processes, file locks, IPC, XDG paths).
5. **Cosmetic Symmetry Fallacy (`nuo-server` vs `nuo-client`)**:
   Creating an artificial `nuo-server` library crate solely to mirror `nuo-client` is a false symmetry. While `nuo-client` is an outward-facing SDK consumed by multiple frontends (`nuox`, `web`, headless tools), the server engine has exactly one consumer: the `nuo` daemon itself.

We must eliminate intermediate shims, resolve naming ambiguities, abolish junk drawers, and flatten the workspace topology with zero legacy burden.

---

## Decision Drivers

- **Occam's Razor & Zero Indirection**: Eliminate artificial directory nesting. Every deliverable and subsystem lives at the workspace root.
- **Uncompromised Architectural Purity**: Applications are first-class deliverables; libraries are single-responsibility building blocks.
- **Asymmetric Pragmatism**: Embrace real-world asymmetry—`nuo-client` is a genuine multi-consumer SDK, whereas the daemon server engine belongs wholly to the `nuo` application.
- **Elimination of "Junk Drawers"**: Drain `nuo-runtime` completely; relocate client transport to `nuo-client` and unify daemon server logic inside `nuo`.
- **Zero Legacy Burden**: Do not introduce backward-compatibility shims or deprecated aliases.

---

## Decision Outcome

1. **Flatten the Workspace Topology**:
   Abolish the `apps/` and `crates/` directories. All packages are hoisted directly to the workspace root:
   - **Applications (`[App]`)**: `nuo`, `nuox`, `web`.
   - **Core Libraries (`[Lib]`)**: `nuo-client`, `nuo-contracts`, `nuo-harness`, `nuo-persistence`, `nuo-providers`, `nuo-system`.

2. **Re-anchor `nuo` as the Unified Daemon Application (Dual-Target)**:
   - Merge `nuo-runtime` directly into `nuo`.
   - `nuo/src/main.rs` owns CLI argument parsing, detachment, and lifecycle control.
   - `nuo/src/lib.rs` owns the daemon server engine, UDS/WebSocket RPC gateway, session driver, and background jobs.
   - Decommission `nuo-runtime` completely.

3. **Establish `nuo-client` as a Genuine Independent SDK**:
   - Extract `client.rs` from the runtime and house all connection, discovery, and event streaming logic natively inside `nuo-client`.
   - `nuo-client` depends solely on `nuo-contracts` and `nuo-system`, with zero dependencies on `nuo`.

4. **Rename `nuo-agent` to `nuo-harness`**:
   - Rename `crates/nuo-agent` to `nuo-harness` to accurately reflect its role: the host execution harness providing human approval barriers, sandboxing, tool scheduling, and causal session compaction around the `nous` cognitive engine.

5. **Rename `nuo-platform` to `nuo-system`**:
   - Rename `crates/nuo-platform` to `nuo-system` to accurately communicate its role: operating system primitives (processes, IPC, XDG paths, secure file I/O, file locks).

6. **Workspace Manifest (`Cargo.toml`)**:
   ```toml
   [workspace]
   members = [
       "nuo",
       "nuox",
       "nuo-client",
       "nuo-contracts",
       "nuo-harness",
       "nuo-persistence",
       "nuo-providers",
       "nuo-system",
   ]
   default-members = ["nuo", "nuox"]
   ```

---

## Invariants & Behavioral Boundaries

- **`[INV-ARCH-FLAT-01] Flat Workspace Structure`**:
  All packages within the `nuo` repository must reside directly at the repository root. Nesting under `apps/` or `crates/` is prohibited.
- **`[INV-ARCH-CLIENT-01] Independent Client SDK`**:
  `nuo-client` must remain a standalone, lightweight SDK. It must never depend on `nuo` (server), and must only depend on `nuo-contracts` and `nuo-system`.
- **`[INV-ARCH-HARNESS-01] Harness Separation of Concerns`**:
  `nuo-harness` encapsulates host-specific approval policies, process execution guards, and round compaction. Generic cognitive loops are delegated to `nous-agent`.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 (Rejected: Create `nuo-server` Crate for Cosmetic Symmetry)
- **Why considered**: Symmetry with `nuo-client`.
- **Why rejected**: Violated Occam's razor. `nuo-server` had exactly one consumer (`nuo` daemon). Creating an artificial crate for a single private consumer causes crate sprawl and requires exporting unnecessary `pub` visibility.

### Option 2 (Rejected: Retain `apps/` and `crates/` Hierarchical Nesting)
- **Why considered**: Preserved traditional convention.
- **Why rejected**: Introduced superfluous directory traversal and created second-class status for `web`. A flat layout provides direct, zero-indirection navigation.

### Option 3 (Rejected: Rename `nuo-platform` to `nuo-os`)
- **Why considered**: Extremely short and punchy.
- **Why rejected**: Induced severe domain confusion in the AI ecosystem, misleading observers to think Nuo is an "AI Operating System". `nuo-system` precisely conveys host system primitives.

### Option 4 (Rejected: Retain `nuo-client` as a Re-export Facade)
- **Why considered**: Required zero refactoring of runtime files.
- **Why rejected**: Maintained a structural lie where client applications secretly pulled in the entire server runtime graph, invalidating architectural boundaries.

---

## Links

- Wire Alignment: [ADR-0001](0001-unify-model-wire-protocol-and-application-provider-boundary.md)
- Tool Contracts: [ADR-0002](0002-unify-tool-contracts-and-mcp-runtime-on-nous-substrate.md)
- Crate Granularity: [ADR-0004](0004-daemon-host-and-app-subsystem-topology.md)
