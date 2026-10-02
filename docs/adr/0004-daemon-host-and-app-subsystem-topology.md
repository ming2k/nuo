---
id: ADR-0004
title: "Pragmatic Crate Granularity and Unified Application Boundaries"
status: accepted
date: 2026-10-01
scope: architecture/apps, daemon/nuo, providers/granularity, workspace/topology
superseded_by: null
negative_knowledge: true
---

# 0004. Pragmatic Crate Granularity and Unified Application Boundaries

- Status: Accepted
- Date: 2026-10-01
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Runtime Team
- Informed: System Architects

---

## Context and Problem Statement

Following the establishment of the `apps/` application boundary (ADR-0003), questions arose regarding the granularity of crate decomposition across two specific areas:

1. **Premature Multi-Crate Partitioning in `apps/nuo`**:
   An initial design explored splitting `apps/nuo` into `apps/nuo/app` (CLI binary) and `apps/nuo/daemon` (daemon supervisor). However, the daemon logic comprised only process detachment, shutdown signals, and diagnostic formatting (~300 lines total). No other workspace crate or external consumer depended on it. Splitting it into an independent Cargo crate introduced artificial manifest overhead, visibility leaks (`pub`), and unnecessary build graph complexity.
2. **Crate Boundary Justification for `nuo-providers`**:
   Conversely, consideration arose whether `crates/nuo-providers` (multi-vendor model adapters) should be merged into `nuo-agent`. Unlike `apps/nuo/daemon`, `nuo-providers` encapsulates heavy external protocol implementations, OAuth2/PKCE flows, multi-modal image decoding, and cryptographic dependencies (`rsa`, `aes`, `cbc`).

We must establish an uncompromised, first-principles standard for **when to split into a Cargo Crate vs when to organize via Rust Modules (`mod`)**.

---

## Decision Drivers

- **First-Principles Simplicity (Occam's Razor)**: Reject cosmetic or "cargo-cult" symmetry. Do not create a Crate where a Rust `mod` is completely sufficient.
- **Rust Compiler Engineering (Parallel Builds & Firewalls)**: Use Crate boundaries where they unlock parallel compilation across CPU cores and shield stable domain cores from volatile third-party API churn.
- **Heavy Dependency Isolation**: Prevent transitive dependency pollution (e.g. image processing, cryptographic suites, OAuth2) from leaking into pure business logic.
- **Zero Legacy Burden**: Eliminate artificial sub-crates immediately without transitional compatibility layers.

---

## The Crate Boundary Razor (Heuristic Standard)

| Decision | Criteria | Canonical Examples |
| :--- | :--- | :--- |
| **Keep as Module (`mod`)** | • Private to a single application/binary.<br>• Small footprint (< 1,000 lines).<br>• Zero external or downstream consumers.<br>• Shares identical lifecycle and dependencies. | **`apps/nuo`** (`mod supervisor`, `mod status`, `mod identity`) |
| **Extract as Crate** | • High compilation cost with multi-core parallelization benefit.<br>• Volatile vendor integration layer (Compilation Firewall).<br>• Pulls heavy/niche native dependencies (crypto, imaging, WASM).<br>• Shared cross-application capability or Hexagonal Adapter (DIP). | **`nuo-providers`** (LLM gateway)<br>**`nuox-render`** (TUI retained-grid engine)<br>**`nuo-client`** (daemon transport SDK) |

---

## Decision Outcome

1. **Consolidate `apps/nuo` as a Single Unified Application Crate**:
   - Eliminated the artificial `apps/nuo/app` and `apps/nuo/daemon` split.
   - `apps/nuo` is a single crate (`nuo`) organized cleanly with internal modules:
     - `cli.rs` and `commands/*`: Operator interface and argument parsing.
     - `supervisor.rs`: Background detachment and process lifecycle.
     - `status.rs`: Daemon diagnostics and terminal status table rendering.
     - `identity.rs`: Default role identity directives.
2. **Consolidate `apps/nuox` and Extract `nuotc` (Nuo Terminal Canvas) to Standalone Repository**:
   - Eliminated the artificial `apps/nuox/app` and `apps/nuox/render` nesting.
   - `apps/nuox` is a single, clean application crate (`nuox`) focused strictly on the TUI product.
   - Extracted the 8,400-line retained-mode 2D character grid and diff engine into a standalone open-source repository at **`../nuotc`** (`git@github.com:ming2k/nuotc.git`) with zero AI domain vocabulary, consumed cleanly via `nuotc = { path = "../nuotc" }`.
3. **Affirm `crates/nuo-providers` as an Isolated Subsystem Crate**:
   - Retained `nuo-providers` in `crates/` as an independent Hexagonal Secondary Adapter.
   - Shields `nuo-agent` from LLM vendor churn and isolates OAuth/crypto dependencies.
4. **Workspace Topology**:
   ```toml
   [workspace]
   members = [
       "apps/nuo",
       "apps/nuox",
       "crates/nuo-agent",
       "crates/nuo-client",
       "crates/nuo-contracts",
       "crates/nuo-persistence",
       "crates/nuo-platform",
       "crates/nuo-providers",
       "crates/nuo-runtime",
   ]
   default-members = ["apps/nuo", "apps/nuox"]
   ```

---

## Pros and Cons of the Options

### Consolidating `apps/nuo` into a Single Crate
- **Positive**: Zero artificial crate overhead; fewer `Cargo.toml` files to maintain.
- **Positive**: Idiomatic Rust: internal implementation details remain unexported.
- **Negative**: None. If external programmatic embedding is ever required in the future, a library target can be added to `apps/nuo` without directory churn.

### Retaining `nuo-providers` as a Separate Crate
- **Positive**: Unlocks full CPU core parallelism during `cargo build`.
- **Positive**: Modifying prompt caching or DeepSeek headers does not trigger full `nuo-agent` recompilation.
- **Negative**: Requires maintaining a separate package manifest.

---

## Rejected Alternatives

### 1. Artificial Sub-Crates under `apps/nuo/` (`app` + `daemon`)
- **Why Rejected**: Premature optimization. `nuo-daemon` was consumed exclusively by `nuo` and consisted of minimal glue code. Crate boundaries exist for dependency decoupling and compilation units, not for cosmetic directory symmetry.

### 2. Merging `nuo-providers` into `nuo-agent`
- **Why Rejected**: Violates Dependency Inversion and destroys the compilation firewall. `nuo-agent` depends only on `dyn Provider` from `nuo-contracts`. Forcing `nuo-agent` to absorb all 8+ concrete vendor network protocols and OAuth/crypto dependencies turns `nuo-agent` into a monolithic compile bottleneck.

---

## Invariants & Validation Contracts

1. **`[INV-ARCH-CRATE-01] Justified Crate Boundary`**:
   A new Cargo crate must not be created unless it satisfies at least one condition of the Crate Boundary Razor (reusability across multiple consumers, heavy dependency isolation, or compilation firewall).
