# Automated Verification & Testing Guide

This guide is the operating manual for Nuo's layered test architecture. The
binding decision is [ADR-0178](../adr/0178-layered-test-architecture-and-hermetic-e2e-boundary.md);
this document explains how to work inside it.

Nuo uses **cargo-nextest for every executable test tier**. `cargo test` is not
the project test runner; there are no `xtask` test commands and no shell test
drivers. Doctests are not executable test targets in this repository;
`cargo doc --workspace --no-deps` owns the documentation contract.

The architecture is a test pyramid with a governance base:

```text
                         ┌─────────────────────────────────────┐
                         │ L3  E2E                             │  real binary, real IPC,
                         │     tests/e2e/main.rs               │  isolated NUO_HOME
                         ├─────────────────────────────────────┤
                         │ L2  Integration                     │  crate boundaries, fakes,
                         │     tests/integration/main.rs       │  temp dirs, no real binary
                         ├─────────────────────────────────────┤
                         │ L1  Unit                            │  inline `mod tests` or
                         │     src/**/tests.rs                 │  sibling `<module>/tests.rs`
                         ├─────────────────────────────────────┤
                         │ L0  Governance & contract           │  architecture firewall,
                         │     tests/integration/governance/   │  provider contract, meta
                         └─────────────────────────────────────┘
              cross-cutting: snapshots (insta), property tests, live-network
              probes explicitly quarantined with `#[ignore = "live network"]`
