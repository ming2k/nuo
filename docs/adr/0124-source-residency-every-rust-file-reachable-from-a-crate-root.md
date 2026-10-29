---
id: ADR-0124
title: "Source Residency: Every Rust Source File Must Be Reachable From a Crate Root"
status: accepted
date: 2026-10-29
scope: workspace, governance, testing, cli/dev
superseded_by: null
negative_knowledge: true
---

# 0124. Source Residency: Every Rust Source File Must Be Reachable From a Crate Root

- Status: Accepted
- Date: 2026-10-29
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0111](0111-elimination-of-scripts-and-consolidation-of-workspace-governance-into-rust.md), [ADR-0109](0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md)

---

## Context and Problem Statement

Rust compiles only what a `mod` declaration reaches. A `.rs` file that no declaration
names is therefore **inert**: it produces no compile error, no warning, and no test
failure — it simply stops existing while still looking like source in every diff, review,
and `grep`.

The repository has paid for this failure mode twice, and the second time it silently
destroyed a regression net:

1. `crates/nuo/src/shell/snapshot_tests.rs` — 2469 lines, 56 test functions, 24 insta
   baselines, and the crate's **only** painted-grid coverage of the tool-step, notice,
   command-card, and turn-band geometry — lost its `mod snapshot_tests;` line when
   commit `29cb306` (ADR-0109/0111) relocated it from `crates/nuo-tui/src/` to
   `crates/nuo/src/shell/`. The file moved; the registration did not. It sat uncompiled
   for multiple releases while every geometry ADR after it (ADR-0115's closed fold,
   ADR-0116's content residency, ADR-0121's notice dock) changed the very surfaces it
   pinned.
2. `crates/nuo-protocol/src/provider_auth.rs` — a passthrough re-export that the
   architecture standards explicitly prohibit (§2.3) — lost its `pub mod provider_auth;`
   line in commit `a21978b` and remained on disk as a file no compiler ever saw.

Both are invisible to every gate the repository has. `cargo check` is green,
`cargo test --workspace` is green, `cargo clippy` is green, the presentation firewall is
clean, and doc links resolve. The defect is not in the code — it is in the **absence of
reachability**, which no per-file tool can observe.

The real cost was deferred and misattributed. When the turn-band `HH:MM` header was found
to be missing its send time, the natural response was "add a test" — but the test that
would have caught it already existed, in a file nothing compiled. The residency defect is
therefore not a tidiness issue: it **silently disarms the test suite**, which is the
mechanism this project relies on for every other invariant.

## Decision Drivers

- **A green build must mean something.** Every gate in this repository is expressed as a
  compiler check, a firewall rule, or an automated test (`docs/dev/standards/invariants.md`
  §1). A whole-tree property that no gate can observe violates that model directly.
- **Blast radius is unbounded.** An unreachable file can hide production code (a real
  behaviour never shipped), a security control, or — as happened — an entire crate's
  regression coverage. The failure is silent in proportion to how much the file mattered.
- **Review cannot catch it.** The file is present, the tests read as real, and the diff
  that orphaned it is one deleted line among hundreds. Only whole-tree reachability can.
- **Governance belongs in Rust** (ADR-0111). This check is a native module and a
  first-class test, not a script.

## Architectural Decisions

### 1. Reachability is defined over Cargo's real compilation model

`nuo::dev::residency` walks each crate under the tier roots (`crates/*`, `providers/*`,
`tools/*`, `[INV-ARCH-TIER-01]`) from the entry points Cargo itself would compile:

- `src/lib.rs` and `src/main.rs`, both unconditionally (tests compile against the union).
- Every auto-discovered target root in `tests/`, `benches/`, `examples/` — one crate root
  per `.rs` file directly in those directories.

`mod` declarations are then followed transitively, honouring every layout Rust accepts:
`name.rs`, `name/mod.rs`, the RFC 2126 sibling directory (`foo.rs` → `foo/bar.rs`), and an
explicit `#[path = "…"]` override (resolved relative to the declaring file).

The scan is a small Rust lexer rather than a regex: it masks comments (including nested
block comments), normal and raw string literals, and char literals before looking for the
`mod` keyword, so prose, doc-comments, and a string that spells a declaration cannot forge
one. Byte offsets are computed against the original text so `#[path]` values remain
comparable with the declarations they annotate.

