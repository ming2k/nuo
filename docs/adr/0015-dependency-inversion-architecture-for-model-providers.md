---
id: ADR-0015
title: "Canonical Model Provider Contract Crate (nuo-provider) and Dedicated Providers Namespace Architecture"
status: accepted
date: 2026-10-04
scope: providers/architecture, substrate/nuo-provider, providers/namespace, architecture/layering
superseded_by: null
negative_knowledge: true
---

# 0015. Canonical Model Provider Contract Crate (nuo-provider) and Dedicated Providers Namespace Architecture

- Status: Accepted
- Date: 2026-10-04
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Capability, Interface Teams, and Model Infrastructure Engineers
- Informed: System Architects
- Amended by: [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md) (accepted) — executes this ADR's unfinished `providers/` relocation and retires `nuo-provider-adapters`. Transport substrate `nuo-provider-transport` was subsequently relocated to the workspace root alongside `nuo-oauth` to preserve namespace purity (`providers/` housing exclusively concrete channel adapters) and prevent downward layer-inversion. This ADR's §1 topology and `[INV-PROV-01..05]` remain the binding direction; §2's renaming table and the §"Monolithic Provider Adapter" rejection are unchanged.
- Complements: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md), [ADR-0008](0008-single-tool-contract.md), [ADR-0010](0010-harness-decomposition-and-agent-unification.md), [ADR-0013](0013-decoupled-tool-namespace-and-infrastructure-purity.md), [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md)

---

## Context and Problem Statement

Following the successful migration of capability tools to the Dependency Inversion Principle (DIP) in [ADR-0008](0008-single-tool-contract.md) and [ADR-0013](0013-decoupled-tool-namespace-and-infrastructure-purity.md), the tool architecture achieved clean decoupling via the standalone leaf contract crate `nuo-tool` and the dedicated implementation namespace `tools/nuo-tool-*`. All cognitive runtimes (`nuo-harness`, `nuo-agent`) depend solely on `nuo-tool`, while concrete capability tools (`tools/nuo-tool-fs`, `tools/nuo-tool-exec`, etc.) implement its contract.

In contrast, model provider handling suffered from several deep structural problems:
1. **Lack of a Dedicated Leaf Contract Crate**: Unlike `nuo-tool` for tools, there was no `nuo-provider` crate. Provider traits and types were scattered between `nuo-model-codec` (wire encoding), `nuo-wire` (vocabulary), and a monolithic adapter crate (`nuo-provider-adapters`).
2. **Coarse-Grained "Vendor" Fallacy vs. "Service Surface / Channel" Reality**: Modeling providers purely by corporate vendor name (e.g., "OpenAI" or "Google") collapsed fundamentally distinct channels:
   - *OpenAI Platform API* (pay-per-token direct key) vs. *ChatGPT Plan* (Plus/Pro Codex subscription over browser PKCE).
   - *Google AI Studio* (standard developer key) vs. *Google Antigravity* (CloudCode OAuth with developer onboarding).
   - *OpenCode Plan* (multi-protocol gateway dispatching Claude, GPT, and GLM across distinct wire protocols under one account).
   Different channels under the same vendor carry completely disjoint endpoints, auth mechanisms, quota models, and model catalogs.
