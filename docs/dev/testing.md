# Automated Verification & Testing Guide

This guide establishes the automated testing strategy, execution commands, and regression prevention practices for developers working on Nuo.

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
cargo test -p nuo-agent
cargo test -p nuo-tui
cargo test -p nuo-server
```
