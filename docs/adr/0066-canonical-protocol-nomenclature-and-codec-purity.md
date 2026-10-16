---
id: ADR-0066
title: "Canonical Client-Server Protocol Nomenclature (nuo-protocol) and Model Codec Purity via Dependency Inversion"
status: accepted
date: 2026-11-05
scope: protocol/nuo-protocol, codec/nuo-model-codec, server/nuo-server, tui/nuo-tui, harness/nuo-harness
superseded_by: null
negative_knowledge: true
---

# 0066. Canonical Client-Server Protocol Nomenclature (nuo-protocol) and Model Codec Purity via Dependency Inversion

- Status: Accepted
- Date: 2026-11-05
- Deciders: Nuo Architecture Working Group
- Consulted: Platform Architecture, Engine, TUI, and Transport Teams
- Informed: Core Engineering, Developer Experience
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0006](0006-wire-contract-consolidation.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md)

---

## Context and Problem Statement

The Nuo codebase historically suffered from two acute architectural anomalies:

1. **"The Two Wires" Naming Ambiguity**:
   The workspace maintained `nuo-model-codec` (governing external cloud LLM wire schemas such as SSE, `WireMessage`, `WireProtocol`) alongside `nuo-wire` (governing local internal Client $\leftrightarrow$ Server IPC envelopes). Because both crates claimed "wire", developers constantly conflated North-South external inference framing with East-West local control-plane IPC.
2. **Re-Export Middleman Smelling**:
   `nuo-wire` progressively accumulated dozens of passthrough `pub use nuo_model_codec::*` re-exports, degrading into an untyped transshipment hub.
3. **Codec Responsibility Breach (DIP Violation)**:
   In earlier iterations of financial accounting, `nuo-model-codec` was inappropriately burdened with currency (`NanoUsd`), model pricing cards (`ModelPricing`), and budget enforcement (`ConnectionBudget`). A low-level wire codec parsing HTTP SSE frames must never be aware of monetary units or financial accounting.

Under an uncompromising zero-legacy standard, we require an unambiguous taxonomy and a strict Dependency Inversion Principle (DIP) boundary.

---

## Decision Drivers

1. **Unambiguous Nomenclature (`[INV-PROTO-01]`)**: Clearly bifurcate external LLM transport (`nuo-model-codec`) from internal Client-Server IPC (`nuo-protocol`).
2. **Codec Purity & Physical Facts (`[INV-CODEC-01]`)**: `nuo-model-codec` must deal strictly with objective, physical protocol telemetry facts (`TokenClass`, `TokenBreakdown`). It must possess zero awareness of currencies, rates, or financial budgets.
3. **Dependency Inversion for Pricing (`[INV-DIP-01]`)**: High-level financial ledgers and pricing engines in `nuo-protocol` and `nuo-provider` consume the normalized `TokenBreakdown` provided by `nuo-model-codec` without backward concept leakage.
4. **Eradication of Passthrough Re-exports (`[INV-ARCH-03]`)**: Subsystems must import model abstractions directly from `nuo-model-codec` rather than relying on `nuo-protocol` as an intermediary mirror.

---

## Architectural Decisions

### 1. Rename `nuo-wire` to `nuo-protocol`

The substrate crate `crates/nuo-wire` is canonically renamed to `crates/nuo-protocol`.
- **Domain Scope**: Owns internal Client $\leftrightarrow$ Server IPC data contracts: `AgentRequest`, `AgentResponse`, `SessionEvent`, `ProtocolError`, `WireEnvelope`, and control frames.
- **Traffic Direction**: Strictly East-West (TUI/CLI/Web $\leftrightarrow$ Server daemon).

### 2. Physical Fact Normalization in `nuo-model-codec`

`nuo-model-codec` is purified. All monetary types (`NanoUsd`, `ModelPricing`, `PricingTier`, `TurnCost`, `ConnectionBudget`) are evicted. It exposes only physical token classification:

```rust
/// Objective classification of tokens crossing provider wire protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenClass {
    UncachedInput,
    CacheRead,
    CacheWrite,
    StandardOutput,
    ReasoningOutput,
    AudioInput,
}

/// Canonical, de-duplicated distribution of billable token counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenBreakdown {
    pub uncached_input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub standard_output: u64,
    pub reasoning_output: u64,
    pub audio_input: u64,
}
```

### 3. Financial & Budget Policy Relocated to `nuo-protocol`

Financial currency units (`NanoUsd`), model rate cards (`ModelPricing`), single-turn costs (`TurnCost`), and connection budget policies (`ConnectionBudget`) are housed in `nuo-protocol::pricing` and `nuo-protocol::budget`. They depend cleanly downward on `nuo-model-codec::TokenBreakdown` to compute financial costs.

---

## Consequences & System Invariants

### Positive Consequences
- Immediate cognitive clarity: `nuo-protocol` communicates with `nuo-server`; `nuo-model-codec` communicates with Gemini/Claude/OpenAI.
- Elimination of redundant re-export noise across the codebase.
- Codec layer is 100% reusable in headless or non-billing contexts without pulling in financial types.

### Binding Invariants
- **`[INV-PROTO-01] Protocol Singularity`**: `nuo-protocol` is the exclusive contract crate for Client-Server IPC.
- **`[INV-CODEC-01] Zero-Financial Codec Boundary`**: `nuo-model-codec` must contain zero references to currency, pricing, or financial budgets.
- **`[INV-DIP-01] Metric Inversion`**: Pricing and budgeting engines depend on codec telemetry metrics, never vice-versa.

---

## Rejected Alternatives & Negative Knowledge

### 1. Retaining `nuo-wire` and Adding `nuo-wire-codec`
- **Discarded Option**: Keeping the `wire` moniker for both crates.
- **Failure Mode**: Sustained cognitive confusion. New contributors and automated agents repeatedly imported types from the wrong crate or generated cyclical passthrough re-exports.

### 2. Renaming to `nuo-ipc`
- **Discarded Option**: Renaming `nuo-wire` to `nuo-ipc`.
- **Failure Mode**: IPC strictly connotes local OS mechanisms (Unix sockets, pipes). Because the protocol envelopes also service remote headless WebSocket and TCP connections, `nuo-protocol` is architecturally accurate.
