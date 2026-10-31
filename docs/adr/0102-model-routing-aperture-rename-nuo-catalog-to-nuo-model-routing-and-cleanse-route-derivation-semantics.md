---
id: ADR-0102
title: "Model Routing Aperture: Rename nuo-catalog to nuo-model-routing and Cleanse Route Derivation Semantics"
status: accepted
date: 2026-11-29
scope: routing/nuo-model-routing, workspace/topology, server/nuo-server, harness/nuo-harness
superseded_by: null
negative_knowledge: true
---

# 0102. Model Routing Aperture: Rename nuo-catalog to nuo-model-routing and Cleanse Route Derivation Semantics

- Status: Accepted
- Date: 2026-11-29
- Deciders: Nuo Architecture Working Group
- Consulted: Agent Runtime, Provider Integration, Server Core, Presentation/TUI
- Informed: Core Engineering
- Amends: [ADR-0081](0081-unidirectional-catalog-pipeline-structural-normalization-and-relay-noise-hygiene.md), [ADR-0082](0082-single-catalog-pipeline-definition-in-a-dedicated-layer2-crate.md)

---

## Context and Problem Statement

`crates/nuo-catalog` was created in ADR-0082 to eliminate a 4,536-line duplicate module tree between `nuo-harness` and `nuo-server`. While ADR-0082 successfully resolved the *cardinality* defect via a dedicated Layer-2 substrate, the crate's naming and conceptual framing remained anchored to a legacy misnomer: **"Catalog"**.

In industrial software architecture, this name induces severe cognitive frictions:

1. **Static Noun vs. Dynamic Pipeline & Factory**:
   "Catalog" suggests a passive, static dictionary or reference file (e.g. a static list of models). In reality, the crate executes a high-density, multi-stage computation:
   - 3-step Set Delta Algebra (`S_base -> S_provider -> S_effective`);
   - Rule-based lexical normalization and relay noise hygiene (`[INV-CATALOG-05]`, `[INV-CATALOG-06]`);
   - Availability gating and sovereign user overrides (`[INV-AVAIL-06]`);
   - Resolution of dynamic OAuth refresh tokens and API key precedence;
   - End-to-end assembly of runnable channels and the executable factory `build_provider_for_model -> Arc<dyn Provider>`.

2. **Severe Namespace Collision with Ingress Discovery**:
   The workspace hosts `providers/nuo-provider-catalog`, which handles north-south wire-level discovery (`GET /models` parsing). Having both `nuo-catalog` and `nuo-provider-catalog` created constant ambiguity: developers frequently conflated the provider-facing discovery parser with the internal route derivation engine.

3. **Cognitive Firewall Obscurity**:
   `nuo-harness` must remain strictly focused on agent reasoning, prompt assembly, and tool execution. It relies on this Layer-2 substrate to shield it from connection endpoints, protocol dialects, and token refresh mechanics. Calling this boundary "catalog" hid its true function as an infrastructure routing and credential firewall.

---

## Decision Drivers

- **Domain Precision Over Nominal Inflation**: The crate name must reflect what the code actually derives and materializes — concrete, callable model routes (`Channel`).
- **Cognitive Firewall Clarity**: The abstraction must explicitly denote that it synthesizes user connection declarations and credentials into callable dispatch channels, shielding upper execution layers from physical transport concerns.
- **Zero Namespace Collision**: The crate name must cleanly decouple from leaf provider discovery crates (`nuo-provider-catalog`).
- **Acyclic Dependency Preservation**: Maintain the Layer-2 positioning mandated by ADR-0082 without creating cycles between `nuo-server` and `nuo-harness`.
- **Uncompromising Clean Break**: Eliminate legacy naming baggage immediately without introducing temporary backwards-compatibility shims or alias bloat.

---

## Architectural Decisions

### 1. `[INV-ROUTING-01]` Single Model Routing Pipeline Definition

The Layer-2 pipeline crate `crates/nuo-catalog` is renamed to **`crates/nuo-model-routing`** (package `nuo-model-routing`). It supersedes `[INV-CATALOG-07]`.

