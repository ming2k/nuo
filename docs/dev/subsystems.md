# Subsystem Architecture: Nuo System Blueprint

- Status: Living Blueprint
- Last Updated: 2026-12-19
- Scope: workspace/topology, architecture/subsystems, substrate/tools, microkernel/topology, terminal/nuotc
- Maintainers: Nuo Architecture Working Group

---

## 1. System Overview & Monorepo Topology

Nuo (傩) is a self-contained AI session server, semantic terminal, and web host system. The project name derives from the Chinese Pinyin for 傩 (*Nuó*)—traditionally a ritual for expelling perils and disasters, later regarded as a ceremony for communing with the divine. According to traditional Nuo customs, the priest dons a Nuo mask (傩面) to convey intent according to the image and persona embodied by the mask. Nuo draws its core inspiration from this: using an execution harness to employ different identities to communicate with intelligence.

It is organized as an uncompromising, modern Cargo workspace ([ADR-0055](../adr/0055-hierarchical-workspace-topology-and-component-namespace.md)) containing specialized crates arranged across three tiers plus the application entry point:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                   Application Entry Point                              │
│   crates/nuo (Unified Executable: CLI, TUI Launcher, Server Daemon)    │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
                    ▼                                ▼
┌────────────────────────────────────────────────────────────────────────┐
│               Component Libraries (crates/nuo-*)                       │
│   crates/nuo-protocol          crates/nuo-acp                          │
│   crates/nuo-server            crates/nuo-ui-primitives                │
│   crates/nuo-view              crates/nuo-host                        │
│   crates/nuo-engine           crates/nuo-persistence                  │
│   crates/nuo-guardrails        crates/nuo-mcp                          │
│   crates/nuo-inference-wire     crates/nuo-model-routing               │
│   crates/nuo-provider           crates/nuo-tool                        │
│   crates/nuo-tool-derive                                               │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
         ┌──────────┴──────────┐          ┌──────────┴──────────┐
         ▼                     ▼          ▼                     ▼
