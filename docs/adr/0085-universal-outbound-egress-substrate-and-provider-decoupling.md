---
id: ADR-0085
title: "Universal Outbound Egress Substrate (nuo-egress) and Cognitive Provider Decoupling"
status: accepted
date: 2026-11-22
scope: egress/nuo-egress, provider/nuo-provider, providers/all, oauth/nuo-oauth, server/nuo-server, wire/nuo-inference-wire
superseded_by: null
negative_knowledge: true
---

# 0085. Universal Outbound Egress Substrate (nuo-egress) and Cognitive Provider Decoupling

- Status: Accepted
- Date: 2026-11-22
- Deciders: Nuo Architecture Working Group
- Consulted: Platform Architecture, Engine, Network, and Provider Integration Teams
- Informed: Core Engineering, Developer Experience
- Amends: [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md), [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md)

---

## Context and Problem Statement

Prior to this decision, the workspace maintained `crates/nuo-provider-transport` as a shared substrate for model providers. However, architectural inspection and real-world implementation analysis revealed acute semantic conflation and dependency inversion:

1. **Semantic Inversion & Responsibility Bloat**:
   While bearing the name "transport" (which conventionally denotes Layer 4/5 byte carriage and connection management), `nuo-provider-transport` accumulated substantial Layer 7 application and cognitive logic:
   - Route-scoped vision projection and image stripping (`vision.rs`);
   - Prompt-cache capability resolution and routing key computation (`prompt_cache.rs`);
   - JSON AST envelope reshaping and private dialect transformations (`pipeline.rs`);
   - Model provider endpoint metadata and authentication header bindings (`endpoint.rs`);
   - Upstream LLM error taxonomy and context overflow diagnostics (`transport.rs`).

2. **Fragmentation of Network Egress**:
   Because `nuo-provider-transport` was branded as a provider-specific crate and burdened with cognitive dependencies (`nuo-provider`, `nuo-inference-wire`, `nuo-tool`), other subsystems requiring outbound HTTP egress were forced into architectural compromises:
   - `tools/nuo-tool-web` (web search and page fetching) duplicated network client construction, fragmenting the process-wide TCP/TLS connection pool.
   - `crates/nuo-oauth` (generic RFC 7636/8628 OAuth operations) was forced to link a crate carrying heavy AI model contracts.

3. **Multi-Subsystem "Transport" Nomenclature Collision**:
   The identifier "Transport" was overloaded across four orthogonal domains: North-South external cloud egress, East-West internal client-server IPC, agent-to-agent ACP coordination, and MCP subprocess stdio channels.

Under an uncompromising zero-legacy standard, we require clean separation of concerns, strict dependency inversion (Layer 0 leaf substrate), and unambiguous nomenclature.

---

## Decision Drivers

1. **Substrate Purity (`[INV-EGRESS-01]`)**: A network egress crate must deal strictly with network I/O, socket pooling, TLS verification, HTTP requests, and byte-exact SSE reassembly. It must possess zero awareness of cognitive abstractions, prompt schemas, or model capabilities.
2. **Universal Egress Singularity (`[INV-EGRESS-02]`)**: Outbound network I/O across the entire process (model inference, OAuth token exchange, catalog fetching, web reader) must converge on a single, shared connection pool and platform TLS trust store.
3. **Cognitive Ownership Realignment**: Prompt caching, vision downsampling, request envelope reshaping, and provider endpoint configuration belong exclusively to the provider contract layer (`nuo-provider`).
4. **Architectural Naming Precision (`[INV-ARCH-01]`)**: Rename the purified substrate to `nuo-egress`, explicitly denoting outbound internet egress without domain leakage.

---

## Architectural Decisions

### 1. Relocate Domain & Cognitive Logic to `nuo-provider`

