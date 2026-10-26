# Request for Comments (RFC) Standards

This document defines the process for authoring, reviewing, and ratifying Request for Comments (RFC) proposals.

---

## 1. When to Author an RFC

While an **ADR** records an accepted architectural decision and its negative knowledge, an **RFC** is an upfront exploratory proposal used during early deliberation when:
- The problem space has multiple competing solutions with divergent trade-offs.
- The proposed change spans multiple major subsystems or foundational data contracts.
- Community or team-wide alignment is required before investing in proof-of-concept implementation.

---

## 2. RFC Lifecycle

```text
  [ Proposed ] ──► [ Under Review ] ──► [ Approved ] ──► [ Migrated to ADR ]
                          │
                          └──► [ Closed / Withdrawn ]
```

1. **Proposed**: Draft RFC submitted for peer feedback.
2. **Under Review**: Active review period with scheduled architectural discussion.
3. **Approved**: Consensus achieved; implementation may proceed.
4. **Migrated to ADR**: Once implementation lands and the decision is ratified in code, the RFC's conclusions are synthesized into a formal Architectural Decision Record in `docs/adr/`.
5. **Closed / Withdrawn**: Proposal rejected or superseded prior to implementation.

---

## 3. RFC Structure

An effective RFC must address:

1. **Motivation & Problem Statement**: What user, architectural, or operational problem requires this change?
2. **Detailed Design**: Proposed interfaces, data models, interaction protocols, and concurrency semantics.
3. **Security, Reliability & Failure Modes**: How does the design behave under boundary failures, malformed payloads, or partial outages?
4. **Migration & Compatibility**: How do existing configurations, schemas, and clients transition smoothly?
5. **Unresolved Questions**: Known unknowns explicitly listed for reviewer deliberation.