```

The pyramid is about **cost and failure locality**, not test count. A unit test
that needs a temp dir is an integration test; an integration test that spawns
the real binary is an E2E test. Put it where its dependencies say it belongs.

---

## 1. Tier Contract

| Tier | Location | May use | Must not use | Runtime budget |
| :--- | :--- | :--- | :--- | :--- |
| **L1 Unit** | `src/**` inline `#[cfg(test)] mod tests`, or sibling `src/<module>/tests.rs` | Pure functions, in-memory state, `tokio::test` with paused time | Network, real `NUO_HOME`, subprocesses, sleeps | milliseconds |
| **L2 Integration** | Root `tests/integration/main.rs` + modules | Every workspace crate, temp dirs, local mock HTTP, in-process registries | The shipped `nuo` binary, detached daemons, live providers | seconds |
| **L3 E2E** | Root `tests/e2e/main.rs` + modules | The freshly built binary, real UDS/WS, real SQLite, `nuo-test-support::MockProvider` | Shared `NUO_HOME`, live providers, process-global `env::set_var` | tens of seconds |
| **L0 Governance** | `tests/integration/governance/`, per-crate `layering.rs` folded into the same entry | Source/manifest scans, AST, `cargo tree` | Business behaviour | milliseconds |

The production workspace remains multi-crate: `crates/*`, `providers/*`, and
`tools/*` are unchanged. The root package `nuo-tests` is a **dev-only test host**
that exists solely to compile external tests into two binaries.

---

## 2. Unit Test Placement

The rule is mechanical and reviewable:

- **Small and medium modules**: inline `#[cfg(test)] mod tests { ... }` beside
  the code it pins.
- **Large test modules**: when the inline test block grows to roughly 300 lines
  or more, move it to a sibling file and keep a two-line declaration:

  ```text
  src/foo.rs              ->  src/foo/tests.rs
  src/foo/mod.rs          ->  src/foo/tests.rs
  src/lib.rs              ->  src/tests.rs
  ```

  ```rust
  #[cfg(test)]
  mod tests;
  ```

  The module path is unchanged (`foo::tests`), so `use super::*`, private items,
  and test names remain valid without a single production change.

The current tree follows this threshold: the largest inline test modules were
externalized, and `tests/integration/governance/test_architecture.rs` rejects a
new per-crate `tests/` directory.

Unit tests must not create a second test architecture. Do not add a `tests/`
directory to a production crate; external tests belong to the root entries.

---

## 3. External Test Host

External tests are compiled by the root `nuo-tests` package, not by each crate:

```text
tests/
  common/mod.rs              shared path/repo helpers
  integration/
    main.rs                  L2 entry
    acp/ engine/ governance/ nuo_host/ persistence/ provider_qoder/ server/ tool/ ...
  e2e/
    main.rs                  L3 entry
    headless_roundtrip.rs    real turn against the mock provider
    server_spawn.rs          real server lifecycle/IPC
    smoke.rs                 read-only CLI contract
```

- `tests/integration/main.rs` declares every L2 module. Cargo links **one**
  integration binary instead of one per crate/file.
- `tests/e2e/main.rs` is Unix-gated and declares every L3 module. Cargo links
  **one** E2E binary.
- `tests/common/mod.rs` is included by both entries with
  `#[path = "../common/mod.rs"]`. It owns repository-path helpers only; process
  sandboxes live in `nuo-test-support`.
- The root package sets `autotests = false` and declares only the two `[[test]]`
  entries. Do not add a third external test target; add a module to one entry.

### L2: Integration tests

- May cross module boundaries inside a crate and may use real filesystem/SQLite
  under a `tempfile` root.
- May stand up a local mock HTTP server (the provider wire suite uses `mockito`).
- Must **not** spawn the shipped binary. The architecture gate rejects
  `NUO_TEST_BIN` / `CARGO_BIN_EXE_nuo` under `tests/integration/`.
- Architecture firewalls, layering checks, provider contracts, and the test
  architecture gate live under `tests/integration/governance/`.

### L3: End-to-end tests

- Every test gets a `nuo_test_support::Sandbox`: a private `NUO_HOME`
  (`config`, `state`, `data`, `cache`, `instance`), a project workspace, and a
  drop guard that stops a leaked server.
- The only upstream is `nuo_test_support::MockProvider`: a deterministic
  OpenAI-compatible HTTP/SSE surface. Live-network tests are not E2E; they are
  quarantined probes (`#[ignore = "live network"]`).
- Environment is passed to the **child process**, never installed in the test
  process. This is what lets E2E tests run under nextest without data races.
- E2E tests execute serially via the nextest `e2e-serial` test group. A leaked
  daemon or socket is a bug, not something the next test should tolerate.

The root test host cannot receive Cargo's `CARGO_BIN_EXE_nuo` (that variable is
only set for tests of the package that owns the binary). The nextest `e2e`
profile therefore runs a setup script:

```text
cargo build -p nuo --bin nuo
NUO_TEST_BIN=<target>/debug/nuo
```

`nuo_test_support::binary_path()` reads `NUO_TEST_BIN`. Always enter E2E
through `cargo nextest run -p nuo-tests --profile e2e`; invoking the target
through libtest would bypass the setup script and leave `NUO_TEST_BIN` unset.

---

## 4. The Shared Harness (`nuo-test-support`)

`crates/nuo-test-support` is a **dev-only** crate. It is deliberately outside
the shipped dependency graph, and `test_architecture.rs` fails if any
`[dependencies]` or `[build-dependencies]` section declares it.

| API | Purpose |
| :--- | :--- |
| `Sandbox::new()` | Private `NUO_HOME` + workspace under a `TempDir` |
| `Sandbox::configure_openai_mock(..)` | Writes `model_providers.toml`, `connections.toml`, `credentials.toml`, `client.toml` for one static mock route |
| `Sandbox::command(bin)` | `Command` with `NUO_HOME` (and ambient `NUO_*_DIR` removed) and cwd set to the workspace |
| `Sandbox::wait_for_discovery(..)` | Polls for `server.json` and parses `pid`/`version`/`port`/`token` |
| `Sandbox::server_cleanup(bin)` | Drop guard that runs `nuo server stop`, then kills the recorded PID if needed |
| `MockProvider::with_response(..)` | Local OpenAI-compatible HTTP/SSE provider; records method/path/headers/body |
| `Cli::new(bin, &sandbox)` | Bounded CLI runner with pipe-drain threads and a kill-on-timeout watchdog |
| `binary_path()` | Resolves the fresh `NUO_TEST_BIN` exported by the nextest E2E setup |
| `eventually(..)` | Deadline-based polling for process-level facts |

---

## 5. Running the Suites

The checked-in `.config/nextest.toml` defines three profiles. They are the
mechanical definition of the tiers:

```bash
# L1 — fast inner loop
cargo nextest run --workspace --profile unit

# L2 — every external integration suite, one linked binary
cargo nextest run --workspace --profile integration

# L3 — real binary, real IPC, hermetic provider
cargo nextest run -p nuo-tests --profile e2e

# The whole pyramid, in cost order
cargo nextest run --workspace --profile unit
cargo nextest run --workspace --profile integration
cargo nextest run -p nuo-tests --profile e2e
```

Useful one-off commands:

```bash
# One crate's unit tests
cargo nextest run -p nuo-engine --profile unit

# One test name, any tier
cargo nextest run -E 'test(headless_turn_runs_against_a_hermetic_provider)'

# One governance gate
cargo nextest run -p nuo-tests --profile integration \
  -E 'test(governance::architecture_firewall::test_inv_arch_firewall_and_topology)'

# Show the concrete selection without running
cargo nextest list --workspace --profile integration

# Re-run only failures from the last invocation
cargo nextest run --profile integration --rerun

# Run a quarantined live-network probe deliberately
cargo nextest run -p nuo-tests --profile integration \
  -E 'test(webtool_e2e)' --run-ignored ignored-only --no-capture
```

`cargo test` is not a supported project command. Do not add `xtask`, shell
driver scripts, or a second test runner for any tier.

---

## 6. Determinism and Isolation Rules

1. **No live network in the required suite.** Provider HTTP is always a local
   mock. Live probes stay `#[ignore = "live network"]` and are run manually.
2. **No shared `NUO_HOME`.** E2E tests pass their sandbox explicitly. Integration
   tests that need process-wide paths use `TestDirsGuard` / `sandbox_once`
   before the first path resolution.
3. **No process-global environment mutation in E2E.** nextest runs tests in one
   process per binary with parallel threads; `env::set_var` is a data race.
4. **RAII cleanup.** Temp directories use `tempfile`; servers use
   `ServerCleanup`/`ChildGuard`; mock sockets close on `Drop`.
5. **Bounded waits.** Use `eventually`, nextest `slow-timeout`, or tokio paused
   time. A bare `sleep(5s)` is a flake with a delay attached.
6. **No stale binaries.** E2E uses the path exported by the nextest setup
   script, never a path probe for an incidental `target/debug/nuo`.

---

## 7. Snapshots and Golden Data

- Terminal output uses `insta` snapshots. A test's snapshot directory follows
  the test source file: `src/foo/tests.rs` reads `src/foo/snapshots/`. When a
  large test module is externalized, move its snapshots with it.
- Review changes with `cargo insta review`; commit accepted `.snap` files.
  `test_architecture.rs` rejects uncommitted `.snap.new` / `.pending-snap` files.
- Wire fixtures are golden JSON/SSE payloads committed beside the integration
  module (`tests/integration/provider_qoder/fixtures`, etc.). A golden change is
  a contract change: explain it in the commit and update the protocol census
  when the wire surface changes.

---

## 8. CI Mapping

| CI step | Command | Gate |
| :--- | :--- | :--- |
| Format | `cargo fmt --all --check` | required |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | required |
| L1 | `cargo nextest run --workspace --profile unit` | required |
| L2 | `cargo nextest run --workspace --profile integration` | required |
| Test architecture | `cargo nextest run -p nuo-tests --profile integration -E 'test(governance::test_architecture::)'` | required |
| Architecture firewall | nextest integration filter on `governance::architecture_firewall` | required |
| Docs | `cargo doc --workspace --no-deps` | required |
| L3 | `cargo nextest run -p nuo-tests --profile e2e` | required, Ubuntu |
| Coverage | `cargo llvm-cov nextest --workspace --summary-only` | advisory (`workflow_dispatch`) |

Platform checks (`cargo check --workspace --all-targets` on macOS/Windows) stay
separate. E2E is Unix-gated because it asserts process-group and Unix-socket
semantics; the portable tiers still compile and run on every platform.

---

## 9. Coverage

Coverage is a diagnostic, not a vanity gate. A line covered by an assertion-free
test is worse than an uncovered error path with a documented reason.

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked
cargo llvm-cov nextest --workspace --summary-only
cargo llvm-cov nextest --workspace --html   # target/llvm-cov/html/index.html
```

`cargo llvm-cov nextest --workspace` runs every nextest test binary, including E2E. Process
boundaries make a workspace-wide percentage approximate; use `-p`/`--test` to
isolate a tier when that distinction matters.

---

## 10. Flakes, Quarantine, and Ignored Tests

- Default retries are **zero**. The `e2e` profile allows one retry because
  process startup/socket binding is genuinely timing-sensitive; every retry is
  reported by nextest as flaky and should be investigated.
- A test that needs a live vendor or a human is quarantined:
  `#[ignore = "live network"]` or `#[ignore = "<reason>"]`.
  Bare `#[ignore]` fails the architecture gate.
- Do not "fix" a flake with a larger sleep. Find the missing synchronization
  edge (discovery file, cancellation token, process exit, socket bind).

---

## 11. Adding Tests: Checklist

When adding a feature:

1. **L1** — pure logic: inline `mod tests` for small/medium modules, or a
   sibling `<module>/tests.rs` when the test block would exceed ~300 lines.
2. **L2** — crossing a crate/module boundary: add a module under
   `tests/integration/` and declare it in `tests/integration/main.rs`.
3. **L3** — a user-visible flow through the binary: add a module under
   `tests/e2e/` and declare it in `tests/e2e/main.rs`.
4. **Contract** — if the change adds a provider, tool, wire shape, or invariant,
   extend the table-driven governance suite rather than a one-off assertion.
5. **Naming** — `test_<behaviour>` for behaviour, `test_inv_<area>_<ordinal>`
   for an ADR invariant; names state the contract, not the implementation.
6. **No new external target** — exactly one integration entry and one E2E entry.

---

## 12. Manual & Exploratory Verification

For the interactive TUI, headless runs, and server lifecycle from source, see
the [Build & Run Guide](build-and-run.md). Manual checks complement the suite;
they never replace a required gate.
