# Agent Directives

<!-- BEGIN DOCGOV DIRECTIVES -->
<!-- CRITICAL: DO NOT MODIFY, MOVE, REORGANIZE, MERGE, OR DELETE THIS BLOCK OR ITS DELIMITER TAGS. MANAGED AUTOMATICALLY BY DOCGOV. -->
## Documentation Governance Directives

You are bound by repository invariants. Violations will fail CI (`docgov check`).

### 1. Machine Invariants (Pre-Submit Checklist)
- `[INV-LINT-01] Location Sanitization`: Never create arbitrary Markdown files at the repository root.
- `[INV-LINT-02] Contributor Firewall`: Public docs (`docs/{tutorials,how-to,reference,explanation}/`) must NEVER link into internal docs (`docs/dev/`).
- `[INV-LINT-03] Frontmatter Schema`: ADRs must contain valid Frontmatter with standardized status enum.
- `[INV-LINT-04] Code-Doc Sync`: Modifying monitored paths in `src/` requires updating `docs/` in the same change.
- `[INV-LINT-05] Agent Directives Boundary & Invariant Preservation`: When modifying AI agent configurations (e.g. `AGENTS.md`), you MUST preserve this block and its delimiter tags (`<!-- BEGIN DOCGOV DIRECTIVES -->` and `<!-- END DOCGOV DIRECTIVES -->`) completely intact and verbatim. NEVER strip delimiter comments, reformat, summarize, or merge this block into surrounding guidelines.

### 2. Cognitive & Architecture Protocols (Thinking Framework)
- `[INV-AGENT-01] Negative Knowledge`: Every new ADR MUST contain a 'Rejected Alternatives' section explaining why discarded options were not chosen.
- `[INV-AGENT-02] Context Routing & Chesterton's Fence`:
  - In feature generation: NEVER use docs marked `status: superseded` or `status: rejected` as active designs (prevents resurrecting dead patterns).
  - In refactoring/investigation: MUST retrieve `superseded` docs as negative constraints (learn from historical failure modes).
- `[INV-AGENT-03] Blameless Postmortem`: Postmortems MUST analyze system defense failures and detection gaps. Attribution of personal human blame is strictly prohibited.

### 3. Canonical Governance Knowledge & Context
Before drafting or restructuring documentation, inspect the local governance specifications:
- 4D Coordinate Tensor: `docs/governance/documentation/core/taxonomy.md`
- System Invariants Constitution: `docs/governance/documentation/core/invariants.md`
- Technical Voice & Link Contracts: `docs/governance/documentation/core/style.md`
- ADR & Architecture RFC Standard: `docs/governance/documentation/profiles/architecture/adr.md`
- Quality & Verification Guides: `docs/governance/documentation/profiles/validation/testing.md`

### 4. Fast Verification
Before completing any task, run:
```bash
docgov check
```
<!-- END DOCGOV DIRECTIVES -->
