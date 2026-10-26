# Architectural Decision Records (ADR) Standards

This document establishes the qualification criteria, lifecycle progression, frontmatter schema, and canonical template for Architectural Decision Records (ADRs) in Nuo.

---

## 1. Intent & Qualification (The 3-Question Test)

To prevent ADR inflation and maintain a high-signal log, author an ADR only if a proposal meets **at least two** of these criteria:

1. **High Reversal Cost**: Would reversing this decision require multi-week refactoring or breaking migration paths?
2. **Cross-Boundary Blast Radius**: Does it alter subsystem boundaries, public API protocols, data layouts, or non-functional SLAs (latency, security)?
3. **Generates Binding Red-Lines**: Does it establish hard structural boundaries that developers and AI assistants must follow?

### Anti-Patterns to Reject
- **The Triviality Smear**: Authoring ADRs for routine implementation choices (e.g., choosing a serializer or renaming a function).
- **The Ephemeral Workaround**: Recording temporary bug fixes, transient feature flags, or environment hacks.
- **The Toothless Declaration**: Vague prose lacking enforceable constraints or clear behavioral boundaries.
- **The Post-Hoc Justification**: Authoring an ADR after code is merged with zero genuine consideration of discarded alternatives.

---

## 2. In-Place Lifecycle & Frontmatter Schema

ADRs evolve strictly in-place through YAML frontmatter transitions. Files are never physically moved to an `archive/` directory:

```text
       [ draft ] ───► [ accepted ] ───► [ superseded ]
           │
           └───► [ rejected ]
```

- **draft**: Proposal under active deliberation.
- **accepted**: Ratified invariant or architectural contract binding on the repository.
- **superseded**: Formally replaced by a subsequent ADR (`superseded_by: ADR-NNNN`).
- **rejected**: Discarded alternative preserved in-place for negative knowledge audit.

### Frontmatter Schema
```yaml
---
id: ADR-NNNN
title: "[Title of Decision]"
status: accepted # [draft | accepted | superseded | rejected]
date: YYYY-MM-DD
scope: [subsystems affected, e.g. server/nuo-server, protocol/nuo-protocol]
superseded_by: null # e.g., ADR-0002 (mandatory when status is superseded)
negative_knowledge: true
---
```

---

## 3. Authoritative ADR Template

```markdown
---
id: ADR-NNNN
title: "[Title of Decision]"
status: draft
date: YYYY-MM-DD
scope: [comma-separated domain/component scopes]
superseded_by: null
negative_knowledge: true
---

# NNNN. [Title of Decision]

- Status: Draft | Accepted | Superseded by [ADR-NNNN](NNNN-slug.md) | Rejected
- Date: YYYY-MM-DD
- Deciders: [Core Engineering, Architecture Working Group]
- Amends: [Optional links to amended ADRs]

---

## Context and Problem Statement

Describe the context and motivation in concise, precise terms. What architectural bottleneck or boundary violation requires resolution?

## Decision Drivers

- Driver 1: e.g., Decouple IPC serialization from external HTTP streaming
- Driver 2: e.g., Guarantee zero-outflow pre-flight spend bounding

## Architectural Decisions

Detail the chosen technical approach, module responsibilities, and structural changes.

### Subsystem Boundaries & Structural Changes
Explain the exact changes to workspace members, crates, and contracts.

### Normative Invariants Established (Optional)
If a non-negotiable architectural red-line is created, specify it with its enforcement mechanism:
- `[INV-XXX-01] Name`: Mandatory constraint backed by `test_inv_xxx` or compiler typestate.

## Consequences

- **Positive**: Expected architectural and operational benefits.
- **Negative & Trade-offs**: Incurred operational complexity and mitigation strategy.

## Rejected Alternatives & Negative Knowledge

Detail why alternative options were discarded:

### Alternative A (Rejected)
- **Why considered**: [e.g., Lower upfront implementation complexity]
- **Why rejected**: [e.g., Reintroduced circular dependencies between server and client]

## Links

- Related ADRs: [Link]

---

## 4. Reference Resolution

A citation is a promise that the reader can open the decision. Two rules follow:

1. **Every `ADR-NNNN` must resolve to `docs/adr/NNNN-*.md`.** The canonical series
   is exactly what `docs/adr/` contains: `scripts/check_adr_lineage.py` derives
   the set from the directory rather than from a pinned range, so this document
   cannot drift behind the registry. Numbers from the pre-ingestion log
   (`ADR-0085`..`ADR-0304`) were never imported and are **not** citable: a
   reader cannot verify them, and they cannot be amended in place.
2. **Cite an in-repo ADR, or carry the rationale itself.** If a decision has no
   in-repo record, state the constraint in the comment or document instead of
   pointing at a number that does not resolve.

The rule is mechanical, not advisory:

- `scripts/check_adr_lineage.py` walks every `.rs` comment and `.md` prose line
  and fails on an unresolvable citation (`python3 scripts/check_adr_lineage.py`).
- CI (`.github/workflows/ci.yml`) runs it on every push and pull request.

Two classes are exempt by construction, and the checker encodes both:

- **String literals are not documentation.** Prompts, log lines and assertion
  messages are behaviour; editing them is a code change, not a citation fix.
- **Shipped migration SQL is immutable.** Its text is pinned by
  `MIGRATION_CATALOG_FINGERPRINT`
  (`crates/nuo-persistence/src/db/tests.rs::migration_catalog_fingerprint_is_stable`),
  because rewriting an applied migration strands databases stamped by the old
  SQL. The legacy numbers inside those SQL comments are frozen artifacts; the
  checker allowlists that file explicitly.

```
