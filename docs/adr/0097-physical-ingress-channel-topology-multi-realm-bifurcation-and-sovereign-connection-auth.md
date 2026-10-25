---
id: ADR-0097
title: "Physical Ingress Channel Topology, Multi-Realm Channel Bifurcation, and Sovereign Connection Auth Orthogonality"
status: accepted
date: 2026-11-28
scope: provider/nuo-provider, server/nuo-server, protocol/nuo-protocol, persistence/nuo-persistence, oauth/nuo-oauth, tui/nuo-tui
superseded_by: null
negative_knowledge: true
---

# 0097. Physical Ingress Channel Topology, Multi-Realm Channel Bifurcation, and Sovereign Connection Auth Orthogonality

- Status: Accepted
- Date: 2026-11-28
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0079](0079-opencode-provider-channel-bifurcation-and-legacy-container-retirement.md), [ADR-0094](0094-codebuddy-browser-web-handoff-oauth-subscription-lane.md), [ADR-0095](0095-canonical-dual-lane-routing-provider-capability-and-byos-byok-unification.md)

---

## Context and Problem Statement

A recurring architectural question across LLM CLI engines is the classification granularity of a **"Model Provider"** and how authentication schemes map to it:
1. **The Granularity Dilemma**: Should a provider represent a Corporate Vendor (OpenAI, Google, Tencent), an End-User Client Product (ChatGPT, CodeBuddy, Copilot, Cursor), or an Atomic Ingress Channel (Target Host Endpoint + Wire Protocol + Catalog Pipeline)?
2. **The Multi-Scheme Monolith Temptation**: The urge to build a "Universal Provider Auth Layer" within a single provider capable of dynamically switching between $N$ validation schemes (BYOK, BYOS, domestic/international regional endpoints, staging/prod gateways).
3. **The Empirical Failure Mode (ADR-0095 Postmortem)**: In ADR-0095, Nuo attempted to model authentication capabilities via a dynamic matrix (`CredentialCapability::Static`, `CredentialCapability::Negotiated`, `AuthCapabilitySet`). In practice, this produced 1,500+ lines of architecture astronautics that created fragile `connections.toml` parsing, introduced unreachable dead code paths (`codebuddy-intl` OAuth surface), and led to silent server connection dropouts. Commit `cd2fc8a` purged this vocabulary outright.
4. **Physical Reality of Network Ingress**: An authentication scheme cannot be isolated as a decorative header swap. When authentication changes (e.g. from Bearer JWT to API Key, or from domestic `copilot.tencent.com` to international `www.codebuddy.ai`), it fundamentally alters the target host URL, the wire serialization headers, the request signature algorithm (e.g. Qoder COSY RSA/AES), the available model universe, and the 401/403 error recovery state machine.

An uncompromising, permanent standard is required to resolve provider granularity, protect against cross-realm token poisoning, and guarantee strict 1:1 connection authentication binding without legacy baggage.

---

## Decision Drivers

- **Physical Sockets Over Corporate Brands**: Agent execution is an I/O subsystem; it connects to concrete network hosts with TLS handshakes and byte codecs, not abstract corporate logos.
- **Zero Cross-Realm Token Poisoning**: Cross-realm token replays (e.g., refreshing domestic CodeBuddy tokens against an international IdP gateway) invalidate token families and trigger provider account suspensions.
- **Sovereign Connection Financial Determinism**: Connections are the sole boundary of economic liability and turn execution. A connection must resolve to exactly one deterministic authentication binding to eliminate silent credit card charges and billing surprises.
- **Zero Architecture Astronautics (Clean Break)**: Eliminate pseudo-capabilities, unconsummated trait frameworks, and internal provider routing engines. Provider drivers remain simple, stateless, and 1:1 with network gateways.

---

## Architectural Decisions

```text
┌────────────────────────────────────────────────────────┐
│ Layer 1: Human / Display Taxonomy (UI Presets & Tags) │  e.g. "Tencent", "Google", "OpenAI"
│          - Pure display metadata, zero runtime I/O      │
└───────────────────────────┬────────────────────────────┘
                            │ Discovers & groups
┌───────────────────────────▼────────────────────────────┐
│ Layer 2: Provider Channel (Physical Ingress Unit)      │  e.g. codebuddy, chatgpt-plan, openai
│          = Target Host URL + Wire Codec + Catalog Pipe │
│          - Exactly 1 authoritative Spec per Channel    │
└───────────────────────────┬────────────────────────────┘
                            │ Instantiates
┌───────────────────────────▼────────────────────────────┐
│ Layer 3: Sovereign Connection (Execution Profile)      │  e.g. "work-codebuddy"
│          - 1:1 Auth Binding: ApiKey OR Subscription   │
│          - Zero internal fallback or silent switching  │
└────────────────────────────────────────────────────────┘
```

### 1. Ingress Channel as the Canonical Provider Atomic Unit
A Model Provider represents strictly:
$$\text{Model Provider} = \text{Target Host Endpoint} + \text{Wire Codec Dialect} + \text{Model Catalog Pipeline}$$
- **"Vendor" is abolished as a runtime abstraction layer**: Corporate brands survive only as inert UI catalog tags and preset search keywords.
- **Every Provider is bound 1:1 to a physical host**: A `ModelProviderSpec` declares exactly one canonical root URL. It never contains internal branching for multi-datacenter, domestic vs. international, or staging vs. production routing.

