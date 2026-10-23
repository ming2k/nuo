---
id: ADR-0095
title: "Canonical Dual-Lane Model Routing, Symmetrical Credential Lifecycle, and Orthogonal Entitlement Governance"
status: accepted
date: 2026-11-28
scope: provider/nuo-provider, protocol/nuo-protocol, persistence/nuo-persistence, server/nuo-server, harness/nuo-harness, agent/nuo-agent
superseded_by: null
negative_knowledge: true
---

# 0095. Canonical Dual-Lane Model Routing, Symmetrical Credential Lifecycle, and Orthogonal Entitlement Governance

- Status: Accepted
- Date: 2026-11-28
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md), [ADR-0094](0094-codebuddy-browser-web-handoff-oauth-subscription-lane.md)

---

## Context and Problem Statement

Nuo previously ratified provider invocation schemes across local-direct and server-proxy lanes in [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), managed OAuth sessions in [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), and introduced dual-mode pricing budgets in [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md). However, comprehensive scrutiny of the entire vertical domain revealed four deep-seated architectural confusions, category errors, and legacy compromises:

1. **Category Error of "ApiKey" vs "OAuth"**: Legacy systems model authentication as `enum Auth { ApiKey, OAuth }`. This is an ontological category error: an API Key is a *passive data artifact* (a noun/token), whereas OAuth 2.0 is an *active negotiation protocol framework* (a verb/pipeline). At the HTTP wire layer, both resolve to token strings injected into headers (`Authorization: Bearer <token>` or `x-api-key: <token>`). Collapsing a static token and an asynchronous state machine into sibling enum variants creates asymmetric, unbalanced types.
2. **Commercial Leakage into Technical Capabilities ("OAuthSubscription")**: Conflating the technical authentication protocol (`OAuth2`) with the commercial billing model (`Subscription`) introduces severe failure modes. In reality, these axes are strictly orthogonal:
   - *OAuth can be metered*: Google Cloud Vertex AI uses OAuth 2.0 / GCP IAM, but charges per token to a cloud billing credit card.
   - *Static keys can be subscriptions*: Enterprise SaaS platforms and CommandCode ([ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md)) issue static keys tied to monthly flat-rate quotas.
3. **Pseudo-Abstraction of the "Vendor Layer"**: Modeling a top-level "Vendor" entity has zero runtime value. In distributed inference, the execution chain is strictly `Router -> Connection -> Driver -> Transport`. Cloud aggregators and multi-tenant hosting (e.g. AWS Bedrock hosting Anthropic models, Azure hosting OpenAI models, OpenRouter hosting DeepSeek) render "Vendor" taxonomically bankrupt.
4. **Commercial Plan Suffixes in Driver Names (`*-plan`)**: Crate names like `nuo-provider-chatgpt-plan` and `opencode-plan` let mutable marketing plans pollute code-level transport drivers. The true boundary is the physical ingress gateway and serialization protocol (e.g. `openai` developer API vs `chatgpt-codex` internal ingress).
5. **Connection Mutual Exclusivity and Agent Runtime Halts**: Sessions depend on a single `config.default_connection: String`. When a flat-rate subscription profile encounters an HTTP 429 rate limit or quota ceiling, the agent turn terminates fatally without the ability to fail over to a metered key profile.

---

## Decision Drivers

- **Driver 1: Ontological Symmetry**: Credential sourcing must be modeled along symmetric lifecycle paradigms: `Static` (pre-shared, passive holding) vs `Negotiated` (protocol-managed, active lifecycle), retiring the `ApiKey` vs `OAuth` category error.
- **Driver 2: Orthogonal Separation of Auth Protocol and Entitlement Model**: Physical authentication (how a token is acquired) must be strictly decoupled from economic entitlement (how tokens are billed: `FlatRate` vs `Metered`).
- **Driver 3: Complete Abolition of Pseudo-Layers**: The "Vendor" layer and `-plan` naming suffixes are permanently expunged from the runtime and domain model.
- **Driver 4: Priority Dual-Lane Failover with Spend Guardrails**: The runtime must natively support `PriorityFailover` routing (consume zero-marginal-cost `FlatRate` first, failover to `Metered` upon 429/quota exhaustion). Failover must be protected by explicit safety guardrails (`RequireInteractiveConfirm` or `AutoWithSessionCap`) to prevent silent financial depletion.
- **Driver 5: Context-Preserving Round Failover**: Round state, message transcripts, and tool envelopes must remain intact across failover, preventing memory loss and duplicate tool execution side-effects.

---

## Architectural Decisions

