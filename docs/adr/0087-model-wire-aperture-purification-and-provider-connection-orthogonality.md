---
id: ADR-0087
title: "Model Wire Aperture Purification and Provider/Connection Orthogonality"
status: accepted
date: 2026-11-24
scope: codec/nuo-inference-wire, oauth/nuo-oauth, provider/nuo-provider, protocol/nuo-protocol, persistence/nuo-persistence, providers/all
superseded_by: null
negative_knowledge: true
---

# 0087. Model Wire Aperture Purification and Provider/Connection Orthogonality

- Status: Accepted
- Date: 2026-11-24
- Deciders: Nuo Architecture Working Group
- Consulted: Codec Engineering, OAuth Platform, Provider Integration, Protocol Governance, Persistence
- Amends: [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md) (extends the codec aperture from cognitive concepts to configuration and identity)
- Amends: [ADR-0085](0085-universal-outbound-egress-substrate-and-provider-decoupling.md) (the egress substrate is the only crate that owns sockets)
- Superseded in part by: [ADR-0088](0088-pure-zero-io-inference-wire-and-transport-pruning.md) (removes the residual `WireClient` transport face and its I/O runtimes)
- Requires: ADR-0081 (`DisplayPolicy` / structural label lifting), ADR-0015 (provider contract crate)

---

## Context and Problem Statement

[ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md) established the Codec
Aperture `[INV-AP-01]`: `nuo-inference-wire` encodes *one* inference request for *one* vendor dialect and demuxes
the answer. It must not host cognition.

The crate nevertheless accumulated three further classes of responsibility that are not encoding at all:

1. **OAuth provider models.** `auth.rs`, `connection_auth.rs`, `provider_auth.rs` and parts of `catalog.rs` defined
   `LoginMethod`, `ConnectionAuth`, `OAuthConfig`, `DeviceFlowMode`, token sets and credential sources. These are
   facts about *how a human authorizes an account*, and they change on OAuth-spec and vendor-console timelines, not
   on wire-format timelines. A vendor that adds a device-authorization flow had to touch the codec crate.
2. **Connection configuration presets.** `client_identity.rs` (1,215 lines) carried the browser/agent identity
   emulation presets — the `User-Agent`, header set and model-id adaptation a provider expects — plus
   `connection_detail.rs` describing a resolved connection for presentation.
3. **Runtime credentials.** `Endpoint`, prompt-cache routing keys and credential resolution lived beside the byte
   encoders, so the codec knew which *account* was speaking and not only which *dialect*.

The consequences were concrete, not stylistic:

- **Directional dependency inversion.** `nuo-oauth` — an orchestration crate — imported `nuo-inference-wire` types
  to describe an authorization, so the OAuth engine depended on the wire codec. A codec edit could break login.
- **Test-surface duplication.** The same `LoginMethod`/`OAuthConfig` vocabulary was needed by the persistence
  layer (to store a connection), by the server (to drive a login) and by `nuo-protocol` (to describe it over IPC),
  so any change rippled through crates that do not encode bytes.
- **Identity confusion at the type level.** `WireClient`/connection helpers accepted a credential *and* a client
  profile *and* an endpoint, which made "which connection is this request for?" a runtime question answered from
  a string, rather than a fact the caller had to state.

The aperture was therefore pure with respect to *cognition* and impure with respect to *configuration*.

---

## Decision Drivers

1. **Single ownership of every fact.** A fact lives in exactly one crate, and that crate is the one whose change
   cadence matches the fact: OAuth flows → `nuo-oauth`; provider/connection semantics → `nuo-provider`; IPC DTOs →
   `nuo-protocol`; persistence records → `nuo-persistence`.
2. **Directional dependency purity.** The orchestration crate depends on the contract crate; neither depends on the
   codec. Encoders must not know which account is speaking.
3. **No transitional façades.** Relocation is a clean break: the source modules are deleted, not re-exported
   through a compatibility shim ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)
   clean-break principle).
4. **Mechanism over exhortation.** The boundary must be enforced by the Cargo dependency graph and by tests, not by
   prose.

---

## Architectural Decisions

