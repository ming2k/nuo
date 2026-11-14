---
id: ADR-0178
title: "Layered test architecture, single external entries, and nextest-only execution"
status: accepted
date: 2026-11-14
scope: [workspace, testing, ci, e2e, provider-contracts]
superseded_by: null
negative_knowledge: true
---

# 0178. Layered Test Architecture, Single External Entries, and Nextest-Only Execution

- Status: Accepted
- Date: 2026-11-14

---

## Context and Problem Statement

The production workspace is correctly multi-crate, but its tests had accumulated
external test targets per crate and per file. Cargo links every integration test
file into its own binary, so the same provider, engine, and persistence stacks
were linked dozens of times; a change in a shared dependency paid that cost
repeatedly. The tiers were also implicit:

- E2E coverage (`CARGO_BIN_EXE_nuo`, detached servers, UDS round trips) lived in
  the same Cargo target as in-process integration tests.
- Every suite re-invented sandbox setup, environment handling, provider mocks,
  and timeout/kill cleanup.
- Large inline `#[cfg(test)] mod tests` blocks made business modules hard to
  read, even when the tests were pure unit coverage.
- CI ran one `cargo test --workspace` bucket, and contributors had two test
  runners to reason about.

The repository needs one explicit shape: small inline unit tests, carved-out
large unit-test modules, two external entries (integration and E2E), and
cargo-nextest as the only executable test runner.

---

## Architectural Decisions

### 1. Keep the multi-crate workspace; add a dev-only root test host

`crates/*`, `providers/*`, and `tools/*` remain the production topology. The
root `Cargo.toml` additionally declares a `nuo-tests` package with:

- `autotests = false`;
- exactly `[[test]] name = "integration" path = "tests/integration/main.rs"`;
- exactly `[[test]] name = "e2e" path = "tests/e2e/main.rs"`.

`nuo-tests` is `publish = false`, has no shipped library, and links every
workspace crate only through `[dev-dependencies]`. It is a test host, not a
fourth production tier.

### 2. One integration entry, one E2E entry

All external tests live under the root `tests/` directory:

```text
tests/
  common/mod.rs
  integration/main.rs          # every L2 module, one linked binary
  e2e/main.rs                  # every L3 module, one linked binary
```

- A new integration suite is a module under `tests/integration/`, declared from
  `tests/integration/main.rs`.
- A new E2E suite is a module under `tests/e2e/`, declared from
  `tests/e2e/main.rs`. The entry is `#![cfg(unix)]`.
- No production crate may own a `tests/` directory. This removes the repeated
  linker invocations and gives each tier one dependency graph.
- `tests/common/mod.rs` is the shared home for repository/path helpers. Process
  sandboxes remain in the dedicated test-support crate.

### 3. Unit tests stay beside the module; large test blocks use a sibling file

- Small and medium modules use inline `#[cfg(test)] mod tests`.
- When an inline test block reaches roughly 300 lines, it moves to the sibling
  path for that module:
  - `src/foo.rs` -> `src/foo/tests.rs`
  - `src/foo/mod.rs` -> `src/foo/tests.rs`
  - `src/lib.rs` -> `src/tests.rs`
- The parent keeps only:

  ```rust
  #[cfg(test)]
  mod tests;
  ```

The module path (`foo::tests`) does not change, so private-item access, `use
super::*`, and test names are unchanged. A large module whose complexity is
business logic still keeps its tests beside it; the move is about test-code
volume, not about demoting business coverage to integration tests.

Snapshot directories follow the test source file (`src/foo/snapshots/`), so an
externalized test module moves its snapshots with it.

### 4. `nuo-test-support` is the only shared test infrastructure

`crates/nuo-test-support` is a dev-only crate (`publish = false`) providing:

- `Sandbox`: private `NUO_HOME`, workspace, and ambient `NUO_*_DIR` isolation;
- `MockProvider`: deterministic OpenAI-compatible HTTP/SSE server with request
  recording;
- `Cli`: bounded runner for the real binary;
- `ServerCleanup` / `ChildGuard`: RAII process cleanup;
- `binary_path()` / `eventually()`: E2E process plumbing.

It must never appear in a shipped `[dependencies]` or `[build-dependencies]`.

### 5. cargo-nextest is the only executable test runner

`.config/nextest.toml` defines exactly three profiles:

- `unit`: `kind(lib) | kind(bin) | kind(proc-macro)`
- `integration`: `kind(test) and not binary(e2e)`
- `e2e`: `binary(e2e)`

No `xtask`, shell test driver, or second test runner is permitted. The root test
host cannot receive Cargo's `CARGO_BIN_EXE_nuo` because it is not the package
that owns the binary; the `e2e` profile therefore runs a nextest setup script
that builds `nuo` and exports `NUO_TEST_BIN`. `nuo_test_support::binary_path()`
consumes that variable.

