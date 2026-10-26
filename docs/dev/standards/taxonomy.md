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
2. **Engineering Surface**: Addressed to contributors, maintainers, and development agents. Emphasizes internal subsystem blueprints, architecture decisions, and development standards.
3. **Audience Firewall**: Public documentation surfaces (`docs/{tutorials,how-to,reference,explanation}/`) must never contain relative links pointing into the internal maintainer space (`docs/dev/**`). Links into `docs/adr/` remain permitted: a public reader may follow a decision record, but not a contributor runbook.
4. **Reverse Linking Permitted**: Internal documents may freely link outward to public reference and how-to guides.

The firewall is a mechanical gate, not a convention: `nuo::dev::doc_links` fails the build when a public surface links into `docs/dev/**` (`cargo test -p nuo --test doc_governance -- test_doc_links`).

---

## 3. Repository Root Whitelist

To keep the repository root clean, professional, and clutter-free, only canonical top-level documentation files are permitted at the root:
- `README.md` (and regional language variants like `README-zh.md`)
- `CHANGELOG.md`
- `CONTRIBUTING.md`
- `AGENTS.md`
- `LICENSE.md`
- `SECURITY.md`

All other documentation must reside within organized directories under `docs/`.
