---
id: ADR-0098
title: "Orthogonal Three-Dimensional Authentication Topology, Category-Error Abolition, and Modernized Credential Governance"
status: accepted
date: 2026-11-28
scope: provider/nuo-provider, protocol/nuo-protocol, persistence/nuo-persistence, server/nuo-server, harness/nuo-harness
superseded_by: null
negative_knowledge: true
---

# 0098. Orthogonal Three-Dimensional Authentication Topology, Category-Error Abolition, and Modernized Credential Governance

- Status: Accepted
- Date: 2026-11-28
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0095](0095-canonical-dual-lane-routing-provider-capability-and-byos-byok-unification.md), [ADR-0097](0097-physical-ingress-channel-topology-multi-realm-bifurcation-and-sovereign-connection-auth.md)

---

## Context and Problem Statement

For multiple generations of AI agent architectures, connection authentication was modeled as a simplistic binary enum:
```rust
// Legacy flaw: ontological category error
pub enum ConnectionAuth {
    ApiKey,
    Subscription { provider: Cow<'static, str> },
}
```

This representation contains a severe **ontological category error**:
1. **`ApiKey` describes a physical credential artifact**: A static pre-shared secret string. It has zero active lifecycle, no delegation protocol, and no refresh state machine.
2. **`Subscription` describes a commercial economic entitlement**: A flat-rate billing model (e.g. monthly quota, all-you-can-eat access, zero marginal token cost).
3. **`OAuth / OIDC / IAM` describes an interactive protocol state machine**: A dynamic credential lifecycle involving authorization handoffs, token leases, and automated background refresh.

Placing `ApiKey` and `Subscription` as sibling variants in a single enum is equivalent to defining `enum Fruit { Apple, WholesaleByBox }`. It collapses fundamentally disjoint architectural axes:
- **Commercial Entitlement Leakage**: Cloud infrastructure provides services authenticated via dynamic OAuth/IAM sessions that are nonetheless billed per token (`Metered`, e.g. Google Cloud Vertex AI, AWS Bedrock via AWS IAM Identity Center/SSO). Conversely, services like Command Code and Qianwen Token Plan authenticate via static API keys while granting fixed-subscription quotas (`FlatRate`).
- **Wire Placement Conflation**: Tokens may be injected as `Authorization: Bearer`, as custom headers (`X-API-Key`, `api-key`), as query parameters (`?key=`), or via cryptographic per-request body signing (AWS SigV4, Qoder COSY). Conflating wire formatting with credential acquisition forces downstream codecs to sniff token string prefixes or rely on ad-hoc boolean markers.
- **Modern Identity Exclusion**: Emerging enterprise execution postures—such as Kubernetes Workload Identity, Cloud IMDS STS tokens, ambient network mesh authentication (mTLS), and local zero-token engines (Ollama, vLLM)—cannot be cleanly modeled within a binary `ApiKey` vs `Subscription` framework.

An uncompromising, modernized architectural standard is required to abolish this category error, establish an orthogonal three-dimensional authentication topology, and provide clean typestates for future-proof credential governance without legacy baggage.

---

## Decision Drivers

- **Ontological Decoupling**: Separate credential acquisition mechanics, wire transport injection, and commercial billing accounting into distinct, independent axes.
- **Symmetrical 3D Topology**: Support all valid permutations of credential lifecycle, wire placement, and economic entitlement without artificial constraints.
- **Typestate-Driven Wire Decoration**: Replace string-sniffing and ad-hoc marker structs with first-class `WirePlacement` typing.
- **Zero Legacy Burden & High Invariant Assurance**: Provide clean typestate predicates (`is_static()`, `is_negotiated()`, `lifecycle()`) while ensuring zero-breakage backward compatibility for persisted `connections.toml` profiles.

---

## Architectural Decisions

```text
┌────────────────────────────────────────────────────────────────────────┐
│ Axis 1: Credential Lifecycle Mechanics (AuthLifecycle)                │
│         - Static: Pre-shared token, passive holding, zero refresh      │
│         - Negotiated: Active protocol lease, background refresh loop   │
│         - Ambient: Network/topological identity, zero token (mTLS/IPC) │
│         - Workload: Cloud/runtime environment token (K8s SA, IMDS)     │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ Resolves Raw Secret / Session
┌───────────────────────────────────▼────────────────────────────────────┐
│ Axis 2: Wire Injection & Signature Placement (WirePlacement)           │
│         - Bearer: Authorization: Bearer <token>                        │
│         - CustomHeader: X-API-Key, api-key, etc.                       │
│         - QueryParam: ?key=<token>                                     │
│         - RequestSigner: Per-request cryptographic HMAC/RSA body sign  │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ Bound to Sovereign Connection
┌───────────────────────────────────▼────────────────────────────────────┐
│ Axis 3: Commercial Entitlement & Accounting (Entitlement)              │
│         - FlatRate: Zero marginal USD/token, cost-optimized priority   │
│         - Metered: NanoUSD pay-as-you-go, spend circuit breakers       │
└────────────────────────────────────────────────────────────────────────┘
```

