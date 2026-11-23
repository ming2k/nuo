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
`docs/how-to/**`, `docs/reference/**`, `docs/tutorials/**`, and `docs/rfc/**`
must not name a retired crate as a current crate. An RFC is a design proposal
about the present tree, not a dated record of it, so its pointers must resolve
too. Historical ADRs may keep the names because they describe the state at their
date.

A line may still name a retired crate when it marks the reference as history
(`former`, `retired`, `renamed`, `consolidated from`, `predecessor`, `replaced`,
`deleted`, `pre-consolidation`). Two names are always current: `nuo-tests` (the
root dev-only test host) and `target/nuo-dev` (the debug sandbox directory).

### 4. The gate is native Rust

`tests/integration/governance/architecture_convergence.rs` owns the checks:
`adr_registry_has_unique_identity_and_valid_frontmatter`,
`adr_frontmatter_values_are_well_formed`,
`adr_invariant_tags_have_one_definition_site`,
`adr_negative_knowledge_flag_is_truthful`, `adr_references_resolve`, and
`markdown_relative_links_resolve`. `nuo::dev` owns only the workspace-root helper
and the wire census; there is no `nuo::dev::docs` module. The project already
replaced Python checkers with native Rust governance (ADR-0111); this extends
that rule to documentation.

### 5. Invariants

- **`[INV-DOC-01]` Unique ADR Identity**: Duplicate or missing frontmatter, an
  invalid status/date, or a `superseded_by` that does not resolve fails the gate.
- **`[INV-DOC-02]` Resolvable Citation**: A dangling `ADR-NNNN` reference or
  relative Markdown link fails the gate.
- **`[INV-DOC-03]` Current Vocabulary**: A living document that names a
  retired crate fails the gate.
- **`[INV-DOC-04]` One Invariant Definition**: A tag defined by two unmarked
  bullets fails the gate. `restates` / `revises` / `reaffirms` / `revokes`
  bullets keep the tag and are not definitions.
- **`[INV-DOC-05]` The Negative-Knowledge Flag Is Falsifiable**:
  `negative_knowledge: true` without an alternatives or negative-knowledge
  section fails the gate; an ADR that records no rejected alternative sets it to
  `false`.

### 6. The one-time invariant cleanup

The invariant registry had 40 tags with more than one definition site; 26 of the
later definitions were genuinely different rules sharing a number. The gate could
not be added without resolving them, so the same change gives each distinct rule
its own tag, marks the genuine restatements/revisions, and updates every citation
that meant the newer rule. This is a correction, not a renumbering of history: the older
tag keeps the rule it always named; the newer rule gets the tag it should have
had.

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
