---
id: ADR-0181
title: "Cognitive Runtime Identity: nuo-engine Is the Only Crate Name"
status: accepted
date: 2026-11-14
scope: engine, server, cli, tests, documentation
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0181. Cognitive Runtime Identity: nuo-engine Is the Only Crate Name

- Status: Accepted
- Date: 2026-11-14
- Amends: [ADR-0100](0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md), [ADR-0101](0101-runtime-internal-layering-and-governance-inner-layer.md)

## Context and Problem Statement

`crates/nuo-harness` was renamed to `crates/nuo-engine` during the 0.0.84
split, but the rename was mechanical only:

- `nuo-engine/README.md` is still titled `# nuo-harness` and describes a
  retired `nuo-tui` frontend.
- `nuo-engine` declares `extern crate self as nuo_harness`; `nuo-server`,
  `nuo`, and the integration entry each declare an external alias.
- 137 code references still say `nuo_harness`; 128 of them are in
  `nuo-server`.
- 127 doc comments in the engine and server describe a `nuo-harness` crate.
- The engine's `governance/` module is not a module at all: it is a re-export
  of the `nuo-guardrails` crate.

Two names for one crate make every boundary discussion ambiguous: a reader
cannot tell whether "harness" means the crate, the runtime role, or the
governance layer.

## Architectural Decisions

### 1. The crate name is `nuo-engine`

`nuo-engine` is the sole name of the cognitive runtime crate. `nuo-harness` is
retired as a crate identifier. The word "harness" may appear in prose only when
it names the runtime role, never as a code path or package.

### 2. All aliases are removed

The `extern crate self as nuo_harness` and `extern crate nuo_engine as
nuo_harness` declarations are deleted, and every `nuo_harness::` path is
rewritten to `nuo_engine::`. The integration test entry gets the same treatment.

### 3. The governance alias is removed

`nuo-engine` no longer re-exports `nuo_guardrails as governance`. Engine code
imports `nuo_guardrails` directly; consumers that need guardrail types import
`nuo_guardrails` directly. The engine keeps only its own public API.

### 4. Documentation is part of the decision

`crates/nuo-engine/README.md`, `docs/dev/subsystems.md`,
`docs/explanation/architecture-overview.md`, the root READMEs, and every module
doc that names the crate are updated in the same change.

### 5. Invariants

- **`[INV-ENGINE-01]` One Name**: No source file may contain the identifier
  `nuo_harness`.
- **`[INV-ENGINE-02]` No Alias**: No `extern crate ... as ...` alias may
  rename a workspace crate.

## Consequences & Trade-offs

- **Positive**: The crate name is unambiguous in code, docs, and reviews; the
  engine's public API stops pretending to be the protocol facade.
- **Negative & Incurred Costs**: A 137-site mechanical rename and a docs sweep.
  No behavior changes are permitted.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep both names as a compatibility alias
- **Why considered**: Zero call-site churn.
- **Why rejected**: The alias has no external consumer and perpetuates the
  ambiguity this ADR exists to remove.

### Alternative B — Rename the crate back to `nuo-harness`
- **Why considered**: The word matches the historical concept.
- **Why rejected**: The crate is a cognition/orchestration engine; "harness"
  describes the product posture, not the crate's responsibility. Reverting
  would also invalidate the 0.0.84 split narrative.

## Links

- [ADR-0100](0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md)
- [ADR-0101](0101-runtime-internal-layering-and-governance-inner-layer.md)
- [ADR-0180](0180-single-ownership-of-shared-vocabulary.md)