### 1. Axis 1: Canonical Credential Lifecycle (`AuthLifecycle`)
The credential lifecycle axis strictly answers: *"How is the secret obtained and refreshed?"*

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthLifecycle {
    /// Static pre-shared secret (passive storage in credentials.toml or env, zero refresh).
    Static,
    /// Protocol-managed dynamic session (active lease, background/intercepted token refresh).
    Negotiated,
    /// Ambient / network-topological identity (mTLS, local socket, zero token).
    Ambient,
    /// Environment-issued workload identity (Kubernetes SA, cloud IMDS/STS).
    Workload,
}
```

- `ConnectionAuth` is modernized to expose canonical lifecycle inspection:
  ```rust
  impl ConnectionAuth {
      pub fn lifecycle(&self) -> AuthLifecycle;
      pub fn is_static(&self) -> bool;
      pub fn is_negotiated(&self) -> bool;
  }
  ```
- Deserialization supports modernized `static` and `negotiated` keys alongside legacy `api-key` and `subscription` aliases.

### 2. Axis 2: Wire Placement and Request Signing (`WirePlacement`)
The wire placement axis strictly answers: *"How is the resolved secret injected into the outbound HTTP transaction?"*

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum WirePlacement {
    /// Standard Authorization: Bearer <token> header.
    Bearer,
    /// Custom HTTP header (e.g. X-API-Key: <token>).
    CustomHeader { header: Cow<'static, str> },
    /// URL query parameter (e.g. ?key=<token>).
    QueryParam { param: Cow<'static, str> },
    /// Cryptographic per-request envelope or body signing (AWS SigV4, Qoder COSY).
    RequestSigner,
}
```

- `ResolvedAuth` carries first-class `pub placement: WirePlacement` (defaulting to `Bearer`), retiring reliance on ad-hoc extension markers (`BearerLoginMarker`) for request lane discrimination.

### 3. Axis 3: Commercial Entitlement Governance (`Entitlement`)
The economic axis strictly answers: *"How is this turn billed and governed?"*
- `Entitlement::FlatRate`: Zero marginal cost per token (subscriptions, included quotas, local models). Scheduled with high priority; exempt from per-turn dollar accounting.
- `Entitlement::Metered`: Positive marginal cost per token (pay-as-you-go NanoUSD). Bound to `ConnectionBudget` circuit breakers.

---

## Normative Invariants Established

- **`[INV-AUTH-ORTHOGONAL-01] Three-Dimensional Independence`**: Credential lifecycle, wire injection placement, and economic entitlement are strictly independent, orthogonal axes. No variant of one axis may constrain or imply another.
- **`[INV-AUTH-LIFECYCLE-01] Deterministic Lifecycle State Machine`**: A static credential must never enter refresh loops; a negotiated session must declare its refresh and expiration bounds; ambient credentials carry zero token.
- **`[INV-AUTH-PLACEMENT-01] Typestate Wire Injection`**: Outbound HTTP headers, parameters, and signatures are governed strictly by typed `WirePlacement`, never by substring sniffing on token prefixes or ambient guessing.

---

## Consequences

### Positive
- **Abolition of Category Errors**: The codebase no longer equates an artifact (`ApiKey`) with a commercial plan (`Subscription`).
- **Complete Enterprise Coverage**: Cloud IAM STS, Kubernetes Workload Identity, and ambient mTLS fit cleanly into the taxonomy without structural churn.
- **Deterministic Downstream Execution**: Inference wire codecs construct HTTP requests based on explicit typestates (`WirePlacement`), guaranteeing protocol fidelity.

### Negative & Trade-offs
- **Expanded Conceptual Vocabulary**: Engineers must understand the distinction between credential lifecycle (`AuthLifecycle`) and economic entitlement (`Entitlement`). Documented in standard architecture routing.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 (Rejected) — Preserving Binary `ApiKey` vs `Subscription`
- **Why considered**: Historic familiarity and legacy momentum.
- **Why rejected**: Ontological failure. Breaks completely when encountering metered OAuth services (Google Vertex AI) or key-authenticated subscriptions (Command Code).

### Option 2 (Rejected) — The Dynamic Meta-Trait Framework (`AuthCapabilitySet`)
- **Why considered**: An unconstrained, dynamic plugin architecture attempting to serialize arbitrary authorization state machines (ADR-0095 experiment).
- **Why rejected**: Catastrophic complexity. Caused serialization fragility, phantom server connection dropouts, and 1,558 lines of unrouted dead code purged in commit `cd2fc8a`. Replaced by clean, strongly typed orthogonal enums.

---

## Links

- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0095](0095-canonical-dual-lane-routing-provider-capability-and-byos-byok-unification.md), [ADR-0097](0097-physical-ingress-channel-topology-multi-realm-bifurcation-and-sovereign-connection-auth.md)
- Implementation Anchors: `crates/nuo-provider/src/connection_auth.rs`, `crates/nuo-provider/src/auth.rs`, `providers/nuo-provider-codebuddy/src/credential.rs`, `providers/nuo-provider-openai/src/chat_completions/mod.rs`
