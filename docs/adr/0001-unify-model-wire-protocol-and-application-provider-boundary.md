---
id: ADR-0001
title: "Unify Model Wire Protocol and Application Provider Boundary"
status: accepted
date: 2026-10-01
scope: substrate/wire, app/providers
superseded_by: null
negative_knowledge: true
---

# 0001. Unify Model Wire Protocol and Application Provider Boundary

- Status: Accepted
- Date: 2026-10-01
- Deciders: Nuo Architecture Working Group
- Consulted: Substrate & Runtime Team
- Informed: Core Contributors

---

## Context and Problem Statement

In the historical codebase, low-level HTTP transport, multi-vendor wire protocol serialization (OpenAI, Anthropic, Google Gemini), and high-level application `Provider` trait implementations were tightly bundled into a monolithic client crate (`muta-llm-client`). During the initial migration to the `nous` cognitive substrate, this crate was ingested into the application layer as `nuo-llm-client`.

Concurrently, the substrate established `nous-model-wire` (`../nous/crates/nous-model-wire`) to provide unified, zero-overhead wire protocol framing and Netune transport for model APIs. This created architectural duality and layer blurring:
1. **Duality of Wire Implementations**: Both `nous-model-wire` and `nuo-llm-client` performed wire protocol framing and SSE stream demuxing.
2. **Layer Inversion & Misplaced Ownership**: The application's domain `Provider` trait (`nuo_contracts::Provider`) was implemented inside the low-level wire client (`nuo-llm-client`) rather than within the application-layer provider subsystem (`nuo-providers`).
3. **Dead Dependencies & Incomplete Layering**: `nuo-agent` depended on `nous-model-wire` nominally, while actually pulling JSON parsing helpers from `nuo-llm-client`.

We must establish a clean, long-term, uncompromised architectural boundary between the cognitive substrate (`nous`) and the application host (`nuo`).

---

## Decision Drivers

- **Orthogonal Separation of Concerns**: Strict decoupling between substrate wire protocol framing (transport/SSE/framing) and application provider semantics (credentials, OAuth, pricing, catalog discovery).
- **Single Source of Truth**: Eliminate dual protocol parsers by standardizing all model wire communication on `nous-model-wire`.
- **Zero Legacy Burden**: Completely retire `nuo-llm-client` instead of maintaining transitional shims or forwarding aliases.
- **Portability & Reusability**: Ensure `nous-model-wire` remains pure and general-purpose across CLI tools, agents, and external consumers, while `nuo-providers` owns product-specific orchestration.

---

## Considered Options

- **Option 1: Retain `nuo-llm-client` as the Permanent Application Wire Engine**
  Keep `nuo-llm-client` inside `nuo` and ignore `nous-model-wire`.
- **Option 2: Absorb Application Provider Semantics into `nous-model-wire`**
  Move catalog discovery, OAuth device flows, and `nuo_contracts::Provider` implementation into `nous-model-wire`.
- **Option 3 (Chosen): Pure Substrate Wire (`nous-model-wire`) with Application Provider Facade (`nuo-providers`)**
  Standardize all wire protocol serialization and Netune transport in `nous-model-wire`. Implement `nuo_contracts::Provider` strictly inside `nuo-providers`, and completely purge `nuo-llm-client`.

---

## Decision Outcome

Chosen option: **Option 3: Pure Substrate Wire (`nous-model-wire`) with Application Provider Facade (`nuo-providers`)**, because it enforces clean hexagonal architecture, eliminates code duplication, and establishes an uncompromised long-term foundation.

### Architecture Topology

```text
┌─────────────────────────────────────────────────────────────┐
│ Application Host Layer (nuo)                                │
│                                                             │
│  crates/nuo-providers                                       │
│   ├── Implement nuo_contracts::Provider                     │
│   ├── Connection management & API keys                      │
│   ├── OAuth2 / PKCE device flows                            │
│   └── Dynamic catalog discovery (/models)                   │
└──────────────────────────────┬──────────────────────────────┘
                               │ drives
                               ▼
┌─────────────────────────────────────────────────────────────┐
│ Cognitive Substrate Layer (nous)                            │
│                                                             │
│  nous-model-wire                                            │
│   ├── WireClient & Netune HTTP pooled transport             │
│   ├── Wire protocol framing (OpenAI, Anthropic, Gemini)     │
│   ├── SSE stream demuxing & thinking block extraction       │
│   └── Balanced JSON framing utilities                       │
└─────────────────────────────────────────────────────────────┘
```

### Invariants & Behavioral Boundaries

- **`[INV-WIRE-01] Substrate Wire Purity`**:
  `nous-model-wire` must remain agnostic of application-specific domain traits (`nuo_contracts::Provider`), storage layers, SQLite databases, or product configuration. It operates exclusively on standard wire structures (`WireRequest`, `WireResponse`, `WireChunk`, `WireStream`, `Endpoint`).
- **`[INV-WIRE-02] Application Layer Provider Ownership`**:
  All implementations of `nuo_contracts::Provider` belong exclusively to `crates/nuo-providers`. The provider translates application `ModelRequest` into `WireRequest` and maps `WireChunk` into `ProviderStreamEvent`.
- **`[INV-WIRE-03] Complete Elimination of Intermediate Crate`**:
  The `crates/nuo-llm-client` crate is completely decommissioned and removed from the workspace. No crate in the `nuo` repository may introduce an intermediate wire adapter layer.

### Positive Consequences

- Eliminates over 40,000 lines of duplicated protocol implementations and obsolete shims.
- Clean dependency flow: `nuo-runtime` -> `nuo-providers` -> `nous-model-wire`.
- `nuo-agent` consumes standard JSON framing (`find_balanced_object`) from `nous-model-wire`.
- Centralizes transport optimization, TLS negotiation, and connection pooling in `nous-model-wire`.

### Negative Consequences & Trade-offs

- Requires porting any advanced wire protocol features (such as OpenAI Responses API tool trace framing) into `nous-model-wire`.
- Requires refactoring `nuo-providers` to construct `nous-model-wire::WireClient` directly.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 (Rejected: Retain `nuo-llm-client` in Application Layer)
- **Why considered**: Avoided cross-workspace modifications between `nuo` and `nous`.
- **Why rejected**: Violated the purpose of the `nous` substrate. Maintaining two separate protocol decoders (one in `nous-model-wire` and one in `nuo-llm-client`) guarantees protocol drift, double maintenance cost, and broken invariants.

### Option 2 (Rejected: Absorb Application Provider Semantics into Substrate)
- **Why considered**: Would have consolidated all provider-related code in one place.
- **Why rejected**: Violated substrate purity. Embedding product-specific authentication storage, XDG paths, and OAuth device UX into `nous-model-wire` would make the substrate bloated, preventing its reuse in headless tools, MCP servers, or lightweight agents.

---

## Links

- Substrate Architecture: Ingested under [ADR-0006](0006-absorb-nous-substrate-and-establish-acp-protocol-standard.md)
- Taxonomy & Invariants: [Invariants Constitution](../governance/documentation/core/invariants.md)
