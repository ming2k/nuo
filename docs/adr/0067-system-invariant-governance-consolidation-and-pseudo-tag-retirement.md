---
id: ADR-0067
title: "DocGov Decoupling, Invariant Realism, and Documentation Topology Modernization"
status: accepted
date: 2026-11-06
scope: architecture/governance, docs/dev, agent/directives
superseded_by: null
negative_knowledge: true
---

# 0067. DocGov Decoupling, Invariant Realism, and Documentation Topology Modernization

- Status: Accepted
- Date: 2026-11-06
- Deciders: Nuo Architecture Working Group
- Consulted: Platform Architecture, Engine, TUI, and Transport Teams
- Informed: Core Engineering, Developer Experience
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md), [ADR-0051](0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md), [ADR-0054](0054-universal-key-affordance-engine-width-adaptive-floor-truncation-and-modeless-f1-topology.md), [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)

---

## Context and Problem Statement

The repository was previously burdened with excessive external governance bureaucracy:

1. **Invasive Upstream Tooling (`docgov`)**:
   An external tool (`docgov`) was introduced alongside a deep mirror directory `docs/governance/documentation/{core,profiles}/`. This imported pseudo-academic taxonomies, rigid linter restrictions, and artificial comment-block boundaries (`<!-- BEGIN/END DOCGOV DIRECTIVES -->`) in `AGENTS.md`.
2. **The "Junk Drawer" Hierarchy**:
   The multi-layered `docs/governance/documentation/core/` and `profiles/` directory structure acted as an over-engineered junk drawer, adding navigation friction while offering zero value to Nuo's core runtime or product development.
3. **Tag Hyperinflation and Concept Smear**:
   The repository accumulated over 325 ad-hoc `INV-` identifiers. Ephemeral UI styling (`INV-UI-02` dim chord formatting, `INV-FOOTER-02` single-line activity strip), local layout heuristics (`INV-AFFORDANCE-02`), and component-internal flags were elevated to "system invariants". When every line of code claims invariant status, the concept loses all disciplinary rigor ("When everything is an invariant, nothing is an invariant").
4. **"Paper Invariants" (Toothless Declarations)**:
   A majority of minted `INV-` tags existed purely as natural-language comments, backed neither by Rust compiler type-level proofs (e.g., Typestates, RAII invariants, private field encapsulation), nor by deterministic automated tests.
5. **Ghost References and Broken Heritage**:
   The codebase retained hundreds of legacy citations pointing to external upstream design records (`ADR-01xx`, `ADR-02xx`), none of which exist in the canonical `docs/adr/` registry (`0001` through `0066`).

---

## Decision Drivers

1. **Decouple and Unvendor Governance Overhead**: Eliminate `docgov`, `.docgov.yml`, and `docs/governance/`. Return to native, idiomatic Rust toolchains (`cargo test --workspace`, `cargo check`, clippy).
2. **Flatten Documentation Topology**: Eliminate deep meta-documentation directories. Maintain clean, direct documentation surfaces in `docs/dev/` and `docs/adr/`.
3. **Mechanisms Over Exhortations**: Invariants must be enforced by concrete mechanisms: Rust type systems, module visibility, or automated regression tests. Natural-language "swearing" in comments is abolished.
4. **Degazette Pseudo-Invariants**: Strip pseudo-invariant labels from UI layouts, keycap styling, and local state transitions. Use typed models, design constants, and normal doc-comments.
5. **Zero Legacy Drag**: Abolish all dangling references to external `ADR-01xx` / `ADR-02xx` records.

---

## Architectural Decisions

### 1. Complete Decoupling from `docgov` and Unification into `docs/dev/standards/`
- Remove `.docgov.yml` and `.docgov.lock`.
- Completely abolish the external and over-nested `docs/governance/` directory.
- Re-home all engineering, architectural, and quality standards directly into `docs/dev/standards/` as a flat, high-cohesion, native suite.
- Purge `docgov check` commands from release checklists and contribution docs in favor of `cargo check --workspace` and `cargo test --workspace`.

### 2. Streamlining `AGENTS.md`
- Strip the artificial `<!-- BEGIN DOCGOV DIRECTIVES -->` boundaries.
- Re-architect `AGENTS.md` into direct, actionable engineering guidelines prioritizing workspace boundaries, concentric architecture, type-driven correctness, and testing discipline.

### 3. Classification of Constraints (Abolishing Tag Inflation)
| Category | Classification | Policy | Enforcement Mechanism |
| :--- | :--- | :--- | :--- |
| **System Architectural Red-Lines** | Core Invariant | Canonical `INV-*` ID (Curated, <20 total) | Compiler typestates, crate separation, dedicated tests |
| **Component Contracts / State Machines** | Domain Contract | **NO INV ID** (Named Enum / Typestate) | Rust enum pattern, private state, Rust doc-comments |
| **UI Layout / Visual Presentation** | Design Spec | **NO INV ID** (Design Constants) | `design.rs`, layout unit tests, style guides |
| **Localized Bug Fixes / Workarounds** | Implementation Note | **NO INV ID** (Doc-comment) | Regression test, standard inline code comment |

### 4. Degazetting & Retirement of Pseudo-Invariants
- All pseudo-invariant tags previously minted for UI presentation (`INV-UI-*`, `INV-FOOTER-*`), layout mathematics (`INV-AFFORDANCE-*`), and localized mode switching are formally degazetted.
- Obsolete or pseudo-tags are stripped cleanly from code comments without ceremonial tombstones.

### 5. Retirement of Ghost ADR Citations
- All citations to non-existent `ADR-01xx` / `ADR-02xx` records are formally retired. Nuo's architectural ledger strictly begins at `ADR-0001` and is self-contained.

---

## Consequences

### Positive
- **Zero External Governance Drag**: Eliminates dependency on `docgov` and fragile delimiter comments.
- **Flat, Readable Documentation**: No more nested "junk drawer" hierarchies under `docs/governance/`.
- **Cognitive Clarity**: Eliminates 300+ noisy pseudo-tags, restoring true engineering meaning to system invariants.
- **Self-Contained Epistemic Integrity**: Zero dead links, zero ghost ADR citations.

### Negative & Trade-offs
- Verification of documentation changes is now guided by human and AI engineering discipline rather than an external AST linter.

---

## Rejected Alternatives & Negative Knowledge

### 1. Retaining `docgov` While Pruning Rules
- **Why considered**: Attempting to keep the external binary while disabling rules in `.docgov.yml`.
- **Why rejected**: Perpetuates dependency on an external protocol tool that mandates invasive boundary comments in `AGENTS.md` and generates redundant mirror directories. Native Rust toolchains (`cargo`) already provide sufficient verification.

### 2. Renaming `docs/governance/documentation/core` to `docs/governance/core`
- **Why considered**: Reducing directory depth while keeping the governance structure.
- **Why rejected**: Still preserves the "junk drawer" anti-pattern. Architecture decisions belong in `docs/adr/`; developer guidelines belong in `docs/dev/`. A separate "governance" drawer is superfluous.

---

## Links

- Related Standards: [ADR Standards](../dev/standards/adr.md), [Invariant Governance](../dev/standards/invariants.md)
- Related Agent Directives: [AGENTS.md](../../AGENTS.md)
- ADR Index: [ADR Index](index.md)
