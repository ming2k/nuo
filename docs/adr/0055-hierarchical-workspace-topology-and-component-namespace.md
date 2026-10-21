---
id: ADR-0055
title: "Hierarchical Workspace Topology, Component Library Namespace, and Root Cleanliness"
status: accepted
date: 2026-10-28
scope: workspace/topology, architecture/layering, packaging/crates
superseded_by: null
negative_knowledge: true
---

# 0055. Hierarchical Workspace Topology, Component Library Namespace, and Root Cleanliness

- Status: Accepted
- Date: 2026-10-28
- Deciders: Nuo Architecture Working Group, Infrastructure & Release Engineering
- Consulted: Core Contributors, Runtime & Tooling Teams
- Informed: System Architects, Documentation Governance
- Supersedes: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) §1 (Flat Workspace Topology)
- Complements: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md)

---

## Context and Problem Statement

ADR-0001 historically mandated a flat workspace topology where all crates resided directly at the repository root to avoid deep directory nesting. However, as the system grew from an initial prototype into an enterprise-grade agent platform with 39 crates, the repository topology drifted into a disjointed "half-flat, half-nested" state:

1. **Root Directory Clutter & Cognitive Overload**: 18 core and substrate crates (`nuo`, `nuo-server`, `nuo-tui`, `nuo-wire`, `nuo-agent`, `nuo-oauth`, etc.) directly crowded the repository root alongside project metadata, governance specifications, and build manifests. Navigating the top level was noisy and obscure.
2. **Topological Asymmetry**: Ecosystem extensions were systematically categorized into `providers/` (17 model adapters per ADR-0015/ADR-0027) and `tools/` (4 capability tools per ADR-0007/ADR-0012), yet core platform components lacked an equivalent dedicated namespace and spilled into the root.
3. **Application vs. Library Boundary Confusion**: While ADR-0005 and ADR-0011 established the **Unified Binary Singleton** (`[INV-CLI-01]`: `nuo` as the single executable entry point) and affirmed that `nuo-server` (composition root runtime) and `nuo-tui` (presentation view library) are pure libraries (`[INV-TUI-01]`), their physical co-location at root obscured this architectural fact.

We require an uncompromising, future-facing workspace topology that scales gracefully to 100+ crates, establishes clear architectural boundaries between the single application host, internal component libraries, and extension ecosystems, and maintains absolute root cleanliness.

---

## Decision Drivers

- **Single Application Delivery Purity**: Emphasize that `nuo` is the single public application executable in the monorepo (`[INV-CLI-01]`).
- **Encapsulated Component Libraries**: Consolidate all internal substrate, protocol, engine, and view crates into a standard `crates/nuo-*` namespace.
- **Ecosystem Namespace Symmetry**: Preserve dedicated top-level extension namespaces (`providers/` and `tools/`) as first-class, independently expandable leaf modules.
- **Root Directory Cleanliness**: Maintain zero library crates at the repository root, reserving root strictly for the primary executable, configuration manifests, and documentation.
- **Declarative Cargo Wildcarding**: Modernize `Cargo.toml` workspace members using standard declarative glob tiers (`nuo`, `crates/*`, `tools/*`, `providers/*`).

---

## Decision Outcome

We establish a tiered, hierarchical workspace topology:

