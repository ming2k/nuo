---
id: ADR-0009
title: "Tool Substrate Consolidation and Derive Macro Sibling Topology"
status: accepted
date: 2026-10-02
scope: workspace/tooling, substrate/derive, architecture/hygiene
superseded_by: null
negative_knowledge: true
---

# 0009. Tool Substrate Consolidation and Derive Macro Sibling Topology

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Runtime Team
- Informed: System Architects

---

## Context and Problem Statement

When the external `nous` codebase was ingested into Nuo (ADR-0006), three crates were mechanically imported: `nuo-tool`, `nuo-tool-derive`, and `nuo-tools`.

This mechanical import produced severe architectural ambiguity and smells:
1. **Singular/Plural Ambiguity**: Having both `nuo-tool` (singular) and `nuo-tools` (plural) at the workspace root caused immense confusion regarding responsibility boundaries.
2. **Dead Weight & Redundant Implementations**: Investigation revealed that `nuo-tools` contained duplicate file/search/todo tools that were completely unreferenced across the codebase. All real production tools (with permissions, sandboxing, and syntax verification) have always lived inside `nuo-harness/src/tools/`.
3. **Procedural Macro Isolation**: Rust requires procedural macros (`proc-macro = true`) to live in a dedicated compilation unit separate from regular structs and traits. However, exposing `nuo-tool-derive` as a general-purpose public dependency in `[workspace.dependencies]` polluted the root dependency surface when it was exclusively an auxiliary compilation companion to `nuo-tool`.

---

## Decision Drivers

- **Zero-Dead-Code Policy**: Remove all unreferenced crates and duplicate tool implementations to eliminate confusing singular/plural naming collisions.
- **Idiomatic Rust Proc-Macro Sibling Pattern**: Organize `nuo-tool-derive` as a flat sibling crate alongside `nuo-tool` (following the industry standard of `serde`/`serde_derive` and `tokio`/`tokio-macros`).
- **Encapsulated Dependency Invariant**: `nuo-tool-derive` must remain a private dependency of `nuo-tool` without being exposed as a shared dependency in root `[workspace.dependencies]`.

---

## Decision Outcome

1. **Delete Dead Crate `nuo-tools`**:
   - Completely remove the redundant `./nuo-tools` directory.
   - Remove `"nuo-tools"` from workspace members and dependencies.
   - Affirm `nuo-harness/src/tools/` as the sole canonical implementation surface for built-in developer tools.

2. **Establish Flat Sibling Topology for `nuo-tool-derive`**:
   - Maintain `nuo-tool-derive` as a flat sibling crate at `./nuo-tool-derive` alongside `./nuo-tool`.
   - `nuo-tool/Cargo.toml` consumes the derive macro via direct relative path:
     ```toml
     [dependencies]
     nuo-tool-derive = { path = "../nuo-tool-derive", version = "0.0.1" }
     ```
   - Re-export `ToolSchema` from `nuo-tool` (`pub use nuo_tool_derive::ToolSchema;`), so downstream consumers depend strictly on `nuo-tool`.
   - Remove `nuo-tool-derive` from root `[workspace.dependencies]`, keeping root public dependencies clean and domain-focused.

---

## Invariants & Validation Contracts

- **`[INV-TOOL-01] Canonical Tool Implementation Authority`**:
  All built-in system tools must be maintained inside `nuo-harness/src/tools/`. Redundant secondary tool utility crates must not be created.
- **`[INV-DERIVE-01] Proc-Macro Encapsulation`**:
  Procedural macro crates that exist solely to support a parent crate must be consumed directly by that parent crate and must not be exposed in root `[workspace.dependencies]`.

---

## Positive Consequences

- The confusing `nuo-tool` vs `nuo-tools` naming collision is permanently resolved.
- Workspace root members cleanly reflect real systems without dead weight.
- Follows canonical Rust open-source conventions (flat sibling macro crates with clean re-exports).

---

## Negative Knowledge & Rejected Alternatives

### 1. Retaining `nuo-tools` as a Secondary Library
- **Why considered**: Thought to provide "standalone tool implementations" for non-harness consumers.
- **Why rejected**: In reality, `nuo-tools` was 100% dead code, duplicated logic from `nuo-harness`, and lacked essential safety guards (permission policies, sandbox jail, and syntax checks).

### 2. Nesting `nuo-tool-derive` inside `nuo-tool/derive`
- **Why considered**: Maximizes directory nesting to hide the derive crate from repository root.
- **Why rejected**: Rejected in favor of the canonical Rust community convention (flat sibling crates ala `serde`/`serde_derive`), providing symmetric Cargo discovery while maintaining encapsulation through internal path resolution.
