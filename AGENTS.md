# Agent Directives

## 1. Operational Guardrails (Hard Constraints)
- **Prohibited Git Commands**: NEVER execute `git stash`, `git branch`, or `git reset`.
- **Command Termination**: All commands MUST be finite and self-terminating. Never start background servers or unbounded loops without a strict timeout.

## 2. Core Mental Model & Subsystem Boundaries
- **Concentric Architecture**: Nuo is a concentric workspace with a single public binary (`nuo`).
- **Server Purity**: `nuo-server` is strictly headless and must never link terminal presentation libraries (`nuotc`, `crossterm`, `nuo-tui`).
- **Client-Server Anti-Inversion**: `nuo-server` never depends on `nuo-client`. Communication flows exclusively through typed `nuo-protocol` IPC.
- **Protocol Separation**: Local East-West IPC (`nuo-protocol`) is strictly decoupled from external North-South LLM streaming (`nuo-model-codec`).

## 3. Engineering Discipline
- **Mechanisms Over Exhortations**: Rely on Rust compiler typestates, private visibility, and automated tests to guarantee invariants. Never rely on natural-language comments alone.
- **Anti-Inflation**: Do not mint pseudo-invariant tags (e.g. `INV-UI`, `INV-FOOTER`). Model states via typed enums and styles via design constants.
- **Atomic Verification**: Always verify code changes with `cargo check --workspace` and relevant crate test suites before declaring tasks complete.
- **Clean Epistemic Lineage**: References must resolve to canonical in-repo ADRs (`docs/adr/0001`+). Never reference non-existent external `ADR-01xx`/`ADR-02xx` legacy numbers.

## 4. On-Demand Standards Routing
Do not guess repository rules or rely on stale assumptions. Consult authoritative specifications in `docs/dev/standards/` as needed:
- Topology & Subsystem Red-Lines: `docs/dev/standards/architecture.md`
- Invariant & Constraint Policy: `docs/dev/standards/invariants.md`
- ADR Authoring & Lifecycle: `docs/dev/standards/adr.md`
- RFC Standards: `docs/dev/standards/rfc.md`
- Documentation Taxonomy & Firewalls: `docs/dev/standards/taxonomy.md`
- Markdown & Hyperlink Style: `docs/dev/standards/style.md`
- PR Gates & Code-Doc Sync: `docs/dev/standards/workflow.md`
- Blameless Postmortems: `docs/dev/standards/postmortems.md`
- Testing Strategy & Execution: `docs/dev/testing.md`
