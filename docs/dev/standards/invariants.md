# Invariant & Constraint Governance

This document codifies the definition, categorization, and lifecycle of invariants. It establishes a policy against pseudo-invariant tag proliferation, ensuring that invariants remain high-value architectural anchors rather than ceremonial comments.

---

## 1. Core Principle: Mechanisms Over Exhortations

In software engineering, an invariant is a condition that must unconditionally hold across all valid states of a system. Historically, projects suffer from "invariant inflation" when mundane UI styles, layout heuristics, or ephemeral flags are labeled as invariants without verification mechanics.

**The Golden Rule**:
> **If a rule has neither a compiler-level proof nor an automated test assertion, it is NOT an invariant — it is merely an aspiration.**

---

## 2. Constraint Admissibility Matrix

Every architectural and operational rule maps deterministically into one of four categories:

| Category | Definition & Scope | Identifier Policy | Mandatory Enforcement Mechanism |
| :--- | :--- | :--- | :--- |
| **System Invariant** | Non-negotiable structural red-lines with cross-boundary blast radius (e.g., subsystem isolation, data purity, protocol decoupling). Max ~20 total across the repository. | Formally ratified in ADRs (`[INV-ARCH-*]`, `[INV-CORE-*]`) | 1. Package dependency firewalls<br>2. Dedicated regression test suite (`test_inv_*`)<br>3. Compiler typestates |
| **Domain Contract** | State-machine transitions and behavioral rules within a single module or subsystem. | **NO global `INV-` tag**.<br>Expressed as named types. | 1. Typed `enum` variants & private field encapsulation<br>2. RAII Guard patterns<br>3. Unit tests |
| **Design Specification** | Visual styling, layout margins, truncation heuristics, theme tokens. | **NO `INV-` tag**.<br>Expressed as design constants. | 1. Design token constants<br>2. Snapshot or layout tests |
| **Implementation Note** | Transient workarounds, compiler bug workarounds, localized edge-case guards. | **NO `INV-` tag**.<br>Standard code comments. | Direct regression unit test |

---

## 3. The 3-Condition Gate for System Invariants

A rule may be elevated to a **System Invariant** if and only if it satisfies all three criteria:

1. **High Blast Radius**: Violation results in state corruption, unrecoverable data loss, cross-process deadlocks, or architectural inversion.
2. **Durable Architecture**: It represents a foundational pillar that will not become obsolete due to routine feature iterations or localized redesigns.
3. **Mechanically Enforced**: It specifies an automated enforcement mechanism (compiler type check, dependency rule, or deterministic automated test).

---

## 4. Degazetting & Retirement Policy

1. **Pseudo-Invariants (Styling, Local State)**:
   - All pseudo-tags (e.g., `INV-UI-*`, `INV-STYLE-*`, `INV-1`) are formally degazetted.
   - Code comments are converted into standard doc-comments or design constants.
   - **No ceremonial tombstones**: Degazetted pseudo-tags are simply deleted from annotations.
2. **True Invariants (Architectural Red-Lines)**:
   - When a true system invariant evolves or is superseded, it follows the **In-Place Lifecycle**:
     - `Active`: Bound and verified in current releases.
     - `Superseded`: Formally superseded by a successor invariant via an ADR (`superseded_by: ADR-NNNN`), retaining the documented failure mode in the ADR's Negative Knowledge section.
     - `Deprecated`: Retired due to subsystem removal, with historical context preserved in the ADR registry.

## 5. Tag Uniqueness

An `[INV-*]` tag is a **citation target**: later documents and code refer to it by
name and expect it to mean one thing. Two definitions of the same tag destroy
that, and nothing else in the build notices — the tag still resolves, it just
resolves to two incompatible rules.

This was not hypothetical. While completing the codec aperture
([ADR-0143](../../adr/0143-codec-aperture-completed-delete-the-unwired-codec-lane.md))
two tags were minted into a live ADR without checking, and both collided:
`[INV-WIRE-01]` already belonged to ADR-0005's zero-I/O rule, `[INV-LANE-01]` to
ADR-0014's three-lane exclusivity. A hand audit caught them. Separately,
ADR-0034 and ADR-0045 had defined the same `[INV-LIFECYCLE-03]` for unrelated
rules for months, with the ADR index and CHANGELOG citing the tag without saying
which rule they meant.

### What Counts as a Definition

Only the line that *introduces* a rule counts, with the tag as the **lead** of a
bullet or a heading:

```text
- **[INV-XXXX-01] A short rule title**: the normative statement, usually with
  MUST/MUST NOT wording, wrapped across as many lines as it needs.
- **INV-XXXX-02** — a compact rule statement.
### 3. `[INV-XXXX-03]` A section that is the rule
```

A citation is not a definition and is never flagged: `see [INV-XXXX-01]`,
`**A short rule title ([INV-XXXX-01])**`, table cells, `### \`[INV-X]\`'s
exception list`, and code comments may each appear dozens of times for one rule.
A checker that counted them would be noise, not a gate.

Sites are counted **per ADR file**. One ADR may introduce a rule in its Decision
section and restate it in its Invariants summary; that is one ADR defining its
own rule, not a collision. Two different ADR files defining the same tag is the
failure. (`INV-XXXX-01` is a placeholder, deliberately not a real tag — an
example that quoted a live tag would itself become a second definition, which is
exactly the collision this check reports.)

Only `docs/` is scanned. A tag's authority is an ADR; a `.rs` comment may cite a
tag but may not mint one.

### Uniqueness Discipline

Invariant tags are centrally curated in `docs/adr/`. Each tag must have a single authoritative definition site, preventing semantic collisions or conflicting definitions.

### Fixing a Collision

1. **Preferred**: give the newer rule its own tag, and update every citation that
   meant the newer rule — the ADR body, the index row, the CHANGELOG entry.
2. If the newer rule genuinely replaces the older one, record the supersession in
   place (§4) and stop re-using the tag; do not leave two definitions alive.

### Revising Without Colliding

A later ADR may need to restate, revise, reaffirm, or revoke an existing rule.
It keeps the tag and **marks the bullet**; the marker is the difference between
"here is the rule" and "here is what changed":

```text
- **[INV-XXX-01] (restates ADR-00NN) Name**: …
- **[INV-XXX-01] (revises ADR-00NN) Name**: …
- **[INV-XXX-01] (reaffirms ADR-00NN) Name**: …
- **[INV-XXX-01] (revokes ADR-00NN) Name**: …
```

A marker is not permission to change the rule's meaning. If the newer rule is
about a different concern, it is a collision: give it a new tag.

### Enforcement

`tests/integration/governance/architecture_convergence.rs::adr_invariant_tags_have_one_definition_site`
scans `docs/adr/` and fails when an unmarked tag is defined in two ADR files.
It recognizes bullet and heading leads, does not count `restates` / `revises` /
`reaffirms` / `revokes` bullets, and does not count citations where the tag is
not the lead. `invariant_definition_forms_are_recognized` pins both the
accepted and the rejected shapes. The positive case is part of the same test:
the scan must find the definitions it is certifying, so an empty corpus is a
failure rather than a pass.