```text
┌────────────────────────────────────────────────────────┐
│ 1. Provider Driver (Ingress & Wire Protocol Contract)  │
│    - Physical Host URL + Serialization Codec           │
│    - Declares: CredentialCapability [Static, Negotiated]│
└───────────────────────────┬────────────────────────────┘
                            │ Instantiated as profiles
┌───────────────────────────▼────────────────────────────┐
│ 2. Connection Profile (Sovereign Credential Instance)  │
│    - CredentialBinding: Static(Secret) | Negotiated(Session)
│    - Entitlement:       FlatRate       | Metered       │
└───────────────────────────┬────────────────────────────┘
                            │ Scheduled by policy
┌───────────────────────────▼────────────────────────────┐
│ 3. Routing Policy (Execution Scheduling & Guardrails)  │
│    - Strategy: Direct(Connection)                      │
│              | PriorityFailover { primary, fallback }   │
│    - Guardrail: RequireInteractiveConfirm              │
│               | AutoWithSessionCap { max_usd }         │
└───────────────────────────┬────────────────────────────┘
                            │ Dispatches turns
┌───────────────────────────▼────────────────────────────┐
│ 4. Round Dispatcher (Zero-State Transport Execution)   │
│    - Resolves Token -> Stamps Header -> HTTP Stream    │
│    - Intercepts 429/QuotaExhausted -> Seamless Failover│
└────────────────────────────────────────────────────────┘
```

### 1. Ingress & Wire Contract Orthogonality (Abolition of Vendor and `-plan`)
- A Provider Driver represents strictly:
  $$\text{Provider Driver} = \text{Target Host Endpoint} + \text{Wire Serialization Protocol} + \text{Model Catalog Pipeline}$$
- "Vendor" is abolished as an architectural layer; it survives only as an inert, display-level catalog grouping tag.
- Commercial `-plan` suffixes are eliminated. Ingress drivers are named purely by physical gateway and protocol (e.g. `openai`, `chatgpt-codex`, `anthropic`, `google-gemini`, `google-antigravity`).

### 2. Symmetrical Credential Lifecycle (`Static` vs `Negotiated`)
We replace the category error with a symmetric lifecycle model:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum CredentialCapability {
    /// Static pre-shared token (user provided, passive holding, no refresh cycle).
    Static {
        default_env: Option<Cow<'static, str>>,
        header: HeaderPlacement,
    },
    /// Dynamically negotiated token (active lifecycle, protocol-managed refresh).
    Negotiated {
        protocol: AuthProtocol,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "header", rename_all = "kebab-case")]
