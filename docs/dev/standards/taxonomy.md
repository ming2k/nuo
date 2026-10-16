# Documentation Taxonomy & Layout

This document defines the organization, scope boundaries, and classification of all documentation assets in the Nuo repository.

---

## 1. The Four-Quadrant System (Diátaxis)

Public user-facing documentation in `docs/` strictly follows the Diátaxis architecture:

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

- **`docs/tutorials/`**: Guided walk-throughs taking a new user or developer through their first successful run (e.g., getting started).
- **`docs/how-to/`**: Step-by-step problem-solving recipes for specific user goals (e.g., configuring an OAuth provider, recovering session state).
- **`docs/reference/`**: Technical descriptions of APIs, schemas, commands, and interfaces without tutorial prose (e.g., CLI commands, configuration options).
- **`docs/explanation/`**: Architectural deep dives, design rationale, and background discussions (e.g., interrupt semantics, session model overview).

---

## 2. Scope Boundaries (Public vs. Internal)

To maintain clarity for external users while preserving complete internal documentation for core contributors, Nuo enforces a strict audience firewall:

```text
┌─────────────────────────────────────────────────────────────┐
│ Public Surface (Users, Integrators, End Developers)         │
│ `docs/tutorials/`, `how-to/`, `reference/`, `explanation/`  │
└──────────────────────────────┬──────────────────────────────┘
                               │
                               │ Scope Boundary: Public docs must
                               │ NEVER link to internal docs.
                               ▼
┌─────────────────────────────────────────────────────────────┐
│ Internal Surface (Maintainers, Core Contributors, Agents)   │
│ `docs/dev/` (internals, migrations, runbooks, standards)    │
│ `docs/adr/` (binding architectural decision records)        │
└─────────────────────────────────────────────────────────────┘
```

1. **Firewall Rule**: Public documentation surfaces (`docs/{tutorials,how-to,reference,explanation}/`) must NEVER contain relative links pointing into internal maintainer spaces (`docs/dev/**`).
2. **Reverse Linking Permitted**: Internal documents may freely link to public reference and how-to guides.

---

## 3. Repository Root Whitelist

To keep the repository root clean, professional, and clutter-free, only canonical top-level documentation files are permitted at the root:
- `README.md` (and regional language variants like `README-zh.md`)
- `CHANGELOG.md`
- `CONTRIBUTING.md`
- `AGENTS.md`
- `LICENSE.md`
- `SECURITY.md`

All other documentation must reside within `docs/`.
