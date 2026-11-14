---
id: ADR-0187
title: "Documentation Governance Is Mechanical: ADR Identity, Frontmatter, and Links"
status: accepted
date: 2026-11-14
scope: docs, governance, ci
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0187. Documentation Governance Is Mechanical: ADR Identity, Frontmatter, and Links

- Status: Accepted
- Date: 2026-11-14
- Amends: [ADR-0111](0111-elimination-of-scripts-and-consolidation-of-workspace-governance-into-rust.md)

## Context and Problem Statement

The repository treats ADRs and invariants as binding architecture, but the
documentation registry has no mechanical gate comparable to the code
firewalls:

- Two accepted ADRs share the number **0178**.
- ADR-0159 through ADR-0169 (11 files) lack the mandatory YAML frontmatter.
- 17 relative Markdown links in `docs/` point at files that do not exist.
- Living documents still name retired crates (`nuo-harness`, `nuo-tui`,
  `nuo-client`, `nuo-model-codec`, `nuo-catalog`).
- The root `Cargo.toml` comment cites `crates/nuo/tests/test_architecture.rs`,
  a path deleted by the ADR-0178 test migration.
- `cargo doc` checks Rust intra-doc links only; it does not read ADR Markdown.

Documentation drift is architecture drift when the documentation is the
authority for boundaries.

## Architectural Decisions

### 1. ADR identity is unique and complete

Every `docs/adr/*.md` file must have YAML frontmatter with:

- a unique `id: ADR-NNNN` matching the filename prefix;
- `title`, `status`, `date`, `scope`, `superseded_by`, `negative_knowledge`;
- a `status: superseded` file must name its successor.

### 2. ADR references and relative links resolve

Every `ADR-NNNN` reference in docs and source comments must resolve to exactly
one `docs/adr/NNNN-*.md`. Every relative Markdown link in `docs/` must resolve
to a file on disk.

### 3. Living docs use current crate names

`README.md`, `README-zh.md`, `docs/explanation/**`, `docs/dev/**`,
`docs/how-to/**`, `docs/reference/**`, and `docs/tutorials/**` must not name a
retired crate as a current crate. Historical ADRs may keep the names because
they describe the state at their date.

### 4. The gate is native Rust

`nuo::dev::docs` implements the checks, and
`tests/integration/governance/architecture_firewall.rs` runs them. The project
already replaced Python checkers with native Rust governance (ADR-0111); this
extends that rule to documentation.

### 5. Invariants

- **`[INV-DOC-01]` Unique ADR Identity**: Duplicate or missing frontmatter
  fails the gate.
- **`[INV-DOC-02]` Resolvable Citation**: A dangling ADR reference or relative
  link fails the gate.
- **`[INV-DOC-03]` Current Vocabulary**: A living document that names a
  retired crate fails the gate.

## Consequences & Trade-offs

- **Positive**: The ADR registry becomes trustworthy; a renamed crate cannot
  leave living docs behind; citations are inspectable.
- **Negative & Incurred Costs**: The current 0178 duplicate, missing
  frontmatter, and broken links are fixed in this change. Future ADRs pay a
  small frontmatter cost.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — A Markdown linter in CI
- **Why considered**: Off-the-shelf link checking.
- **Why rejected**: It cannot enforce ADR identity, frontmatter schema, or the
  retired-vocabulary rule. A native checker reuses the workspace's tier and
  path knowledge.

### Alternative B — Manually maintained `docs/adr/index.md`
- **Why considered**: A human-readable registry.
- **Why rejected**: ADR standards explicitly reject a second index; the flat
  self-indexing directory is the registry (`docs/dev/standards/adr.md` §2).

## Links

- [ADR-0111](0111-elimination-of-scripts-and-consolidation-of-workspace-governance-into-rust.md)
- [ADR-0178](0178-layered-test-architecture-and-hermetic-e2e-boundary.md)