pub enum HeaderPlacement {
    /// Authorization: Bearer <token>
    Bearer,
    /// Custom header (e.g. x-api-key: <token>, api-key: <token>)
    Custom(Cow<'static, str>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "kebab-case")]
pub enum AuthProtocol {
    OAuth2 {
        provider_id: Cow<'static, str>,
        flows: Vec<LoginMethod>,
    },
    CloudIam {
        provider_id: Cow<'static, str>,
    },
}
```

### 3. Orthogonal Economic Entitlement (`FlatRate` vs `Metered`)
Economic cost attributes belong exclusively to the `Connection` profile and `RoutingPolicy`, completely detached from the authentication protocol:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Entitlement {
    /// Zero marginal cost per turn (subscription, included monthly quota, free tier).
    FlatRate,
    /// Positive marginal cost per turn (pay-as-you-go, metered in NanoUSD).
    #[default]
    Metered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub name: String,
    pub provider: String,
    pub credential: CredentialBinding,
    pub entitlement: Entitlement,
}
```

### 4. Native Routing Policy and Spend Guardrail Architecture
The naive `default_connection: String` is superseded by a first-class `RoutingPolicy`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "kebab-case")]
pub enum RoutingPolicy {
    /// Direct single-connection dispatch (legacy compatible).
    Direct {
        connection: String,
    },
    /// Prioritized dual-lane dispatch: consume zero-marginal-cost FlatRate first,
    /// failover to Metered upon 429/quota ceiling.
    PriorityFailover {
        primary: String,
        fallback: String,
        guardrail: SpendGuardrail,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum SpendGuardrail {
    /// Suspend the round and emit an interactive audit confirmation to the client UI.
    RequireInteractiveConfirm,
    /// Automatic failover constrained by a session-scoped dollar ceiling.
    AutoWithSessionCap { max_usd: f64 },
}
```

### 5. Context-Preserving Round Failover Engine
When `PriorityFailover` encounters an HTTP 429 (`TooManyRequests`) or explicit quota exhaustion on the primary channel:
1. The transport layer intercepts the error and flags the primary connection with an active cooldown timer.
2. The agent harness verifies the `guardrail`:
   - If `RequireInteractiveConfirm`: emits `ServerEvent::FailoverAuditRequired` with estimated turn cost and enters `Suspended` state.
   - If `AutoWithSessionCap`: verifies session expenditure against `max_usd`.
3. Upon clearance, the active channel is swapped to `fallback` and the turn's existing message sequence, tool envelopes, and prompt-cache prefix are re-dispatched without turn abort or orphan tool state.

---

## Normative Invariants Established

- **`[INV-ROUTE-01] Provider Ingress and Wire Contract Orthogonality`**: A `ModelProviderSpec` represents exclusively physical network ingress host, protocol serializer, and catalog discovery. Provider specs must never be bifurcated along commercial pricing plans or vendor brand umbrellas.
- **`[INV-ROUTE-02] Symmetrical Credential Lifecycle Typestate`**: Credential capabilities must be modeled symmetrically as `Static` vs `Negotiated`. Top-level enum variants like `ApiKey` vs `OAuth` are prohibited as category errors. Specific protocols (`OAuth2`, `CloudIam`) reside strictly as sub-variants of `Negotiated`.
- **`[INV-ROUTE-03] Orthogonal Entitlement and Credential Decoupling`**: Authentication mechanisms and economic entitlement models (`FlatRate` vs `Metered`) are strictly independent. No transport or authentication struct may assume that OAuth denotes a subscription or that an API key denotes metered billing.
- **`[INV-ROUTE-04] Strict Non-Silent Failover and Spend Guardrail Barrier`**: Automated transition from a zero-marginal-cost `FlatRate` connection to a `Metered` connection within an agent turn MUST satisfy a `SpendGuardrail`. Silent downgrade without user interactive confirmation or a configured dollar ceiling is strictly prohibited.
- **`[INV-ROUTE-05] Context-Preserving Round Failover`**: Failover dispatch between connections preserves turn history, tool execution envelopes, and cache boundaries. Interrupted turns must never discard already verified tool outputs or duplicate side-effects.

---

## Consequences

### Positive Consequences
- **Elimination of Category Errors**: Establishes theoretical and ontological purity across all credential and provider abstractions.
- **Future-Proof Extensibility**: Adding enterprise IAM protocols (AWS STS, GCP Workload Identity, Passkeys) requires adding an `AuthProtocol` variant without modifying connection, routing, or transport code.
- **Financial Safety**: Users can confidently run long-horizon agents on flat-rate subscriptions with guaranteed protection against unexpected API credit card charges.
- **Clean Workspace Topology**: Paves the path for merging duplicated crates (`nuo-provider-*-plan`) into authentic physical gateway drivers.

### Negative Consequences & Trade-offs
- **State Suspension Complexity**: `nuo-harness` must maintain suspended round states while awaiting interactive failover approval.
- **Dual Connection Configuration**: Users configuring failover must declare two connections and bind them in a `RoutingPolicy`.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 (Rejected) — The "Vendor Layer" Pseudo-Abstraction
- **Why considered**: Grouping providers under vendor brands (e.g. OpenAI, Google, Anthropic).
- **Why rejected**: Collapses completely in real-world hosting: AWS Bedrock hosts Claude; Azure hosts OpenAI; OpenRouter hosts open-source models; local Ollama hosts Llama. Introducing a "Vendor" layer produces artificial multi-to-multi relationships with zero runtime utility.

### Option 2 (Rejected) — Sibling Enum of `ApiKey` vs `OAuth`
- **Why considered**: Familiar industry slang matching user-facing UI buttons.
- **Why rejected**: Category error. Collapses an artifact (static key string) and a protocol state machine (OAuth2) into the same level. Replaced by symmetric `Static` vs `Negotiated`.

### Option 3 (Rejected) — `OAuthSubscription` Conflation
- **Why considered**: OAuth is commonly used by consumers to connect subscriptions (ChatGPT Plus, Copilot).
- **Why rejected**: Entitlement leakage. Breaks completely on OAuth-authenticated metered services (Google Vertex AI, AWS Bedrock SSO) and key-authenticated subscriptions. Replaced by orthogonal `Entitlement` tagging.

### Option 4 (Rejected) — Silent Transport-Level Fallback
- **Why considered**: Implementing automatic failover in the HTTP client avoids changes to the agent harness.
- **Why rejected**: Severe financial safety hazard. Silently charging a user's API credit card when their subscription is rate-limited violates user trust and `[INV-ROUTE-04]`.

---

## Links

- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md), [ADR-0094](0094-codebuddy-browser-web-handoff-oauth-subscription-lane.md)
- Implementation Anchors: `crates/nuo-provider/src/connection_auth.rs`, `crates/nuo-provider/src/spec.rs`, `crates/nuo-protocol/src/connection_auth.rs`, `crates/nuo-persistence/src/connections.rs`, `crates/nuo-server/src/provider_registry.rs`, `crates/nuo-harness/src/round_lifecycle.rs`
