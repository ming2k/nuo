# Subsystem Architecture: Nuo System Blueprint

- Status: Living Blueprint
- Last Updated: 2026-10-08
- Scope: workspace/topology, architecture/subsystems, substrate/tools, microkernel/topology, terminal/nuotc
- Maintainers: Nuo Architecture Working Group

---

## 1. System Overview & Monorepo Topology

Nuo (傩) is a self-contained AI session server, semantic terminal, and web host system. The project name derives from the Chinese Pinyin for 傩 (*Nuó*)—traditionally a ritual for expelling perils and disasters, later regarded as a ceremony for communing with the divine. According to traditional Nuo customs, the priest dons a Nuo mask (傩面) to convey intent according to the image and persona embodied by the mask. Nuo draws its core inspiration from this: using an execution harness to employ different identities to communicate with intelligence.

It is organized as a flat Cargo workspace containing 14 specialized crates arranged across three distinct tiers:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Applications & Frontends                        │
│   nuo (Server Host & CLI)    nuox (Semantic TUI)    web (SvelteKit)   │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
                    ▼                                ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      Subsystems & Host Harness                         │
│   nuo-client (SDK & Wire)  nuo-harness (Execution & Policy Harness)    │
│   nuo-host (PAL & Sandbox) nuo-persistence (SQLite Store & Memory)     │
│   nuo-providers (Catalog)                                              │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
                    ▼                                ▼
┌────────────────────────────────────────────────────────────────────────┐
│                       Substrates & Protocols                           │
│   acp (Coordination Protocol)       nuo-agent (Cognitive Loop)         │
│   nuo-tool & nuo-tool-derive        nuo-model-codec (Dialects & SSE)   │
│   nuo-mcp (MCP Transport)           nuotc (Retained Terminal Canvas)   │
└────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Core Subsystems and Domain Responsibilities

### 2.1 Protocols & Substrates
- **`nuo-wire`**: Canonical wire contracts — byte-level envelopes (`WireEnvelope`), control verbs (`ControlRequest`), streaming deltas (`StreamDelta`), barrier interception frames, and the shared zero-I/O domain contracts (capability traits, conversation/tool-output types, context-pressure model, events). Consolidated from the former `nuo-contracts` crate (ADR-0001).
- **`nuo-acp`** (*Agent Coordination Protocol*): Defines canonical inter-agent communication, addressing (`agent://`, `acp://`), tamper-evident HMAC signature envelopes, mailboxes, and multi-party channels. Optional feature-gated capability on `nuo-server`.
- **`nuo-agent`**: Implements the autonomous cognitive loop (think → act → observe), session turns, channel notification policies, two-tier context hygiene, and token compaction.
- **`nuo-tool` & **`nuo-tool-derive`**: Zero-runtime tool contracts defining schema, hazard risk profiles (`RiskProfile`), execution scopes (`ToolScope`), and cancellation tokens. `nuo-tool-derive` provides compile-time derive macros for JSON schemas.
- **`nuo-provider`**: Canonical leaf model provider specification defining core inference (`Provider`), catalog discovery (`CatalogDiscovery`), quota tracking (`QuotaTracker`), and pluggable `ProviderRegistry` (ADR-0015). Zero runtime and zero heavy persistence dependencies.
- **`nuo-model-codec`**: Multi-vendor wire protocol serialization (OpenAI, Anthropic, Google Gemini, DeepSeek) and SSE stream demuxing. Completely decoupled from agent cognitive loops.
- **`nuo-oauth`**: Canonical RFC 7636 (PKCE), RFC 8628 (Device Authorization), token refresh, and OAuth engine for model provider credentials (ADR-0027).
- **`nuo-provider-transport`**: Shared HTTP egress, SSE stream demuxing, retry policies, client presets, prompt caching, and request pipeline substrate consumed by concrete provider packages and OAuth authentication flows.
- **`nuo-mcp`**: Native Model Context Protocol client and server transport over stdio and JSON-RPC 2.0.
- **`nuotc`**: Retained-mode 2D terminal canvas, differential rendering pipeline, and Flexbox layout solver. Free of AI domain vocabulary.

### 2.2 Applications & Interface Engines
- **`nuo`**: The unified CLI application executable (ADR-0005). Dispatches interactive TUI (default), headless pipeline (`-p`), and server service (`serve`).
- **`nuo-tui`**: High-performance semantic terminal presentation engine built on `nuotc`. Library-only view crate consumed by `nuo`.
- **`nuo-client`**: Standalone client SDK providing discovery, connection management, event streaming, and command completion over IPC/WebSocket.

