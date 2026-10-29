---
id: ADR-0111
title: "Elimination of scripts/ Directory and Consolidation of Workspace Governance into Native Rust"
status: accepted
date: 2026-12-06
scope: workspace, governance, testing, cli/dev
superseded_by: null
negative_knowledge: true
---

# 0111. Elimination of scripts/ Directory and Consolidation of Workspace Governance into Native Rust

- Status: Accepted
- Date: 2026-12-06
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0105](0105-presentation-linkage-mechanism-and-dependency-firewall.md), [ADR-0109](0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md)

---

## Context and Problem Statement

Following ADR-0109, Nuo established the pure three-tier workspace topology (`[INV-ARCH-TIER-01]`), requiring all code to reside exclusively within `crates/*`, `providers/*`, and `tools/*`. However, mechanical enforcement of this topology and associated dependency firewalls (ADR-0105) was historically implemented via ad-hoc Python 3 scripts in a root `scripts/` directory:
- `scripts/check_presentation_firewall.py` (Topology & dependency firewall: R1–R5)
- `scripts/check_doc_links.py` (Markdown link and audience firewall verification)
- `scripts/check_adr_lineage.py` (ADR citation resolution verification)

This configuration introduced severe architectural and operational liabilities:
1. **Toolchain Contamination and Ergonomic Friction**: Requiring a secondary Python 3 runtime in an otherwise pure, high-assurance Rust monorepo fractured developer workflows and broke the "All-in-Cargo" mental model.
2. **Topology Inconsistency**: While ADR-0109 eliminated root crates, the untiered `scripts/` directory persisted at the root of the workspace.
3. **Lexical Fragility**: Regex-based Python scripts parsing Rust comments and TOML manifests were prone to edge-case parsing inaccuracies and lacked compiler type safety.
4. **CI Fragmentation**: Workspace gates were split across Python scripts, `cargo test`, `cargo doc`, and inline shell pipelines in CI workflows.

## Decision Drivers

- Driver 1: Achieve 100% single-language toolchain purity (Rust) across compilation, testing, and workspace governance.
- Driver 2: Completely eradicate the root `scripts/` directory, aligning physical workspace reality with `[INV-ARCH-TIER-01]`.
- Driver 3: Strengthen mechanical enforcement of architectural red-lines by embedding them as native, typed integration tests and first-class developer CLI commands.

## Architectural Decisions

### 1. Complete Elimination of `scripts/` Directory

The `scripts/` directory is permanently deleted. Python bytecode patterns (`__pycache__/`, `*.pyc`) are removed from `.gitignore`. No Python runtime is required for any CI or local verification gate.

### 2. Native Rust Governance Engine (`crates/nuo/src/dev`)

All verification logic is reimplemented in pure Rust within `crates/nuo/src/dev/`:
- `nuo::dev::firewall`: Mechanically enforces R1 (Server purity), R2 (Anti-inversion & retired crate isolation), R3 (Zero-IO presentation substrates), R4 (UI primitives purity), R5 (Pure three-tier layout), and Lean CLI Link (`[INV-ARCH-CLI-LEAN-LINK]`).
- `nuo::dev::doc_links`: Mechanically enforces relative link existence (`style.md` §1.3), prohibition of absolute locators (`style.md` §1.2), heading anchor resolution (`style.md` §1.4), and audience firewall protection (`taxonomy.md` §2).
- `nuo::dev::adr_lineage`: Mechanically verifies that every `ADR-NNNN` reference resolves to a canonical document in `docs/adr/`, honoring the immutable SQL allowlist for shipped migrations (`docs/dev/standards/adr.md` §4).

### 3. Native Integration Test Targets

Verification gates are declared as standard Cargo integration tests in `crates/nuo/tests/`:
- `crates/nuo/tests/architecture_firewall.rs`: Executes `test_inv_arch_firewall_and_topology`, `test_inv_arch_cli_lean_link`, and `test_inv_arch_source_residency`.
- `crates/nuo/tests/doc_governance.rs`: Executes `test_doc_links` and `test_adr_lineage`.

These run automatically as part of `cargo test --workspace` and can be invoked individually via `cargo test -p nuo --test <name>`.

The residency gate is specified by [ADR-0124](0124-source-residency-every-rust-file-reachable-from-a-crate-root.md), which establishes `[INV-ARCH-RESIDENCY-01]`: every `.rs` file under a tier root must be reachable from a crate root by `mod` declarations.

### 4. First-Class Developer CLI Commands

The `nuo dev` command family (ADR-0073) is expanded to support offline verification:
```bash
nuo dev check all        # Run all gates
nuo dev check firewall   # Run architecture firewall and lean CLI link check
nuo dev check docs       # Run documentation hyperlink and audience check
nuo dev check adr        # Run ADR reference lineage check
nuo dev check residency  # Run Rust source reachability check (ADR-0124)
```

### 5. Harmonized CI Workflow

In `.github/workflows/ci.yml`, the legacy `python3 scripts/...` steps and inline bash grep scripts are replaced with native `cargo test` invocations.

---

## Consequences

### Positive
- **Zero Polyglot Overhead**: The repository is 100% Rust. Development and CI environments require zero external scripting interpreters.
- **Topology Purity**: Root directory contains strictly configuration (`Cargo.toml`), docs (`docs/`), and the three sanctioned tiers (`crates/`, `providers/`, `tools/`).
- **Unified Testing Model**: All architectural and documentation invariants run within the standard Rust test harness, producing standard diagnostic output and timing metrics.

### Negative & Trade-offs
- Integration tests compile into `target/debug/deps/`, incurring a one-time build step during testing, though incremental build times remain negligible.

---

## Rejected Alternatives & Negative Knowledge

### Alternative A: Moving Scripts into `tools/scripts/` (Rejected)
- **Why considered**: Keeps Python scripts while hiding them in a subfolder.
- **Why rejected**: Violates ADR-0109 (`tools/` contains only `nuo-tool-*` agent plugins) and preserves Python runtime dependency.

### Alternative B: External Standalone `xtask` Root Crate (Rejected)
- **Why considered**: Common pattern in some open-source Rust repositories.
- **Why rejected**: Violates `[INV-ARCH-TIER-01]` by introducing a fourth root directory or non-standard crate naming, and duplicates dependencies already linked by `crates/nuo`.

### Alternative C: Relying Purely on Manual Code Review (Rejected)
- **Why rejected**: Violates core project invariant `[INV-ARCH-INVARIANTS]` and "Mechanisms Over Exhortations" (`docs/dev/standards/invariants.md` §1). Unenforced conventions rot within weeks across active development.

## Links

- Amends:
  [ADR-0105](0105-presentation-linkage-mechanism-and-dependency-firewall.md),
  [ADR-0109](0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md)
