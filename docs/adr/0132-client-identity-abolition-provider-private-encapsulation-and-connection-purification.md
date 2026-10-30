---
id: ADR-0132
title: "Client Identity Abolition: Provider-Private Encapsulation and Connection Model Purification"
status: accepted
date: 2026-11-28
scope: provider/nuo-provider, persistence/nuo-persistence, protocol/nuo-protocol, server/nuo-server, presentation/nuo-view, host/nuo
superseded_by: null
negative_knowledge: true
---

# 0132. Client Identity Abolition: Provider-Private Encapsulation and Connection Model Purification

- Status: Accepted
- Date: 2026-11-28
- Deciders: Core Engineering, Architecture Working Group
- Amends: [ADR-0087](0087-model-wire-aperture-purification-and-provider-connection-orthogonality.md) (Purges vestigial `client_identity` module from `nuo-provider`), [ADR-0097](0097-physical-ingress-channel-topology-multi-realm-bifurcation-and-sovereign-connection-auth.md) (Localizes CodeBuddy wire headers to provider spec)

---

## Context and Problem Statement

Nuo previously maintained a centralized, 1,200-line client impersonation module (`crates/nuo-provider/src/client_identity.rs`) exporting a global `ClientProfile` / `ClientPreset` enum covering 14 distinct third-party IDE and CLI identities (`OpenCode`, `ClaudeCode`, `Codex`, `Cursor`, `Antigravity`, `CodeBuddy`, `Copilot`, etc.).

This design suffered from four fundamental architectural defects and category errors:

1. **Category Error between Protocol Invariants and Connection Instances**:
   A `Connection` is a user-configured named pipe to an upstream model provider (owning credentials, budgets, and model filters). Placing `client_identity` on `Connection` treated low-level transport handshake requirements as user-facing account attributes. In reality, 95% of standard connections (OpenAI, Anthropic, DeepSeek, Google, etc.) have zero need for client impersonation.
2. **Broken Encapsulation of Provider-Private Handshakes**:
   Platform-specific ingress requirements (e.g., Google Antigravity requiring `antigravity/cli/...` User-Agent, Tencent CodeBuddy requiring SaaS/CLI headers, OpenAI Codex requiring conversation-edits intent markers) were centralized into a monolithic global enum. Each provider's private transport markers were exposed to the entire workspace.
3. **Invalid State Space Inflation**:
   Placing a global `ClientPreset` on `Connection` allowed nonsensical combinations at the type level (e.g., configuring an OpenAI connection with `client_identity: Antigravity`), producing undefined upstream behavior.
4. **Pseudo-Requirement Exposure in Presentation**:
   When configuring connections in the TUI, curated providers correctly hid the `ClientIdentity` field, but custom providers offered an arbitrary cycling selector through unrelated IDE identities. Custom proxy/relay endpoints fundamentally require the ability to pass arbitrary HTTP headers, not an artificial, closed enum of IDE identities.

---

## Decision Drivers

- **First-Principles Demarcation**: Transport headers required by upstream vendors are provider-dialect wire invariants, not connection configurations.
- **Provider-Private Encapsulation**: Vendor-specific User-Agents and header sets belong strictly to their respective provider crates (`providers/nuo-provider-*`) and must never leak into shared protocol types.
- **Connection Model Purity**: The `Connection` struct in `nuo-persistence` and `connections.toml` must be cleansed of `client_identity`. Generic custom header passthrough is supported via `headers: BTreeMap<String, String>`.
- **Zero Legacy Burden** ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)): Completely abolish `ClientProfile`, `ClientPreset`, and `client_identity.rs` across all layers without deprecated facades or compatibility shims.

---

## Architectural Decisions

### 1. Abolition of `client_identity.rs` and Global Presets
The `crates/nuo-provider/src/client_identity.rs` module and all associated types (`ClientProfile`, `ClientPreset`, `ClientProfileSpec`, `ClientCapabilities`) are abolished.
- Baseline User-Agent for standard requests is declared as `nuo_provider::NUO_USER_AGENT` (`nuo/<version>`).
- Vendor-specific constants (Antigravity build numbers, CodeBuddy SaaS headers, Codex intent markers, Copilot integration IDs, ZCode agent markers) are relocated to the specific provider crates that speak those wire dialects.

### 2. Provider Spec Ingress Declarations (`user_agent` & `default_headers`)
`ModelProviderSpec` in `crates/nuo-provider/src/spec.rs` replaces the `default_client_profile` and `client_profile_sensitive` fields with pure transport attributes:
- `user_agent: Option<Cow<'static, str>>`: Upstream required User-Agent (or `None` for native `NUO_USER_AGENT`).
- `default_headers: Cow<'static, [(&'static str, &'static str)]>`: Fixed transport headers required by the provider's ingress gateway.

### 3. Connection Purification and Custom Header Passthrough
`Connection` in `crates/nuo-persistence/src/connections.rs` is purified:
- `client_identity` is removed.
- `headers: BTreeMap<String, String>` is added to allow user-defined custom proxy/relay endpoints to pass arbitrary HTTP headers (including custom `User-Agent`) when required.
- Deserialization gracefully sheds any legacy `client_identity` / `client_profile` field without serialization re-emission.

### 4. Route Derivation & Endpoint Simplification
In `crates/nuo-model-routing/src/derive.rs` and `crates/nuo-provider/src/endpoint.rs`:
- Effective User-Agent and headers are derived directly: `connection.headers` overlays `spec.user_agent` and `spec.default_headers`.
- `Endpoint` and `Transport` hold resolved `user_agent: String` and `headers: Vec<(String, String)>`, completely unburdened by third-party IDE profile abstractions.

### 5. Presentation Clean Break
`CustomField::ClientIdentity` is removed from `nuo-view` and `nuo::shell`. Adding or editing a provider exposes only relevant fields (`Name`, `BaseUrl`, `Token`, `Model`, `Protocol`, `Budget`).

---

## Invariants Established

- **`[INV-PROV-ENCAP-01] Provider Ingress Encapsulation`**:
  Vendor-specific User-Agent strings and proprietary ingress headers MUST be declared within the provider's own spec or channel implementation. No shared crate may maintain a cross-vendor catalog of client simulation strings.
- **`[INV-CONN-PURITY-01] Connection Schema Orthogonality`**:
  `Connection` represents an account pipe (identity, credentials, model scopes, budgets). It MUST NOT carry client emulation presets or protocol-level dialect flags.

---

## Negative Knowledge

- **Centralized Client Preset Monolith**: Attempting to model all industry AI coding tools (Cursor, Claude Code, Cline, Windsurf, Aider) in a single shared enum created high maintenance churn and leaky abstractions without providing real end-user value.
- **Client Identity on Connection**: Exposing `client_identity` as a first-class connection property created category errors and user confusion. Generic proxy header needs are satisfied by simple key-value header maps.
