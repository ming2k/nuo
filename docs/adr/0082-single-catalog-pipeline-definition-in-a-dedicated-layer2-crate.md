---
id: ADR-0082
title: "Single Catalog Pipeline Definition in a Dedicated Layer-2 Crate"
status: accepted
date: 2026-10-19
scope: workspace/topology, catalog/nuo-model-routing, server/nuo-server, harness/nuo-harness
superseded_by: null
negative_knowledge: true
---

# 0082. Single Catalog Pipeline Definition in a Dedicated Layer-2 Crate

- Status: Accepted
- Date: 2026-10-19
- Deciders: Nuo Architecture Working Group
- Consulted: Agent Runtime, Provider Integration, Model Infrastructure, Presentation/TUI
- Informed: Core Engineering
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0010](0010-harness-decomposition-and-agent-unification.md)
- Amended by: [ADR-0102](0102-model-routing-aperture-rename-nuo-catalog-to-nuo-model-routing-and-cleanse-route-derivation-semantics.md)
- Implements: [ADR-0081](0081-unidirectional-catalog-pipeline-structural-normalization-and-relay-noise-hygiene.md)

---

## Context and Problem Statement

ADR-0081 made the catalog a **unidirectional pipeline** — Source, then Mapping, then
Overwrite — and moved display-label resolution into it. That decision fixed the *shape*
of the pipeline. It did not fix the pipeline's **cardinality**: the code had two copies.

`catalog/{derive,picker,sync,tests}.rs` existed twice, byte-for-byte identical
(4 536 lines each), in `crates/nuo-harness/src/catalog/` and
`crates/nuo-server/src/catalog/`. Both copies were live: the harness derives entries and
refreshes catalogs inside `rounds.rs`/`orchestration.rs`, and the server derives them
through `handlers_provider.rs`, `session_driver.rs`, `kernel_host.rs`, `serve.rs`, and
`bootstrap.rs`. Neither copy was vestigial, so neither could simply be deleted.

The ordinary fix — let one crate use the other's module — is barred by the dependency
edge that already exists: `nuo-server` depends on `nuo-harness`, so the harness cannot
name the server's module, and the server's copy could therefore never become the only
copy.

The drift cost is not theoretical. During ADR-0081's own implementation, every edit to
the mapping stage, the filter policy, and the hygiene valve had to be applied twice;
the two `picker.rs` files already carried a one-line divergence
(`crate::provider_registry` vs `nuo_server::provider_registry`) that a mechanical copy
would have propagated silently. A pipeline whose behaviour depends on which of two
identical files a maintainer remembered to edit has no single definition, which is
precisely what ADR-0081 §2 forbids for *labels* and what `[INV-PROV-06]` forbids for
*provider specs*.

## Decision Drivers

- **Single definition over synchronization discipline**: an invariant that relies on
  "remember to edit both files" is an aspiration, not a mechanism (`docs/dev/standards/invariants.md` §1).
- **Cargo dependency direction is the only enforcement that cannot be forgotten**: the
  duplication must become *unrepresentable*, not *discouraged*.
- **Layer purity**: the pipeline must not link presentation crates (`nuo-tui`, `nuotc`)
  and must not link any concrete `providers/*` crate (`[INV-PROV-04]`, `[INV-PROV-09]`).
- **No path shims**: a `pub use nuo_x::y;` alias in a consumer is a passthrough conduit
  (`docs/dev/standards/architecture.md` §2.3) and hides which crate owns the code.

## Architectural Decisions

### 1. `[INV-CATALOG-07]` The Catalog Pipeline Has Exactly One Definition

The pipeline lives in exactly one crate, `crates/nuo-model-routing` (package `nuo-model-routing`):
`derive` (Source → per-model route), `picker` (the presentation snapshot), `sync`
(remote-catalog fetch + fitted overlay), and their tests. Consumers link it; they never
host a second copy.

The enforcement is the workspace itself: there is no second module to drift, and a
re-introduced copy would need a new `pub mod catalog` plus a duplicated ~4 500-line test
suite to compile alongside it — a visible, reviewable act rather than a silent copy-paste.

Its position is **Layer 2 (Core Runtime Substrate)**, beside `nuo-persistence`:

```text
Layer 1  nuo-wire (protocol) · nuo-provider · nuo-model-codec
             ▲
Layer 2  nuo-model-routing · nuo-persistence · nuo-agent · nuo-harness
             ▲
Layer 3  nuo-server
```