### 2.3 Subsystems & Host Infrastructure
- **`nuo-server`**: Pure service runtime managing Unix Domain Socket / WebSocket connection gateways, session registry, SQLite persistence coordination, and graceful shutdown.
- **`nuo-harness`**: Host execution harness: manages agent identity/role projection, permission brokering, human confirmation checkpoints, execution sandboxing, tool scheduling, and causal context compaction.
- **`nuo-persistence`**: SQLite transactional store, database migrations, configuration parsing, role memory, and full-text search indexing.
- **`providers/nuo-provider-*`** *(the authoritative provider namespace — [ADR-0027](../adr/0027-provider-definition-single-source-and-adapters-retirement.md), Accepted)*: Multi-vendor model catalog resolution, OAuth2/PKCE authentication flows, credential management, and concrete provider adapters implementing `nuo-provider` (ADR-0015). Each provider id has exactly one `ModelProviderSpec`, owned by its `providers/nuo-provider-*` crate, and the shipped build links every such crate (`[INV-PROV-06..09]`). The retired `nuo-provider-adapters` monolith's residual engines now live in `providers/nuo-provider-{catalog,siliconflow}` and the workspace-root **`nuo-oauth`** engine; each vendor crate additionally owns its `OAuthConfig`, device grant, and enricher and registers it through the typed `nuo_oauth::OAuthProvider` port. The **provider composition root** (`init()`, `MODEL_PROVIDER_SPECS`, `build_provider_for_channel`, typed `QuotaPort` dispatch, and OAuth-provider registration) lives in `nuo-server::provider_registry`, and both entry points (`nuo` and the server bootstrap) call it. Operates the **two local-direct invocation lanes** plus a **subscription server-proxy surface** (ADR-0014):
  - *Local-direct · static-key lane* — `ConnectionAuth::ApiKey` resolves a static bearer from `api_key_env` (env-first) or `credentials.toml` (mode 0600), speaking OpenAI chat-completions / Responses / Anthropic Messages / Google Gemini against first-party (OpenAI, Anthropic, DeepSeek, xAI, Kimi, GLM-CN, Qianwen) or relay (OpenRouter, OpenCode Zen/Go) endpoints; keyless relays send no auth header at all.
  - *Local-direct · OAuth subscription lane* — `ConnectionAuth::Subscription` resolves live bearer tokens from `auth.toml` through per-provider `OAuthConfig` presets (ChatGPT/Codex PKCE loopback at `127.0.0.1:1455`, Google Antigravity browser PKCE w/ `cloudaicompanionProject` onboarding, GitHub Copilot device grant + internal token mint, xAI SuperGrok device grant, Qoder custom device flow, OpenCode Console device grant). Tokens refresh proactively on a 120 s skew and reactively on HTTP 401 single-flight retry.
  - *Subscription server-proxy lane (planned surface)* — a `DialectSurface::SubscriptionProxy` sends vendor-generic envelopes to `POST {api}/alpha/generate` (SSE); the server holds vendor credentials, rewrites the model family, enforces `planId` × `windowLimits{fiveHour, weekly}` entitlement, and returns per-turn routing provenance in `providerMetadata.<resolved>.routing.planningReasoning` + `finish-step.response.headers`. The client never sees a vendor key; `--local-only` / `NUO_LOCAL_ONLY=1` refuses this lane outright.
- **`nuo-host`**: Host execution environment: process supervision (process groups / Windows Job Objects), workspace sandboxing, cross-platform paths, secure file operations, and native host tools.

---

## 3. Terminal Canvas Subsystem (`nuotc`)

`nuotc` provides retained-mode terminal rendering structured into six decoupled, acyclic modules:

```text
┌────────────────────────────────────────────────────────┐
│                      nuotc (Root)                      │
│   Unified Retained-Mode Terminal Canvas & Diff Engine  │
└───────────────────────────┬────────────────────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        ▼                   ▼                   ▼
 ┌──────────────┐    ┌──────────────┐    ┌──────────────┐
 │      ui      │    │   widgets    │    │    layout    │
 │ Retained     │    │ Block, Para, │    │ Rect, Flex,  │
 │ Scene Graph  │    │ Clear, Spans │    │ Anchor       │
 └──────┬───────┘    └──────┬───────┘    └──────┬───────┘
        │                   │                   │
        └───────────────────┼───────────────────┘
                            │
        ┌───────────────────┴───────────────────┐
        ▼                                       ▼
 ┌──────────────┐                        ┌──────────────┐
 │   terminal   │                        │    render    │
 │ Backend,     │                        │ Run-length   │
 │ Frame, Loop  │                        │ Diff, Driver │
 └──────┬───────┘                        └──────┬───────┘
        │                                       │
        └───────────────────┬───────────────────┘
                            │
                            ▼
                     ┌──────────────┐
                     │    buffer    │
                     │ Cell, Grid,  │
                     │ Text, Glyph  │
                     └──────────────┘
```