┌────────────────────────────────┐      ┌────────────────────────────────┐
│   Model Extensions (providers/)│      │   Tool Extensions (tools/)     │
│   nuo-provider-anthropic       │      │   nuo-tool-fs                  │
│   nuo-provider-openai          │      │   nuo-tool-exec                │
│   ... (16 provider crates)     │      │   nuo-tool-web, nuo-tool-ast   │
└────────────────────────────────┘      └────────────────────────────────┘
```

**Physical layout (`[INV-ARCH-TIER-01]`, ADR-0109)**: All crates uniformly reside within one of three flat tiers:
- `crates/*`: Layers 1–4, with `crates/nuo` as the sole public application executable.
- `providers/nuo-provider-*`: Layer-3 model provider adapters. The only sanctioned vendor edges are the `nuo-provider-catalog` dispatcher and the `codebuddy-auth` shared base (ADR-0183).
- `tools/nuo-tool-*`: Layer-3 tool plugins (leaves).

No production crate resides directly at the workspace root. The sole root package is the dev-only `nuo-tests` harness that owns the two external test entries; gated mechanically by the `governance::architecture_firewall` integration suite.

### Crate Inclusion Threshold
New crates are admitted to `crates/*`, `providers/*`, or `tools/*` only if their responsibilities cannot be cleanly modeled as internal modules of existing domain crates and their position in the hierarchy is unambiguously ratified via an ADR.

---

## 2. Core Subsystems and Domain Responsibilities

### 2.1 Protocols & Substrates
- **`nuo-protocol`** (ADR-0066, ADR-0180): Canonical East-West IPC contracts — byte-level envelopes, control verbs, streaming deltas, session/thread entities, discovery/version negotiation — plus the **provider-neutral** domain vocabulary (`Provider`, `ModelRequest`, `ToolSpec`, model/pricing, context pressure, policy schemas, events). It owns no tool-facing types: `Tool`, `ToolOutput`, `Message`, `Role`, and `TodoList` live in `nuo-tool`; provider transport and concrete specs live in `nuo-provider`. Consolidated from the former `nuo-contracts` crate (ADR-0001); discovery DTOs repatriated from the retired client crate (ADR-0089). North-South model streaming belongs to `nuo-inference-wire` (ADR-0066).
- **`nuo-acp`** (*Agent Coordination Protocol*): Defines canonical inter-agent communication, addressing (`agent://`, `acp://`), tamper-evident HMAC signature envelopes, mailboxes, and multi-party channels. Optional feature-gated capability on `nuo-server`.
- **`nuo-tool` & **`nuo-tool-derive`**: Zero-runtime tool contracts defining schema, hazard risk profiles (`RiskProfile`), execution scopes (`ToolScope`), cancellation tokens, and the zero-dependency class-based token estimator ([ADR-0056](../adr/0056-zero-dependency-class-based-token-estimator.md)). `nuo-tool-derive` provides compile-time derive macros for JSON schemas.
- **`nuo-provider`**: Canonical model provider specification and runtime substrate (ADR-0015, ADR-0108). Owns core inference contracts (`Provider`), catalog discovery, outbound HTTP/SSE transport (`netune`, including the failure-diagnostic evidence carried by `TransportDiagnostic`), and dynamic credential lifecycle negotiation (OAuth2/PKCE RFC 7636/8628, device flow, loopback redirect, token lease refresh, [ADR-0098](../adr/0098-orthogonal-three-dimensional-authentication-topology-category-error-abolition-and-modernized-credential-governance.md)). Consolidated from the former satellite crates `nuo-egress` and `nuo-oauth` (ADR-0108).
- **`nuo-inference-wire`** (formerly the model codec, ADR-0084; aperture completed by ADR-0143): the model *wire substrate* — the `WireProtocol` identity enum every provider spec and route derivation keys on, the shared JSON-boundary and loop-detector scanners, token classification for pricing, and the one shared vendor parser (`Google Interactions`, consumed by `nuo-provider-google-antigravity`). It is **not** a codec suite: the five per-vendor `build_request` implementations and their dispatcher were deleted because no shipped binary ever called them — every provider builds and parses its own wire body. Dependency set is `serde`/`serde_json`/`thiserror`/`uuid`, so it structurally cannot perform I/O or host a transport.
- **`nuo-mcp`**: Native Model Context Protocol client and server transport over stdio and JSON-RPC 2.0.
- **`nuotc`**: Retained-mode 2D terminal canvas, differential rendering pipeline, and Flexbox layout solver. Free of AI domain vocabulary.

### 2.2 Applications & Interface Engines
- **`nuo`**: The unified CLI application executable (ADR-0005). Dispatches the interactive TUI (default), the headless pipeline (`--headless` / `-p`), and the session server (`nuo server start`, ADR-0061). Owns the internal client transport (`nuo::transport`) and the interactive terminal shell (`nuo::shell`, behind the default-on `interactive` feature).
- **`nuo-ui-primitives`**: The domain-agnostic, pure zero-IO terminal UI substrate built on `nuotc` (ADR-0003, ADR-0072, ADR-0105, ADR-0107). Owns the design-system tokens (`theme`, `design`, `elevation`), driver-free key chords, geometric alignment and text wrapping, pure algorithms (fuzzy match, syntax highlighting, time formatting), and the generic component set (`modal`, `dropdown`, `popover`, `toast`, `footer`, `keycap`, `detail`, …). It has no knowledge of AI agents, sessions, or tool calls (`[INV-ARCH-UI-PRIMITIVES-PURITY]`), so it is reusable by any terminal application.
- **`nuo-view`**: The pure, zero-IO agent-domain presentation layer (ADR-0107), depending strictly downward on `nuo-ui-primitives`. Owns the transcript document and selection model, conversational layout strategies, message-body rendering, the per-tool presenter registry, trust-gate and phase displays, and the agent-specific views (`notice`, `selectable_body`) — all pure `f(Props, State, Theme) -> Frame` projections with no terminal driver, network, subprocess or clipboard dependency (`[INV-ARCH-VIEW-ZERO-IO]`). Consumed by `nuo::shell`; it never consumes the shell.

### 2.3 Subsystems & Host Infrastructure
- **`nuo-server`**: The **application core** (ADR-0184): session runtime, provider/tool composition root, UDS/WebSocket control plane, MCP/skills/credential hosts, and the Nuo product policy (project scaffolding, built-in slash vocabulary, export). Modules are grouped into `composition/`, `runtime/`, `handlers/`, `transport/`, and `product/`; `api.rs` is the small facade the `nuo` binary consumes.
- **`nuo-engine`**: The single cognitive runtime ([ADR-0100](../adr/0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md), ADR-0181): the streaming ReAct loop, model-request assembly, causal context compaction, and multi-agent interop, wrapped by agent identity/role projection, permission brokering, human confirmation checkpoints, execution sandboxing, trajectory supervision, and tool scheduling. Governance is a separate crate (`nuo-guardrails`), not a module of this crate. The engine links only the `nuo-tool` contract; the composition root links concrete capability leaves (ADR-0183).
- **`nuo-guardrails`**: The policy layer: permission brokering, bash policy, execution containment, stream/trajectory loop guards, and interactive gating. It depends on `nuo-protocol` and `nuo-host`, never on `nuo-persistence` (ADR-0188).
- **`nuo-model-routing`**: The single definition of the model routing pipeline ([ADR-0081](../adr/0081-unidirectional-catalog-pipeline-structural-normalization-and-relay-noise-hygiene.md), [ADR-0082](../adr/0082-single-catalog-pipeline-definition-in-a-dedicated-layer2-crate.md), [ADR-0102](../adr/0102-model-routing-aperture-rename-nuo-catalog-to-nuo-model-routing-and-cleanse-route-derivation-semantics.md)): derives a `ProviderEntry`/`Channel` per route from the connection store, provider specs, and the discovery cache; resolves the display label once (Source → Mapping → Overwrite); syncs remote `/models` discovery through the `nuo_provider` fetch port and maintains the fitted-model overlay; and materializes the provider-picker snapshot. Links no `providers/*` crate and no presentation crate. Both `nuo-engine` and `nuo-server` import it directly (`[INV-ROUTING-01]`).
- **`nuo-persistence`**: SQLite transactional store, database migrations, configuration parsing, role memory, and full-text search indexing.
- **`providers/nuo-provider-*`** *(the authoritative provider namespace — [ADR-0027](../adr/0027-provider-definition-single-source-and-adapters-retirement.md), Accepted)*: Multi-vendor model catalog resolution, OAuth2/PKCE authentication flows, credential management, and concrete provider adapters implementing `nuo-provider` (ADR-0015). Each provider id has exactly one `ModelProviderSpec`, owned by its `providers/nuo-provider-*` crate, and the shipped build links every such crate (`[INV-PROV-06..09]`). The retired `nuo-provider-adapters` monolith's residual engines now live in `providers/nuo-provider-{catalog,siliconflow}` and the **`nuo-provider`** engine (ADR-0108); each vendor crate additionally owns its `OAuthConfig`, device grant, and enricher and registers it through the typed `nuo_provider::auth::OAuthProvider` port. The **provider composition root** (`init()`, `MODEL_PROVIDER_SPECS`, `build_provider_for_channel`, typed `QuotaPort` dispatch, and OAuth-provider registration) lives in `nuo-server::provider_registry`, and both entry points (`nuo` and the server bootstrap) call it. Operates the **two local-direct invocation lanes** plus a **subscription server-proxy surface** (ADR-0014):
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
- **`[INV-ARCH-TIER-01]` Pure Tiered Workspace Topology (ADR-0109)**: All crates uniformly
  reside within one of three flat tiers: `crates/*` (Layers 1–4, with `crates/nuo` as the sole
  public application executable), `providers/nuo-provider-*` (Layer-3 providers), and
  `tools/nuo-tool-*` (Layer-3 tools). Zero crates reside directly at the workspace root, and
  workspace admission is strictly the wildcard set `["crates/*", "tools/*", "providers/*"]`.
  Gated mechanically by `nuo::dev::firewall` rule R5 via the nextest integration filter `test(governance::architecture_firewall::test_inv_arch_firewall_and_topology)`.
- **`[INV-HOST-01]` Sandbox & Containment**: All local command execution and file mutations must pass through `nuo-host` sandbox and containment boundaries.
- **`[INV-TOOL-01]` Decentralized Capability Tools**: Tools are natively owned and implemented by their capability crates (`acp`, `nuo-host`, `nuo-persistence`) using the zero-runtime `nuo-tool` standard, rather than concentrated in a monolithic harness.
- **`[INV-VER-01]` Federated Cluster SemVer**: The workspace lockstep suite (`crates/*`, `providers/*`, `tools/*`) evolves under unified versioning, while external substrates (`nuotc`) maintain autonomous release cycles.
- **`[INV-MOD-01]` Zero Domain Leakage**: Terminal graphics substrates (`nuotc`) and the model wire substrate (`nuo-inference-wire`, formerly the model codec, per ADR-0084) must remain completely free of application-level agent concepts. Mechanism: the `Provider` cognitive contract lives in `nuo-provider::capability`, and the wire crate's dependency set (`serde`/`serde_json`/`thiserror`/`uuid`) structurally forbids I/O, a transport, or an async trait — `tokio` and `async-trait` were removed with the code that needed them (ADR-0143). `[INV-AP-01]`/`[INV-AP-02]` refine this for the split crate; `[INV-AP-03]` forbids re-growing an unwired second implementation.
- **`[INV-MOD-02]` Ghost-Free CJK Cells**: Wide-character trailing cells are populated by the writer with matching background attributes; diff never emits unowned padding spaces.
- **`[INV-MOD-03]` Authoritative Retained Grid**: The retained `Grid` is the single source of truth for desired visual state; dirty line tracking eliminates redundant full-frame rasterization.
- **`[INV-AUTH-04]` Dialect-Carried Transport (ADR-0014, ADR-0132)**: Wire protocol and transport headers derive from `ProviderDialect` and `ModelProviderSpec`, never from the auth lane or the connection name; request builders never branch on auth mode to pick a wire shape.
- **`[INV-AUTH-02]` Secret Storage Firewall (ADR-0014, ADR-0032)**: OAuth token sets live only in `auth.toml` (`$XDG_STATE_HOME/nuo/auth.toml`, 0600), API keys only in `credentials.toml` (`$XDG_STATE_HOME/nuo/credentials.toml`, 0600); raw secrets never enter `$XDG_CONFIG_HOME/nuo`, `connections.toml`, or any versionable file.
- **`[INV-LANE-03]` Server-Enforced Entitlement (ADR-0014)**: On the subscription server-proxy lane, plan gate, model availability, credits and provider routing are resolved only by the control plane (`api.commandcode.ai/alpha/generate`); client-side `Availability` filter is a projection of the resolved answer, never authority over it — live-probed with a GOAT key where `claude-sonnet-4-6`/`gpt-5.4`/`muse-spark-1.1` return `403 MODEL_NOT_IN_PLAN` but `claude-sonnet-5-5` routes through a gateway, upstream-provider mismatch (`openai/gpt-5.6-sol → openai`, `moonshotai/Kimi-K2.5 → bedrock`, `deepseek-v4-flash → deepseek-v4.1-flash@novita`).
- **`[INV-LANE-04]` Routing Provenance Record (ADR-0014)**: The client persists `providerMetadata.<resolved>.routing.planningReasoning` verbatim from the `finish-step` envelope into session telemetry; a local-direct lane with no proxy provenance takes `None`, never a fabricated sentence.
- **`[INV-PROV-06]` Single Definition (ADR-0027)**: Each model provider id has exactly one authoritative `ModelProviderSpec`, in its `providers/nuo-provider-*` crate. A second definition of the same id is a defect, not a staging state.
- **`[INV-PROV-07]` No Orphan Contract (ADR-0027)**: Every public item in `nuo-provider` has at least one non-test consumer in the shipped build; a contract with zero production consumers must be wired or deleted in the same change that introduces it.
- **`[INV-PROV-08]` No String-Sniffing Dispatch (ADR-0027)**: Capability dispatch keys off typed declarations; no code path selects behaviour by matching a provider id or base-URL substring.
- **`[INV-PROV-09]` Providers Are Linked (ADR-0027)**: The shipped build links every crate that claims to be a provider; an unlinked `providers/nuo-provider-*` crate is dead code.
- **`[INV-PROV-10]` Behaviour Behind Ports (ADR-0027)**: Static facts are `&'static` tables; runtime facts are overlays; behaviour is a trait port. No fourth mechanism.
- **`[INV-CATALOG-04]` Single Unidirectional Catalog Pipeline (ADR-0081)**: Catalog rows flow Source → Mapping → Overwrite, once and in that order; no stage re-derives what a previous stage settled, and a Source is never labelled by its own downstream.
- **`[INV-CATALOG-05]` Declared Display Policy (ADR-0081)**: A surface's labels come from its declared `DisplayPolicy` (`Native` trusts the published name; `Structural` lifts bare ids), never from an id-shaped guess or a per-model lookup table. The declaration is enforced registry-wide by `every_registered_surface_renders_its_seeds_without_routing_syntax` (`crates/nuo-server/src/provider_registry/tests.rs`): no registered surface may publish an id whose rendered label still contains `/` or `:` — an ids-only surface must declare `Structural`.
- **`[INV-CATALOG-05a]` Routing Syntax Is Never Presentation (ADR-0081)**: `vendor/` namespaces and OpenRouter-style `:variant` service tiers are routing facts, not name parts. Both are absorbed by the structural lift applied to *wire ids*; authored labels are only stripped of execution-mode annotations. A relay namespace survives into the label solely when it states something the tail does not (`meta/muse-spark-1.1` → `Meta Muse Spark 1.1`), decided by the shared `BRAND_LEXICON` so a model added upstream renders without a Nuo release.
- **`[INV-CATALOG-06]` Structural Noise Hygiene (ADR-0081)**: Relay noise is pruned by structural markers only (`-free`, `-preview`, `-contributor`, `alpha-`, `-alpha`, `test`), never by an ad-hoc product blacklist; a connection's `models.include` bypasses the valve unconditionally.
- **`[INV-ROUTING-01]` Single Definition of the Model Routing Pipeline (ADR-0082, ADR-0102)**: The pipeline exists in exactly one crate (`nuo-model-routing`); consumers import it directly and MUST NOT host a duplicate module or re-export it as a local path shim. Supersedes `[INV-CATALOG-07]`.
- **`[INV-CONTEXT-01]` Capability Is Not Budget (ADR-0153)**: Every context watermark derives from an **effective** window `min(declared_window, context.max_input_tokens)`; no pressure threshold may be derived from an uncapped model window. A model's advertised maximum is an admission bound, not a performance target.
- **`[INV-CONTEXT-02]` Retention Is a Request Projection (ADR-0153)**: Historical tool output is bounded per request (verbatim / frozen / cleared bands), never by rewriting durable transcript facts; the projection is deterministic and re-derived on resume.
- **`[INV-CONTEXT-03]` Remote Continuations Are Exempt (ADR-0153)**: A `RemoteStored` route's `previous_response_id` chain ignores the tool-output retention projection; freezing old content there would invalidate the cursor each turn.
- **`[INV-CONTEXT-04]` Lossy Projections Preserve the Escape Hatch (ADR-0153)**: Any projected tool result that drops bytes retains its `Spool File` / `Artifact Path` / `Virtual Path` pointer.
- **`[INV-CONTEXT-05]` Attachments Are Normalized Once (ADR-0153)**: Clipboard, pasted-file, tool-produced, and rehydrated-artifact images are bounded at ingestion (1568 px long edge, 4 MiB encoded) before they can enter model context.

### 4.1 Convergence Invariants (ADR-0180 – ADR-0188)

- **`[INV-VOCAB-01..03]`** One owner per public type; no cross-crate re-export
  modules; no wildcard compatibility facades.
- **`[INV-ENGINE-01]`** `nuo-engine` is the only crate name; no `nuo_harness`
  alias or retired identifier.
- **`[INV-ARCH-TOOL-LINK-01]`** The composition root links capability leaves;
  the engine manifest declares no `nuo-tool-*` dependency.
- **`[INV-SERVER-TOPOLOGY-01]`** `nuo-server/src` has no root modules except
  `lib.rs` and `api.rs`.
- **`[INV-TUI-SINGLE-RENDER-01]`** One production renderer; the offline
  storybook and declarative component suite are retired.
- **`[INV-SIZE-01]`** Functions ≤ 400 lines, structs ≤ 80 fields, live source
  files ≤ 2,000 lines (immutable migrations exempt).
- **`[INV-DOC-01..03]`** ADR identity/frontmatter and relative links are
  mechanically checked; living docs use current crate names.
- **`[INV-GUARD-01]`** `nuo-guardrails` does not depend on `nuo-persistence`.

---

## 5. Architectural Lineage & Decision Records

All binding architectural decisions, structural boundaries, and discarded alternatives are codified canonically in the [`docs/adr/`](../adr/) flat registry. Foundational architectural pillars and their governing records include:

- **Concentric Architecture & Entry Point**: Single public binary dispatching across client, daemon, and headless modes ([ADR-0005](../adr/0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0061](../adr/0061-pure-thread-topology-headless-unified-entry-and-unburdened-cli-architecture.md)).
- **Tiered Workspace Topology**: Flat 3-tier layout (`crates/*`, `providers/*`, `tools/*`) with single application host and zero root library crates ([ADR-0055](../adr/0055-hierarchical-workspace-topology-and-component-namespace.md), [ADR-0109](../adr/0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md)).
- **Server Purity & Protocol Decoupling**: Headless server hosting threads as single source of truth, strictly decoupled from presentation crates and communicating exclusively via typed IPC ([ADR-0011](../adr/0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0033](../adr/0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0066](../adr/0066-canonical-protocol-nomenclature-and-codec-purity.md)).
- **Presentation Substrates & Peer Focus Planes**: Retained canvas diffing engine (`nuotc`), generic UI primitives (`nuo-ui-primitives`), agent views (`nuo-view`), and symmetric focus crossings ([ADR-0003](../adr/0003-autonomous-terminal-canvas-substrate-nuotc.md), [ADR-0053](../adr/0053-capability-first-orthogonal-terminal-composition-and-autonomous-archetype-projection.md), [ADR-0145](../adr/0145-composer-and-transcript-are-peer-focus-planes.md)).
- **Cognitive Runtime & Inversion**: Provider capability contracts with decentralized tools and single-definition specifications ([ADR-0014](../adr/0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0015](../adr/0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0027](../adr/0027-provider-definition-single-source-and-adapters-retirement.md), [ADR-0100](../adr/0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md)).

For standards governing proposal qualification, in-place lifecycle progression, and code citation discipline, consult the [ADR Standards](standards/adr.md).