### 2. Target assets are not modules

`tests/<target>/…` is a source module tree only for the `tests/<target>.rs` root that
declares it. When no such root exists, the directory holds data the tests read — a parser
corpus, a fixture, a sample the code under test ingests. Those `.rs` files are **inputs**,
not crate modules, and are excluded. This distinction is what lets a Rust-parsing tool ship
a corpus of Rust fixtures without tripping its own gate.

### 3. The gate is a test, not a habit

Enforcement is `crates/nuo/tests/architecture_firewall.rs::test_inv_arch_source_residency`,
which runs inside `cargo test --workspace`. The same check is reachable from the developer
CLI as `nuo dev check residency` (and `all`).

### Normative Invariants Established

- `[INV-ARCH-RESIDENCY-01]` **Every `.rs` file under a tier root is reachable from a crate
  root by `mod` declarations.** Enforced by `test_inv_arch_source_residency`; reported as
  one grouped failure listing every orphan per crate, so a backlog is fixed in one pass
  rather than one file per run. Two remedies exist and no third: declare the module, or
  delete the file.

## Consequences

- **Positive**: The silent-disarm failure mode is closed at the only layer that can see it.
  Restoring `snapshot_tests.rs` to compilation immediately surfaced **three** defects that
  no gate could previously report — a baseline predating ADR-0115's closed fold geometry,
  a notice-body wrapping bug that dropped every continuation row of a multi-line notice,
  and the second orphan (`nuo-protocol/src/provider_auth.rs`). That is the expected yield:
  the check does not just prevent future loss, it recovers what was already lost.
- **Negative & Trade-offs**: The check is a lexer, not a `rustc` invocation, so it can be
  wrong about exotic syntax. It is deliberately conservative: ignoring `cfg` gating means
  a file behind `#[cfg(test)] mod` counts as reachable (correct — it is compiled), and
  `#[path]` is honoured rather than interpreted. Anything the lexer cannot resolve falls
  through to "reachable" only when a real candidate file exists, so the failure mode is a
  missed orphan, never a false accusation.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Rely on `cargo build` warnings (Rejected)

- **Why considered**: Zero new code.
- **Why rejected**: Rust has no diagnostic for an unreferenced file. There is no warning to
  elevate. This alternative is the status quo that produced both incidents.

### Alternative B: A blanket `#![deny(unused)]`-style lint / `include_str!` assertion (Rejected)

- **Why considered**: Would keep the check inside the compiler.
- **Why rejected**: `unused` does not model files. Any assertion strong enough to notice an
  unregistered file must itself enumerate the filesystem, at which point it is this check
  with worse error reporting.

### Alternative C: Adopt the check only as a manual `nuo dev check` verb (Rejected)

- **Why considered**: Cheaper than wiring it into the test suite; avoids slowing
  `cargo test`.
- **Why rejected**: A gate nobody runs is not a gate. It measured at ~0.16 s over 843 files.
  The decision was made to make it a test precisely because the failure mode is one that
  human diligence had already missed twice.

### Alternative D: Report only the first orphan (Rejected)

- **Why considered**: Simpler output.
- **Why rejected**: Restoring `snapshot_tests.rs` alone would have hidden
  `provider_auth.rs`. The check groups all orphans per crate so one run reveals the full
  backlog, which is what made the second incident visible on the first run.

## Links

- Related ADRs: [ADR-0111](0111-elimination-of-scripts-and-consolidation-of-workspace-governance-into-rust.md) (native Rust governance engine), [ADR-0109](0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md), [ADR-0105](0105-presentation-linkage-mechanism-and-dependency-firewall.md) (the sibling firewall), [ADR-0115](0115-command-output-fold-geometry-closed-compaction-and-disclosure-ladder.md) (the geometry the orphaned baseline silently stopped pinning)
- Standards: [`docs/dev/standards/invariants.md`](../dev/standards/invariants.md),
  [`docs/dev/testing.md`](../dev/testing.md), [`docs/dev/standards/architecture.md`](../dev/standards/architecture.md)
