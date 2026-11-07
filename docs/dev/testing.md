# Automated Verification & Testing Guide

This guide establishes the automated testing strategy, architectural verification gates, execution commands, and regression prevention practices for developers working on Nuo.

---

## 1. The Test Pyramid

Nuo organizes automated verification across four structured tiers:

```text
                  ▲
                 / \
                /   \     Tier 4: End-to-End System Tests
               / E2E \    (CLI execution, IPC daemon roundtrips)
              /───────\
             /  Integ  \  Tier 3: Subsystem Integration Tests
            /  -ration  \ (Agent rounds, persistence, provider wire)
           /─────────────\
          /   Property    \ Tier 2: Property-Based & Snapshot Tests
         /   & Snapshot    \ (Token estimators, terminal ANSI views)
        /───────────────────\
       /        Unit         \ Tier 1: Fast Unit Tests
      /      Assertions       \ (State machines, decoders, routing)
     /─────────────────────────\
```

---

## 2. Testing Principles

1. **Deterministic Execution**:
   - Tests must NOT depend on live external network connections (e.g., live cloud LLM APIs).
   - Use golden mock fixtures, recorded responses, or mock transports for external providers.
2. **Invariant Verification via Tests (`test_inv_*`)**:
   - Whenever an ADR defines a true system invariant that cannot be enforced purely by the type system at compile time, a dedicated automated test matching `test_inv_*` or asserting the invariant must exist in the appropriate test suite.
   - Example: Ensuring non-destructive Detach keeps background sessions intact.
3. **Purity and Leak-Free Fixtures**:
   - Tests creating temporary directories or database files must use RAII fixtures (e.g., `tempfile`) that clean up on drop, preventing disk pollution.
4. **Fast Feedback**:
   - Tier 1 unit tests must execute in milliseconds to support sub-second developer inner loops.

---

## 3. Running the Test Suite

```bash
# Run unit tests across all workspace crates
cargo test --workspace

# Run tests for a specific crate
cargo test -p nuo
cargo test -p nuo-server
cargo test -p nuo-ui-primitives
cargo test -p nuo-view
```

---

## 4. Repository & Architectural Governance Gates

Nuo enforces foundational repository contracts, structural boundaries, and epistemic integrity through dedicated integration tests in `crates/nuo/tests/`:

### 4.1 Architecture Firewall & Topology
- **Test**: `cargo test -p nuo --test architecture_firewall`
- **Enforces**:
  - `[INV-ARCH-TIER-01]` Flat 3-tier layout (`crates/*`, `providers/*`, `tools/*`) with zero crates at the repository root.
  - Headless server container purity: `nuo-server` must never depend on terminal presentation libraries (`nuotc`, `crossterm`, `nuo-ui-primitives`, `nuo-view`).
  - Source residency: every `.rs` file under a tier root must be reachable from an entry point ([ADR-0124](../adr/0124-source-residency-every-rust-file-reachable-from-a-crate-root.md)).
  - Wire surface census: protocol version bump must coincide with checked-in wire census update ([ADR-0136](../adr/0136-wire-drift-is-a-frame-not-a-disconnect.md)).
  - `[INV-TUI-COLOR-01]` Color purity firewall: presentation code must use semantic theme tokens instead of raw color literals ([ADR-0147](../adr/0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md)).

### 4.2 Content Purity & Invariants
- **Test**: `cargo test -p nuo --test content_purity`
- **Enforces**:
  - `[INV-LAYER-04]` Content payloads carry no presentation layers.
  - `[INV-DIALOG-01]` Dialog surfaces hold no thread capability.
  - Single writer rule for session command ledger.

### 4.3 Tuning & Wire Isolation
- **Test**: `cargo test -p nuo --test tuning_firewall`
- **Enforces**:
  - `[INV-TUNE-01]` No vendor wire encoding or family branch in core vocabulary.
  - `[INV-TUNE-04]` Zero client-side knob resolution.

---

## 5. Manual & Exploratory Verification

For running and manually testing the interactive TUI, headless prompt execution, and server lifecycle from source, consult the [Build & Run Guide](build-and-run.md).
