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
- **The Rename Record**: A private rename is a style edit, not an ADR. A rename
  that changes a public contract may qualify — but only if it also answers a
  second question (a false claim, a security consequence, or a binding red-line).
  Say which two questions it answers; "the code now spells it" is not one.
- **The Batch Dump**: Bundling independent decisions into one ADR. A batch may
  share a date, and one decision may have several instances; each ADR must still
  stand alone, and the context must say whether it records one decision or many.
  If a reader cannot reject one part without rejecting the whole, it is one
  decision.

---

## 2. In-Place Lifecycle & Flat Registry

ADRs evolve strictly in-place through YAML frontmatter transitions. Files are never physically moved to an `archive/` directory:

```text
       [ draft ] ───► [ accepted ] ───┬──► [ superseded ]
           │                          │
           │                          └──► [ deprecated ]
           └───► [ rejected ]
```

Five statuses, each with one meaning:

- **draft**: Proposal under active deliberation.
- **accepted**: Ratified invariant or architectural contract binding on the repository.
- **superseded**: Formally replaced by a subsequent ADR (`superseded_by: ADR-NNNN`, mandatory).
- **deprecated**: Was accepted and in force, but the decision, the feature, or the
  structure it governed has since been removed **without a direct successor ADR**
  (for example a crate deleted in a consolidation, or a rename record that no
  longer carries an architectural rule). `superseded_by` stays `null`; the body's
  status line names what removed it.
- **rejected**: Never became binding — a proposal discarded or never implemented,
  preserved in-place for negative knowledge audit.

Choosing between them: if a later ADR replaces the decision, use `superseded`;
if the decision was once true but is now simply gone, use `deprecated`; if it was
never true of the code, use `rejected`.

**Partial supersession.** When a later ADR replaces only part of a decision and
the rest still binds, keep `status: accepted`, set `superseded_by` to the
successor, and say which section in the body's status line
(`Accepted; §2 superseded by ADR-NNNN`).

### Self-Indexing Directory
The `docs/adr/` directory is flat and self-indexing. File names follow `NNNN-<slug>.md` (e.g. `0145-composer-and-transcript-are-peer-focus-planes.md`), encoding both the permanent ID and semantic domain directly in the filesystem. No separate `archive/` directory or manually maintained `index.md` is kept: this eliminates dual-maintenance drift and cognitive context traps while preserving permanent, stable citation targets across git history.

### Frontmatter Schema
```yaml
---
id: ADR-NNNN
title: "[Title of Decision]"
status: accepted # [draft | accepted | superseded | deprecated | rejected]
date: YYYY-MM-DD
scope: [subsystems or modules affected]
superseded_by: null # e.g., ADR-0002 (mandatory when status is superseded)
negative_knowledge: true # true iff an alternatives/negative-knowledge section exists
---
```

`date` is the **decision date on the project's logical timeline**, not the git
author date. It must be a real `YYYY-MM-DD`; it is not required to match the
commit date, and a gate must not compare it to the wall clock. `scope` must not
be empty. `negative_knowledge: true` is a claim: the ADR must carry a
`Rejected/Discarded Alternatives`, `Considered Options`, or `Negative Knowledge`
section. An ADR that records no rejected alternative sets it to `false`; the
flag is not a decoration copied from the template.

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

- Status: Draft | Accepted | Superseded by `ADR-NNNN` (`NNNN-slug.md`) | Deprecated (reason) | Rejected
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

An invariant tag has exactly one **unmarked definition site** in `docs/adr/`,
counted per ADR file ([`invariants.md`](invariants.md) §5). A definition is a
bullet or a heading whose **lead** is the tag (`- **[INV-X] Title**: …`,
`- **INV-X** — …`, `### 1. \`[INV-X]\` Title`); a citation that starts with a
title is not a definition. One ADR may state the rule in its Decision section
and restate it in its Invariants summary — that is one site. A later ADR that
restates, revises, reaffirms, or revokes the same rule keeps the tag but marks
the bullet:

```text
- **[INV-XXX-01] (restates ADR-00NN) Name**: …
- **[INV-XXX-01] (revises ADR-00NN) Name**: …
- **[INV-XXX-01] (reaffirms ADR-00NN) Name**: …
- **[INV-XXX-01] (revokes ADR-00NN) Name**: …
```

Those markers are not a second definition. A tag whose meaning actually changed
under an old number is a collision, not a revision: give the newer rule its own
tag (`invariants.md` §5, "Fixing a Collision").

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
4. **Amendment backlinks are for contradictions, not for the graph.** The
   amender's `Amends:` line is authoritative. When the target's body asserts
   something the amendment makes false, add an `Amended by:` line to the target
   naming the amender and the section. Do not add a backlink to a target whose
   body is unchanged: a complete bidirectional graph doubles the registry
   without adding information. The gate enforces reference resolution, not the
   presence of a backlink for every edge.

### Invariant Tags

`docs/dev/standards/invariants.md` §5 owns the tag uniqueness rule. One
**unmarked** definition per tag; `restates` / `revises` / `reaffirms` /
`revokes` bullets are marked and are not definitions. The gate is
`adr_invariant_tags_have_one_definition_site`.

### Mechanical Gates

`tests/integration/governance/architecture_convergence.rs` runs
`adr_registry_has_unique_identity_and_valid_frontmatter`,
`adr_frontmatter_values_are_well_formed`,
`adr_invariant_tags_have_one_definition_site` (plus its shape test
`invariant_definition_forms_are_recognized`),
`adr_negative_knowledge_flag_is_truthful`, `adr_references_resolve`, and
`markdown_relative_links_resolve`. A rule here without one of those gates is a
review convention, not a mechanical invariant.

These conventions preserve traceability across the repository while preventing circular references or citation of untrusted workspace files.