### 2. Multi-Realm Channel Bifurcation Rule
Whenever a commercial service operates geographically or operationally disjoint gateways with separate authentication realms:
- They **MUST** be modeled as distinct, first-class Provider Channels (e.g. `codebuddy-cn` for `copilot.tencent.com` vs. `codebuddy-intl` for `www.codebuddy.ai`; `openai` for `api.openai.com` vs. `chatgpt-plan` for `chatgpt.com`).
- They **MUST NOT** be consolidated into a single polymorphic provider with internal endpoint switching.

### 3. Sovereign Connection 1:1 Authentication Binding
- A `Connection` instance persists exactly one `ConnectionAuth` variant: `ConnectionAuth::Static` or `ConnectionAuth::Negotiated { provider }`.
- Multi-scheme dynamic switching inside an active connection is prohibited.
- If an API key is supplied on a negotiated template during creation, the connection is downgraded outright to `ConnectionAuth::Static` (`[INV-LANE-01]`).
- Credentials for static keys live in `credentials.toml` (mode `0600`); dynamic OAuth token sets live in `auth.toml` (mode `0600`).

---

## Normative Invariants Established

- **`[INV-PROV-CHANNEL-01] Physical Ingress Symmetry`**: Every registered `ModelProviderSpec` is permanently bound to exactly one physical host endpoint. Dynamic host switching inside an active provider driver is forbidden.
- **`[INV-PROV-AUTH-01] Connection-Level Auth Exclusivity`**: A `Connection` instance must resolve its bearer through exactly one authentication scheme (`ConnectionAuth::Static` or `ConnectionAuth::Negotiated`). No mixed-state or runtime scheme mutation is permitted.
- **`[INV-PROV-REALM-01] Cross-Realm Token Firewall`**: Tokens and refresh credentials minted for one provider channel must never be replayed or refreshed against another channel's host.
- **`[INV-PROV-TAXONOMY-01] Presentation/Ingress Separation`**: Corporate brand names and client product groupings exist exclusively in the UI layer (`ConnectionTemplate`, `PROVIDER_PRESETS`), completely decoupled from server-side provider registry drivers.

---

## Consequences

### Positive
- **Compile-Time Isolation**: Upstream changes to one channel's authentication flow or endpoint cannot break or introduce regressions into other channels.
- **Zero Cross-Realm Collisions**: Guarantees that token refresh requests are always sent to the exact IdP realm that minted the grant.
- **Financial Safety**: Users have absolute transparency over whether a connection consumes subscription quota (`FlatRate`) or incurs pay-per-token API charges (`Metered`).
- **Clean Workspace Topology**: Eliminates over-engineered capability traits and complex internal dispatchers.

### Negative & Trade-offs
- **Catalog Preset Multiplicity**: Distinct channels under the same brand appear as separate entries in the connection presets list (e.g., `OpenAI Platform` vs. `ChatGPT Plan`). Mitigated by multi-token fuzzy search and search tag aliases in `/connections`.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 (Rejected) — Universal In-Provider Multi-Scheme Auth Layer (`AuthCapabilitySet`)
- **Why considered**: Attempting to make a single provider declare $N$ authentication schemes and endpoints via a dynamic capability matrix (ADR-0095).
- **Why rejected**: Catastrophic failure in commit `cd2fc8a`. Produced 1,558 lines of dead-code abstractions, deserialization fragility in `connections.toml`, and unreachable code paths. An authentication scheme is not a decorative header; it dictates the entire wire transport, host URL, and error state machine.

### Option 2 (Rejected) — The Monolithic Corporate "Vendor" Container
- **Why considered**: Grouping all services under a single corporate brand (e.g. "Tencent", "Google", "OpenAI").
- **Why rejected**: Collapses completely in modern cloud hosting (AWS Bedrock hosting Claude; Azure hosting OpenAI; OpenRouter hosting multi-vendor models). Produces artificial multi-to-multi relationships and monolithic God Objects with conflicting cryptographic dependencies.

### Option 3 (Rejected) — Dynamic In-Connection Endpoint / Region Switching
- **Why considered**: Allowing a connection to select "Domestic" vs. "International" or "Staging" vs. "Prod" via an in-connection toggle.
- **Why rejected**: Inevitably leads to cross-realm token poisoning. Refreshing a domestic token against an international gateway causes upstream IdPs to invalidate the entire refresh family and ban the session.

---

## Links

- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0079](0079-opencode-provider-channel-bifurcation-and-legacy-container-retirement.md), [ADR-0094](0094-codebuddy-browser-web-handoff-oauth-subscription-lane.md), [ADR-0095](0095-canonical-dual-lane-routing-provider-capability-and-byos-byok-unification.md)
- Implementation Anchors: `crates/nuo-provider/src/connection_auth.rs`, `crates/nuo-provider/src/spec.rs`, `crates/nuo-server/src/provider_registry.rs`, `crates/nuo-persistence/src/connections.rs`, `crates/nuo-tui/src/providers.rs`