The application-level provider logic is evicted from the transport substrate and returned to its authoritative home in `crates/nuo-provider`:
- `endpoint.rs`: Relocated to `crates/nuo-provider/src/endpoint.rs`. Defines `Endpoint` connection configurations and header mappings.
- `pipeline.rs`: Relocated to `crates/nuo-provider/src/pipeline.rs`. Defines `ProviderPipeline` (aliased to `TransportPipeline`), `EnvelopePhase`, `BodyCodecPhase`, and `RequestSignerPhase`.
- `prompt_cache.rs`: Relocated to `crates/nuo-provider/src/prompt_cache.rs`. Governs `PromptCacheConfig` resolution against `ModelRequest`.
- `vision.rs`: Relocated to `crates/nuo-provider/src/vision.rs`. Governs `project_images_for_route`.
- `response.rs`: Created in `crates/nuo-provider/src/response.rs`. Houses domain-rich HTTP error classification (`ensure_success`), context-overflow recognition (`is_context_overflow_payload`), and token-counted error previews (`decode_response_json`).

### 2. Purify Substrate into `nuo-egress` (Layer 0 Leaf Crate)

`crates/nuo-provider-transport` is renamed and cleansed to become `crates/nuo-egress`:
- **Dependencies Purged**: Completely removes dependencies on `nuo-provider`, `nuo-tool`, `nuo-inference-wire`, and `nuo-host`.
- **Domain-Free Errors**: Introduces `EgressError` representing physical HTTP status codes, network timeouts, transport failures, and SSE line decoding errors.
- **Unified Telemetry**: Defines `EgressTelemetry`, `EgressTimings`, and `EgressObservation`.
- **Reusable Core**:
  - `Client`: Pooled HTTP client executing requests through `Egress`.
  - `Egress` / `NuoNetEgress`: Transport seam driven by `netune`.
  - `Http` / `Request` / `Reply`: Lightweight bounded control-plane HTTP handle.
  - `sse`: Byte-exact SSE stream demuxer and strict UTF-8 reassembly (`data_payloads`, `payloads_from_chunks`).
  - `network`: Process-wide singleton `shared_client()`.

### 3. Converge `nuo-inference-wire` WireClient onto `nuo-egress`

`nuo-inference-wire::WireClient` now consumes the process-wide client from `nuo_egress::shared_client()`, ensuring that wire clients and provider egress clients share the exact same warm connection pool and TLS session cache.

---

## Consequences & System Invariants

### Positive Consequences
- **True Layer 0 Substrate**: `nuo-egress` has zero knowledge of models or prompts; it can be safely used by tools (`nuo-tool-web`), authentication (`nuo-oauth`), and external integrations without pulling in AI agent domain dependencies.
- **Zero Duplicated Connection Pools**: The entire Nuo process shares one unified connection cache and platform TLS configuration.
- **Cognitive Clarity**: Developers writing provider implementations find `Endpoint`, `PromptCacheConfig`, and `project_images_for_route` in `nuo-provider` where they logically belong.
- **Unambiguous Nomenclature**: "Egress" unambiguously denotes outbound internet traffic, ending the lexical collision with IPC and ACP transports.

### Binding Invariants
- **`[INV-EGRESS-01] Egress Substrate Purity`**: `nuo-egress` shall contain only network I/O, connection pooling, HTTP client primitives, SSE stream reassembly, and physical telemetry. It must never link or reference cognitive contracts (`nuo-provider`), wire dialects (`nuo-inference-wire`), or agent tools (`nuo-tool`).
- **`[INV-EGRESS-02] Shared Pool Convergence`**: All outbound HTTP and HTTPS communication in production must converge on `nuo_egress::shared_client()` to prevent connection cache fragmentation.

---

## Negative Knowledge & Rejected Alternatives

### 1. Rename to `nuo-transport` (Rejected)
- **Why considered**: Removes the `provider-` prefix.
- **Why rejected**: In Nuo, Client-Server IPC, ACP multi-agent routing, and MCP subprocesses all feature transport layers. Using a bare `nuo-transport` name creates workspace-level namespace ambiguity and violates `[INV-ARCH-01]`.

### 2. Keep Domain Logic in `nuo-egress` (Rejected)
- **Why considered**: Avoided migrating `Endpoint` and `pipeline` references across provider crates.
- **Why rejected**: Leaving image-stripping and prompt-caching in an egress crate preserves the exact architectural inversion and cognitive bloat this refactoring was designed to eradicate.
