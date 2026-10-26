---
id: ADR-0107
title: "Two-Tier Presentation Purification: Decoupling Pure UI Primitives, Domain Views, and Retirement of nuo-present"
status: accepted
date: 2026-12-03
scope: presentation/nuo-ui-primitives, presentation/nuo-view, presentation/nuo-present, host/nuo, workspace/topology, architecture/enforcement
superseded_by: null
negative_knowledge: true
---

# 0107. Two-Tier Presentation Purification: Decoupling Pure UI Primitives, Domain Views, and Retirement of `nuo-present`

- Status: Accepted
- Date: 2026-12-03
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md), [ADR-0105](0105-presentation-linkage-mechanism-and-dependency-firewall.md), [ADR-0106](0106-final-presentation-topology-nuo-present-and-nuo-shell.md)

---

## Context and Problem Statement

[ADR-0106](0106-final-presentation-topology-nuo-present-and-nuo-shell.md) consolidated all presentation code into a single library crate, `crates/nuo-present`, alongside the interactive terminal shell in `nuo::shell`. While this achieved the critical milestone of zero-IO isolation (`[INV-ARCH-UI-ZERO-IO]`), it left severe architectural defects:

1. **Semantic Category Error in Nomenclature (`nuo-present`)**:
   "Present" is a polysemous word (adjective: current/attending; noun: gift/now; verb: to exhibit). In an AI Agent system with session history, turn checkpoints, and timeline forks (ADR-0058/ADR-0104), `nuo-present` creates severe cognitive friction, falsely suggesting "the present/current state of Nuo" rather than the Presentation layer.
2. **Junk Drawer Anti-Pattern and Level-of-Abstraction Mismatch**:
   `crates/nuo-present` bundled three fundamentally distinct concerns into one heterogeneous container:
   - *Layer 3a: Pure UI Primitives & Design System*: theme tokens, geometric alignment, driver-free key chords, and reusable UI components (`modal`, `dropdown`, `toast`, `popover`, `footer`).
   - *Layer 3b: Domain Presentation & Agent Views*: agent transcript document models (`model/document`), tool execution cards (`tools/diff`, `tools/mcp`), conversational layout bands, and permission trust gates.
   - *Pure Algorithm Utilities*: fuzzy search, syntax highlighting, and time formatting.
   A generic UI component should never know what an "MCP Tool Call" or an "Agent Round" is.
3. **Hidden Cyclic Seams Across Abstraction Boundaries**:
   Because domain models and generic components resided in the same crate, seams like `components/notice.rs` and `model/document.rs` cyclically depended on each other.

We require a principled, permanent, two-tier architecture that completely segregates generic UI primitives from domain-specific agent presentation.

---

## Architectural Decisions

### 1. Bifurcation of Presentation into Two Pure Crate Tiers

We permanently replace `crates/nuo-present` with two strictly layered, zero-IO library crates:

```text
┌────────────────────────────────────────────────────────────────────────┐
│ Layer 4: Host Binary Runtime (`nuo::shell`)                            │
│ - Terminal Raw Mode, Signal Trapping, Tokio Event Loop, IPC Client     │
├────────────────────────────────────────────────────────────────────────┤
│ Layer 3b: Agent Domain Presentation (`crates/nuo-view`)                │
│ - Transcript Document & Selection Models (`model/*`)                   │
│ - Agent Tool Presenters (`tools/*`: diff, streaming, exec, mcp...)     │
│ - Conversational Layout & Message Body (`layout/*`, `message_body.rs`) │
│ - Agent-Specific Views: `notice`, `selectable_body`, `trust_gate`      │
│ - Strictly Zero-IO (`[INV-ARCH-UI-ZERO-IO]`)                           │
├────────────────────────────────────────────────────────────────────────┤
│ Layer 3a: Pure UI Primitives & Design System                           │
│           (`crates/nuo-ui-primitives`)                                 │
│ - Design Tokens: `theme`, `design`, `elevation`                        │
│ - Semantic Driver-Free Key Chords: `chord`, `key`, `keyvocab`          │
│ - Universal UI Components: `modal`, `dropdown`, `popover`, `toast`,    │
│                            `footer`, `keycap`, `detail`, `options`...  │
│ - Viewport Geometry & Text Wrap: `primitives`, `text_layout`           │
│ - Pure Utilities: `fuzzy`, `syntax`, `time`                            │
│ - Zero domain dependencies, Zero I/O (`[INV-ARCH-PRIMITIVES-ZERO-IO]`) │
├────────────────────────────────────────────────────────────────────────┤
│ Canvas Substrate: `nuotc`                                              │
└────────────────────────────────────────────────────────────────────────┘
```

### 2. Dependency Direction and Firewall Rules

1. **`nuo-ui-primitives` is Domain-Agnostic**:
   `crates/nuo-ui-primitives` MUST NOT depend on `nuo-view`, `nuo-protocol`, `nuo-harness`, or any domain/runtime crate. It is a general-purpose, reusable terminal UI primitives library.
2. **Strict Unidirectional Flow**:
   `nuo-view` depends downward on `nuo-ui-primitives` and Layer-1 contracts (`nuo-protocol`, `nuotc`). It NEVER depends upward on `nuo::shell` or Layer-2 runtimes (`nuo-server`, `nuo-harness`).
3. **Zero-IO Invariant Maintained**:
   Both `nuo-ui-primitives` and `nuo-view` operate exclusively in pure memory as $f(\text{Props}, \text{State}, \text{Theme}) \to \text{RenderOutput}$. Neither links network, subprocess, filesystem sockets, or terminal raw-mode drivers.

### 3. Permanent Retirement of `crates/nuo-present`

- `crates/nuo-present` is permanently deleted.
- `nuo-present` is added to `RETIRED_CRATES` in the architecture firewall (ADR-0111).
- Any workspace crate depending on `nuo-present` fails CI build-time checks.

---

## Normative Invariants Established

- **`[INV-ARCH-UI-PRIMITIVES-PURITY]`**: `crates/nuo-ui-primitives` must have zero compile-time dependencies on any agent, model, server, or transport subsystem.
- **`[INV-ARCH-VIEW-ZERO-IO]`**: `crates/nuo-view` must have zero compile-time dependencies on async I/O runtimes, network sockets, or raw terminal drivers.