`nuo-model-routing` depends only on Layer-1 contracts (`nuo-wire`, `nuo-provider`,
`nuo-model-codec`) and on `nuo-persistence` for the stores it reads. It never names a
concrete provider: remote catalog fetching is dispatched through
`nuo_provider::fetch_remote_catalog`, whose implementation the composition root supplies.

### 2. Consumers Import It Directly, Never Through a Shim

`nuo-harness` and `nuo-server` call `nuo_model_routing::…` at every site. Neither re-exports
the crate as `crate::catalog`. A crate-level re-export would restore the illusion of a
local module and re-open the question of which crate owns the pipeline — the exact
ambiguity this ADR removes.

### 3. The Composition Root Edge Is Test-Only, Declared and Commented

The catalog tests exercise the real spec table and the real credential-source builders,
which only the composition root owns (`[INV-PROV-04]`). `nuo-model-routing` therefore
**dev-depends** on `nuo-server`. Cargo admits a cycle only when it passes through a
dev-dependency, so the shipped graph (`nuo-server` → `nuo-model-routing`) stays acyclic; the
manifest states this, and `nuo-harness` declares the same edge for the same reason. This
is the only backward-looking edge in the workspace, and it is impossible to ship: no
`[dependencies]` entry exists in either direction it would need.

## Consequences

### Positive Consequences

- **One edit, one behaviour**: the mapping stage, filter policy, and hygiene valve exist
  once. A change cannot reach the server and miss the harness.
- **Test parity by construction**: the 61 catalog tests now run once against the code
  both consumers execute — previously they ran twice against two copies, so a divergence
  could pass both suites independently.
- **The layer diagram became true**: "catalog derivation" is no longer a hidden subsystem
  duplicated inside two Layer-2/3 crates; it is a named Layer-2 crate with an explicit
  dependency set.

### Negative Knowledge & Prohibitions

- **Prohibition on Re-Duplication**: Do not reintroduce a `catalog` module inside
  `nuo-harness` or `nuo-server`, "temporarily" or otherwise. If a consumer needs
  different behaviour, it composes the crate's API; it does not fork it. The historical
  justification ("the harness needs it at runtime, the server needs it at runtime, and
  they cannot see each other") is exactly what the Layer-2 crate resolves.
- **Prohibition on Path Shims**: `pub use nuo_model_routing as catalog;` (or any equivalent
  alias) in a consumer crate root is a red-line violation of `architecture.md` §2.3.
- **Prohibition on Provider Linkage**: `nuo-model-routing` must not gain a dependency on any
  `providers/*` crate, `nuo-tui`, or `nuotc`. Fetching stays behind the `nuo-provider`
  port; presentation stays in `nuo-tui`.
- **Prohibition on a Sync-Enforcement Test in Place of the Merger**: a test that diffs
  two directory trees would "enforce" the old duplication instead of removing it. The
  merger is the mechanism; no drift detector is owed.

## Alternatives Considered

### Rejected — Move the pipeline into `nuo-persistence`

- **Why considered**: both consumers already depend on `nuo-persistence`, so no new crate
  would be needed (avoiding the crate-admission threshold in `architecture.md` §3).
- **Why rejected**: the pipeline is not storage. It resolves provider specs, chooses
  dialects, maps display labels, and derives credentials — it would drag `nuo-provider`
  semantics into a store crate that must stay dumb, and `nuo-persistence` would grow a
  public surface that is not about persistence. Layer-2 placement is right; *that* Layer-2
  crate is wrong.

### Rejected — Make `nuo-harness` depend on `nuo-server`

- **Why considered**: it would let the harness delete its copy with a one-line manifest
  change.
- **Why rejected**: it inverts the runtime edge. `nuo-server` is the container that hosts
  harness sessions; a harness that needs the container to compute a catalog is an
  anti-inversion defect of the same family as `architecture.md` §2.1.

### Rejected — Keep both copies and add a drift test

- **Why considered**: cheapest option, no manifest churn.
- **Why rejected**: it institutionalises the defect the repository's own invariant policy
  forbids — a rule with a passive detector instead of a structural mechanism — and it
  leaves every future edit needing two applications. The 4 536 duplicate lines are also
  read by the compiler twice on every build, so the "cheap" option is not even cheap.

### Rejected — `include!` of a shared source file

- **Why considered**: one file, two module trees, no crate boundary.
- **Why rejected**: `include!` is a textual splice, not a definition. It defeats rustdoc,
  hides the dependency from Cargo (so nothing prevents unlisted crates from being pulled
  in), and makes the pipeline's dependencies invisible to the workspace dependency graph
  — trading a visible crate for an invisible one.