3. **Monolithic Cryptographic Leakage**: Legacy dialects (such as Qoder's COSY signature) required heavy cryptographic libraries (`rsa`, `aes`, `cbc`, `md5`). In a monolithic adapter, every consumer was forced to compile this heavy cryptographic stack.
4. **Downstream Inversion Failure**: High-level governance (`nuo-harness`) and presentation (`nuo-tui`) were directly coupled to concrete adapter crates rather than pure contracts.

We require an uncompromising, future-facing DIP architecture: establishing `nuo-provider` as the sole canonical leaf contract crate, establishing a dedicated `providers/nuo-provider-*` namespace matching `tools/nuo-tool-*`, extracting shared transport primitives into `providers/nuo-provider-transport`, isolating vendor-specific cryptographic dialects into independent crates, and enforcing channel-oriented service surfaces.

---

## Decision Drivers

- **Canonical Symmetry with `nuo-tool`**: Just as `nuo-tool` owns tool contracts and `tools/nuo-tool-*` houses concrete implementations, `nuo-provider` must own provider contracts and `providers/nuo-provider-*` must house concrete channel implementations.
- **Service Surface / Channel as First-Class Unit**: Classify integrations by concrete access channel/plan (`chatgpt-plan`, `commandcode-plan`, `opencode-plan`, `kimi-code`, `google-antigravity`) rather than vague corporate brand names.
- **Dependency Inversion Principle (DIP)**: Orchestration (`nuo-harness`, `nuo-agent`) and presentation (`nuo-tui`) depend strictly on `nuo-provider` abstractions; they must never link concrete implementation or transport crates.
- **Cryptographic & Protocol Sandboxing**: Heavy cryptographic dependencies (Qoder's RSA/AES stack) must be physically isolated in dedicated channel crates (`providers/nuo-provider-qoder`), ensuring general-purpose providers compile with zero crypto bloat.
- **Separation of RFC Primitives from Vendor Workflows**: Standard RFC 7636 (PKCE), RFC 8628 (Device Code), and loopback HTTP redirect listeners reside in the workspace-root `nuo-oauth` (which also owns the stateful engine); vendor-specific parameters and token exchange workflows reside in channel crates and register through the typed `OAuthProvider` port.

---

## Decision Outcome

### 1. Architectural Topology

```text
nuo-provider (Layer 0: Pure Contract Substrate at root)
  • Provider (Inference), CatalogDiscovery (Models), QuotaTracker (Balance)
  • ProviderFactory, CredentialHost, ProviderDescriptor, ProviderRegistry
nuo-provider-transport (Layer 1: Shared Transport Substrate at root)
  • Shared HTTP egress, SSE demuxing, request pipeline, prompt cache
nuo-oauth (Layer 1: RFC & OAuth Engine Substrate at root)
  • RFC 7636 PKCE, RFC 8628 Device Code, token refresh
  ▲
  │ implements / consumes
providers/ (Layer 2: Dedicated Channel & Adapter Namespace)
  ├── nuo-provider-qoder               # Qoder dialect & sandboxed RSA/AES/MD5 crypto
  ├── nuo-provider-openai              # OpenAI Developer Platform API
  ├── nuo-provider-chatgpt-plan        # ChatGPT Plan Codex subscription (OAuth PKCE)
  ├── nuo-provider-anthropic           # Claude Messages API (Thinking, Prompt Caching)
  ├── nuo-provider-google-antigravity  # Google AI Studio & Google Antigravity CloudCode
  ├── nuo-provider-copilot             # GitHub Copilot Device Code & session minting
  ├── nuo-provider-commandcode-plan    # CommandCode Plan (SSPL proxy & window credits)
  ├── nuo-provider-opencode            # OpenCode Console, Plan, and Zen multi-protocol surfaces
  ├── nuo-provider-deepseek            # DeepSeek V4 (Responses & /user/balance)
  ├── nuo-provider-kimi                # Kimi Code (K3 always-on reasoning & /balance)
  ├── nuo-provider-openrouter          # OpenRouter gateway & /auth/key quota
  ├── nuo-provider-xai                 # xAI Grok API
  ├── nuo-provider-qianwen             # Qianwen Token Plan
  └── nuo-provider-zai                 # ZAI / GLM-CN Platform
```

### 2. Elimination of Unused Subscription Channels and Renaming

- **`chatgpt-plan`**: Formally renamed from `openai-subscription` to `chatgpt-plan` (`ChatGPT Plan`).
- **OpenAI Subscription Removal**: The redundant OpenAI API subscription channel is eliminated; OpenAI maintains solely `openai` (OpenAI Platform API).
- **Google Subscription Removal**: Redundant Google API subscription channels are eliminated; Google maintains `google` (AI Studio API) and `google-antigravity` (CloudCode OAuth).
- **`commandcode-plan`**: Formally designated as `commandcode-plan` (`CommandCode Plan`).
- **`kimi-code`**: Standardized as `kimi-code` (`Kimi Code`).
- **`opencode-plan`**: Renamed from `opencode-go` to `opencode-plan` (`OpenCode Plan`) to accommodate upcoming multi-tier subscription plans.

### 3. Substrate Purity and Decoupled Downstream

- `nuo-provider` maintains `default-features = false` on `nuo-model-codec`, ensuring **zero Netune / TLS / HTTP dependencies** in the contract crate.
- `nuo-harness` has **zero dependencies** on `nuo-provider-adapters` or any crate in `providers/`. It communicates solely with `nuo-provider` contracts and receives concrete drivers via the composition root (`nuo` / `nuo-server`).

---

## Invariants & Behavioral Boundaries

- **`[INV-PROV-01] Canonical Provider Leaf Contract`**: `nuo-provider` is the sole source of truth for provider capability contracts (`Provider`, `CatalogDiscovery`, `QuotaTracker`, `ProviderDescriptor`). It must reside directly at the workspace root and have zero heavy runtime or network dependencies.
- **`[INV-PROV-02] Dedicated Providers Namespace`**: Concrete channel adapters and provider drivers MUST reside in `providers/nuo-provider-*`. Root directory crates are reserved for core substrates, protocols, and host infrastructure.
- **`[INV-PROV-03] Orthogonal Capability Segregation`**: Inference, catalog discovery, quota tracking, and credential resolution are strictly separate traits. No composite "fat provider" trait combining these concerns is permitted.
- **`[INV-PROV-04] Downstream Inversion Enforcement`**: High-level orchestrators (`nuo-harness`, `nuo-agent`) and presentation clients (`nuo-tui`) must depend exclusively on `nuo-provider` abstractions; they must never link concrete implementation crates in `providers/`.
- **`[INV-PROV-05] Cryptographic Sandboxing`**: Heavy cryptographic algorithms (RSA, AES-CBC, MD5) required by proprietary protocols (Qoder) must be isolated within `providers/nuo-provider-qoder`. They must never appear in shared transport or generic provider crates.

---

## Negative Knowledge & Rejected Alternatives

### 1. Naive "One Crate per Corporate Brand" (Vendor-First Fallacy)
- **Why considered**: Grouping all services from Google under `nuo-provider-google` or all from OpenAI under `nuo-provider-openai`.
- **Why rejected**: A vendor is a commercial company, not a technical service surface. Under OpenAI, the API platform and the ChatGPT Codex subscription have completely different endpoints, auth mechanisms, model catalogs, and rate limits. Collapsing them into one vendor crate re-introduces internal branching and coupling.

### 2. Pure "Wire-Protocol-First" Crate Partitioning
- **Why considered**: Creating `nuo-provider-openai-wire`, `nuo-provider-anthropic-wire`, etc.
- **Why rejected**: Fails for multi-protocol aggregation platforms (OpenCode Plan, GitHub Copilot). An OpenCode account routes Claude models over Anthropic wire, GPT models over OpenAI Responses, and GLM models over Chat Completions under a single authentication token. Organizing crates purely by wire protocol breaks platform providers that span multiple wire formats.

### 3. Monolithic Provider Adapter (nuo-provider-adapters)
- **Why considered**: Kept all provider code in a single secondary adapter crate.
- **Why rejected**: Creates a single point of failure and compilation bottleneck. Adding a new provider forced modifying the monolithic registry, and legacy crypto dependencies leaked across all providers.
