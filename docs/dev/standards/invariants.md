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