The crate owns the single authoritative definition of:
- **Route Derivation (`derive.rs`)**: Evaluating connection declarations, credentials, discovery caches, and availability rules to materialize concrete model `Channel`s and instantiate `Arc<dyn Provider>`.
- **Presentation State Projection (`picker.rs`)**: Producing the read-only `ProviderPickerSnapshot` for consumer UIs with availability badges, recency, and favorites.
- **Remote Model Cache Synchronization (`sync.rs`)**: Coordinating background ETag validation, cache TTLs, and incremental update streams (`ConnectionUpdate`).

Enforcement: the `nuo-model-routing` test
`tests/invariants.rs::pipeline_is_defined_in_exactly_one_crate` scans every workspace crate
for the canonical entrypoint (`build_routes`) and fails if it is defined anywhere but here. A
tagged invariant with no mechanism is only an aspiration.

```text
Layer 1  nuo-protocol · nuo-provider · nuo-inference-wire
              ▲
Layer 2  nuo-model-routing · nuo-persistence · nuo-harness
              ▲
Layer 3  nuo-server
```

### 2. Canonical Route Building and Sync APIs (`build_routes`, `RoutingSyncOutcome`)

The legacy vocabulary is fully excised from the crate's public interface:
- **`build_routes()`**: Supersedes `build_catalog()`, returning a vector of `ProviderEntry` records containing fully resolved, concrete model `Channel`s.
- **`RoutingSyncOutcome` & `RoutingSyncFailureEntry`**: Supersede `CatalogSyncOutcome` and `CatalogSyncFailureEntry`, accurately modeling the outcome of route cache synchronization.
- **`sync_remote_routes` & `sync_connection_routes`**: Supersede `sync_remote_catalog` and `sync_connection_catalog`.

### 3. Direct Ingestion with Zero Compatibility Aliases

Consumers (`nuo-server`, `nuo-harness`) import `nuo_model_routing::*` directly. No re-export conduit or alias (`pub use nuo_model_routing as nuo_catalog;` or `pub use build_routes as build_catalog;`) is permitted. Legacy naming baggage is eradicated with zero backwards-compatibility compromise.

---

## Consequences

### Positive Consequences

- **Mental Model Alignment**: The name `nuo-model-routing` immediately signals to developers and operators that the crate manages *where and how model invocations are directed*, rather than storing passive catalog records.
- **Ingress vs. Routing Disambiguation**: `providers/nuo-provider-catalog` is unambiguously identified as the remote HTTP discovery adapter, while `nuo-model-routing` is the internal route materialization engine.
- **Explicit Cognitive Firewall**: The role of `nuo-model-routing` as the credential-shielding boundary for `nuo-harness` is clear and mechanically enforced.
- **Topological Integrity**: The Layer-2 position remains strictly acyclic and headless; `nuo-model-routing` links zero concrete `providers/*` crates and zero presentation crates.

### Negative & Trade-offs

- **Manifest Realignment**: Workspace dependencies, Cargo manifests, and internal call sites were mechanically migrated to `nuo-model-routing`.

---

## Rejected Alternatives & Negative Knowledge

### 1. Retaining `nuo-catalog`
**Rejected**: Retaining `nuo-catalog` perpetuates the illusion of a static data store, conceals the dynamic assembly of `Arc<dyn Provider>`, and preserves the persistent naming confusion with `nuo-provider-catalog`.

### 2. Renaming to `nuo-model-resolver`
**Rejected**: "Resolver" is an over-specified micro-action verb that evokes symbol resolution, DNS lookups, or model parameter introspection (e.g., inspecting neural weights). It fails to convey that the crate integrates multiple connection declarations into operational routes.

### 3. Renaming to `nuo-provider-runtime`
**Rejected**: "Runtime" is a classic architectural "junk-drawer" label. Designating this crate a "runtime" invites future developers to dump unrelated execution lifecycle state, background tasks, and event loops into it, eroding the pure functional pipeline boundary established by ADR-0081.

### 4. Renaming to Bare `nuo-routing` or `nuo-route`
**Rejected**: A bare `nuo-routing` lacks domain qualification and is ambiguous in a networked server application; it invites confusion with HTTP Web routing (e.g. Axum routes) or IPC dispatch routing in `nuo-protocol`. The domain qualifier `model-` is strictly required to indicate LLM route derivation.

### 5. Renaming to `nuo-connection`
**Rejected**: "Connection" already represents the persistent user declaration struct (`nuo_persistence::connections::Connection`) and physical transport sockets. Reusing it for the derivation pipeline creates namespace collisions.
