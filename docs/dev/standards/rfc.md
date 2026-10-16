# Request for Comments (RFC) Standards

This document defines the process for authoring, reviewing, and ratifying Request for Comments (RFC) proposals in Nuo.

---

## 1. When to Author an RFC

While an **ADR** records an accepted architectural decision and its negative knowledge, an **RFC** is an upfront exploratory proposal used during early deliberation when:
- The problem space has multiple competing solutions with divergent trade-offs.
- The proposed change spans multiple major subsystems (e.g., rewriting the session persistence layer or altering the global execution loop).
- Community or team-wide alignment is required before investing in proof-of-concept code.

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
4. **Migrated to ADR**: Once the implementation lands and the decision is ratified in code, the RFC's conclusions are synthesized into a formal Architectural Decision Record (`docs/adr/`).
5. **Closed / Withdrawn**: Proposal rejected or superseded prior to implementation.

---

## 3. RFC Structure

An effective RFC must address:

1. **Motivation & User Stories**: What real-world user or system problem requires this change?
2. **Detailed Design**: Precise APIs, data models, network protocol messages, and concurrency semantics.
3. **Security, Privacy & Failure Modes**: How does the design behave under network partition, malformed model payloads, or process crashes?
4. **Migration & Compatibility**: How do existing persistent sessions and configurations transition smoothly?
5. **Unresolved Questions**: Known unknowns explicitly listed for reviewer input.
