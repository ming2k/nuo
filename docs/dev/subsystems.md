# Subsystem Architecture: Nuo System Blueprint

- Status: Living Blueprint
- Last Updated: 2026-12-19
- Scope: workspace/topology, architecture/subsystems, substrate/tools, microkernel/topology, terminal/graticule
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
│               Component Libraries (crates/*)                           │
│   crates/protocol          crates/acp                          │
│   crates/server            crates/ui-primitives                │
│   crates/view              crates/host                        │
│   crates/engine           crates/persistence                  │
│   crates/guardrails                                                  │
│   crates/inference-wire     crates/model-routing               │
│   crates/provider           crates/tool                        │
│   crates/tool-derive                                               │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
         ┌──────────┴──────────┐          ┌──────────┴──────────┐
         ▼                     ▼          ▼                     ▼
┌────────────────────────────────┐      ┌────────────────────────────────┐
│   Model Extensions (crates/)   │      │   Tool Extensions (crates/)    │
│   crates/provider-anthropic│      │   crates/tool-fs           │
│   crates/provider-openai   │      │   crates/tool-exec         │
│   ... (20 provider crates)     │      │   tool-web, tool-ast   │
└────────────────────────────────┘      └────────────────────────────────┘
```

**Physical layout (`[INV-ARCH-TIER-01]`)**: All workspace crates uniformly reside under `crates/*`:
- `crates/nuo`: The sole public application executable.
- `crates/*`: Core platform crates (Layers 1–4).
- `crates/provider-*`: Layer-3 model provider adapters. The only sanctioned vendor edges are the `provider-catalog` dispatcher and the `codebuddy-auth` shared base (ADR-0183).
- `crates/tool-*`: Layer-3 tool plugins (leaves).

No production crate resides directly at the workspace root. The sole root package is the dev-only `nuo-tests` harness that owns the two external test entries; gated mechanically by the `governance::architecture_firewall` integration suite.

### Crate Inclusion Threshold
New crates are admitted to `crates/*` only if their responsibilities cannot be cleanly modeled as internal modules of existing domain crates and their position in the hierarchy is unambiguously ratified via an ADR.

---

## 2. Core Subsystems and Domain Responsibilities

### 2.1 Protocols & Substrates
- **`protocol`** (ADR-0066, ADR-0180): Canonical East-West IPC contracts — byte-level envelopes, control verbs, streaming deltas, session/thread entities, discovery/version negotiation — plus the **provider-neutral** domain vocabulary (`Provider`, `ModelRequest`, `ToolSpec`, model/pricing, context pressure, policy schemas, events). It owns no tool-facing types: `Tool`, `ToolOutput`, `Message`, `Role`, and `TodoList` live in `tool`; provider transport and concrete specs live in `provider`. Consolidated from the former `nuo-contracts` crate (ADR-0001); discovery DTOs repatriated from the retired client crate (ADR-0089). North-South model streaming belongs to `inference-wire` (ADR-0066).
- **`acp`** (*Agent Coordination Protocol*): Defines canonical inter-agent communication, addressing (`agent://`, `acp://`), tamper-evident HMAC signature envelopes, mailboxes, and multi-party channels. Optional feature-gated capability on `server`.
- **`tool` & `tool-derive`**: Zero-runtime tool contracts defining schema, hazard risk profiles (`RiskProfile`), execution scopes (`ToolScope`), cancellation tokens, and the zero-dependency class-based token estimator ([ADR-0056](../adr/0056-zero-dependency-class-based-token-estimator.md)). `tool-derive` provides compile-time derive macros for JSON schemas.
- **`provider`**: Canonical model provider specification and runtime substrate (ADR-0015, ADR-0108). Owns core inference contracts (`Provider`), catalog discovery, outbound HTTP/SSE transport (`netune`, including the failure-diagnostic evidence carried by `TransportDiagnostic`), and dynamic credential lifecycle negotiation (OAuth2/PKCE RFC 7636/8628, device flow, loopback redirect, token lease refresh, [ADR-0098](../adr/0098-orthogonal-three-dimensional-authentication-topology-category-error-abolition-and-modernized-credential-governance.md)). Consolidated from the former satellite crates `nuo-egress` and `nuo-oauth` (ADR-0108).
- **`inference-wire`** (formerly the model codec, ADR-0084; aperture completed by ADR-0143): the model *wire substrate* — the `WireProtocol` identity enum every provider spec and route derivation keys on, the shared JSON-boundary and loop-detector scanners, token classification for pricing, and the one shared vendor parser (`Google Interactions`, consumed by `provider-google-antigravity`). It is **not** a codec suite: the five per-vendor `build_request` implementations and their dispatcher were deleted because no shipped binary ever called them — every provider builds and parses its own wire body. Dependency set is `serde`/`serde_json`/`thiserror`/`uuid`, so it structurally cannot perform I/O or host a transport.
- **`protocol::mcp` + `server::runtime::mcp`**: The client-facing MCP configuration/status DTOs live in `protocol`; the one MCP client — stdio and Streamable HTTP, JSON-RPC 2.0, cancellation, progress, reconnect — lives in `server::runtime::mcp`. Nuo is an MCP client only; it does not export itself as a server (ADR-0230, ADR-0234).
- **`graticule`** (`graticule-canvas` + `graticule-tty`): Retained-mode 2D terminal cell canvas, differential rendering pipeline, Flexbox layout solver, retained `ui` scene graph, and physical TTY host transport (timeout-disambiguated VT input parser, mouse modes, opt-in terminal session). Free of AI domain vocabulary ([ADR-0223](../adr/0223-graticule-substrate-vt-input-parser-and-all-motion-hover.md)).

### 2.2 Applications & Interface Engines
- **`nuo`**: The unified CLI application executable (ADR-0005). Dispatches the interactive TUI (default), the headless pipeline (`--headless` / `-p`), and the session server (`nuo server start`, ADR-0061). Owns the internal client transport (`nuo::transport`) and the interactive terminal shell (`nuo::shell`, behind the default-on `interactive` feature).
- **`ui-primitives`**: The domain-agnostic, pure zero-IO terminal UI substrate built on `graticule-canvas` (ADR-0072, ADR-0105, ADR-0107, ADR-0223). Owns the design-system tokens (`theme`, `design`, `elevation`), driver-free key chords, geometric alignment and text wrapping, pure algorithms (fuzzy match, syntax highlighting, time formatting), and the generic component set (`modal`, `dropdown`, `popover`, `toast`, `footer`, `keycap`, `detail`, …). It has no knowledge of AI agents, sessions, or tool calls (`[INV-ARCH-UI-PRIMITIVES-PURITY]`), so it is reusable by any terminal application.
- **`view`**: The pure, zero-IO agent-domain presentation layer (ADR-0107), depending strictly downward on `ui-primitives`. Owns the transcript document and selection model, conversational layout strategies, message-body rendering, the per-tool presenter registry, trust-gate and phase displays, and the agent-specific views (`notice`, `selectable_body`) — all pure `f(Props, State, Theme) -> Frame` projections with no terminal driver, network, subprocess or clipboard dependency (`[INV-ARCH-VIEW-ZERO-IO]`). Consumed by `nuo::shell`; it never consumes the shell.

### 2.3 Subsystems & Host Infrastructure
- **`server`**: The **application core** (ADR-0184): session runtime, provider/tool composition root, UDS/WebSocket control plane, MCP/skills/credential hosts, and the Nuo product policy (project scaffolding, built-in slash vocabulary, export). Modules are grouped into `composition/`, `runtime/`, `handlers/`, `transport/`, and `product/`; `api.rs` is the small facade the `nuo` binary consumes.
- **`engine`**: The single cognitive runtime ([ADR-0100](../adr/0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md), ADR-0181): the streaming ReAct loop, model-request assembly, causal context compaction, and multi-agent interop, wrapped by agent identity/role projection, permission brokering, human confirmation checkpoints, execution sandboxing, trajectory supervision, and tool scheduling. The prompt is a **compiled context policy** (`cognition::context`): every source declares its topic, inputs, rank, stability, and budget, and the whole composition is proven once at agent construction (ADR-0215). Governance is a separate crate (`guardrails`), not a module of this crate. The engine links only the `tool` contract; the composition root links concrete capability leaves (ADR-0183).
- **`guardrails`**: The policy layer: permission brokering, bash policy, execution containment, stream/trajectory loop guards, and interactive gating. It depends on `protocol` and `host`, never on `persistence` (ADR-0188).
- **`model-routing`**: The single definition of the model routing pipeline ([ADR-0081](../adr/0081-unidirectional-catalog-pipeline-structural-normalization-and-relay-noise-hygiene.md), [ADR-0082](../adr/0082-single-catalog-pipeline-definition-in-a-dedicated-layer2-crate.md), [ADR-0102](../adr/0102-model-routing-aperture-rename-nuo-catalog-to-nuo-model-routing-and-cleanse-route-derivation-semantics.md)): derives a `ProviderEntry`/`Channel` per route from the connection store, provider specs, and the discovery cache; resolves the display label once (Source → Mapping → Overwrite); syncs remote `/models` discovery through the `provider` fetch port and maintains the fitted-model overlay; and materializes the provider-picker snapshot. Links no `providers/*` crate and no presentation crate. Both `engine` and `server` import it directly (`[INV-ROUTING-01]`).
- **`persistence`**: per-session append-only journals (the durable source of truth, ADR-0236/0237/0239) carrying the transcript, branch graph, path identity, and terminal deletion; a derived SQLite projection; a bounded best-effort request-projection ring (ADR-0240); database migrations, configuration parsing, role memory, and full-text search indexing. Application code commits to the journal; the SQLite view is rebuilt from it and may lag. Storage work is split by the resource it needs, not by who owns the database (ADR-0248): the writer executes SQL only, backups and `PRAGMA optimize` use their own connections, projections run on a bounded fair lane pool, and reader contention is a bounded wait rather than a silent empty result. The catalog/content split of that view is staged in ADR-0242.
- **`crates/provider-*`** *(the authoritative provider namespace — [ADR-0027](../adr/0027-provider-definition-single-source-and-adapters-retirement.md), Accepted)*: Multi-vendor model catalog resolution, OAuth2/PKCE authentication flows, credential management, and concrete provider adapters implementing `provider` (ADR-0015). Each provider id has exactly one `ModelProviderSpec`, owned by its `crates/provider-*` crate, and the shipped build links every such crate (`[INV-PROV-06..09]`). The retired `provider-adapters` monolith's residual engines now live in `crates/provider-{catalog,siliconflow}` and the **`provider`** engine (ADR-0108); each vendor crate additionally owns its `OAuthConfig`, device grant, and enricher and registers it through the typed `provider::auth::OAuthProvider` port. The **provider composition root** (`init()`, `MODEL_PROVIDER_SPECS`, `build_provider_for_channel`, typed `QuotaPort` dispatch, and OAuth-provider registration) lives in `server::provider_registry`, and both entry points (`nuo` and the server bootstrap) call it. Operates the **two local-direct invocation lanes** plus a **subscription server-proxy surface** (ADR-0014):
  - *Local-direct · static-key lane* — `ConnectionAuth::ApiKey` resolves a static bearer from `api_key_env` (env-first) or `credentials.toml` (mode 0600), speaking OpenAI chat-completions / Responses / Anthropic Messages / Google Gemini against first-party (OpenAI, Anthropic, DeepSeek, xAI, Kimi, GLM-CN, Qianwen) or relay (OpenRouter, OpenCode Zen/Go) endpoints; keyless relays send no auth header at all.
  - *Local-direct · OAuth subscription lane* — `ConnectionAuth::Subscription` resolves live bearer tokens from `auth.toml` through per-provider `OAuthConfig` presets (ChatGPT/Codex PKCE loopback at `127.0.0.1:1455`, Google Antigravity browser PKCE w/ `cloudaicompanionProject` onboarding, GitHub Copilot device grant + internal token mint, xAI SuperGrok device grant, Qoder custom device flow, OpenCode Console device grant). Tokens refresh proactively on a 120 s skew and reactively on HTTP 401 single-flight retry.
  - *Subscription server-proxy lane (planned surface)* — a `DialectSurface::SubscriptionProxy` sends vendor-generic envelopes to `POST {api}/alpha/generate` (SSE); the server holds vendor credentials, rewrites the model family, enforces `planId` × `windowLimits{fiveHour, weekly}` entitlement, and returns per-turn routing provenance in `providerMetadata.<resolved>.routing.planningReasoning` + `finish-step.response.headers`. The client never sees a vendor key; `--local-only` / `NUO_LOCAL_ONLY=1` refuses this lane outright.
- **`host`**: Host execution environment: process supervision (process groups / Windows Job Objects), workspace sandboxing, cross-platform paths, secure file operations, and native host tools.

---

## 3. Terminal Substrate (`graticule`)

`graticule` organizes terminal rendering and TTY host transport into two independent workspace crates under one umbrella (`graticule-canvas` and `graticule-tty`):

```text
┌────────────────────────────────────────────────────────┐
│                     graticule (Umbrella)                  │
├────────────────────────────┬───────────────────────────┤
│       graticule-canvas        │        graticule-tty         │
│ ui, widgets, layout,       │ event, parser (VtParser), │
│ render, terminal, buffer   │ mouse, reader, session    │
└────────────────────────────┴───────────────────────────┘
```

- **`graticule-canvas`**:
  - **`buffer`**: Compact cell memory (`Cell`, `Color`, `Modifier`, `Style`), 2D `CellGrid` with write-marks-dirty (`Damage`) line tracking, Unicode grapheme clustering, and standard glyph tables.
  - **`layout`**: Spatial geometry (`Rect`), Flexbox layout solver (`Flex`, `FlexItem`, `SolvedFlex`), and collision-free anchored popups (`AnchorPlacement`, `compute_anchored_rect`).
  - **`render`**: Pure run-length packed differential rendering (`diff`), `Paint`/`Widget`, escape code drivers, and standards-based terminal profiles (`TerminalProfile`).
  - **`terminal`**: Output writer (`Backend`, `Bce`) and double-buffered commit loop (`Frame`, `Terminal`, `TestTerminal`).
  - **`widgets`**: Declarative UI primitives (`Block`, `Paragraph`, `Clear`) and structural composition containers (`Column`, `Row`, `Stack`, `Container`, `Spacer`, `Divider`).
  - **`ui`**: Retained scene graph (`Scene`) and component event dispatch runtime (`UiRuntime`).
- **`graticule-tty`**:
  - **`event` & `parser`**: Graticule-owned `Event`/`KeyEvent`/`MouseEvent` types and the timeout-disambiguated incremental `VtParser` (20 ms `ESC` deadline, Kitty CSI-u, SGR-1006 mouse, bracketed paste).
  - **`mouse`, `reader`, `session`**: `MouseMode`, single-reader `InputReader` (`ProbeHandle`), and opt-in `TerminalSession` RAII guard.

---

## 4. Architectural Invariants Constitution

- **`[INV-WS-01]` Self-Contained Workspace**: All workspace crates and substrates must resolve within the local repository without relying on external sibling path dependencies (`../`).
- **`[INV-ARCH-TIER-01]` Flat `crates/*` Workspace Topology**: All workspace crates uniformly
  reside under `crates/*` (including `crates/nuo` as the sole public application executable,
  `crates/provider-*` adapters, and `crates/tool-*` tools). Zero crates reside directly at
  the workspace root, and workspace admission is strictly the wildcard set `["crates/*"]`.
  Gated mechanically by `nuo::dev::firewall` rule R5 via the nextest integration filter `test(governance::architecture_firewall::test_inv_arch_firewall_and_topology)`.
- **`[INV-HOST-01]` Sandbox & Containment**: All local command execution and file mutations must pass through `host` sandbox and containment boundaries.
- **`[INV-TOOL-01]` Decentralized Capability Tools**: Tools are natively owned and implemented by their capability crates (`acp`, `host`, `persistence`) using the zero-runtime `tool` standard, rather than concentrated in a monolithic harness.
- **`[INV-VER-01]` Federated Cluster SemVer**: The workspace lockstep suite (`crates/*`) evolves under unified versioning, while external substrates (`graticule`) maintain autonomous release cycles.
- **`[INV-MOD-01]` Zero Domain Leakage**: Terminal substrates (`graticule-canvas`, `graticule-tty`) and the model wire substrate (`inference-wire`, formerly the model codec, per ADR-0084) must remain completely free of application-level agent concepts. Mechanism: the `Provider` cognitive contract lives in `provider::capability`, and the wire crate's dependency set (`serde`/`serde_json`/`thiserror`/`uuid`) structurally forbids I/O, a transport, or an async trait — `tokio` and `async-trait` were removed with the code that needed them (ADR-0143). `[INV-AP-01]`/`[INV-AP-02]` refine this for the split crate; `[INV-AP-03]` forbids re-growing an unwired second implementation.
- **`[INV-MOD-02]` Ghost-Free CJK Cells**: Wide-character trailing cells are populated by the writer with matching background attributes; diff never emits unowned padding spaces.
- **`[INV-MOD-03]` Authoritative Retained Grid**: The retained `Grid` is the single source of truth for desired visual state; dirty line tracking eliminates redundant full-frame rasterization.
- **`[INV-AUTH-04]` Dialect-Carried Transport (ADR-0014, ADR-0132)**: Wire protocol and transport headers derive from `ProviderDialect` and `ModelProviderSpec`, never from the auth lane or the connection name; request builders never branch on auth mode to pick a wire shape.
- **`[INV-AUTH-02]` Secret Storage Firewall (ADR-0014, ADR-0032)**: OAuth token sets live only in `auth.toml` (`$XDG_STATE_HOME/nuo/auth.toml`, 0600), API keys only in `credentials.toml` (`$XDG_STATE_HOME/nuo/credentials.toml`, 0600); raw secrets never enter `$XDG_CONFIG_HOME/nuo`, `connections.toml`, or any versionable file.
- **`[INV-LANE-03]` Server-Enforced Entitlement (ADR-0014)**: On the subscription server-proxy lane, plan gate, model availability, credits and provider routing are resolved only by the control plane (`api.commandcode.ai/alpha/generate`); client-side `Availability` filter is a projection of the resolved answer, never authority over it — live-probed with a GOAT key where `claude-sonnet-4-6`/`gpt-5.4`/`muse-spark-1.1` return `403 MODEL_NOT_IN_PLAN` but `claude-sonnet-5-5` routes through a gateway, upstream-provider mismatch (`openai/gpt-5.6-sol → openai`, `moonshotai/Kimi-K2.5 → bedrock`, `deepseek-v4-flash → deepseek-v4.1-flash@novita`).
- **`[INV-LANE-04]` Routing Provenance Record (ADR-0014)**: The client persists `providerMetadata.<resolved>.routing.planningReasoning` verbatim from the `finish-step` envelope into session telemetry; a local-direct lane with no proxy provenance takes `None`, never a fabricated sentence.
- **`[INV-PROV-06]` Single Definition (ADR-0027)**: Each model provider id has exactly one authoritative `ModelProviderSpec`, in its `crates/provider-*` crate. A second definition of the same id is a defect, not a staging state.
- **`[INV-PROV-07]` No Orphan Contract (ADR-0027)**: Every public item in `provider` has at least one non-test consumer in the shipped build; a contract with zero production consumers must be wired or deleted in the same change that introduces it.
- **`[INV-PROV-08]` No String-Sniffing Dispatch (ADR-0027)**: Capability dispatch keys off typed declarations; no code path selects behaviour by matching a provider id or base-URL substring.
- **`[INV-PROV-09]` Providers Are Linked (ADR-0027)**: The shipped build links every crate that claims to be a provider; an unlinked `crates/provider-*` crate is dead code.
- **`[INV-PROV-10]` Behaviour Behind Ports (ADR-0027)**: Static facts are `&'static` tables; runtime facts are overlays; behaviour is a trait port. No fourth mechanism.
- **`[INV-CATALOGPIPE-01]` Single Unidirectional Catalog Pipeline (ADR-0081)**: Catalog rows flow Source → Mapping → Overwrite, once and in that order; no stage re-derives what a previous stage settled, and a Source is never labelled by its own downstream.
- **`[INV-CATALOGPIPE-02]` Declared Display Policy (ADR-0081)**: A surface's labels come from its declared `DisplayPolicy` (`Native` trusts the published name; `Structural` lifts bare ids), never from an id-shaped guess or a per-model lookup table. The declaration is enforced registry-wide by `every_registered_surface_renders_its_seeds_without_routing_syntax` (`crates/server/src/composition/provider_registry/tests.rs`): no registered surface may publish an id whose rendered label still contains `/` or `:` — an ids-only surface must declare `Structural`.
- **`[INV-CATALOGPIPE-04]` Routing Syntax Is Never Presentation (ADR-0081)**: `vendor/` namespaces and OpenRouter-style `:variant` service tiers are routing facts, not name parts. Both are absorbed by the structural lift applied to *wire ids*; authored labels are only stripped of execution-mode annotations. A relay namespace survives into the label solely when it states something the tail does not (`meta/muse-spark-1.1` → `Meta Muse Spark 1.1`), decided by the shared `BRAND_LEXICON` so a model added upstream renders without a Nuo release.
- **`[INV-CATALOGPIPE-03]` Structural Noise Hygiene (ADR-0081)**: Relay noise is pruned by structural markers only (`-free`, `-preview`, `-contributor`, `alpha-`, `-alpha`, `test`), never by an ad-hoc product blacklist; a connection's `models.include` bypasses the valve unconditionally.
- **`[INV-ROUTING-01]` Single Definition of the Model Routing Pipeline (ADR-0082, ADR-0102)**: The pipeline exists in exactly one crate (`model-routing`); consumers import it directly and MUST NOT host a duplicate module or re-export it as a local path shim. Supersedes `[INV-CATALOG-07]`.
- **`[INV-CONTEXT-01]` Thresholds Follow the Model Window (ADR-0214, supersedes the ADR-0153 ceiling)**: Every context threshold derives from the active model's declared context window through the internal `CompactionPolicy`; there is no global input ceiling and no user-facing `[context]` configuration. The status bar and the compaction gate read the same window.
- **`[INV-CONTEXT-02]` Retention Is a Request Projection (ADR-0153)**: Historical tool output is bounded per request (verbatim / frozen / cleared bands), never by rewriting durable transcript facts; the projection is deterministic and re-derived on resume.
- **`[INV-CONTEXT-03]` Remote Continuations Are Exempt (ADR-0153)**: A `RemoteStored` route's `previous_response_id` chain ignores the tool-output retention projection; freezing old content there would invalidate the cursor each turn.
- **`[INV-CONTEXT-04]` Lossy Projections Preserve the Escape Hatch (ADR-0153)**: Any projected tool result that drops bytes retains its `Spool File` / `Artifact Path` / `Virtual Path` pointer.
- **`[INV-CONTEXT-05]` Attachments Are Normalized Once (ADR-0153)**: Clipboard, pasted-file, tool-produced, and rehydrated-artifact images are bounded at ingestion (1568 px long edge, 4 MiB encoded) before they can enter model context.
- **`[INV-CTXSRC-01]` Single Topic Owner (ADR-0215)**: At most one active context source may own a `Topic`. Two active sources on one topic is `ContextPolicyError::DuplicateTopic` unless one names the other in `supersedes`; there is no silent winner.
- **`[INV-CTXSRC-02]` Declared Budget (ADR-0215)**: Every context source declares a token budget and an overflow policy, and the whole instruction region is bounded by `ContextBudget::instructions_ceiling_tokens`. A directive may not use `Overflow::Drop`.
- **`[INV-CTXSRC-03]` Derived Stability (ADR-0215)**: A source's `Stability` is the maximum over the `InputKey`s it declares; a source cannot claim more stability than its inputs have, and `fixed` is ordered by ascending stability so a workspace edit reaches only the tail.
- **`[INV-CTXSRC-04]` Rank Is Placement (ADR-0215)**: `Rank::Untrusted` never enters `fixed`; `Rank::Directive` never becomes a conversation message. Rank derives from the topic, never from the source.
- **`[INV-CTXSRC-05]` Infallible Request Path (ADR-0215)**: Layout is proven once by `Policy::compile`; no request-path function may panic on a layout condition.
- **`[INV-CTXSRC-06]` No Dead Context Declaration (ADR-0215)**: Every `ContextInputs` accessor has a production reader and every `cognition::context` submodule is reachable from `mod.rs`. Supersedes the retired Phase-A substrate ban of ADR-0182 by reaching dead *fields* and dead *modules*, which `dead_code` cannot see.
- **`[INV-CTXSRC-07]` One Budget Vocabulary (ADR-0215)**: Token thresholds and watermark fractions are derived only in `protocol::{pressure,tuning}`. A module that multiplies a model window by a fraction is a second truth about the same model.
- **`[INV-CTXSRC-08]` Purity (ADR-0215)**: `ContextSource::render` and `is_active` read `ContextInputs` only — no clock, filesystem, network, or interior mutability. The render memo is keyed on the inputs hash, so an impure read serves a stale body silently.
- **`[INV-TOOLSURFACE-01]` No Tool-Name Branch (ADR-0216)**: Prompt policy gates guidance on a declared `BuiltinTool` capability, never on a tool name or alias. A rename cannot delete guidance.
- **`[INV-TOOLSURFACE-03]` Prefix Immutability Under Activation (ADR-0216)**: Activating or deactivating a deferred tool leaves `fixed` byte-identical; `ToolSurface::deferred_active` is diagnostic and excluded from the surface's identity.
- **`[INV-CTXPLAN-01]` Plan/Request Agreement (ADR-0217)**: The `fixed`/`transient` text in a `ContextPlan` is byte-identical to the `InstructionText` of the request assembled from it. An inspection surface can never show a prompt the model did not receive.
- **`[INV-CTXPLAN-03]` Persisted Identity Is Stable (ADR-0217)**: Durable projection records carry the SHA-256 prefix fingerprint, never an in-process hash: a `DefaultHasher` value is not stable across builds.

### 4.1 Convergence Invariants (ADR-0180 – ADR-0188)

- **`[INV-VOCAB-01..03]`** One owner per public type; no cross-crate re-export
  modules; no wildcard compatibility facades.
- **`[INV-ENGINE-01]`** `engine` is the only crate name; no `nuo_harness`
  alias or retired identifier.
- **`[INV-ARCH-TOOL-LINK-01]`** The composition root links capability leaves;
  the engine manifest declares no `tool-*` dependency.
- **`[INV-SERVER-TOPOLOGY-01]`** `server/src` has no root modules except
  `lib.rs` and `api.rs`.
- **`[INV-TUI-SINGLE-RENDER-01]`** One production renderer; the offline
  storybook and declarative component suite are retired.
- **`[INV-SIZE-01]`** Functions ≤ 400 lines, structs ≤ 80 fields, live source
  files ≤ 2,000 lines (immutable migrations exempt).
- **`[INV-DOC-01..03]`** ADR identity/frontmatter and relative links are
  mechanically checked; living docs use current crate names.
- **`[INV-GUARD-01]`** `guardrails` does not depend on `persistence`.

---

## 5. Architectural Lineage & Decision Records

All binding architectural decisions, structural boundaries, and discarded alternatives are codified canonically in the [`docs/adr/`](../adr/) flat registry. Foundational architectural pillars and their governing records include:

- **Concentric Architecture & Entry Point**: Single public binary dispatching across client, daemon, and headless modes ([ADR-0005](../adr/0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0061](../adr/0061-pure-thread-topology-headless-unified-entry-and-unburdened-cli-architecture.md)).
- **Flat Workspace Topology**: Single `crates/*` container with single application host (`crates/nuo`) and zero root library crates (consolidated from the earlier 3-tier layout of [ADR-0055](../adr/0055-hierarchical-workspace-topology-and-component-namespace.md) and [ADR-0109](../adr/0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md)).
- **Server Purity & Protocol Decoupling**: Headless server hosting threads as single source of truth, strictly decoupled from presentation crates and communicating exclusively via typed IPC ([ADR-0011](../adr/0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0033](../adr/0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0066](../adr/0066-canonical-protocol-nomenclature-and-codec-purity.md)).
- **Presentation Substrates & Peer Focus Planes**: Retained canvas and TTY substrate (`graticule-canvas`, `graticule-tty`), generic UI primitives (`ui-primitives`), agent views (`view`), and symmetric focus crossings ([ADR-0223](../adr/0223-graticule-substrate-vt-input-parser-and-all-motion-hover.md), [ADR-0053](../adr/0053-capability-first-orthogonal-terminal-composition-and-autonomous-archetype-projection.md), [ADR-0145](../adr/0145-composer-and-transcript-are-peer-focus-planes.md)).
- **Cognitive Runtime & Inversion**: Provider capability contracts with decentralized tools and single-definition specifications ([ADR-0014](../adr/0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0015](../adr/0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0027](../adr/0027-provider-definition-single-source-and-adapters-retirement.md), [ADR-0100](../adr/0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md)).

For standards governing proposal qualification, in-place lifecycle progression, and code citation discipline, consult the [ADR Standards](standards/adr.md).
