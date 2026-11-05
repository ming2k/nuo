# Documentation Taxonomy & Information Architecture

This document defines the organization, scope boundaries, and classification of all documentation assets in the repository.

---

## 1. The Four-Quadrant System (Diátaxis)

Public documentation in `docs/` follows the Diátaxis information architecture:

```text
               PRACTICAL                   THEORETICAL
          ┌─────────────────────────┬─────────────────────────┐
          │  Tutorials              │  How-To Guides          │
LEARNING  │  (Learning-Oriented)    │  (Task-Oriented)        │
          │  `docs/tutorials/`      │  `docs/how-to/`         │
          ├─────────────────────────┼─────────────────────────┤
          │  Reference              │  Explanation            │
WORKING   │  (Information-Oriented) │  (Understanding-Orient) │
          │  `docs/reference/`      │  `docs/explanation/`    │
          └─────────────────────────┴─────────────────────────┘
```

- **`docs/tutorials/`**: Guided, hands-on lessons taking a new reader through their first successful experience. Focuses on learning and building confidence.
- **`docs/how-to/`**: Step-by-step problem-solving recipes for specific user goals. Focuses on accomplishing a practical task.
- **`docs/reference/`**: Technical descriptions of APIs, schemas, commands, configuration options, and interfaces without tutorial prose. Focuses on precision and completeness.
- **`docs/explanation/`**: Conceptual deep dives, design rationale, and background discussions. Focuses on understanding the system and its architecture.

---

## 2. Documentation Spaces & Audiences

Documentation is structured into two surfaces separated by a one-way visibility boundary:

```text
┌─────────────────────────────────────────────────────────────┐
│ Public Surface (Users, Integrators, Application Developers) │
│ `docs/tutorials/`, `how-to/`, `reference/`, `explanation/`  │
└──────────────────────────────┬──────────────────────────────┘
                               │
                               │ Scope Boundary: public docs must
                               │ never link into the internal space.
                               ▼
┌─────────────────────────────────────────────────────────────┐
│ Engineering Surface (Contributors, Maintainers, Agents)     │
│ `docs/dev/` (internals, standards, testing runbooks)        │
│ `docs/adr/` (canonical architectural decision records)      │
└─────────────────────────────────────────────────────────────┘
```

1. **User Surface**: Addressed to end users and external developers. Emphasizes user workflows, stable API contracts, and usage scenarios.
2. **Engineering Surface**: Addressed to contributors, maintainers, and development agents. Emphasizes internal subsystem architecture, decision records, and development standards.
3. **Audience Firewall**: Public documentation surfaces (`docs/{tutorials,how-to,reference,explanation}/`) must never contain relative links pointing into the internal maintainer space (`docs/dev/**`). Links into `docs/adr/` remain permitted: a public reader may follow a decision record, but not a contributor runbook.
4. **Reverse Linking Permitted**: Internal documents may freely link outward to public reference and how-to guides.

This boundary ensures that end-user documentation remains focused on public workflows and does not leak maintainer-internal runbooks.

---

## 3. Repository Root Whitelist

To keep the repository root clean, professional, and clutter-free, only canonical top-level documentation files are permitted at the root:
- `README.md` (and regional language variants like `README-zh.md`)
- `CHANGELOG.md`
- `CONTRIBUTING.md`
- `LICENSE.md`
- `SECURITY.md`

All other documentation must reside within organized directories under `docs/`.

### User-Owned Surfaces (Excluded)

A root file may also exist because users expect it there, without being repository documentation. Such files are **outside** every rule in this document and in [`style.md`](style.md): they are neither citable as repository authority nor part of the product's documentation.

- `AGENTS.md` — user-authored agent guidance. The product loads it as untrusted workspace input behind the trust gate (`nuo_host::security::PROJECT_RULE_FILES`), alongside `.cursorrules` and `.windsurfrules`. Its shape belongs to whoever owns the workspace, not to this repository ([ADR-0141](../../adr/0141-agents-md-is-user-owned-workspace-input-not-repository-documentation.md)).

1. **Nomination**: a user-owned surface must be named as an inline code span. A bare filename in prose is avoided, because it is indistinguishable from an authority claim.
2. **Citation**: a code-styled mention qualified by a section number (for example the string `AGENTS.md §3`) is rejected. Code styling must not launder a citation of the file's contents; to discuss such a defect, quote the whole citation as one code span.
