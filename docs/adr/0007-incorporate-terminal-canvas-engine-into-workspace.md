---
id: ADR-0007
title: "Incorporate Terminal Canvas Engine (Nuotc) into Self-Contained Workspace"
status: accepted
date: 2026-10-02
scope: workspace/topology, terminal/nuotc, ui/engine
superseded_by: null
negative_knowledge: true
---

# 0007. Incorporate Terminal Canvas Engine (Nuotc) into Self-Contained Workspace

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Terminal UI Team
- Informed: System Architects

---

## Context and Problem Statement

In ADR-0004, the terminal rendering engine was extracted into an external standalone repository at `../nuotc` with zero AI domain vocabulary. In ADR-0006, when all agent substrate crates (`acp`, `nuo-tool`, `nuo-model-wire`, etc.) were consolidated into the monorepo, a pragmatic compromise was retained in `[INV-WS-01]`:
> *"All workspace crates and substrates must resolve within the repository without relying on external sibling path dependencies (except standalone system canvas `nuotc`)."*

However, maintaining `nuotc` as an external repository while `nuox` was its sole active consumer created substantial operational and engineering friction:
1. **Broken Hermetic Builds**: A clean clone of `nuo` in CI runners, container builds, or new developer environments failed immediately with `failed to read /data/projects/nuotc/Cargo.toml` due to the sibling path dependency `nuotc = { path = "../nuotc" }`.
2. **Double-Commit & Broken Atomicity**: During active evolution (such as the recent restructuring of `buffer`, `layout`, `render`, `terminal`, `widgets`, and `ui` subsystems), changing a layout primitive or terminal profile in `nuotc` required non-atomic cross-repo commits, preventing single-commit verification against `nuox`'s 1,200+ regression and UI snapshot tests.
3. **Crate vs. Repository Equivocation**: In Rust, physical dependency and compilation firewalls are enforced at the **crate** boundary, not by the Git repository boundary. An in-tree crate with zero internal dependencies preserves full architectural purity without multi-repo tax.

---

## Decision Drivers

- **Zero-Friction Hermetic Builds**: Every developer, container, and CI runner must be able to `git clone` and immediately execute `cargo build --workspace` without implicit sibling directory requirements.
- **Strict Invariant Restoration (`[INV-WS-01]`)**: Fully eliminate the temporary exception from `[INV-WS-01]` and restore complete self-containment for the repository.
- **Atomic Refactoring & Comprehensive Snapshot Testing**: UI component additions, flexbox enhancements, and terminal driver optimizations must be testable and committable atomically alongside `nuox`.
- **Preserved Domain Purity**: `nuotc` must remain completely decoupled from AI agent semantics, tool definitions, persistence schemas, or network protocols.
- **Independent Publishability**: `nuotc` retains its own package manifest, versioning, documentation, and MIT license, and remains ready for independent publishing to `crates.io` at any time via `cargo publish -p nuotc`.

---

## Decision Outcome

1. **Ingest `nuotc` into Workspace Root (`nuotc/`)**:
   - Relocate the complete `nuotc` codebase, tests, README, and architecture blueprint into `./nuotc`.
   - Incorporate `"nuotc"` into root `[workspace] members`.
   - Update `[workspace.dependencies]` to resolve `nuotc = { path = "nuotc" }`.
   - Standardize `nuotc/Cargo.toml` with workspace inherited metadata (`version.workspace = true`, `edition.workspace = true`, `license.workspace = true`) and hoist `bitflags = "2"` to workspace dependencies.

2. **Restore Strict Workspace Invariant (`[INV-WS-01]`)**:
   - Remove the `(except standalone system canvas nuotc)` exception.
   - The workspace is now 100% self-contained: all crates compile from clean checkouts without sibling path dependencies.

3. **Subsystem Documentation Ingestion**:
   - Import the 6-subsystem architecture living blueprint into `docs/architecture/subsystems.md`.

---

## Invariants & Behavioral Boundaries

- **`[INV-WS-01] Self-Contained Workspace`**:
  All workspace crates and substrates must resolve strictly within the repository without relying on any external sibling path dependencies.
- **`[INV-NUOTC-01] Zero Domain Vocabulary`**:
  The `nuotc` crate must remain completely free of application-level domain knowledge (no agent loops, no token counters, no prompt schemas, no SQLite models). It knows only cells, grids, flexbox solvers, escape drivers, and declarative TUI primitives.

---

## Positive Consequences

- `cargo check --workspace` and `cargo test --workspace` execute hermetically in single checkouts.
- Full atomic refactorings between `nuotc` rendering primitives and `nuox` client views are enabled.
- CI pipelines require only a single checkout step with zero multi-repo synchronization scripts.
- `nuotc` remains completely eligible for standalone publishing to crates.io (`cargo publish -p nuotc`).

---

## Negative Knowledge & Rejected Alternatives

### Option 1 (Rejected: Keep `nuotc` as a Separate Git Repo with Sibling Path Dependency)
- **Why considered**: Kept terminal canvas physically separated in git log.
- **Why rejected**: Directly breaks CI and developer onboarding unless every consumer manually clones `nuotc` side-by-side. Creates constant dual-repo synchronization overhead during active 0.x iteration.

### Option 2 (Rejected: Keep `nuotc` as an External Git Dependency `git = "..."`)
- **Why considered**: Solves the clean checkout problem by having Cargo pull from GitHub.
- **Why rejected**: Severely degrades local development iteration velocity. Any local tweak to the rendering engine requires pushing to remote, bumping git revisions, and cache clearing before testing in `nuox`.

### Option 3 (Rejected: Git Submodule at `nuotc/`)
- **Why considered**: Enables monorepo layout while pointing to an external repository.
- **Why rejected**: Git submodules introduce detached HEAD issues, submodule pointer sync churn, and complex branching workflows for negligible benefit given that `nuox` is currently the primary co-evolving driver of `nuotc`.
