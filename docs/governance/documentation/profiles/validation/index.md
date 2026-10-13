# Product Validation Profile

Domain capability profile governing the dual-tier product validation standard, industrial standards alignment, and execution decoupling for software engineering repositories.

Activate this profile in `.docgov.yml` under `validation`:
```yaml
validation:
  acceptance_path: "docs/dev/acceptance.md"
  testing_path: "docs/dev/testing.md"
```

---

## 1. The Dual-Tier Validation Model

Software correctness is verified across two complementary tiers:

```text
┌─────────────────────────────────────────────────────────────┐
│ Tier 1: Acceptance (`acceptance.md`)                       │
│ User Journey Verification & Scenario Matrices               │
│ Question: "Does the system deliver what the user needs?"    │
│ Target: Outer-Loop Customer Outcomes & Release Readiness    │
└──────────────────────────────┬──────────────────────────────┘
                               │ Backed by
                               ▼
┌─────────────────────────────────────────────────────────────┐
│ Tier 2: Implementation Testing (`testing.md`)               │
│ Programmatic Test Suites, CLI Commands, Unit/E2E Coverage   │
│ Question: "Is the implementation mechanically correct?"     │
│ Target: Inner-Loop Developer Regression Prevention          │
└─────────────────────────────────────────────────────────────┘
```

| Entity File | Surface | Scope | Focus | Industrial Mapping |
| :--- | :--- | :--- | :--- | :--- |
| [Acceptance Entity](acceptance.md) | `docs/dev/acceptance.md` | Outer loop | End-to-end user journeys, acceptance scenario matrices | ISO/IEC/IEEE 29119-4 (Level 4), ISO 12207 §6.4.8, ISTQB Acceptance |
| [Testing Entity](testing.md) | `docs/dev/testing.md` | Inner loop | Automated test runner commands, unit/integration suites | ISO/IEC/IEEE 29119-4 (Levels 1–3 Component/Integration), Unit/Mocking |

---

## 2. Decoupling Acceptance from Execution Medium

A critical failure in legacy engineering governance is conflating **Acceptance** with **Manual Testing**. Acceptance defines the *contractual requirement and release gate* (What/Why); Manual vs. Automated defines the *execution mechanism* (How).

```text
┌────────────────────────────────┬───────────────────────────────────────────────┐
│                                │             EXECUTION MEDIUM                  │
│                                ├───────────────────────┬───────────────────────┤
│                                │ AUTOMATED (Machine)   │ MANUAL (Human)        │
├───────────┬────────────────────┼───────────────────────┼───────────────────────┤
│           │ Tier 1: ACCEPTANCE │ Automated E2E Suites  │ Exploratory Testing   │
│           │ (Outer Loop)       │ CLI/API Smoke Tests   │ Usability/UX Audit    │
│ TEST      │ "Does it deliver   │ BDD/Contract Runners  │ Release Cold-Start    │
│ LEVEL /   │ what user needs?"  │ (Target: >=90%)       │ Walkthrough (Signoff) │
│ INTENT    ├────────────────────┼───────────────────────┼───────────────────────┤
│           │ Tier 2: TESTING    │ Unit Test Suites      │ Ad-hoc Debugging      │
│           │ (Inner Loop)       │ Integration Tests     │ Manual Breakpoint     │
│           │ "Is implementation │ Mocked Benchmarks     │ Inspection            │
│           │ mechanically ok?"  │ Fast Local Feedback   │ (Ephemeral)           │
└───────────┴────────────────────┴───────────────────────┴───────────────────────┘
```

### Strategic Principles
1. **Automate the Monotonous**: Repetitive manual regression testing ("human runners" executing repetitive test sheets) is technical debt. Predictable acceptance scenarios must be automated in CI/CD.
2. **Elevate Human Intelligence**: Manual testing is preserved strictly for non-deterministic, high-value human activities:
   - **Exploratory Testing**: Probing edge interactions with human curiosity and adversarial intuition.
   - **Usability & Ergonomics Walkthrough**: Assessing visual polish, cognitive load, and documentation clarity.
   - **Cold-Start Sign-off**: Maintainer execution of canonical journeys on clean machines prior to releases.

---

## 3. Directory Layout Bindings

When the `validation` profile is active, the repository establishes:

| Surface | Path | Required | Temperature | Purpose |
| :--- | :--- | :--- | :--- | :--- |
| Acceptance Guide | `docs/dev/acceptance.md` | Yes | **HOT** | Real user journey acceptance matrices & release gates |
| Testing Guide | `docs/dev/testing.md` | Yes | **HOT** | Automated test commands and suite catalog |

---

## 4. Governance Invariant Cross-Reference

- `[INV-VAL-01]`: Real User Journey Parity (No synthetic backdoors; cold-start verification).
- `[INV-VAL-02]`: Deterministic Test Reproducibility (All test commands runnable deterministically).
- `[INV-TEMP-01]`: Hot Data Synchronization (Updated in same PR as user-visible capability changes).
- `[INV-CORE-01]`: Contributor Firewall (Internal validation documents must not be hyperlinked from public docs).