E2E is assigned to the `e2e-serial` test group (`max-threads = 1`) because it
owns sockets, process groups, and sandbox roots. Default retries are zero; the
E2E profile allows exactly one retry for process-startup timing only.

Doctests are not executable in nextest. The repository currently has no active
doctests; `cargo doc --workspace --no-deps` owns documentation compilation.

### 6. Governance is itself tested

`tests/integration/governance/test_architecture.rs` enforces:

- every workspace crate owns at least one inline/sibling unit test (or a
  reason-bearing exemption);
- no production crate owns a `tests/` directory;
- exactly one integration entry and one E2E entry exist;
- E2E uses `binary_path`, never `CARGO_BIN_EXE_nuo` or a path probe;
- no process-global `env::set_var` in E2E;
- every `#[ignore]` carries a reason;
- `nuo-test-support` stays dev-only and `publish = false`;
- the nextest profiles and the E2E setup script exist;
- no unreviewed insta snapshot is committed.

`tests/integration/nuo_host/it/provider_contract.rs` iterates
`nuo_server::provider_registry::MODEL_PROVIDER_SPECS` and asserts unique
kebab-case ids, valid roots/endpoints, unique headers, unique baselines, and
dialect/protocol compatibility.

### Normative Invariants

- `[INV-TEST-01] Every Crate Owns a Unit Test`: each workspace crate has at
  least one inline or sibling unit test, or a reason-bearing exemption.
- `[INV-TEST-02] E2E Is Real and Isolated`: L3 runs the binary built by the
  nextest setup script, in a private `NUO_HOME`, with no live provider and no
  process-global env mutation.
- `[INV-TEST-03] Tests Are Hermetic`: required suites use local mocks and RAII
  temp roots; live-network probes are explicitly `#[ignore = "live network"]`.
- `[INV-TEST-04] Two External Entries`: exactly one root integration entry and
  one root E2E entry; production crates own no external test targets.
- `[INV-TEST-05] Quarantine Is Explained`: every ignored test states why.
- `[INV-TEST-06] Shared Test Code Is Not Shipped`: `nuo-test-support` is
  dev-only and never published.
- `[INV-TEST-07] Large Unit-Test Blocks Are Files`: an inline test block that
  reaches the review threshold moves to the module's sibling `tests.rs`.

Enforcement: `tests/integration/governance/test_architecture.rs`,
`tests/integration/nuo_host/it/provider_contract.rs`,
`.config/nextest.toml`, and the CI workflow.

---

## Consequences & Trade-offs

- **Positive**: external tests link once per tier instead of once per file;
  failures identify L1/L2/L3; the inner loop never starts a daemon; a new crate
  or provider cannot silently opt out of coverage; sandbox cleanup survives
  panics.
- **Positive**: large business modules stay readable, and unit tests keep their
  module-scoped privacy with no production API widening.
- **Positive**: one runner and one checked-in profile definition remove the
  `cargo test` vs nextest ambiguity and forbid xtask drift.
- **Negative**: the root `nuo-tests` package is a non-production package at the
  workspace root. Accepted because it is `publish = false`, dev-only, and
  explicitly outside the three production tiers.
- **Negative**: the E2E setup script pays one `cargo build -p nuo` per E2E run.
  Mitigation: the build is incremental and the E2E job is isolated.
- **Negative**: coverage is not a hard percentage gate. Accepted: assertion
  quality and failure locality matter more than a number that invites
  assertion-free tests.

---

## Discarded Alternatives (Negative Knowledge)

- **Keep per-crate external test targets**: rejected because every target is a
  separate link of the full dependency graph. The root entries make the cost
  proportional to tiers, not to files.
- **Keep one `cargo test --workspace` bucket**: rejected because a slow daemon
  test blocked the unit inner loop and CI could not attribute a failure to a
  tier.
- **Add a third external test target for governance**: rejected. Governance is
  an L2 module under `tests/integration/governance/`; a third target recreates
  the link cost the architecture removes.
- **Keep all unit tests inline**: rejected for large blocks. A 1,000-line test
  module inside a 1,700-line production file is test bloat, regardless of how
  important the business logic is.
- **Move all unit tests to the root integration entry**: rejected. That would
  lose module-private access and turn pure logic into a linked integration
  suite.
- **Discover `target/debug/nuo` by path**: rejected. It can be stale and makes
  E2E depend on an incidental artifact. The nextest setup script exports the
  exact binary it just built.
- **Write a shell/Python tier runner or xtask**: rejected. Nextest profiles and
  setup scripts are declarative, checked in, and native to the runner.
- **Gate on a coverage percentage**: rejected as a primary gate. It optimizes
  for executed lines, not asserted contracts.
