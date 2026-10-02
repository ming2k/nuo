# Entity: Acceptance Criteria & User Journeys

Intent: verify that the software delivers intended end-to-end user outcomes through rigorous, real-world user journeys, scenario matrices, and formal release readiness gates.

---

## 1. Industrial Standards Mapping

`docgov` aligns the Acceptance Entity directly with established international software engineering standards:

- **ISO/IEC/IEEE 29119-2/4 (Software Testing - Test Levels)**:
  - Establishes **Acceptance Testing (Level 4)** as an autonomous phase distinct from component, integration, and system testing.
  - Covers **User Acceptance Testing (UAT)** (business workflow satisfaction), **Operational Acceptance Testing (OAT)** (cold-start installation, recovery, and configuration drift), and **Contractual Acceptance**.
- **ISO/IEC/IEEE 12207 (Software Life Cycle Processes)**:
  - Fulfills the formal **Software Acceptance Support Process** (§6.4.8), establishing verifiable criteria that authorize deliverable acceptance and deployment.
- **ISTQB (International Software Testing Qualifications Board)**:
  - Implements the definition of acceptance: establishing confidence in the system's fitness for purpose and fulfillment of business processes, rather than defect-hunting in isolated algorithms.
- **ATDD / BDD (Acceptance Test-Driven Development)**:
  - Serves as the single living source of truth for repository acceptance criteria, guiding both automated E2E harnesses and human verification.

---

## 2. Decoupling: Contract vs. Execution Medium

A mature repository does not equate "Acceptance" with "Manual Testing":

| Dimension | Acceptance Specification (`docs/dev/acceptance.md`) | Execution Medium (Automated vs. Manual) |
| :--- | :--- | :--- |
| **Category** | **What & Why** (Contract, Target, Deliverable) | **How** (Runner, Tool, Human) |
| **Role** | Single source of truth for release readiness | Mechanism used to verify scenarios |
| **Automation** | Independent of whether tested by script or human | Target: **>=90% automated** in CI pipelines |
| **Manual Scope** | Defines steps for human release walk-through | Constrained to **Exploratory**, **UX**, and **Sign-off** |

> **Anti-Pattern Warning**: Repetitive, manual regression verification ("human runners" clicking buttons or re-typing identical commands per release) is an engineering anti-pattern. Predictable acceptance scenarios must be automated. Manual testing must be reserved for high-value human cognition: exploratory boundary testing, user experience audit, and final cold-start sign-off.

---

## 3. Non-Negotiable Invariants

- **`[INV-VAL-01]` Real User Journey Parity**:
  Acceptance journeys must exercise the system strictly through real user interfaces, public CLI commands, or documented public APIs from a cold start, without invoking private test harness backdoors, pre-seeded caches, or mock overrides.
- **`[INV-TEMP-01]` Hot Data Synchronization**:
  Whenever a user-visible feature is added, changed, or removed, `docs/dev/acceptance.md` must be updated in the same Pull Request.
- **`[INV-CORE-01]` Contributor Firewall**:
  `docs/dev/acceptance.md` lives behind the contributor firewall (`docs/dev/`). User-facing guides must never link directly into it.

---

## 4. Document Structure Requirements

Every `docs/dev/acceptance.md` document must provide four core sections:
1. **Scope & Execution Medium Strategy**: Explicitly states system prerequisites, scope, and which portions of the acceptance matrix are automated in CI vs. manually walked.
2. **Core User Journeys**: Narrative end-to-end workflows executed from a cold start (`Initial State` -> `Action` -> `Verification` -> `Outcome`).
3. **Four-Quadrant Acceptance Scenario Matrix**: Tabular coverage across:
   - `Happy Path`: Canonical workflows under standard configuration.
   - `Edge Case`: Boundary values, high volume, or extreme inputs.
   - `Error Recovery`: Disconnects, graceful degradation, and resilience.
   - `Security / Permissions`: Authorization, tampering, and boundary integrity.
4. **Verification Checklist**: Formal release readiness sign-off criteria.

---

## 5. Authoritative Acceptance Template (`docs/dev/acceptance.md`)

```markdown
# Acceptance Criteria & User Journeys

This document defines the real-world user journeys, acceptance scenario matrices, and deliverable criteria for this repository, aligned with ISO/IEC/IEEE 29119 acceptance standards.

---

## 1. Scope & Execution Strategy

- **Target System**: [Artifact Name / Version / Scope]
- **Prerequisites**: [Clean environment requirements: OS, runtime, public network]
- **Automation Status**:
  - Automated Acceptance: [e.g., CI workflow `.github/workflows/e2e.yml` runs Scenarios `SCEN-01` through `SCEN-04`]
  - Human Walkthrough: [e.g., Pre-release cold-start verification and UX sanity checks]

---

## 2. Core User Journeys

### Journey 1: Clean Onboarding & Primary Workflow
1. **Initial State**: Pristine host environment with prerequisites installed; zero repository caches or pre-existing state.
2. **Action**: Run the canonical setup / installation command:
   ```bash
   ./install.sh
   ./bin/app init
   ```
3. **Verification**: Command exits with code 0; produces valid initialized directory structure and configuration.
4. **Outcome**: System is verified ready for end-user operation.

### Journey 2: Standard Production Operation
1. **Initial State**: Initialized production environment.
2. **Action**: Execute core capability:
   ```bash
   ./bin/app run --input sample.dat
   ```
3. **Verification**: Outputs match specification schema without diagnostic warnings; runtime meets latency expectations.
4. **Outcome**: User outcome successfully delivered end-to-end.

---

## 3. Four-Quadrant Acceptance Scenario Matrix

| Scenario ID | Category | Initial Condition | Action / Trigger | Expected Observable Outcome | Verification Medium |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `SCEN-01` | Happy Path | Default config | Execute primary workflow | Status 0; valid artifact generated | Automated (CI) |
| `SCEN-02` | Edge Case | Oversized payload | Ingest data exceeding limit | Clean exit 1; clear diagnostic error message; no panic | Automated (CI) |
| `SCEN-03` | Error Recovery | Network disconnected | Trigger remote sync | Retries with exponential backoff; reports network failure; zero state corruption | Automated (CI) |
| `SCEN-04` | Security | Unauthorized token | Invoke authenticated API | HTTP 401; security audit log emitted | Automated (CI) |
| `SCEN-05` | Usability / UX | Cold-start terminal | Run CLI with `--help` | Help text aligns with actual arguments; no formatting anomalies | Human Walkthrough |

---

## 4. Release Readiness Checklist

Before signing off on a production release:
- [ ] Every journey in Section 2 succeeds from a cold start on a clean environment.
- [ ] All automated acceptance scenarios in Section 3 pass in CI.
- [ ] Exploratory and usability walkthroughs conducted with zero high-severity ergonomic defects.
- [ ] [INV-VAL-01] verified: No synthetic test backdoors or private mock overrides were used during verification.
```