```text
nuo-repo/
├── nuo/                         # [INV-TOPO-01] Sole application binary host
│   ├── src/main.rs              # Single executable entry point ([INV-CLI-01])
│   └── Cargo.toml
│
├── crates/                      # [INV-TOPO-02] Component & substrate libraries
│   ├── nuo-wire/                # Canonical wire contracts and event envelopes
│   ├── nuo-acp/                 # Agent Coordination Protocol
│   ├── nuo-agent/               # Autonomous cognitive ReAct runtime
│   ├── nuo-host/                # Process abstraction, sandboxing & OS PAL
│   ├── nuo-server/              # Headless session runtime & composition root
│   ├── nuo-tui/                 # Terminal presentation view library ([INV-TUI-01])
│   ├── nuo-client/              # Standalone IPC/WebSocket client SDK
│   ├── nuo-provider/            # Canonical model provider contracts
│   ├── nuo-tool/                # Tool abstraction, schema & execution contracts
│   ├── nuo-tool-derive/         # Compile-time JSON schema derive macros
│   ├── nuo-oauth/               # RFC 7636/8628 authentication engine
│   ├── nuo-provider-transport/  # HTTP egress, SSE demux & retry substrate
│   ├── nuo-model-codec/         # Multi-vendor dialect serialization
│   ├── nuo-mcp/                 # Model Context Protocol stdio/JSON-RPC engine
│   ├── nuo-persistence/         # SQLite transactional store & state memory
│   └── nuo-harness/             # Policy governor, human checkpoints & sandboxing
│
├── providers/                   # [INV-TOPO-03] Model provider channel adapters
│   ├── nuo-provider-anthropic/
│   ├── nuo-provider-openai/
│   └── ... (17 concrete adapters)
│
├── tools/                       # [INV-TOPO-03] Agent tool implementations
│   ├── nuo-tool-fs/
│   ├── nuo-tool-exec/
│   ├── nuo-tool-web/
│   └── nuo-tool-ast/
│
├── docs/                        # Architecture & governance blueprints
├── Cargo.toml                   # Root workspace manifest with glob members
└── AGENTS.md
```

### Invariants & Behavioral Boundaries

- **`[INV-TOPO-01] Single Application Executable Root`**: `nuo` is the sole top-level application directory and owns the only public binary entry point (`[[bin]]`) across the workspace.
- **`[INV-TOPO-02] Component Library Namespace`**: All internal shared platform, substrate, engine, and view libraries MUST reside strictly in `crates/nuo-*`.
- **`[INV-TOPO-03] Ecosystem Namespace Integrity`**: Model provider adapters MUST reside in `providers/nuo-provider-*` (`[INV-PROV-02]`), and decoupled agent tool capabilities MUST reside in `tools/nuo-tool-*` (`[INV-EXT-01]`).
- **`[INV-TOPO-04] Root Cleanliness`**: No library crates may reside directly at repository root. Root is reserved exclusively for the `nuo` application, workspace manifests, and governance metadata.
- **`[INV-TOPO-05] Workspace Members Wildcarding`**: Root `Cargo.toml` declares members using glob patterns (`members = ["nuo", "crates/*", "tools/*", "providers/*"]`), enabling zero-touch workspace admission for new extensions.

---

## Rejected Alternatives

### 1. Completely Flat Root (Historical ADR-0001 status quo)
- **Why considered**: Avoids any directory hierarchy; keeps every crate 1 level deep.
- **Why rejected**: Proved unsustainable at 39 crates. With `providers/` and `tools/` already partitioned into subdirectories, keeping the remaining 18 crates at root created topological inconsistency, cognitive noise, and directory pollution.

### 2. Monolithic Flat `crates/` Dump
- **Why considered**: Standard pattern in smaller Rust crates where every crate is placed inside `crates/`.
- **Why rejected**: Dumping all 39 crates (and 60+ future provider/tool crates) into a single flat `crates/` folder drowns core architecture in voluminous peripheral plugins. Nuo is a platform with an open-ended extension ecosystem; keeping `providers/` and `tools/` as dedicated namespaces protects platform clarity.

### 3. Artificial `apps/` Directory Layer
- **Why considered**: Common monorepo convention creating `apps/` and `crates/`.
- **Why rejected**: Nuo is governed by `[INV-CLI-01] Unified Binary Singleton`—there is only **one** executable product (`nuo`). `nuo-server` and `nuo-tui` are libraries, not standalone deployed applications. An `apps/` directory containing a solitary `nuo` directory introduces needless nesting depth without semantic value.
