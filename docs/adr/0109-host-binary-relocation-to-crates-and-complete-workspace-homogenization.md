---
id: ADR-0109
title: "Host Binary Relocation to crates/nuo and Complete Workspace Homogenization"
status: accepted
date: 2026-12-04
scope: workspace/topology, packaging/crates, host/nuo, architecture/enforcement
superseded_by: null
negative_knowledge: true
---

# 0109. Host Binary Relocation to `crates/nuo` and Complete Workspace Homogenization

- Status: Accepted
- Date: 2026-12-04
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0055](0055-hierarchical-workspace-topology-and-component-namespace.md)

---

## Context and Problem Statement

[ADR-0055](0055-hierarchical-workspace-topology-and-component-namespace.md) established the hierarchical workspace topology by moving 17 platform libraries into `crates/nuo-*`, while preserving dedicated extension namespaces (`providers/` and `tools/`). However, it retained the host executable crate at the repository root as `nuo/` under the rationale of "Single Application Executable Root" (`[INV-TOPO-01]`).

Over time and across extensive refactoring, keeping `nuo/` directly at the workspace root introduced structural defects:

1. **Topological Category Error & Asymmetry**:
   The repository root contained four directories: `crates/` (plural container), `providers/` (plural container), `tools/` (plural container), and `nuo/` (a single specific crate). A single crate residing as a peer to category containers is a category mismatch that clutters the root directory.
2. **Artificial Tooling Exceptions**:
   Because `nuo` sat outside `crates/`, every tool, script, and manifest had to encode special-case handling:
   - Root `Cargo.toml` required `members = ["nuo", "crates/*", "tools/*", "providers/*"]`, preventing a pure wildcard list.
   - The presentation dependency firewall required a custom `HOST_PATH = "nuo/Cargo.toml"` path variable and separate scanning logic.
   - CI workflows required custom target path exceptions (`nuo/Cargo.toml`).
3. **Application Identity Is a Cargo Contract, Not a Filesystem Location**:
   In modern Rust engineering (exemplified by industry standard monorepos such as Zed, Rust-Analyzer, and Typst), an executable crate's identity is defined by its `[[bin]]` target and `default-members = ["crates/nuo"]`, not by physical placement at the filesystem root.

We require an uncompromising, fully homogenized workspace topology where every crate uniformly resides within one of three flat tiers.

---

## Architectural Decisions

### 1. Relocation of `nuo` to `crates/nuo`

The sole public application binary crate is relocated from `nuo/` to `crates/nuo/`:
- `crates/nuo/Cargo.toml` owns the single `[[bin]]` target (`bin: "nuo"`).
- Root `Cargo.toml` sets `default-members = ["crates/nuo"]`.
- Root `Cargo.toml` members becomes purely wildcarded:
  ```toml
  [workspace]
  members = [
      "crates/*",
      "tools/*",
      "providers/*",
  ]
  default-members = ["crates/nuo"]
  ```

### 2. Pure Three-Tier Topology Invariant (`[INV-ARCH-TIER-01]`)

We establish **`[INV-ARCH-TIER-01]` Pure Tiered Workspace Topology** (amending and completing `[INV-TOPO-01..05]`):
1. **Exactly Three Tiers**:
   - `crates/` — Layers 1–4 core platform crates (contracts, runtime, presentation) and the Layer-4 host executable (`crates/nuo`).
   - `providers/` — Layer-3 model provider channel adapters (`nuo-provider-*`).
   - `tools/` — Layer-3 capability tool implementations (`nuo-tool-*`).
2. **Zero Root Crates**:
   No crate manifest may reside directly at the repository root. The root directory contains only workspace configuration (`Cargo.toml`), documentation, and tier directories.
3. **Flat Tier Guarantee**:
   No tier may nest deeper than `<tier>/<crate>/`.
4. **Mechanical Gate (R5)**:
   Enforced mechanically by `test_inv_arch_firewall_and_topology` rule R5 (ADR-0111) on every CI run.

---

## Consequences

### Positive
- **100% Homogeneous Topology**: Root directory contains strictly the three plural category containers (`crates/`, `providers/`, `tools/`).
- **Zero Special-Case Tooling**: Path discovery across CI and tools is simplified to `<tier>/*/Cargo.toml`.
- **Aligns with Modern Rust Standards**: Matches the monorepo architecture of major Rust systems projects.

### Negative / Neutral
- Paths referencing `nuo/Cargo.toml` in external release workflows must be updated to `crates/nuo/Cargo.toml`.