### 1. `nuo-inference-wire` Keeps Only the Encoding Aperture

The crate retains exactly the modules that encode or decode wire bytes:

```text
crates/nuo-inference-wire/src/
├── endpoint.rs        # dialect/endpoint algebra (which path a dialect addresses)
├── error.rs           # WireError: codec-local, zero cognition
├── credentials.rs     # credential *sources* as an encoding input, not a store
├── generative/        # request/response encoders + SSE demux per dialect
└── discriminative/    # probabilistic classification / ordinal rubric lane
```

Deleted from the crate (moved, not re-exported):

| Deleted module | New owner | Reason |
| :--- | :--- | :--- |
| `auth.rs`, `provider_auth.rs`, `connection_auth.rs` | `nuo-provider` (`auth.rs`, `connection_auth.rs`) | Authorization is a provider-contract fact |
| `client_identity.rs` | `nuo-provider::client_identity` | Client emulation presets are provider semantics |
| `catalog.rs` (discovery/query types) | `nuo-provider::catalog` | Catalog reconciliation is provider semantics |
| `model.rs`, `model_providers.rs` | `nuo-provider` | Model identity and seed tables are provider facts |
| `connection_detail.rs` | `nuo-protocol::connection_detail` | A resolved connection is an IPC DTO |
| `provider_surface.rs`, `wire_surface.rs` | `nuo-provider::surface` / `wire_surface` | Declarative dialect tables belong with `ProviderDialect` |
| `provider_state.rs` | `nuo-provider::state` | Continuation state is a provider contract |
| `generative/{cache,effort,instructions,reasoning}.rs` | `nuo-provider` | Request shaping is provider cognition, not byte encoding |
| `provider_error.rs` | `nuo-provider::error` (completed by ADR-0088) | Cognitive errors are contract errors |

### 2. `nuo-oauth` Depends on the Contract, Never on the Codec

`nuo-oauth` resolves authorization shapes for vendors: RFC 7636 (PKCE), RFC 8628 (device flow), loopback redirect,
token endpoint, refresh and enricher. Its dependency set is `nuo-provider` (ports and config types), `nuo-egress`
(the only socket owner) and `nuo-host` (durable stores). It declares **no** dependency on
`nuo-inference-wire`: a change to a byte encoder cannot alter a login flow, and a change to an OAuth scope cannot
alter an encoding.

Vendor-specific OAuth surfaces (`OAuthConfig`, device grant, enricher) are owned by their
`providers/nuo-provider-*` crate and registered through the typed `nuo_oauth::OAuthProvider` port. The generic engine
links no vendor; the composition root (`nuo-server::provider_registry`) is the only place that knows them all.

### 3. Provider/Connection Orthogonality

A **provider spec** (`ModelProviderSpec`) is a *surface*: endpoint root, protocol, dialect, baselines, catalog
source, quota port, prompt-cache policy, `DisplayPolicy`. A **connection** is a *user-authorized instance* of a
surface: credentials, overrides, route settings.

The two are orthogonal, and the orthogonality is expressed in types rather than strings:

- `ModelProviderSpec` is `&'static` builtin data registered once through `register_provider_specs`, or a
  `UserDeclaredProvider` synced through `sync_user_declared_providers`; it is never mutated per connection.
- The codec never sees either. It receives a resolved dialect plus encoded bytes.
- Resolution is a total function of declared data: `model_provider_spec(id)` looks up the registry, `model_protocol`
  reads the baseline table. No path in the system infers a provider, a protocol, or a quota port by sniffing a
  model-id or base-URL string.

`[INV-PROV-08]` (typed `QuotaPort`) and ADR-0081's `DisplayPolicy` are the two places that rule is most visible:
balance-query dispatch and label production are both declared per surface and resolved by lookup.

### 4. Protocol Layer Owns IPC DTOs

`nuo-protocol` hosts the types that cross a process boundary: `ConnectionDetail`, credential/config projections,
`Discovery`, and version-negotiation errors. This keeps the East-West IPC contract free of North-South streaming
types, and lets a client project a connection without linking a codec.

---

## Normative Invariants Established