- **`buffer`**: Compact cell memory (`Cell`, `Color`, `Modifier`, `Style`), 2D `Grid` with write-marks-dirty (`dirty_col`) line tracking, Unicode grapheme clustering, and standard glyph tables.
- **`layout`**: Spatial geometry (`Rect`), Flexbox layout solver (`Flex`, `FlexItem`, `SolvedFlex`), and collision-free anchored popups (`AnchorPlacement`, `compute_anchored_rect`).
- **`render`**: Pure run-length packed differential rendering (`diff`), display list recording (`Canvas`, `DisplayList`, `RenderNode`), escape code drivers, and standards-based terminal profiles (`TerminalProfile`).
- **`terminal`**: Low-level terminal mode management (`Backend`, `Bce`), alternate screen lifecycles, and double-buffered `Frame` loop.
- **`widgets`**: Declarative UI primitives (`Block`, `Paragraph`, `Clear`) and structural composition containers (`Column`, `Row`, `Stack`, `Container`, `Spacer`, `Divider`).
- **`ui`**: Retained scene graph (`Scene`) and component event dispatch runtime (`UiRuntime`).

---

## 4. Architectural Invariants Constitution

- **`[INV-WS-01]` Self-Contained Workspace**: All workspace crates and substrates must resolve within the local repository without relying on external sibling path dependencies (`../`).
- **`[INV-ARCH-FLAT-01]` Flat Workspace Topology**: No intermediate directory grouping (`apps/` or `crates/`). Every crate lives directly at the workspace root to eliminate path indirection.
- **`[INV-HOST-01]` Sandbox & Containment**: All local command execution and file mutations must pass through `nuo-host` sandbox and containment boundaries.
- **`[INV-TOOL-01]` Decentralized Capability Tools**: Tools are natively owned and implemented by their capability crates (`acp`, `nuo-host`, `nuo-persistence`) using the zero-runtime `nuo-tool` standard, rather than concentrated in a monolithic harness.
- **`[INV-VER-01]` Federated Cluster SemVer**: Autonomous substrates (`nuotc`, `acp`) maintain independent versions; the host suite (`nuo`, `nuox`, `nuo-*`) evolves in unified lockstep.
- **`[INV-MOD-01]` Zero Domain Leakage**: Terminal graphics substrates (`nuotc`) and wire codecs (`nuo-model-codec`) must remain completely free of application-level agent concepts.
- **`[INV-MOD-02]` Ghost-Free CJK Cells**: Wide-character trailing cells are populated by the writer with matching background attributes; diff never emits unowned padding spaces.
- **`[INV-MOD-03]` Authoritative Retained Grid**: The retained `Grid` is the single source of truth for desired visual state; dirty line tracking eliminates redundant full-frame rasterization.
- **`[INV-AUTH-04]` Dialect-Carried Transport (ADR-0014)**: Wire protocol and client-identity profile derive from `ProviderDialect` and `ClientPreset`, never from the auth lane or the connection name; request builders never branch on auth mode to pick a wire shape.
- **`[INV-AUTH-02]` Secret Storage Firewall (ADR-0014, ADR-0032)**: OAuth token sets live only in `auth.toml` (`$XDG_STATE_HOME/nuo/auth.toml`, 0600), API keys only in `credentials.toml` (`$XDG_STATE_HOME/nuo/credentials.toml`, 0600); raw secrets never enter `$XDG_CONFIG_HOME/nuo`, `connections.toml`, or any versionable file.
- **`[INV-LANE-03]` Server-Enforced Entitlement (ADR-0014)**: On the subscription server-proxy lane, plan gate, model availability, credits and provider routing are resolved only by the control plane (`api.commandcode.ai/alpha/generate`); client-side `Availability` filter is a projection of the resolved answer, never authority over it — live-probed with a GOAT key where `claude-sonnet-4-6`/`gpt-5.4`/`muse-spark-1.1` return `403 MODEL_NOT_IN_PLAN` but `claude-sonnet-5-5` routes through a gateway, upstream-provider mismatch (`openai/gpt-5.6-sol → openai`, `moonshotai/Kimi-K2.5 → bedrock`, `deepseek-v4-flash → deepseek-v4.1-flash@novita`).
- **`[INV-LANE-04]` Routing Provenance Record (ADR-0014)**: The client persists `providerMetadata.<resolved>.routing.planningReasoning` verbatim from the `finish-step` envelope into session telemetry; a local-direct lane with no proxy provenance takes `None`, never a fabricated sentence.
- **`[INV-PROV-06]` Single Definition (ADR-0027)**: Each model provider id has exactly one authoritative `ModelProviderSpec`, in its `providers/nuo-provider-*` crate. A second definition of the same id is a defect, not a staging state.
- **`[INV-PROV-07]` No Orphan Contract (ADR-0027)**: Every public item in `nuo-provider` has at least one non-test consumer in the shipped build; a contract with zero production consumers must be wired or deleted in the same change that introduces it.
- **`[INV-PROV-08]` No String-Sniffing Dispatch (ADR-0027)**: Capability dispatch keys off typed declarations; no code path selects behaviour by matching a provider id or base-URL substring.
- **`[INV-PROV-09]` Providers Are Linked (ADR-0027)**: The shipped build links every crate that claims to be a provider; an unlinked `providers/nuo-provider-*` crate is dead code.
- **`[INV-PROV-10]` Behaviour Behind Ports (ADR-0027)**: Static facts are `&'static` tables; runtime facts are overlays; behaviour is a trait port. No fourth mechanism.

