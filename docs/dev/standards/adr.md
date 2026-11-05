# Architectural Decision Records (ADR) Standards

This document establishes the qualification criteria, lifecycle progression, frontmatter schema, and canonical template for Architectural Decision Records (ADRs).

---

## 1. Intent & Qualification (The 3-Question Test)

To prevent ADR inflation and maintain a high-signal registry, author an ADR only if a proposal meets **at least two** of these criteria:

1. **High Reversal Cost**: Would reversing this decision require multi-week refactoring or breaking migration paths?
2. **Cross-Boundary Blast Radius**: Does it alter subsystem boundaries, public API protocols, data layouts, or non-functional SLAs (latency, security)?
3. **Generates Binding Red-Lines**: Does it establish hard structural boundaries that developers and AI assistants must follow?

### Anti-Patterns to Reject
- **The Triviality Smear**: Authoring ADRs for routine implementation choices (e.g., choosing a serializer or renaming an internal helper).
- **The Ephemeral Workaround**: Recording temporary bug fixes, transient feature flags, or environment hacks.
- **The Toothless Declaration**: Vague prose lacking enforceable constraints or clear behavioral boundaries.
- **The Post-Hoc Justification**: Authoring an ADR after code is merged with zero genuine consideration of discarded alternatives.

---

## 2. In-Place Lifecycle & Flat Registry

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

### Self-Indexing Directory
The `docs/adr/` directory is flat and self-indexing. File names follow `NNNN-<slug>.md` (e.g. `0145-composer-and-transcript-are-peer-focus-planes.md`), encoding both the permanent ID and semantic domain directly in the filesystem. No separate `archive/` directory or manually maintained `index.md` is kept: this eliminates dual-maintenance drift and cognitive context traps while preserving permanent, stable citation targets across git history.

### Frontmatter Schema
```yaml
---
id: ADR-NNNN
title: "[Title of Decision]"
status: accepted # [draft | accepted | superseded | rejected]
date: YYYY-MM-DD
scope: [subsystems or modules affected]
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
- Amends: [Optional links to amended ADRs]

---

## Context and Problem Statement

Describe the context and motivation in concise, precise terms. What architectural bottleneck or boundary violation requires resolution?

## Architectural Decisions

Detail the chosen technical approach, module responsibilities, and structural changes.

### Subsystem Boundaries & Structural Changes
Explain the exact changes to workspace members, modules, and contracts.

### Normative Invariants Established (Optional)
If a non-negotiable architectural red-line is created, specify it with its enforcement mechanism:
- `[INV-XXX-01] Name`: Mandatory constraint backed by automated integration test or compiler typestate.

## Consequences & Trade-offs

- **Positive**: Expected architectural and operational benefits.
- **Negative & Incurred Costs**: Incurred operational complexity and mitigation strategy.

## Discarded Alternatives (Negative Knowledge)

Detail why alternative options were discarded:

### Alternative A (Rejected)
- **Why considered**: [e.g., Lower upfront implementation complexity]
- **Why rejected**: [e.g., Reintroduced circular dependencies or unbounded latency]

## Links

- Related ADRs: [Link]
```

---

## 4. Reference Resolution & Epistemic Integrity

A citation is a promise that the reader can inspect the decision. Three rules follow:

1. **Every `ADR-NNNN` reference must resolve to a canonical file `docs/adr/NNNN-*.md`.** Unregistered or phantom ADR numbers must not be cited in documentation or codebase comments.
2. **Self-Contained Rationale & Code Citation Discipline**:
   - **Never Naked Citations**: A code comment or document must state the operational constraint or rationale in plain prose before citing the ADR (e.g., `// Headless container: zero presentation dependencies (ADR-0011)`). A naked citation like `// ADR-0011` without context degrades readability and forces reader context switches.
   - **Active Successor Discipline**: When citing decisions in production code, point to the active invariant or ratified successor ADR; avoid pointing readers to superseded decisions without noting the active successor.
3. **Cite the Owning Document, Not a User-Owned File**: Repository doctrine lives in `docs/adr/` and `docs/dev/standards/`. A user-owned surface such as `AGENTS.md` is untrusted workspace input loaded by the product behind the trust gate, so it is never the authority a repository document cites ([ADR-0141](../../adr/0141-agents-md-is-user-owned-workspace-input-not-repository-documentation.md), [`taxonomy.md`](taxonomy.md) §3).
   - **Nomination**: mention the file as an inline code span, never as bare prose. A bare filename is indistinguishable from an authority claim.
   - **Citation**: a code-styled mention qualified by a section number (for example the string `AGENTS.md §3`) is a citation of the file's content and is rejected. Quote a whole citation as one code span to discuss the defect itself; cite the standards document that owns the rule to state it.

These conventions preserve traceability across the repository while preventing circular references or citation of untrusted workspace files.
