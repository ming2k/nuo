---
id: ADR-0199
title: "The Default Agent Has a Coding Baseline and a Skill Catalog"
status: accepted
date: 2026-11-15
scope: engine/instructions, skills, prompt
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0199. The Default Agent Has a Coding Baseline and a Skill Catalog

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

The instruction registry is a good mechanism with an empty default. A bare
`developer` session with no identity, no AGENTS.md, and no provider guidance
could reach the provider with **no system message at all**; the integration
suite even asserted that behavior. Tool discipline, read-before-edit,
verification, and todo usage therefore depended entirely on the model's
priors. Separately, skills were discoverable only by an explicit `@name`
mention: the model could not know a skill existed. The registry owned the
right seam for both, but nothing used it.

## Architectural Decisions

### 1. `agent.coding_baseline` is always active

A short, model-agnostic working contract renders before user instructions:
read before editing, smallest change, use tools, parallelize independent
reads, verify with the project's own command, adapt on denial/failure, use the
todo tool for multi-step work, cite paths, keep answers concise, and never
reveal secrets. It is not an identity override; an embedding that wants a
different persona still supplies identity.

### 2. The skill catalog is a fixed-prefix section

`InstructionContext` carries up to 64 enabled, unquarantined skills as
`(name, short description)`. `skills.catalog` renders a compact list with the
explicit instruction that `@name` loads the full body. Bodies stay lazy; only
the catalog is paid on every request. The list is sorted by name so the
cacheable prefix is stable.

### 3. Disabled and quarantined skills are absent

The catalog filters `enabled && !quarantined`, matching the implicit-load
policy. A skill the harness would refuse to load is not advertised.

### 4. The baseline is not a provider note

Provider guidance remains a separate section for dialect-specific output
rules. The baseline is the shared coding contract and must not be replaced by
a provider-specific prompt.

## Consequences

- **Positive**: a bare session has consistent tool and verification behavior;
  skills become discoverable without paying every body's token cost; the
  cacheable prefix stays deterministic.
- **Negative & Incurred Costs**: the baseline adds a small fixed token cost to
  every request. The catalog can grow; the 64-entry cap bounds it.
- **Neutral**: embeddings can still disable or reorder sections through the
  existing registry API. Identity remains opt-in.

## Discarded Alternatives (Negative Knowledge)

- **Inject every skill body**: unbounded prompt cost and stale instructions.
- **Hardcode a provider-specific prompt as the baseline**: fights the
  provider guidance section and the user's identity.
- **Leave the default empty and rely on AGENTS.md**: a new workspace has no
  AGENTS.md, and the harness should not be silently degraded until one exists.

## Links

- [ADR-0164](0164-instructions-are-one-pipeline-over-system-and-project-sources.md)
- [ADR-0180](0180-single-ownership-of-shared-vocabulary.md)
