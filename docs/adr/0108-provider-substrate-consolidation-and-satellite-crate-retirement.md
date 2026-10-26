---
id: ADR-0108
title: "Provider Substrate Consolidation: Absorb nuo-egress and nuo-oauth into nuo-provider and Retire Satellite Crates"
status: accepted
date: 2026-12-03
scope: provider/nuo-provider, egress/nuo-egress, oauth/nuo-oauth, server/nuo-server, workspace/topology
superseded_by: null
negative_knowledge: true
---

# 0108. Provider Substrate Consolidation: Absorb `nuo-egress` and `nuo-oauth` into `nuo-provider` and Retire Satellite Crates

- Status: Accepted
- Date: 2026-12-03
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md), [ADR-0085](0085-universal-outbound-egress-substrate-and-provider-decoupling.md), [ADR-0087](0087-model-wire-aperture-purification-and-provider-connection-orthogonality.md), [ADR-0098](0098-orthogonal-three-dimensional-authentication-topology-category-error-abolition-and-modernized-credential-governance.md)

---

## Context and Problem Statement

In previous architectural evolutions ([ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md), [ADR-0085](0085-universal-outbound-egress-substrate-and-provider-decoupling.md)), two satellite crates were carved out of the former provider adapters monolith:
1. `crates/nuo-egress`: Purportedly a "universal outbound network egress substrate" intended to unify process-wide connection pooling across tools, inference, and server.
2. `crates/nuo-oauth`: Purportedly a "cross-cutting authentication engine" intended to isolate generic RFC OAuth primitives from provider logic.

In real-world practice, these abstractions suffered from critical divergence and artificial boundary fragmentation:

### 1. `nuo-egress` is Model Provider Transport, Not a Universal Substrate
- The motivating premise of ADR-0085 was that tools (`nuo-tool-web`) and wire codecs (`nuo-inference-wire`) would share `nuo-egress`. In reality:
  - `nuo-tool-web` requires per-hop SSRF validation and explicitly bypassed `nuo-egress`, constructing its own confined client directly over `netune`.
  - `nuo-inference-wire` was purified into a zero-IO pure codec ([ADR-0088](0088-pure-zero-io-inference-wire-and-transport-pruning.md)), having zero network I/O dependencies.
  - As a result, **100% of `nuo-egress` consumers were solely `nuo-provider` and the `providers/*` crates**.
- Internally, `nuo-egress` suffered from architectural schizophrenia, maintaining two disjoint HTTP clients (`Client` for inference streaming and `Http` for control-plane calls) while duplicating URL and form encoding logic.

### 2. Authentication in Nuo is a Provider-Internal Concern
- Nuo is an autonomous agent CLI and server. There are zero general end-user authentication flows, zero multi-tenant accounts, and zero cross-system SSO mechanisms. All authentication in Nuo strictly concerns **Model Provider upstream connection leases** (`connections.toml`).
- Splitting `nuo-oauth` from `nuo-provider` broke domain cohesion:
  - `nuo-provider` owned `TokenSet`, `CredentialStore`, `CredentialHost`, and `ConnectionAuth` (`Static` vs `Negotiated`, [ADR-0098](0098-orthogonal-three-dimensional-authentication-topology-category-error-abolition-and-modernized-credential-governance.md)).
  - `nuo-oauth` owned `OAuthCredentialSource`, token refresh, device flows, and loopback handlers.
  - Providers were forced to take a triple dependency (`nuo-provider` + `nuo-oauth` + `nuo-egress`), while `nuo-oauth` had to re-export `nuo-provider` types back to callers.

Under our zero-legacy and architectural integrity standards, these two satellite crates represent unjustified workspace fragmentation.

---

## Decision Drivers

- **Domain Cohesion & Simplicity**: Model provider execution requires inference contracts, credential leases, and HTTP/SSE transport. They form one cohesive Layer 1 domain: `nuo-provider`.
- **Crate Inclusion Threshold (`architecture.md` §3)**: Crates must exist only when their responsibilities cannot be cleanly modeled as internal modules of existing domain crates.
- **Direct Substrate Alignment**: Nuo already owns its observable network engine in `netune`. `nuo-provider` should directly build its HTTP/SSE transport adapters over `netune` without an unnecessary intermediary crate.
- **Single Provider Dependency**: Any vendor crate in `providers/*` must only need a single dependency: `nuo-provider = { workspace = true }`.

---

## Architectural Decisions

### 1. Absorb `nuo-egress` into `nuo_provider::transport`
- Evict and retire `crates/nuo-egress`.
- Relocate connection pooling, singleton `shared_client()`, `RequestBuilder`, `HttpResponse`, SSE stream reassembly (`data_payloads`), and physical telemetry (`TransportTimings`, `TransportObservation`) directly into `nuo_provider::transport`.
- Unify URL component and form encoding into `nuo_provider::transport::encoding`.
- `netune` and `netune-trace` are consumed directly by `nuo-provider`.

### 2. Absorb `nuo-oauth` into `nuo_provider::auth`
- Evict and retire `crates/nuo-oauth`.
- Relocate RFC primitives (PKCE S256, RFC 8628 Device Flow, RFC 8252 Loopback Server), vendor `OAuthProvider` port, `OAuthConfig`, `OAuthLoginSession`, and `OAuthCredentialSource` into `nuo_provider::auth`.
- Fully reconcile the dynamic credential engine with ADR-0098's `AuthLifecycle::Negotiated`.
- Eliminate all indirect network calls from the auth engine by having token and device flows call `nuo_provider::transport` directly.

### 3. Simplify Provider & Server Dependencies
- All 16 channel crates in `providers/*` drop `nuo-egress` and `nuo-oauth` from `Cargo.toml`. They link exclusively to `nuo-provider`.
- `nuo-server` drops `nuo-egress` and `nuo-oauth`, resolving both provider specs and OAuth registries strictly through `nuo-provider`.

---

## Consequences

### Positive
- **Reduced Crate Count**: Exactly 2 redundant crates deleted from `crates/`.
- **Unified Provider API Surface**: Provider developers only import `nuo-provider`. All capabilities (`transport`, `auth`, `credentials`, `model`, `pipeline`) are unified and coherent.
- **Eliminated Code Duplication**: Form and URL encoding exist in exactly one place (`nuo_provider::transport::encoding`).
- **Clean Epistemic Boundary**: Layer 1 contracts in `architecture.md` are unified: `nuo-protocol` (East-West IPC), `nuo-inference-wire` (Zero-IO North-South Codecs), and `nuo-provider` (Model Provider Contracts, Transport, and Negotiated Leases).

### Negative & Trade-offs
- `nuo-provider` gains network I/O dependencies (`netune`, `netune-trace`). This is an explicit and correct trade-off: since `nuo-inference-wire` is already zero-IO, `nuo-provider` is the authoritative home for provider-level HTTP carriage.

---

## Rejected Alternatives

### 1. Rename `nuo-egress` to `nuo-http` and `nuo-oauth` to `nuo-auth` (Rejected)
- **Why considered**: Solves the naming stutter without moving files.
- **Why rejected**: Leaves the underlying category and dependency fragmentation intact. A provider crate would still need 3 separate workspace dependencies to implement an authenticated endpoint.

---

## System Invariants Ratified

- **`[INV-PROV-13] Unified Provider Authority`**: All outbound model provider HTTP transport, SSE stream reassembly, dynamic credential negotiation, and provider registries belong exclusively to `crates/nuo-provider`. Creating secondary satellite crates for provider-scoped transport or authentication is prohibited.
