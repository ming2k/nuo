---
id: ADR-0006
title: "Absorb Nous Substrate and Establish ACP Protocol Standard"
status: accepted
date: 2026-10-02
scope: workspace/substrate, comm/acp, tool/contracts, agent/hygiene
superseded_by: null
negative_knowledge: true
---

# 0006. Absorb Nous Substrate and Establish ACP Protocol Standard

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Runtime Team
- Informed: System Architects

---

## Context and Problem Statement

Initially, `nous` was conceived as a standalone multi-crate repository (`../nous`) intended to serve as a generic cognitive agent SDK. However, as `nuo` evolved into a production host daemon and rich semantic terminal, several architectural tensions emerged:

1. **The SDK Framework Trap**: Real-world agent hosts require host-tailored lifecycles, SQLite transactional persistence, interactive terminal diffing, and cooperative cancellation tokens. Attempting to generalize the cognitive loop into an external Rust library created artificial abstraction friction and glue code (such as bidirectional tool bridges).
2. **Multi-Repo Friction**: Maintaining `../nous` as a separate external workspace imposed unnecessary overhead: multi-repo git synchronization, redundant lockfile maintenance, and fragile relative path dependencies (`../nous/crates/...`).
3. **Protocol vs. Library Distinction**: Multi-agent collaboration does not require sharing a monolithic Rust binary library across projects; it fundamentally requires an open, interoperable **communication protocol standard** that any agent runtime (in Rust, Python, Go, or TypeScript) can implement natively.

We must absorb the cohesive substrate packages directly into Nuo's flattened workspace, elevate inter-agent communication into a recognized protocol standard (**ACP: Agent Coordination Protocol**), and eliminate multi-repository overhead.

---

## Decision Drivers

- **Protocol Over Implementation**: Inter-agent communication is an open protocol standard (**ACP**), not a vendor-locked library implementation.
- **Zero External Submodule Friction**: Absorb all foundational substrate crates directly into the top level of the workspace, completely eliminating cross-repository relative path dependencies.
- **Preservation of Clean Crate Boundaries**: Maintain the modular boundaries established by the substrate (`acp`, `nous-tool`, `nous-tool-derive`, `nous-tools`, `nous-mcp`, `nous-model-wire`, `nous-agent`).
- **Pruning Dead Dependencies**: Eliminate unreferenced workspace dependencies across crates to streamline the dependency DAG.

---

## Decision Outcome

1. **Establish ACP (Agent Coordination Protocol)**:
   - Rename `nous-comm` to `acp`.
   - Package `acp` as a clean, self-contained protocol crate at the workspace root (`acp/`).
   - Support standard agent URI addressing schemes (`agent://` and `acp://`).
   - Define canonical envelope formats, HMAC cryptographic verification, typed collaboration intents (`Delegate`, `Progress`, `Resolve`, `Reject`, `Steer`, `Signal`), and timeline collaborative fabrics.

2. **Absorb Substrate Crates into Workspace Root with Unified Nuo Naming**:
   - Hoist and align naming conventions: `nuo-tool`, `nuo-tool-derive`, `nuo-tools`, `nuo-mcp`, `nuo-model-wire`, and `nuo-agent` into the primary repository root.
   - Update `[workspace.dependencies]` in root `Cargo.toml` to reference local workspace paths.
   - Retire the external `/data/projects/nous` repository.

3. **Prune Dead Substrate Dependencies**:
   - Remove unused declarations of `nuo-agent`, `acp`, and `nuo-mcp` from `nuo-harness/Cargo.toml`.
   - Remove unused `acp` declaration from `nuo-mcp/Cargo.toml`.

---

## Invariants & Behavioral Boundaries

- **`[INV-ACP-01] Protocol Purity`**:
  The `acp` crate must remain completely free of host-application assumptions (no SQLite persistence, no terminal diffing, no interactive approval prompts). It defines pure protocol state machines, addressing, envelopes, and collaborative fabrics.
- **`[INV-WS-01] Self-Contained Workspace`**:
  All workspace crates and substrates must resolve within the repository without relying on external sibling path dependencies (consolidated with nuotc in ADR-0007).
- **`[INV-TOOL-01] Substrate Tool Compatibility`**:
  Canonical tool contracts (`nuo-tool`) and MCP clients (`nuo-mcp`) remain cohesive, shared building blocks within the workspace.

---

## Positive Consequences

- The entire agent substrate and application stack builds in a single, unified workspace with a single `Cargo.lock`.
- Cross-repo synchronization friction is permanently removed.
- `acp` stands as a professional, standard-aligned protocol specification ready for polyglot agent interoperability.

---

## Negative Knowledge & Rejected Alternatives

### Option 1 (Rejected: Retaining `../nous` as an External Repository)
- **Why considered**: Kept codebases physically isolated.
- **Why rejected**: Creates constant dependency synchronization friction, doubles CI build matrices, and hinders atomic refactoring across host and substrate.

### Option 2 (Rejected: Collapsing All Substrates into Monolithic `nuo-contracts`)
- **Why considered**: Would minimize the number of crates in the workspace.
- **Why rejected**: Collapsing `acp`, `nous-tool`, and `nous-model-wire` into a single monolithic crate violates separation of concerns. The clear modular boundaries designed in the substrate allow gateways or lightweight tools to consume only `acp` or `nous-model-wire` without pulling the entire application surface.