- **`[INV-WIRE-03] No Direct Async Runtime in the Codec`**: `nuo-inference-wire` declares no I/O runtime in its
  dependency table; its encoding and demuxing surface is synchronous and in-memory. Mechanism: Cargo manifest
  inspection (`crates/nuo-inference-wire/Cargo.toml` names no `tokio`/`reqwest`/`nuo-egress` for shipped code) plus
  the pure-codec test suite. Tightened to full zero-I/O by ADR-0088.
- **`[INV-OAUTH-01] OAuth Engine Independence from Codecs`**: `nuo-oauth` must not depend on
  `nuo-inference-wire`. Mechanism: Cargo dependency graph (the crate compiles with the codec removed) and
  `cargo tree` inspection in review.
- **`[INV-PROV-12] Behaviour Preservation Through Relocation`**: relocation changes ownership, never wire bytes,
  capability resolution or cache semantics. Mechanism: golden-frame and route-derivation tests
  (`providers/*/tests/*_golden.rs`, `nuo-server::provider_registry::tests`) pass unchanged across the move.

---

## Consequences

- **Positive**: Each fact has one owner with a matching change cadence. The dependency graph became strictly
  directional: `nuo-oauth → nuo-provider → nuo-inference-wire`, never the reverse. Provider authors find
  configuration, identity and catalog contracts in the crate that already owns the provider contract.
- **Negative & Trade-offs**: `nuo-provider` grows into a large contract crate, and a relocation of this size
  produces a wide, mostly mechanical diff. Mitigation: the move is one commit with no behavioural edits, so review
  reads the module map rather than the line noise; the crate is organised by fact (`auth`, `cache`, `catalog`,
  `client_identity`, `surface`, `state`), and no module re-exports a second copy.
- **Deferred**: `WireClient` and the remaining transport-adjacent residue in the codec were left for a follow-up.
  ADR-0088 completed that pass: `WireClient`/`WireStream` were excised and every network dependency was pruned.

---

## Rejected Alternatives & Negative Knowledge

### Alternative A: A shared `nuo-auth` crate between `nuo-oauth` and the codec (Rejected)
- **Why considered**: Both needed `LoginMethod` and credential plumbing, so a common crate looked like the minimal
  change.
- **Why rejected**: It would have made a *third* crate own a fact that already had an owner, and it kept the codec
  in the authorization dependency chain. Shared vocabulary is not a reason to create a crate; the contract crate
  (`nuo-provider`) already existed.

### Alternative B: Keep re-export shims so downstream code compiles untouched (Rejected)
- **Why considered**: The relocation touches every provider crate; shims would have kept the diff small.
- **Why rejected**: A re-export shim is a second public path for the same type. It defeats the ownership rule
  (which crate owns the fact becomes ambiguous), and it silently preserves the inverted dependency the ADR exists
  to remove.

### Negative Knowledge

- **Do not re-export codec modules from `nuo-provider`.** The direction is `nuo-provider → nuo-inference-wire`;
  a facade in either direction reintroduces the cycle.
- **Do not infer a provider, protocol or quota port from a string.** Substring matching on ids or URLs is the
  defect ADR-0027 and ADR-0068 abolished; the typed spec registry is the only lookup.
- **Do not let the codec accept a credential store.** Credential *sources* are encoding inputs; the store belongs
  to `nuo-host`/`nuo-provider`.

---

## Links

- [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md): provider contract crate and the
  `providers/` namespace this ADR keeps as the single definition per provider.
- [ADR-0081](0081-unidirectional-catalog-pipeline-structural-normalization-and-relay-noise-hygiene.md):
  `DisplayPolicy` on the provider spec — the label-production half of "declared per surface".
- [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md): the Codec Aperture
  `[INV-AP-01]` this ADR extends from cognition to configuration.
- [ADR-0085](0085-universal-outbound-egress-substrate-and-provider-decoupling.md): the single socket owner every
  token refresh and inference stream converges on.
- [ADR-0088](0088-pure-zero-io-inference-wire-and-transport-pruning.md): completes the aperture by deleting
  `WireClient` and the residual I/O runtimes.
