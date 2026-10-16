# Incident Postmortem Standards

This document establishes the blameless postmortem standard, timeline reconstruction methodology, and systemic defense analysis for incidents in Nuo.

---

## 1. Blameless Culture & Intent

Incidents are opportunities to identify systemic fragility, missing automation, and testing gaps.
- **Strictly Blameless**: Postmortems must NEVER assign personal blame or attribute root causes to "human error" or "developer negligence".
- **Focus on Systemic Safeguards**: The central question is always: *Why did the compiler, test suite, or architecture allow an unsafe state to manifest?*

---

## 2. Postmortem Structure

Every postmortem recorded in `docs/dev/postmortems/` must follow this structure:

1. **Executive Summary**: High-level impact (affected users, duration, data integrity status).
2. **Timeline of Events (UTC)**:
   - $T_0$: Introduction of defect (commit, dependency bump).
   - $T_{\text{impact}}$: First manifestation in production or test.
   - $T_{\text{detect}}$: Detection by monitoring or user report.
   - $T_{\text{mitigate}}$: Mitigation or rollback applied.
   - $T_{\text{resolve}}$: Permanent fix verified.
3. **Detection Gaps**: Why did unit tests, integration tests, or CI fail to detect this before release?
4. **Trigger & Underlying Root Cause**: Detailed technical analysis of the state-machine failure or boundary leak.
5. **Action Items & Defense-in-Depth**:
   - Preventative actions (e.g., adding a new type-level invariant, adding a compiler lint).
   - Detection actions (e.g., adding a specific regression test).
   - Each action item must have an owner and a verifiable completion criterion.
