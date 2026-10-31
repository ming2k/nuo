---
id: ADR-0088
title: "Pure Zero-I/O Inference Wire Codec and Transport Pruning"
status: accepted
date: 2026-11-25
scope: codec/nuo-inference-wire, provider/nuo-provider, agent/nuo-agent, protocol/nuo-protocol
superseded_by: null
negative_knowledge: true
---

# 0088. Pure Zero-I/O Inference Wire Codec and Transport Pruning

- Status: Accepted
- Date: 2026-11-25
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Agent Runtime, Transport Team, Protocol Governance
- Amends: [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md) (fulfills the deferred full dependency pruning of `[INV-AP-01]`)
- Amends: [ADR-0087](0087-model-wire-aperture-purification-and-provider-connection-orthogonality.md) (completes the eviction of cognitive errors and transport telemetry from the wire crate)

---

## Context and Problem Statement

When [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md) established the Codec Aperture invariant `[INV-AP-01]`, it candidly recorded a transitional compromise:

> *"The `WireClient` transport face (one module) legitimately keeps `tokio`/`futures` because the channel itself is I/O... If a later pass extracts `WireClient` into a transport crate, this clause tightens to full dependency pruning."*

Furthermore, subsequent architectural inspections revealed subtle cognitive residual leaks left behind in `nuo-inference-wire`:
1. **Network I/O Dependencies**: `nuo-inference-wire` declared runtime dependencies on `tokio`, `netune`, `netune-trace`, `nuo-egress`, and `reqwest`, despite 95% of the crate being pure data structures and synchronous stream decoding algorithms.
2. **Cognitive Error Residue**: `crates/nuo-inference-wire/src/provider_error.rs` hosted `ProviderError`, `ProviderErrorKind`, `ToolError`, `ToolErrorKind`, and `HarnessError`. The wire codec itself never returned these errors (it operates strictly on `WireError`), yet it was forced to implement `From<nuo_egress::EgressError> for ProviderError`, forcing an unnecessary dependency on `nuo-egress`.
3. **Transport Passthrough Smells**: `endpoint.rs` served as a passthrough re-export hub for `nuo_egress::{TransportObservation, TransportTelemetry, TransportTimings}`.
4. **Transport Duplication**: While all production providers converged onto `nuo-egress::Client` per [ADR-0085](0085-universal-outbound-egress-substrate-and-provider-decoupling.md), `WireProvider` in `nuo-agent` remained coupled to the obsolete `WireClient`.

Under an uncompromising zero-legacy standard, we complete the modernization and purification of the inference wire.

---

## Decision Drivers

1. **True Codec Aperture Purity (`[INV-AP-01]`)**: `nuo-inference-wire` must be a 100% in-memory, zero-I/O, zero-asynchronous-runtime pure codec crate.
2. **Domain Contract Alignment**: Cognitive errors (`ProviderError`, `ToolError`, `HarnessError`) belong to the contract authority crate `nuo-provider`.
3. **Shared Pool Singularity (`[INV-EGRESS-02]`)**: Outbound requests from `WireProvider` must directly utilize `nuo-egress::Client`, eliminating the redundant `WireClient` transport wrapper.

---

## Architectural Decisions

### 1. Extraction of `WireClient` and Zero-I/O Pruning

- `crates/nuo-inference-wire/src/client.rs` is excised and permanently deleted.
- The `client` module and types `WireClient`, `WireStream` are purged from `nuo-inference-wire`.
- The synchronous and streaming transport wrappers `generative::execute` and `discriminative::execute` are purged from the wire crate.
- `tokio`, `netune`, `netune-trace`, `nuo-egress`, `reqwest`, and `url` network features are purged from `nuo-inference-wire/Cargo.toml`. The crate's dependencies are pruned strictly to pure leaf libraries: `serde`, `serde_json`, `bytes`, `http`, `thiserror`, `async-trait`, `futures`, `uuid`, `chrono`, `tracing`, `inventory`, `nuo-host`, `nuo-tool`, `sha2`, and `url`.

### 2. Relocation of `ProviderError`, `ToolError`, and `HarnessError` to `nuo-provider`

- `crates/nuo-inference-wire/src/provider_error.rs` is excised from `nuo-inference-wire` and relocated to `crates/nuo-provider/src/error.rs`.
- `nuo-provider` re-exports `ProviderError`, `ProviderErrorKind`, `RetryDisposition`, `ToolError`, `ToolErrorKind`, and `HarnessError` at its crate root.
- `nuo-provider` implements `From<nuo_egress::EgressError> for ProviderError` directly, honoring DIP and removing `nuo-egress` from `nuo-inference-wire`.
- `nuo-protocol/src/error.rs` updates its re-export from `nuo_inference_wire::provider_error` to `nuo_provider::error`.

### 3. Eviction of Transport Telemetry Passthroughs from `Endpoint`

- `nuo-inference-wire/src/endpoint.rs` removes passthrough re-exports of `TransportObservation`, `TransportTelemetry`, and `TransportTimings`.
- `nuo-provider` directly re-exports these types from `nuo-egress`.
- Downstream providers and `nuo-protocol/src/token_ledger.rs` source transport telemetry from `nuo-provider`.

### 4. Convergence of `WireProvider` onto `nuo-egress`

- `crates/nuo-agent/src/provider/wire.rs` (`WireProvider`) directly embeds `nuo_egress::Client`.
- `WireProvider::complete` and `WireProvider::stream` compose `nuo_inference_wire::protocol::build_request`, `nuo-egress::RequestBuilder`, and `nuo_inference_wire::stream::SseDecoder` natively.
- `Client` in `crates/nuo-egress/src/client.rs` derives `Clone` for cheap shared connection pool distribution.

---

## Consequences & System Invariants

### Positive Consequences
- **Zero I/O Guarantee**: `nuo-inference-wire` compiles with sub-second latency and is structurally incapable of executing unauthorized network I/O.
- **Architectural Symmetry**: All providers in the system (Anthropic, OpenAI, Antigravity, and WireProvider) share the exact same structural pattern: `Endpoint` + `nuo_egress::Client` + Pure Dialect Codecs.
- **Cognitive Hygiene**: `nuo-inference-wire` contains zero mentions of "harness" or "tool error".

### Tightened Invariant
- **`[INV-AP-01] Pure Codec Aperture`**: `nuo-inference-wire` shall contain strictly wire-level primitives: vendor request/response codecs, SSE stream demuxing, canonical block IR (`WireRequest`, `ContentBlock`), and physical telemetry facts (`TokenClass`, `TokenBreakdown`). It must declare zero network I/O runtime dependencies (no `netune`, no direct `tokio` in `[dependencies]`, no `nuo-egress`).

---

## Negative Knowledge & Rejected Alternatives

### 1. Retaining `WireClient` in a New Dedicated Crate (`nuo-inference-client`)
- **Discarded Option**: Creating a standalone crate for `WireClient`.
- **Failure Mode**: Violates `docs/dev/subsystems.md` §"Crate Inclusion Threshold". Because `WireProvider` is the sole consumer of this workflow and all other providers already use `nuo-egress` directly, creating a dedicated crate introduces unjustified workspace fragmentation.

### 2. Discarding Canonical Wire IR (`WireRequest`, `ContentBlock`)
- **Discarded Option**: Deleting `WireRequest` and forcing raw per-vendor types at the wire boundary.
- **Failure Mode**: Destroys the universal SSE stream decoder (`SseDecoder`), breaks stream loop detection, and creates N×M duplicate byte-reassembly code across disparate provider implementations.