---

## 5. Architectural Lineage & ADR Registry

| ADR | Title | Decision Summary | Primary Impact |
| :--- | :--- | :--- | :--- |
| **ADR-0001** | Flat Workspace Topology and Microkernel Capability Architecture | Abolish nested dirs for flat workspace, enforce microkernel harness role, decentralize capability tools, and eliminate catch-all contracts | Workspace topology, `nuo-harness`, `nuo-host`, `nuo-model-codec` |
| **ADR-0002** | Canonical Agent Coordination Protocol (ACP) Standard | Establish ACP specification with URI addressing, HMAC envelopes, fabric channels, and native collaboration tools | `acp`, inter-agent collaboration |
| **ADR-0003** | Autonomous Retained-Mode Terminal Canvas Engine (Nuotc) | Retained-mode 2D terminal canvas with differential double-buffered rendering strictly decoupled from AI domain concepts | `nuotc`, `nuox` |
| **ADR-0004** | Federated Cluster SemVer and Release Topology | Partition workspace into 4 SemVer clusters with dual-resolving version and path dependencies | Release engineering, manifests, versioning |
| **ADR-0014** | Model Provider Invocation Schemes: Local Direct, and the Subscription Server-Proxy Lane | Ratify three-lane dispatch (`ConnectionAuth::{ApiKey, Subscription}` over trait-object credential sources + verified SSPL `/alpha/generate` server-proxy DialectSurface); entitlement authority belongs to the credential-holding lane, never to client UI gating | `nuo-providers`, `nuo-model-codec`, `nuo-persistence/connections`, `nuo-server` |
| **ADR-0015** | Canonical Model Provider Contract Crate (nuo-provider) and Dedicated Providers Namespace Architecture | Establish canonical leaf contract crate `nuo-provider`, dedicate `providers/nuo-provider-*` namespace matching `tools/`, extract `nuo-provider-transport`, and sandbox cryptographic dialects | `nuo-provider`, `providers/nuo-provider-*`, `nuo-harness`, `nuo-server` |
| **ADR-0026** | Streamed Tool-Input Progress: Announce the Tool Call Before Its Arguments Finish | Emit `ToolCallStarted` (name known, arguments still streaming) plus a count-only `ToolInputProgress` tick; the full argument object still gates execution, so a running step and the tool phase appear before arguments finish | `nuo-harness`, `nuo-wire`, `nuo-tui` |
| **ADR-0027** (Accepted) | Provider Definition Single Source: Retire nuo-provider-adapters and Relocate Its Residual Engines | Make `providers/nuo-provider-*` the single authoritative definition per provider and retire the duplicated `nuo-provider-adapters` (OAuth engine, catalog parsers, and composition root relocated to dedicated crates / `nuo-server`); establishes one-definition, no-orphan-contract, no-string-sniffing, and linked-provider invariants | `nuo-provider-adapters` (removed), `providers/nuo-provider-*`, `nuo-server`, `nuo-provider` |
