# Invariant & Constraint Governance

This document codifies the definition, categorization, and lifecycle of invariants within Nuo. It establishes an uncompromising policy against pseudo-invariant tag proliferation, ensuring that invariants remain high-value architectural anchors rather than ceremonial comments.

---

## 1. Core Principle: Mechanisms Over Exhortations

In software engineering, an invariant is a condition that must unconditionally hold across all valid states of a system. Historically, projects suffer from "invariant inflation" when mundane UI styles, layout heuristics, or ephemeral flags are labeled as invariants without verification mechanics.

**The Golden Rule**:
> **If a rule has neither a compiler-level proof nor an automated test assertion, it is NOT an invariant — it is merely an aspiration.**

---

## 2. Constraint Admissibility Matrix

Every architectural and operational rule in Nuo maps deterministically into one of four categories:

| Category | Definition & Scope | Identifier Policy | Mandatory Enforcement Mechanism |
| :--- | :--- | :--- | :--- |
| **System Invariant** | Non-negotiable structural red-lines with cross-boundary blast radius (e.g., server/client isolation, non-destructive detach, protocol purity). Max ~20 total across the repository. | Formally ratified in ADRs (`[INV-ARCH-*]`, `[INV-PROTO-*]`) | 1. Cargo dependency firewalls<br>2. Dedicated regression test suite (`test_inv_*`)<br>3. Rust compiler typestates |
| **Domain Contract** | State-machine transitions and behavioral rules within a single crate or subsystem (e.g., Composer quadrant modalities, draft stash capsules). | **NO global `INV-` tag**.<br>Expressed as named types. | 1. Rust `enum` variants & private field encapsulation<br>2. RAII Guard patterns<br>3. Unit tests |
| **Design Specification** | Visual styling, layout margins, truncation heuristics (e.g., Floor-1 rule), keyboard shortcut layouts, theme tokens. | **NO `INV-` tag**.<br>Expressed as design constants. | 1. `crates/nuo-tui/src/design.rs`<br>2. Viewport snapshot tests |
| **Implementation Note** | Transient workarounds, compiler bug workarounds, localized edge-case guards. | **NO `INV-` tag**.<br>Standard code comments. | Direct regression unit test |

---

## 3. The 3-Condition Gate for System Invariants

A rule may be elevated to a **System Invariant** if and only if it satisfies all three criteria:

1. **High Blast Radius**: Violation results in process corruption, unrecoverable data loss, cross-process deadlocks, or architectural inversion.
2. **Durable Architecture**: It represents a foundational pillar that will not become obsolete due to regular feature iterations or UI redesigns.
3. **Mechanically Enforced**: It specifies an automated enforcement mechanism (compiler type check, cargo dependency rule, or deterministic automated test).

---

## 4. Degazetting & Retirement Policy

1. **Pseudo-Invariants (UI, Styling, Local State)**:
   - All pseudo-tags (e.g., `INV-UI-*`, `INV-FOOTER-*`, `INV-AFFORDANCE-*`, `INV-1`) are formally degazetted.
   - Code comments are converted into standard doc-comments or design constants.
   - **No ceremonial tombstones**: Degazetted pseudo-tags are simply deleted from annotations.
2. **True Invariants (Architectural Red-Lines)**:
   - When a true system invariant evolves or is superseded, it follows the **In-Place Lifecycle**:
     - `Active`: Bound and verified in current releases.
     - `Superseded`: Formally superseded by a successor invariant via an ADR (`superseded_by: ADR-NNNN`), retaining the documented failure mode in the ADR's Negative Knowledge section.
     - `Deprecated`: Retired due to subsystem removal, with the historical context preserved in the ADR registry.
