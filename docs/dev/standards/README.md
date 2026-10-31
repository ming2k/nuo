# Nuo Engineering Standards

This directory defines the authoritative engineering, architectural, and quality standards for the Nuo project.

Rather than adopting external meta-frameworks with deeply nested junk-drawer directories or creating disconnected top-level governance silos, Nuo maintains a **flat, high-cohesion, practical, and code-aligned standards suite directly inside `docs/dev/standards/`**.

---

## Standards Directory Structure

All engineering standards reside flatly in `docs/dev/standards/`:

| Document | Purpose & Authority |
| :--- | :--- |
| **[`invariants.md`](invariants.md)** | **Invariant Standards & Admissibility**: Codifies true system invariants (mechanisms over exhortations), establishes the classification matrix, and degazettes pseudo-tags. |
| **[`adr.md`](adr.md)** | **Architectural Decision Records**: The 3-Question Test for ADR qualification, in-place lifecycle (`draft`, `accepted`, `superseded`, `rejected`), frontmatter schema, and template. |
| **[`rfc.md`](rfc.md)** | **Request for Comments (RFC)**: Deliberation and consensus protocol for cross-cutting, high-reversal-cost subsystem redesigns prior to ADR ratification. |
| **[`taxonomy.md`](taxonomy.md)** | **Documentation Taxonomy**: Four-quadrant Diátaxis layout, audience boundaries (public user vs contributor internal), and knowledge lifecycle. |
| **[`style.md`](style.md)** | **Documentation & Markdown Style**: Canonical hyperlink conventions, relative routing, formatting consistency, and prose guidelines. |
| **[`postmortems.md`](postmortems.md)** | **Blameless Incident Postmortems**: Root-cause analysis, timeline reconstruction, detection gap mitigation, and defense-in-depth principles. |

For hands-on testing execution and QA runbooks, see [`docs/dev/testing.md`](../testing.md) and [`docs/dev/manual-testing.md`](../manual-testing.md).

---

## Core Governance Philosophy

1. **Mechanisms Over Exhortations**:
   Rules and system boundaries must be enforced by concrete mechanisms: Rust compiler type mechanics (typestates, ownership, private visibility), crate dependency firewalls, or deterministic test assertions. Natural-language "swearing" in comments is forbidden.
2. **Anti-Inflation**:
   A sharp line separates **System Invariants** (non-negotiable structural red-lines), **Domain Contracts** (internal state machines), **Design Specifications** (UI styling, keyboard shortcuts), and **Implementation Notes**.
3. **Flat & Pragmatic**:
   Zero arbitrary nesting or nested "core" junk drawers. Every governance document directly addresses an active engineering concern in Nuo.
4. **Self-Contained Epistemic Lineage**:
   All decisions and references must resolve directly within `docs/adr/` (ADR-0001 through current). Ghost references to external upstream lineages are strictly prohibited.
