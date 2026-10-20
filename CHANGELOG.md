# Changelog

All notable changes to this project are documented in this file.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/), and
the project adheres to the federated SemVer model described in
[ADR-0004](docs/adr/0004-federated-cluster-semver-and-release-topology.md).

## [0.0.41] - 2026-10-19

### Added

- **Time-Varying Rate Cards and Pure Cost Rating ([ADR-0080](docs/adr/0080-time-varying-rate-cards-and-off-peak-rating.md))**:
  `ModelPricing` now supports scheduled rate cards (`RateSchedule`, `OffPeakPricing`)
  with pure `(TokenUsage, RatingInstant)` evaluation (`[INV-PRICING-04]`). DeepSeek
  V4.1 Flash and Pro rate cards reflect official 50% off-peak pricing across Beijing
  off-peak hours and Chinese statutory holidays.
- **On-Demand Modal Pricing Disclosure**: Both the flat model details modal
  (`/models` -> `i`) and connection details modal (`/connections` -> `Enter` or
  `@connection`) now feature structured rate cards and off-peak schedules
  co-located with account balance, rejecting bottom-bar clutter.
- **DeepSeek Canonical Model Identity ([ADR-0083](docs/adr/0083-deepseek-model-identity-reasoning-toggle-and-responses-dialect-purity.md))**:
  `nuo-provider-deepseek` now seeds exactly the ids `api.deepseek.com` serves —
  `deepseek-flash` (DeepSeek-V4.1-Flash-1017, multimodal, 1M context) and
  `deepseek-v4-pro`. The retired names `deepseek-v4-flash` and
  `deepseek-v4-flash-vision-exp` stay registered (never seeded) with the serving
  model's capabilities; `deepseek-v4.1-flash` was a Novita/OpenRouter *relay* id,
  never a platform id, and is no longer registered. Plan-included `-free` models
  resolve to zero-cost cards.
- **DeepSeek Reasoning Toggle and Alias Tiers**: `effort_ladders::DEEPSEEK_NATIVE`
  declares the platform's full accepted vocabulary, so `none` (thinking off) is
  reachable and is no longer clamped up to `low`, and `medium`/`xhigh` no longer
  clamp down to `low`. A live `/models` `effort.supported_levels` advertisement is
  expanded to the same set, so a catalog sync cannot change the menu.
- **DeepSeek Responses Dialect Purity**: the Responses builder emits
  `reasoning.summary` / `include: ["reasoning.encrypted_content"]` only where the
  dialect declares them (never for DeepSeek); `response.incomplete` and
  `response.failed` are handled as terminal events (truncated turns keep their
  output, replay artifact and usage); Chat Completions replays an assistant
  turn's `reasoning_content` on the DeepSeek dialect, where the vendor requires it.
- **RAII Test Path Sandbox (`paths::TestDirsGuard`)**: a single guard that holds
  the process-wide override lock and restores the default roots on drop,
  including while unwinding. Every test sandbox now uses it.

### Changed

- **DeepSeek Effort Ladder Correction**: the OpenCode Go/Zen DeepSeek rows now
  carry the platform's accepted reasoning vocabulary (shared ids must agree
  across provider tables), and the retired Flash generation is declared
  multimodal there too, matching the model that serves it upstream.
- **Baseline Fidelity Proof Amendments**: capability rows deliberately changed
  after the registry migration are listed explicitly in
  `POST_MIGRATION_AMENDMENTS` instead of being folded into the historical
  pre-migration snapshot.
- **Dispatch-Anchored Request Rating (`[INV-PRICING-05]`)**: `BeginRequestParams`
  and `RequestUsageRecord` carry the provider dispatch epoch timestamp (`started_at_ms`)
  as the definitive temporal anchor for both durable ledger persistence and cost rating.
- **Session Pre-flight Spend Accumulation**: The local spend circuit breaker reads
  monotonically accumulated turn spend rather than re-evaluating historical tokens
  against current time-varying rates.

- **Netune Sourced by Git Tag**: `netune` / `netune-trace` now resolve from
  `https://github.com/ming2k/netune.git` tag `v0.0.1` instead of crates.io, whose
  `0.1.x` line is yanked and unmaintained. All crates declare them once through
  `[workspace.dependencies]`. No API change.
- **Fallible Provider Construction**: `build_provider_for_channel` and
  `build_credential_source` return `Option` instead of panicking when the
  composition root has not registered a factory; callers surface the error at
  their own boundary instead of aborting the task.
- **Isolated Test Roots**: test builds resolve an isolated per-process temporary
  root instead of the developer's real XDG directories. An explicit `NUO_HOME`
  (and a production `set_default`) still wins, so spawned servers and end-to-end
  harnesses keep observing their sandbox.

### Fixed

- **Connection Store No Longer Clobbered by Tests**: the spend-circuit-breaker
  fixture rewrote the developer's real `$XDG_STATE_HOME/nuo/connections.toml`;
  the isolated test root makes that whole class of mistake inert.
- **Per-Crate Builds Without Netune**: `WireClient::new` now exists for every
  feature combination, so `cargo test -p nuo-persistence` and similar builds no
  longer fail to compile.

## [0.0.40] - 2026-10-19

### Changed

- **Process-Shared Netune Connection Pool & Request Prewarm (ADR-0005)**:
  - Converged every netune adapter — the provider egress, the `nuo-model-codec` wire client, and the control-plane `Http` handle — onto one process-wide connection pool via `nuo-provider-transport::network::shared_client`. A warm socket is now warm for the whole process instead of being fragmented across per-instance `Pool::default()` caches.
  - Added `TransportClient::prewarm` and raced it against auth resolution in the Anthropic, Google Antigravity, and OpenAI providers, collapsing DNS/TCP/TLS setup into a pool hit by dispatch time; prewarm failures are advisory and logged, never propagated.
  - `NuoNetEgress` and `WireClient` expose `from_shared_client` for callers that own the client configuration; `Http` handles are now cheap clones over the single pool.

### Fixed

- **Byte-Exact SSE Reassembly Across Chunk Boundaries**: `SseDecoder` now buffers raw bytes and decodes only at `\n` line boundaries, reassembling multi-byte UTF-8 sequences (CJK, emoji) split across network chunks. Eliminates the per-chunk lossy-decode `U+FFFD` corruption in streamed non-ASCII deltas, with regression coverage for split UTF-8 and split CRLF frames.
- **Panic-Free Egress Degradation**: `owned_egress` now degrades a failed shared-client initialization to a logged plaintext connector instead of panicking, so the send path reports the genuine error at dispatch.

## [0.0.39] - 2026-10-18

### Added

- **Unified Remote Transport Topology, Secure Endpoint Resolution & Interactive Remote Attach (ADR-0078)**:
  - Introduced the explicit `ClientTarget` abstraction in `nuo-client`, routing every attach flow through one stream-generic handshake with explicit project scoping — whether the target is a locally discovered/spawned server or an explicitly addressed remote server.
  - Hardened remote endpoint resolution in `nuo-provider-transport` and enabled `rustls-tls-native-roots` certificate verification for websocket transport.
  - `nuo server start` is now detached by default, with `--fg` as the sole explicit foreground declaration.

- **OpenCode Provider Channel Bifurcation & Legacy Container Crate Retirement (ADR-0079)**:
  - Bifurcated `nuo-provider-opencode` into dedicated leaf provider channel crates: `nuo-provider-opencode-plan` (OpenCode Plan subscription surface) and `nuo-provider-opencode-zen` (OpenCode Zen relay surface).
  - Enforced `[INV-PROV-06]` Canonical Provider Spec Export and `[INV-PROV-07]` Channel Boundary Segregation, restoring symmetrical `MODEL_PROVIDER_SPEC` registration at the composition root (`nuo-server`).
  - Completely retired and removed the legacy `nuo-provider-opencode` container crate from the workspace.

### Changed

- **Workspace Namespace Pure Surface Topology & Categorized Palette (ADR-0077)**:
  - Pruned legacy strokes from the `C-x …` workspace namespace: the `Quit` scene verb and its `C-x C-c` / `C-x k` spellings are excised, leaving `Leave` bound exclusively to `C-x w`.
  - Categorized command palette entries (`CommandCategory`) with `@` scoping to Navigation / Settings and `>` scoping to Actions / Commands.
- **`nuox` → `nuo` Surface Consolidation**: renamed the terminal application surface to `nuo` across CLI help, shell completions, and client documentation.

## [0.0.38] - 2026-10-18

### Added

- **Dynamic Composer Height Scroll Reflow & Sticky Viewport Underflow Protection**:
  - Unified composer height calculation across the event loop and transcript renderer via `calculate_composer_height`, eliminating geometry drift during multiline editing.
  - Interactive transcript view now adjusts dynamically in real time when multiline input expands or shrinks the composer box, maintaining proper viewport scroll anchors and preventing content jumping.
  - Added sticky collapse underflow guard: collapses transcript scroll to top when total content fits within the viewport.
  - Sanitized connection quota headers in `/connections`: suppressed redundant provider/quota plan names (e.g. `Antigravity Quota`, `google-antigravity`) to keep badge rows clean and uncluttered.
  - Cleaned Google Antigravity quota parser to emit `None` for plan names when upstream responses echo generic service titles rather than actual tier names.

## [0.0.37] - 2026-11-17

### Added

- **Two-Tier Model Detail Inspector and Actionable Tuning View**:
  Integrated model capability inspection and live behavioral tuning directly into the `/models` dialog surface (`ModelsDialog.info_detail`).
  - Added dedicated Model Detail sub-view accessed via <kbd>i</kbd> (and <kbd>Enter</kbd> on isolated unavailable search results), presenting full un-truncated provider refusal reasons, capabilities, context token limits, and multimodal support.
  - Interactive live tuning knobs directly on the inspector card: <kbd>←</kbd> / <kbd>→</kbd> steps reasoning effort ladders, <kbd>Space</kbd> toggles extended thinking, <kbd>Enter</kbd> saves and activates immediately, and <kbd>Esc</kbd> saves and returns to list view.
  - Sanitized `/models` list row status tokens: replaced verbose multi-clause upstream error prose with clean, compact status badges (`locked`), preventing column collisions and ugly ellipsis truncation.
  - Retired the legacy 50-row plaintext models accordion in `/connections` (`ConnectionDetail`) in favor of a 2-line gateway summary and direct <kbd>m</kbd> shortcut to browse models in `/models` pre-filtered.

## [0.0.36] - 2026-11-17

### Added

- **Canonical Unselectable List Item Topology, Inert Skip Navigation, and Gestalt Spacing Firewall (ADR-0076)**:
  Unified disabled and locked list item interactions across dialog surfaces and option pickers.
  - Ratified `[INV-TUI-INERT-01]` Inert Disabled Item Navigation: selection cursors automatically skip unusable items during arrow navigation and wrap-around, and unusable items are strictly inert to mouse clicks and activation inputs.
  - Ratified `[INV-TUI-INERT-02]` Gestalt Spacing Firewall & Text-Join Hygiene: `ListRow::finish` enforces an unconditional minimum separation (`GROUP_GAP`) before trailing badges, eliminating string collisions on narrow viewports. Status reasons strictly follow canonical em-dash grammar (`locked — {reason}`).
  - Subdued visual presentation: unusable rows render uniformly in `theme.dim()` without bold modifiers.

## [0.0.35] - 2026-10-18

### Added

- **Orthogonal Workspace Container Topology and Intra-Tab Scene Discipline (ADR-0075)**:
  Decoupled horizontal workspace containers (Row 1 TabBar: Thread, Dashboard, Settings) from
  vertical intra-tab drill-in depth (Row 3 SceneBar: Thread, Subagent).
  - Enforced `[INV-TAB-10]` Workspace Container Purity: opening Dashboard or Settings strictly
    allocates or focuses its dedicated sovereign Tab container rather than polluting active thread history.
  - Enforced `[INV-SCENE-02]` Container-Scoped Scene Discipline: `ClientTab::push_scene` mechanically
    rejects cross-container scene injections.
  - Ratified `[INV-COMP-01]` Component Nesting Boundary: code diffs and tool execution outputs remain
    encapsulated as components within the active scene canvas, eliminating scene inflation.
  - Regression tests: `surfaces::tests::test_inv_tab_10_and_inv_scene_02_discipline`.

### Fixed

- **Trust gate confirm loop (workspace trust could never persist)**: a
  wedged SQLite WAL state in the daemon — the on-disk `nuo.db-wal`/`-shm`
  pair removed while the daemon was live — made every fresh read-only open
  fail with `disk I/O error`, so workspace-security reads failed closed and
  the `[file asset trust]` pre-attach gate re-presented itself on every
  Enter with no visible cause. Two-sided hardening:
  - *Persistence (ADR-0231 reader-wedge self-heal)*: the supervisor gained a
    typed `RebuildEngine` verb with a `RetireEngine` handshake — the live
    engine is drained with an explicit `wal_checkpoint(FULL)` *while still
    fully functional* (an unlinked WAL's frames are recovered
    deterministically, since the fd-backed read and the main-database write
    are both valid inodes), the engine is then closed cleanly, and only
    after that does the fresh generation open and recreate the sidecar
    pair. `PersistenceHandle::reader()` counts consecutive construction
    failures and requests exactly one rebuild per wedge episode at the
    3-failure threshold (CAS latch, cleared by the next successful open);
    non-owner handles never request a rebuild.
  - *TUI (ADR-0175 attestation outcome)*: a failed trust-grant round-trip is
    now rendered on the gate itself (the server's `trust_changed` notice
    text), clearing the submitting spinner so the user can retry with full
    knowledge of the cause instead of looping a silent dialog; trust
    notices with no gate mounted keep their normal transcript/toast flow.
  - Regression tests: `supervision::reader_wedge_heals_through_engine_rebuild`
    (wedge → threshold → rebuild → recovery → durability) and
    `tests::pre_attach_gate::*`. Blameless record:
    `docs/dev/postmortems/2026-10-18-trust-gate-loop-persistence-reader-wedge.md`.

## [0.0.34] - 2026-10-18

### Fixed

- **Release-build regression in `QuestionModel::update_mut`**: the in-place
  update helper was gated behind `#[cfg(debug_assertions)]` while its sole
  production caller (`declarative/ask_user.rs`) invokes it unconditionally —
  release builds (e.g. `wright upgrade`) failed with E0599. The erroneous gate
  is removed; the method is now unconditionally available.

## [0.0.33] - 2026-10-17

### Fixed

- **Tab-Aware `/new` Workspace Projection (ADR-0039, ADR-0058)**: Aligned `/new`
  harness command and its canonical `/session new` invocation with the sovereign
  `ClientTab` workspace topology. When invoked in an active thread with dialogue
  history, `/new` preserves the existing tab and transcript intact, opening and
  focusing a fresh sovereign `ClientTab::Thread` (with optional initial prompt
  execution if arguments are provided), eliminating in-place transcript wipes.
  Invoking `/new` in an already-empty thread resets the session in place,
  preventing tab pollution.
- **Antigravity `claude-{opus,sonnet}-5-5` 404** (`Requested entity was not
  found`): the bare 5-5 ids are catalog-side fold targets, not registered
  PredictionService entities. Reverse-engineering the shipped `agy` 1.4.0
  binary and the paid-pro `fetchAvailableModels` snapshot confirmed the wire
  registers only the per-level entities
  `claude-{opus,sonnet}-5-5-{low,medium,high,max}` — the same
  entity-suffix-is-effort pattern as `gemini-4-argon`. Channel effort and bare
  base ids now route onto them through the typed evidence-based
  `WireEntityResolution` mechanism ([ADR-0074](docs/adr/0074-observed-wire-entity-resolution-and-evidence-driven-per-level-model-routing.md)), with the
  parser→persistence→request-builder evidence belt; both 5-5 flagships were
  added to the provider's baseline surface and offline model seed.

## [0.0.32] - 2026-10-17

### Added

- **OpenCode 2026-10 Promotional Free Models**:
  - Ingested `deepseek-v4.1-flash-free` and `glm-5.3-flash-free` into `nuo-provider-opencode` baselines (`plan.rs`, `zen.rs`, `console.rs`) and model seeds (`OPENCODE_GO_MODELS`, `OPENCODE_ZEN_MODELS`, `OPENCODE_CONSOLE_MODELS`).
  - Marked models with `EPHEMERAL: OpenCode Promotional Free Tier` lifecycle tags, restricting context window to 200K (vs 1M paid) and max output to 32K.
  - Expanded OpenCode Go and Zen baselines with latest flagship models (`glm-5.3`, `glm-5.3-flash`, `qwen3.8-max`, `qwen3.8-flash`, `claude-haiku-5-5`, `gemini-4-argon-contributor`, `kimi-k3`, `mimo-v2.6-flash`, `mimo-v2.6-pro`, `grok-4.7`).
- **OpenCode Multi-Protocol Routing**:
  - Configured protocol roots for OpenCode Go and Zen surfaces: routed `AnthropicMessages` to `inference/go/anthropic/v1` and `GoogleGemini` to `inference/go/google/v1beta`.

### Changed

- **Abolished OpenCode OAuth2 Device Flow & Console Subscription**:
  - Removed obsolete `device.rs` and `oauth.rs` modules, along with `nuo-oauth` and `netune` dependencies from `nuo-provider-opencode`.
  - Re-aligned the general `opencode` provider to standard API key and public keyless access over `https://opencode.ai/zen/v1` catalog.
  - Upgraded OpenCode client profile emulation to `opencode/2.0.32`.
- **Cognitive Memory Role Scoping**:
  - Dialogue memory recording is now strictly guarded by presence of `recall_memory` tool (e.g. `philosophist`), permanently excluding `developer` and `ops` roles from recording conversational transcripts.
- **TUI Surface Navigation**:
  - Refined scene and tab transitions for Dashboard, Settings, and Thread views, including mouse click and tab switching.

## [0.0.31] - 2026-11-09

### Added

- **Unified CAS & VFS In-Process Projection Architecture (ADR-0070)**:
  - Established rootless sovereign VFS addressing under `/tmp/nuo/vfs/{sha256}.{ext}`, operating with zero root requirements across workspace-bound (`developer`) and workspace-free (`ops`, `philosophist`) roles.
  - Implemented in-process virtual path resolution in `read_text` and `read_image`, resolving offstream context directly from memory/CAS with 0 disk I/O and zero workspace git pollution.
  - Abolished the legacy `recall_context` meta-tool, unifying all context inspection under standard POSIX tools (`read_text`, `execute_command (grep)`, `read_image`).
  - Added deterministic physical metrics (`lines` + `KB` for multi-line streams; `characters` + `KB` with `cut/jq` guidance for giant single-line blobs), completely replacing volatile local token estimations.

### Changed

- **TUI `execute_command` Presentation Modernization**:
  - Implemented three-tier flush stacking layout (Part 1: Terminal canvas, Part 2: Process exit status, Part 3: Harness governance) with zero gap intervals, differentiated purely by layered surface background colors (`code_surface`, `match_title_surface`, `harness_bg`).
  - Transformed output folding into a 3-row compact negative space gap (`blank` + `  N lines omitted (KB)` + `blank`), eliminating all intrusive glyph symbols and dashed lines.
  - Eliminated decorative middle dots (`·`) from step headers, standardizing on clean typography: `Run cargo (running)`, `Run cargo (120ms)`, `Run cargo (failed 0ms)`.
  - Replaced aggressive yellow-bold markers with an unaggressive, distinct metadata status footer explaining canonical POSIX exit codes (`exit 1 (general error)`, `exit 127 (command not found)`, `exit 137 (killed / SIGKILL)`).

## [0.0.30] - 2026-10-17

### Fixed

- **Thread Picker Tab Overwrite & Ghost Thread Elimination (`/threads`)**:
  - Empty placeholder threads are now overwritten in-place when selecting an existing thread from `/threads` (or clicking via mouse in `ThreadsDialog`), preventing phantom tab proliferation.
  - Automatically emits `AgentRequest::DeleteSession` for abandoned empty threads so they are discarded without persisting to SQLite.
  - Aligned `CreateNewSession` ('N' in `/threads` modal): resets in-place when current thread is empty; arms `switch_to_new` to create a fresh thread in a new tab when current thread has active conversation, preventing in-place transcript wipes.
- **TUI Lifecycle Elevation & Zero-Flicker Tab Switching**:
  - Elevated `nuotc::terminal::Terminal` instance and `EnterAlternateScreen` lifecycle to outer loop in `run_attached`.
  - Switching thread tabs now preserves raw mode and Alternate Screen continuously without dropping back to the host shell, eliminating physical screen flash.
  - Upgraded terminal substrate `nuotc` to `0.0.3`: decoupled DEC Mode 2026 synchronized updates from color depth, introduced uninitialized sentinels for atomic full-screen diffing, and added `StyledClear` to eliminate background color inversion flicker.

### Fixed

- **Google Antigravity 404 Entity Resolution & PredictionService Wire Alignment**:
  - Mapped `gemini-3.8-flash` directly to upstream CloudCode's verified PredictionService entity `gemini-3.8-flash-tiered`, eliminating HTTP 404 `Requested entity was not found` errors.
  - Decoupled reasoning effort from model naming; effort is conveyed orthogonally via Google-native `thinkingConfig` (`thinkingLevel` / `thinkingBudget`).
  - Aligned Claude and GPT-OSS wire entities to `claude-opus-4-6-thinking` and `gpt-oss-120b-medium`.
  - Ingested upstream `tieredModelIds` (`flash`, `pro`, `flashLite`) in `parse_antigravity_catalog` to ensure primary agent flagship models are prioritized in dynamic catalog discovery without omission.
  - Modernized baseline catalog definitions and purged legacy hallucinated models.

## [0.0.28] - 2026-11-06

### Fixed

- **Google Antigravity End-to-End Inference Wire Mapping & Project Routing**:
  - Dynamically mapped canonical `gemini-3.8-flash` with orthogonal reasoning effort depth to concrete wire variants (`gemini-3.8-flash-minimal/low/medium/high`) preventing upstream 404/400 errors.
  - Standardized companion project fallback to `aicode-consumers` across inference execution (`prepare_request_for_auth`) and quota queries (`fetch_usage_with_project`, `retrieve_antigravity_quota_summary`), preventing HTTP 429 `RESOURCE_EXHAUSTED` faults.
  - Preserved curated priority ordering (`[INV-CATALOG-01]`) for `CatalogShape::GoogleCloudCode` in `fetch_remote_catalog` to guarantee top-tier frontier models (`gemini-4-argon`, `gemini-3.8-flash`) lead model selection.
  - Lifted subscription-disabled variants (e.g. `gemini-4-argon-disabled`) into canonical representations with `Availability::locked` per ADR-0273.

## [0.0.27] - 2026-11-06

### Added

- **Canonical Model Catalog Ingestion, Semantic Lifting, and Orthogonal Capability Normalization (ADR-0068)**:
  - Curated Agent Surface Primacy (`[INV-CATALOG-01]`): Prioritized upstream curated agent model sets (`agentModelSorts`) while systematically isolating dedicated sub-task lane models (`commandModelIds`, `commitMessageModelIds`, etc.).
  - Orthogonal Capability Lifting (`[INV-CATALOG-02]`): Folded parameter-suffixed wire variants (`-high`, `-medium`, `-low`, `-minimal`, `-max`, `-tiered`) into canonical model identities and lifted implied parameters into first-class `effort_levels`.
  - Abolition of Legacy Blacklists (`[INV-CATALOG-03]`): Eliminated brittle, hardcoded version string heuristics (`starts_with("gemini-3.6-flash")`) in favor of generic, structural filtering.
  - Provider Dialect Encapsulation (`[INV-CATALOG-04]`): Encapsulated Antigravity live catalog parsing in `nuo-provider-google-antigravity::catalog`, purifying `nuo-provider-catalog` as a shape-keyed dispatch engine.
  - Antigravity 1.3.6 Modernization: Added native support for flagship `claude-sonnet-5-5` and `claude-opus-5-5` on Google Antigravity with full `[low, medium, high, max]` effort depth and transparent wire envelope mapping.

## [0.0.26] - 2026-11-06

### Added

- **Multi-Factor Pricing Specification, Dual-Mode Connection Budget, and Spend Circuit Breaker (ADR-0065)**:
  - Multi-factor model pricing specification capturing prompt token tiers (≤128K, >128K), completion tokens, and cache read/creation costs.
  - Dual-mode connection budget enforcement across token counts and currency spend limits with spend circuit breaker halting execution upon budget exhaustion.
  - Granular token classification and cost accounting recorded across connection usage events.
- **Canonical Protocol Nomenclature and Codec Purity (ADR-0066)**:
  - Renamed `crates/nuo-wire` to `crates/nuo-protocol` to reflect canonical IPC nomenclature across client-server boundaries.
  - Decoupled internal East-West IPC structures from external North-South LLM streaming codecs.

### Changed

- **DocGov Decoupling, Invariant Realism, and Documentation Topology Modernization (ADR-0067)**:
  - Decoupled external docgov tooling, retired `.docgov.yml`, and flattened governance structures to `docs/dev/standards/` and `docs/dev/testing.md`.
  - Degazetted pseudo-invariant tagging across presentation components in favor of typed models, Rust compiler invariants, and deterministic tests.
  - Swept legacy citations and resolved references to canonical ADRs.
- **Google Antigravity Provider Alignment**:
  - Synchronized Antigravity User-Agent profile to `agy/cli/1.3.6` (`cl=999748552`).
  - Purged obsolete legacy models and retired synthetic `-tiered` duplicate; cleanly standardized canonical model set to `gemini-4-argon` and `gemini-3.8-flash` with orthogonal reasoning effort depth.

## [0.0.25] - 2026-11-04

### Fixed

- **Tool Execution Slot State Machine and Viewport Argument Isolation**:
  - Eliminated legacy `("raw", ...)` fallback in `parse_arguments_kv` that leaked unclosed streaming JSON arguments into expanded tool steps.
  - Hardened in-flight component state machine preventing incomplete arguments from triggering execution-phase diff projections during the `Ingesting` phase.
  - Enforced atomic JSON validation and Canonical formatting on lowered tool calls, converting truncated/aborted streams into explicit `[Aborted tool ...]` dialogue facts and preventing unparsed syntax debris from leaking into session history.

## [0.0.24] - 2026-11-04

### Added

- **Orthogonal Agent Tool Surface, Meta-Tool Pruning, and Mutation Topology (ADR-0064)**:
  - Pruned meta-tools and stepwise exploration tools (`ask_user`, `todo`, `list_dir`) from developer role capability sets to eliminate turn inflation and stepwise browsing loops.
  - Hardened `write_file` tool parameters and descriptions requiring complete file content and strictly forbidding placeholder truncation comments.
  - Reinforced `edit_text` as the exclusive primitive for surgical source code modifications.
  - Shifted directory inspection guidance exclusively to pattern-based `find_files` and content-based `search_text`.
- **Consolidated Activity Strip, Scene History Navigation, and Diátaxis Documentation Suite**:
  - Integrated header activity strip providing unified real-time feedback for active tool runs, subagent delegations, and model states.
  - Scene history navigation support in interactive terminal sessions.
  - Complete Diátaxis documentation suite restructuring across tutorials, how-to guides, reference specifications, and architectural explanations.
- **Polymorphic Scenebar Projection and Bifurcated Shell Tool Presentation (ADR-0062, ADR-0063)**:
  - Polymorphic scenebar projection dynamically adapting to session context, multi-thread execution, and telemetry feeds.
  - Bifurcated shell tool execution model cleanly separating interactive foreground actions from background streaming processes.

## [0.0.23] - 2026-10-30

### Added

- **Gemini 4 Argon and Gemini 3.1 Flash Lite Model Support**:
  - Registered `gemini-4-argon` and `gemini-3.1-flash-lite` across model codec, provider specs, and catalog discovery.
  - Wire protocol alignment for Google Gemini API handling `thought: true` boolean flags in response parts.
  - Active running thread visual indicator (`[RUNNING]`) rendered in the session picker overlay.
  - Sovereign thread projection and client tab attach when selecting sessions from the session modal.
- **Pure Thread Topology, Headless Unified Entry, and Persistence Modularization (ADR-0061)**:
  - Headless one-shot execution via top-level prompt and `--headless` flags, retiring legacy `run`.
  - Consolidated thread management under pure `nuo thread` CLI topology and retired vestigial subcommands.
  - Modularized `DatabaseEngine` in `nuo-persistence` into clean domain-specific modules.
- **Canonical Thread Persistence Schema and Spatial Header Topology (ADR-0059, ADR-0060)**:
  - Canonical thread persistence schema and zero-legacy clean break (`[INV-PERSIST-01]`, `[INV-PERSIST-02]`).
  - Three-tier spatial header topology, governance band, and clean tab stack (`[INV-HEADER-01]`).

### Added

- **Granular Causal Session Forking and Sovereign Tab Projection (ADR-0058).**
  - **Granular Temporal Addressing (`[INV-FORK-03]`)**: Extended `/fork` command with granular temporal targeting (`/fork <round>` or `/fork <round>.<turn>`) to branch at arbitrary historical decision points.
  - **Sovereign ClientTab Projection (`[INV-FORK-04]`)**: Mounts forked child threads as sovereign `ClientTab::Thread` viewports on Row 1, allowing concurrent multi-branch exploration with `Alt+1..9` tab switching while preserving the parent session intact.

### Changed

- **Retirement of the `/btw` Aside Subsystem (`[INV-DEPR-01]`, ADR-0058).**
  - Retired the legacy `/btw` command, aside modal overlays, and dedicated aside wire routing, subsuming isolated exploration under first-class causal session forks.
  - Non-destructively redirect legacy `/btw` invocations to `/fork` guidance.

## [0.0.21] - 2026-10-28

### Added

- **Tool Output Claim-Check Mechanism and Context Stub Topology (ADR-0057).**
  - **Zero Context Blowup Guarantee (`[INV-CLAIM-01]`)**: Universal Claim-Check gate offloading oversized tool executions to CAS BlobStore while projecting bounded Context Stubs with head/tail anchors into model context.
  - **In-Flight StreamGuard Defense**: Real-time online stream bounds (200 lines / 16 KB) halting runaway or continuous streaming processes early to prevent context and memory flooding.
  - **Non-UTF-8 Binary-Safe Pipe Ingestion**: Lossless byte-level stream ingestion preventing premature aborts on raw profiling or debugging dumps.
  - **Clean Process Harvesting**: Guaranteed process reaping (`wait` following `kill`) preventing zombie process leaks.


### Changed

- **Terminal configuration canonicalization and disclosure pruning (ADR-0031, ADR-0054).**
  - Canonicalize presentation preferences to `$XDG_CONFIG_HOME/nuo/terminal.toml` and eliminate legacy `tui.toml` shims.
  - Prune redundant tool expansion overrides when matching component defaults, and skip empty `[default_expanded]` tables in TOML serialization.
  - Hermetically sandbox presentation persistence tests with `PathsOverride` to prevent host environment pollution.

## [0.0.19] - 2026-10-27

### Changed

- Maintenance release and workspace dependency synchronization.

## [0.0.18] - 2026-10-27

### Added

- **Capability-first orthogonal terminal composition and autonomous archetype projection (ADR-0053).**
  - **Orthogonal Capability Honesty (`[INV-TERM-01]`)**: Replaces brittle emulator brand heuristics with standards-based orthogonal capability dimensions (color depth, sync updates, mouse protocols, charset encoding).
  - **Two-Phase Archetype Projection (`[INV-TERM-02]`)**: Maps detected capabilities into autonomous elevation archetypes (`RichTrueColor`, `ANSI256`, `ANSI16`, `Monochrome`) and unifies surface elevation containers across presentation views.
  - **Autonomous Zero-Config Adaptation (`[INV-TERM-03]`)**: Automatically tunes visual presentation and theme contrast according to the terminal capability ceiling without manual user configuration.
  - **Spatial Cost Accounting (`[INV-TERM-04]`)**: Accounts for explicit spatial margins and border constraints in responsive layout breakpoints for structured framing on degraded terminal profiles.

## [0.0.17] - 2026-10-24

### Added

- **Unified instruction pipeline, pure role topology, and end-to-end cache fidelity (ADR-0052).**
  - **Deterministic Pipeline Structure (`[INV-PROMPT-01]`)**: Implements strict 5-tier instruction bundling (Global Rules -> Runtime Baseline -> Profile Persona -> Project Directives -> Dynamic Directives) with semantic dependency-directed topological ordering and memoization.
  - **Zero Frame Mutation (`[INV-PROMPT-02]`)**: Directs all harness instruction layers into top-level system frames with zero conversational history mutation or synthetic turn injection.
  - **Role Topology Purity (`[INV-PROMPT-03]`)**: Enforces pure `developer`/`system`/`user`/`assistant`/`tool` role fidelity across all provider adapters without mimicked role multiplexing.
  - **Prompt Cache Frontier Preservation (`[INV-PROMPT-04]`)**: Pinpoints and aligns prompt cache breakpoints across Anthropic, Google Antigravity, and OpenAI wire adapters for optimal prefix cache hits.

- **Composer canonical quadrant modality, atomic draft capsule, and in-place recall topology (ADR-0051).**
  - **Canonical Modality Quad-Partition (`[INV-COMP-01]`)**: Establishes four orthogonal, mutually exclusive composer states: `Normal`, `Recall`, `FollowUp`, and `Steer`.
  - **Atomic Draft Capsule (`[INV-COMP-02]`)**: Unifies text buffer, caret position, and staged attachment handles into an atomic snapshot stashed and restored deterministically across modality shifts.
  - **In-Place Recall Topology (`[INV-COMP-03]`)**: Binds history search and ghost preview entirely to the native composer text surface without separate popup input fields.
  - **Zero Mode Ambiguity (`[INV-COMP-04]`)**: Unifies `Enter` / `Esc` semantic dispatch with visual modal indicator badges and distinct tone highlights.

- **Authentic thread title lifecycle, clean scene head, and semantic tab projection (ADR-0050).**
  - **Single Title Source of Truth (`[INV-TITLE-01]`)**: Binds thread titles durably to session identity with automatic cognitive generation on first turn completion and manual lock preservation.
  - **Clean Scene Head (`[INV-HEAD-01]`)**: De-clutters top-level scene headers into distinct identity and scene-status bands, eliminating redundant title and status duplication.
  - **Semantic Tab Projection (`[INV-TAB-01]`)**: Automatically projects authentic session titles and role badges into the top TabBar tabs with graceful middle-truncation.

- **CLI modernization, resource symmetry, and universal state snapshot protocol (ADR-0048, ADR-0049).**
  - **Domain Alignment & Thread Terminology (`[INV-CLI-01]`, `[INV-RES-01]`)**: Modernizes CLI verbs (`nuo thread`, `nuo start`, `nuo stop`) with symmetric subcommands and deprecates legacy commands with actionable hints.
  - **Universal State Snapshot Protocol (`[INV-SNAP-01]`)**: Standardizes JSON/tabular diagnostic outputs across client and server with real-time daemon status monitoring.
  - **Client-Driven Lifecycle & Zero-TCP IPC (`[INV-IPC-01]`)**: Robust local socket communication with instant client-driven daemon lifecycle handoff.

## [0.0.16] - 2026-10-23

### Added

- **Autonomous tool execution slot, fault-tolerant streaming projection, and bifurcated lifecycle architecture (ADR-0046).**
  - **In-Flight Phase Bifurcation (`[INV-TOOL-LIFE-01]`)**: Deconstructs monolithic tool running states into explicit first-class `Ingesting` (model generating parameters) and `Executing` (local host/sandbox execution) stages with dedicated visual channels and theme accent coloring.
  - **Fault-Tolerant Streaming Projection (`[INV-SLOT-02]`)**: Introduces a resilient streaming JSON parser extracting closed parameter fields (`path`, `command`, `query`) while upstream JSON syntax streams, eliminating generic `Edit text` placeholders in favor of instant semantic file targeting (`Edit src/auth.rs`).
  - **Pre-Commit In-Flight Diff Preview (`[INV-SLOT-05]`)**: Automatically projects syntax-highlighted diff previews into expanded tool steps upon argument availability before disk write operations commit.
  - **Streaming Arguments Wire Delivery**: Upgrades wire protocol and harness event channels so `ToolInputProgress` delivers partial argument payloads for real-time frontend projection.

## [0.0.15] - 2026-10-22

### Added

- **Dynamic port sharing palette, orthogonal control plane, and server lifecycle handoff (ADR-0045).**
  - **Intent-Driven Server Handoff (`[INV-LIFECYCLE-03]`)**: Interactive sessions launched with explicit network port declarations (`nuo --port <port>`) decouple server lifecycles from client detachment, printing actionable handoff guidance upon TUI exit while keeping server tasks running.
  - **Port & Remote Sharing Palette (`C-x p`, `[INV-PALETTE-01]`)**: Introduces a dedicated client-level palette modal to hot-enable, hot-disable, or copy connection commands without interrupting ongoing prompts.
  - **Dynamic Endpoint Control Plane**: Extends wire protocol with `OpenTcpListener`, `CloseTcpListener`, and `GetServerEndpoints` verbs with automatic `server.json` synchronization.
  - **Canonical Baseline Port Realignment (`[INV-PORT-01]`)**: Unifies system-wide default TCP port to 9800 across all diagnostic and discovery fallbacks, eliminating historical 9527 drift.
  - **Command Space Purity (`[INV-CMD-02]`)**: Purged `/debug` from conversational slash commands, preserving Thread Command Purity.

## [0.0.14] - 2026-10-21

### Added

- **Composer-native history search, ghost preview, and draft stash topology (ADR-0044).**
  - **Single Input Source of Truth (`[INV-HIST-01]`)**: Establishes Composer as the sole authoritative text input and physical caret owner (`CaretOwner::Composer`) throughout history search, eliminating floating dual-input popups and eye-jumping caret displacement.
  - **Live Ghost Multiline Preview (`[INV-HIST-02]`)**: Dynamically projects selected historical entries into the Composer viewport with full multi-line ghost preview styling, eliminating blind selection of complex prompts.
  - **Atomic Draft Stash & Rollback (`[INV-HIST-03]`)**: Automatically stashes active uncommitted drafts and attachments into `history_draft` upon entering history search, guaranteeing 100% loss-free restoration on `Esc` dismissal.
  - **Zero Dual-Input Chrome (`[INV-HIST-04]`)**: Strips embedded query inputs, fake carets, and prompt noise from the floating history panel, keeping it as a pure candidate drawer.

### Added

- **Thread domain nomenclature unification and compact TabBar affordance (ADR-0043).**
  Standardizes thread entity domain terminology across CLI (`nuo thread`),
  slash commands (`/threads`), and interactive picker surfaces (`ThreadsDialog`)
  while maintaining backward-compatible aliases (`/sessions`, `/resume`).
  Reorganizes the TabBar client menu affordance into a compact "label-first, dim-chord"
  `menu C-x` presentation, and completes the `C-x` Client Global Workspace namespace
  wiring Settings (`C-x ,`), Threads (`C-x s`), and Close Tab (`C-x w`).
- **Client viewport routing ownership, tab-autonomous scene stacks, and domain scene self-projection (ADR-0042).**
  Formalizes Row 1 / Row 2 client shell vs Row 3 / body domain scene ownership.
  Isolates per-tab navigation history with the root `Esc` invariant, and clarifies
  subagent drill-in vs thread fork orthogonality across TUI surfaces.

## [0.0.12] - 2026-10-09

### Added

- **Thread entity taxonomy and server SSOT fan-out (ADR-0038).** Eliminates
  session abstraction ambiguity by strictly separating transport connections, UI
  viewports, and persistent thread business entities. The server is the canonical
  single source of truth for thread state, fanning out updates across attached
  viewports.
- **Client-centric tab workspace and reactive surface sync (ADR-0039).**
  Unified scene and session tab management in the client workspace, with full
  reactive synchronization of surface and dialog states.
- **Cross-domain action matrix and polymorphic TUI architecture (ADR-0040).**
  Introduces domain-scoped action routing, spatial topology, and a redesigned
  three-tier view header cleanly decoupling display layout from input processing.
- **Thread command purity and explicit tab management topology (ADR-0041).**
  Pure thread command boundaries, strict viewport/workspace separation, and
  tab navigation shortcuts (`Alt+W`, `Alt+1..9`).

## [0.0.11] - 2026-10-08

### Added

- **Provider quota aggregation dashboard (ADR-0036, [INV-QUOTA-01..04]).** Nuo
  now separates forward-looking provider *capacity* (`Quota`) from historical
  token *consumption* (`Usage`). New typed wire aggregates
  (`ProviderQuotaSnapshot`, `ConnectionQuotaEntry`) and the
  `QueryProviderQuotas` request/reply pair back three new surfaces: a `/quota`
  TUI dashboard with ambient selectable account rows, and a headless
  `nuo quota [provider] [--refresh]` CLI verb. Quota fetch is type-scoped and
  concurrent across an account pool, reusing pooled connections with a
  memory TTL cache and explicit project-ID propagation.
- **`nuo quota` CLI command.** Fast, headless inspection of provider
  allowances, sliding-window buckets, credit balances, and account pools.
- **Public reference `docs/reference/lifecycle-and-action-semantics.md`.**
- **`edit_text` tool refinements** for terminal-side text editing.

### Changed

- **Session telemetry bifurcated into two surfaces (ADR-0037,
  [INV-STATS-01, INV-TRACE-01, INV-AFFORDANCE-01]).** The former tabbed
  telemetry modal is split into `SessionStats` (`/stats`, `Ctrl+O`) for
  context/token accounting and `SessionTrace` (`/trace`) for the
  round/turn/attempt execution waterfall. Lateral `Left`/`Right` tab chording
  is eradicated; arrow keys now exclusively scroll or select, and `Enter`
  semantics are consistent per surface, with a frictionless cross-surface
  affordance from Stats to Trace.

### Fixed

- **Deterministic pre-bootstrap takeover and lock ordering (ADR-0034
  [INV-LIFECYCLE-07]).** The server runtime now executes conflicting-process
  takeover and acquires the global instance lock (`server.lock`) *before*
  initializing the session registry or any persistence handles. Database
  handles are acquired with bounded-backoff polling and unrecoverable startup
  errors are no longer cached globally, preventing startup race conditions and
  handle poisoning.

## [0.0.10] - 2026-10-07

### Changed

- **Domain-scoped surface architecture completed (ADR-0035).** Every dialog is
  now an encapsulated entity owning its own cursor, scroll, embedded
  `TextInput`, and sub-layer state; the overlay stack owns each open dialog's
  `Box<dyn DialogView>`; dialogs declare a single `DialogScope`, are gated by
  `is_available`, and are evicted only through reverse-LIFO unwinding that runs
  `on_dismiss`. Removed the shared `App` scratchpad fields, the composer-draft
  hijacking, and the blunt `overlay_stack.clear()`.
- **Configuration matrix is the sole runtime source (ADR-0031).** `Config::load`
  reads only `server.toml` / `client.toml` / `agent.toml`; a one-way promotion
  retires a legacy `config.toml` (as does `nuo config migrate`). The schema is
  strict — `deny_unknown_fields`, fail-fast on parse errors — and the legacy
  `[providers]` / `[websearch]` / `daemon` aliases are gone.
- **"Server" replaces "daemon" in the domain vocabulary (ADR-0033).** Canonical
  types (`ServerConfig`, `ServerAction`, `ServerInfo`, …), runtime artifacts
  (`server.json` / `server.sock` / `server.lock`), user-facing strings, and the
  active docs now say "Server".

### Added

- **`nuo server reload` soft reload (ADR-0034 Level 1).** Re-reads the
  configuration matrix and re-syncs MCP servers + skills with zero connection
  drop, completing the three-tier restart architecture.

## [0.0.9] - 2026-10-07

### Added

- **Domain-separated configuration matrix (ADR-0031).** Split monolithic `config.toml` into domain-isolated server, terminal, client, and agent configuration files; added `nuo config migrate`.
- **Credential relocation to state store (ADR-0032).** Relocated authentication credentials and provider keys into the state store beside `auth.toml`, protecting public dotfiles from secret leaks.
- **Canonical Server entity and dual hosting postures (ADR-0033).** Ratified Server as the canonical container entity; separated client-bound posture (pure UDS, zero TCP listeners) from standalone hosted posture (deterministic TCP port 9800 + UDS).
- **Deterministic lifecycle governance (ADR-0034).** Introduced single-flight startup, phased graceful draining, and a tiered restart hierarchy.
- **Encapsulated dialog lifecycle and surface architecture (ADR-0035).** Dialogs refactored into encapsulated entities with context-bound stacks and deterministic LIFO unwinding.
- **MCP presenter and namespaced tool dispatch.** Integrated `McpPresenter` for Model Context Protocol tools (`⚡ server · tool`) and added support for namespaced tool execution resolution.

### Changed

- Renamed crate `providers/nuo-provider-transport` to `nuo-provider-transport`.
- Renamed crate `providers/nuo-provider-google` to `providers/nuo-provider-google-antigravity`.
- Introduced `nuo server <start|stop|restart|status|token>` CLI verbs.

### Changed

- **Client-lifecycle-bound daemon, explicit foreground headless host, and aggressive interface takeover (ADR-0029).**
  Interactive TUI sessions now couple daemon lifecycle directly to connected clients, automatically terminating when the last client disconnects. Foreground execution is enforced by default for headless and service commands (`nuo start`), and endpoint collisions (Unix Domain Socket or TCP port) trigger automatic termination and takeover of conflicting stale instances.
- **`list_dir` steps render as an `ls`-style table (ADR-0030).** The expanded listing no
  longer leads with a count tally, no longer prefixes each row with a type glyph
  (`▸` / `·`), and no longer draws directories in the block's generic `info`
  tone. It now opens with a **`Name` / `Size` column header** and renders each
  entry as **name + aligned size**, colouring the name the way the shell's `ls`
  does — **blue** for a directory, **green** for an executable, **cyan** for a
  symbolic link, and the scheme's normal content tone for a plain file — with a
  directory carrying a trailing `/`. To make the executable/symlink classes
  observable, `list_dir`'s per-row tag became a four-way `[DIR]` / `[EXEC]` /
  `[LINK]` / `[FILE]` class (it previously emitted only `[DIR]` / `[FILE]` and
  so could not distinguish an executable or a link); the renderer colours the
  tool's observed class rather than guessing a type from the name. The `ls`
  hues collapse to the plain content tone under the monochrome (DEC VT100)
  archetype — no colour cue there, the trailing `/` still carries the type — and
  use the terminal's own named slots under ANSI-16. `find_files` keeps its
  count-band + per-directory title-band layout.

## [0.0.7] - 2026-10-06

### Changed

- **Listing blocks (`find_files` / `list_dir`) are now layered, matching the
  search block.** An expanded listing previously dumped every result line — the
  tool's header included — through the path formatter onto one flat surface:
  `Found N matching files:` rendered as a bogus path row, `list_dir`'s `[DIR]` /
  `[FILE]` rows were unknown to the renderer (so *directories*, which carry no
  trailing `/`, were colored as files), and the byte sizes / omission trailer
  were shown raw. A listing now reuses the three-tier contract the search block
  already had (extracted into shared `draw_band_row` / `draw_title_band`
  primitives, so the two blocks stay identical by construction): a brand-tinted
  **count band** parsed from the tool's own header (`Found N files` for a glob
  search, `path · N items` for a directory listing), a per-directory **title
  band** so `find_files` siblings no longer repeat their shared prefix on every
  row, typed `[DIR]` / `[FILE]` rows as a **glyph + name + aligned dim size
  column**, and a dim **omission band** for `... (N additional entries
  omitted)`. Every selectable row's byte range stays anchored in the raw tool
  output.

- **Reasoning milestone summaries no longer inject a leading `the`.** The
  thinking-trace header (`Thinking through …` / `Thought through …`) used to
  prepend an article to every milestone topic unless it already started with a
  determiner, which produced ungrammatical lines for code-like headings
  (`Thinking through the derive(Clone, copy, debug, PartialEq, eq)`). The
  article is now dropped entirely (`Thinking through derive(Clone, copy, debug,
  PartialEq, eq)`); the summary verb supplies the grammatical frame.

### Fixed

- **The transcript reading position is preserved across a terminal resize
  (scroll anchoring).** Scroll was stored as a raw content-line offset, and a
  content line's meaning depends entirely on the wrap width — so after a column
  change the same offset addressed unrelated text and a manually-scrolled
  viewport jumped, drifting further the larger the width delta. The viewport now
  captures the semantic identity of the content at its top (anchored message id
  + row offset) and, on the pass that follows a width change, resolves it back to
  a content-line offset against the new layout before the frame is committed, so
  the same text stays under the viewport top. A steady width never re-anchors
  (so ordinary scrolling is untouched), `follow_bottom` still re-pins to the
  bottom, and the resolve walks the full transcript so an anchor displaced far by
  a large reflow is still found. See
  [ADR-0028](docs/adr/0028-scroll-anchoring-across-terminal-reflow.md).

- **Single-file `search_text` results no longer lose their filename.** Searching
  a *file* (e.g. `path: "nuo-tui/src/event_loop/actions.rs"`) rooted the walker at
  that file, so stripping the search root from each hit produced an empty path
  and the tool emitted a pathless `:2452: pub(super) fn enter_scene(` line —
  wrong for the model's own tool result and, in the TUI, silently degrading the
  expanded step: the count band tallied `0 files` and the per-file title row
  vanished. `search_text`/`find_files` now fall back to the workspace-relative
  path (then the bare filename) when the search root *is* the file, so the real
  path is preserved. The redundant `· N files` segment is also dropped from the
  match count band when a result spans a single file — `Found N matches` alone
  reads cleaner and no longer risks a bogus `0 files`.

## [0.0.6] - 2026-10-06

### Changed

- **Canonical, strongly-typed built-in tool identity (`BuiltinTool`).** The
  native tools are now enumerated once in `nuo_tool::BuiltinTool` (re-exported
  through `nuo-wire`) and tool identity — `Tool::name`, role and subagent
  whitelists, capability admission, and the TUI's verb/expansion tables — keys
  off the enum instead of raw strings. This removes silent misspellings,
  phantom tools, and registration drift across crates. New surface:
  `Tool::builtin`, `Capability::admits`, `ToolScope::admits_builtin` /
  `admits_tool`, `ToolSelection::only_builtin`,
  `SessionRoleManifest::builtin_tools`, and typed `ToolPolicy::allowed_tools`.
- **Tool admission now honours aliases.** A capability is admitted when either
  its canonical name or any registered variant alias matches the scope, so a
  scope that names a compatibility alias still resolves the canonical tool.

### Removed

- **The legacy `run_command`, `bash`, and `read` tool spellings are retired.**
  `execute_command` is the single canonical name; the shell family's old aliases
  no longer resolve, and the trajectory guard, TUI verb/expansion policy,
  session export, and token-pressure heuristics no longer special-case them.

### Fixed

- **Restored source accidentally truncated by the previous commit.** The tails
  of `nuo-tui/src/model/document.rs` (the compaction card, command settlement,
  and all ADR-0026 tool-step methods) and `nuo-tui/src/lib.rs` (the user-logo
  loader and test module) were lost mid-write; the missing code and the
  ADR-0026 announce/collapse methods are restored so the workspace builds and
  the suite runs.

## [0.0.5] - 2026-10-08

### Added

- **Streamed tool-input progress (ADR-0026):** Announce the tool call before its
  arguments finish streaming. Emits `RoundEvent::ToolCallStarted` (name known,
  arguments still streaming) plus count-only `RoundEvent::ToolInputProgress`
  ticks; the full argument object still gates execution, but the UI reflects
  the tool phase and running step immediately.

### Changed

- **`Esc Esc` (interrupt) is now scene-scoped.** Pressing it inside a zoomed
  subagent stops *that subagent* instead of the enclosing primary round — it no
  longer penetrates the scene boundary and cancels the whole outer turn (and
  every sibling subagent). The Subagent scene reads the *viewed child's* own run
  state for the interrupt affordance and arm window, so a finished child under a
  still-running parent no longer advertises an interrupt. The change adds the
  wire verb `AgentRequest::InterruptSubagent { call_id }` (routed by the driver
  into the one child via the shared `SubagentRegistry`; the parent round and its
  siblings are untouched), an `InputAction::InterruptSubagent`, and a
  scene-target dispatch enum shared by the Conversation / Aside / Subagent Esc
  ladders. Adds `[INV-TUI-SCOPE-01..03, INV-WIRE-SCOPE-01]` (ADR-0025).

- **Search-result blocks are now layered instead of a flat run of rows.** An
  expanded `search_text` step draws a top count band (`Found N matches · M
  files`, bold on a brand-tinted surface), a per-file title band carrying the
  path (bold on a raised surface), and the match rows on the code surface with
  the literal query bolded inside each line. The three tiers each sit on their
  own background (`theme::match_count_surface` > `match_title_surface` >
  `code_surface`), so a file heading and the matched text read as distinct from
  the surrounding content rather than blending into it. The count band appears
  only when the tool emitted its `Found N match(es):` header (a structured
  `Matches` payload drops it, so no band is invented); the literal-query
  highlight is skipped when the search ran as a regex. Listings
  (`find_files`/`list_dir`) are unchanged.

- **`ask_user` steps render as a readable question→answer list.** An expanded
  `ask_user` step previously fell through to the generic code renderer, dumping
  the harness's answer JSON (`User answered the question(s). Selected option
  labels:\n[[…]]`) as a line-numbered blob — the questions were unrecoverable and
  the answers unreadable. The step now declares a dedicated `ResultKind::Questions`
  renderer that rejoins the questions (recovered from the call's `arguments`:
  header chip, text, option count, multi-select flag) with the recorded selection
  from the result, drawing each question as `[header]` / `question (N options[,
  multi-select])` with its picks on dim `↳` rows (multi-select continuations
  align under the first label), and a cancelled request as `↳ cancelled — no
  answer` rather than an empty array. The collapsed header is now count-led
  (`Ask N question(s) · <header chips>`) instead of privileging the first
  question. Parsing is defensive: a restored or truncated call degrades to the
  raw result text instead of panicking.

## [0.0.4] - 2026-10-06

### Changed

- **Unified the legacy `muta` / `mutx` on-disk vocabulary into the `nuo`
  namespace.** TUI preferences moved from `$XDG_CONFIG_HOME/mutx/config.toml`
  to `$XDG_CONFIG_HOME/nuo/tui.toml` — a sibling of the daemon's `config.toml`,
  not a field inside it. Themes and the ASCII logo resolve under
  `$XDG_CONFIG_HOME/nuo/`. Runtime environment variables were renamed from
  `MUTA_*` / `NUOX_*` to `NUO_*` (`NUO_HOME`, `NUO_PORT`, `NUO_LOG`,
  `NUO_LOG_RETENTION`, `NUO_BIN`, `NUO_HEADLESS`, …), the daemon log file is
  now `nuo.log`, and user-facing strings plus MCP / OpenRouter identities and
  provider `referrer` / user-agent values no longer carry the old name. The
  pre-rename locations and the one-shot `history.json` legacy import are
  dropped outright — no compatibility shims.
- **Two-row head band: uniform session identity + scene row (ADR-0024):** the
  head band's two rows now have fixed jobs on every scene. Row 1 is always the
  ambient **session identity** (`SESSION` + id tail + `[ROLE]` badge +
  workspace); row 2 is the **scene row**, naming the scene the user stands in in
  plain lowercase (`conversation`, `dashboard`, `settings`, `subagent`,
  `aside`) followed by its context (the chat title, the fleet summary, the
  settings breadcrumb, …). The session's `UNATTENDED` / `UNCONFINED` run-mode
  flags move from the conversation's row 1 to row 2's right edge, beside the
  standing `C-x menu` namespace pair.
- **Command Palette entry is now `C-x p` (ADR-0023):** the palette's sole
  canonical chord is the `Ctrl+X` scene namespace's switcher verb. The former
  `Ctrl-L` binding — and the router special-case that *disabled* it behind other
  modals — are retired, so the palette opens from every context through one
  discoverable entry. `Ctrl-L` is now inert; a user can still remap `palette` in
  `[keybindings]`.
- **Standing `C-x menu` head legend (ADR-0023):** row 2 of every scene's head
  band now always carries the `C-x menu` namespace pair (the scene namespace's
  renamed label), so the palette / switcher entry is visible everywhere. The
  session head's row-1 `Ctrl-l palette` keycap is removed.
- **Borderless floating popups (ADR-0023):** the which-key card, tooltip,
  dropdown, and popover render without edge lines on modern (TrueColor /
  256-color) terminals, separated by an elevated background instead — matching
  the toast's visual language. Hybrid (ANSI-16) and monochrome terminals keep
  their frames.

## [0.0.3] - 2026-10-05

### Added

- **Daemon dev-drift detection by content identity (ADR-0021):** the daemon
  publishes a bounded digest of its own executable image in the discovery
  record, so a client detects a rebuilt (same-version, same-protocol) daemon by
  comparing executable content rather than the Linux-only inode probe.
- **Idle-gated self-heal:** when the running daemon's image differs from the
  installed one and it is provably idle (no active sessions or daemon tasks),
  the client reclaims it through the canonical drain pipeline and respawns the
  fresh build. A busy daemon is refused with a message naming the work that
  would be interrupted.
- `nuo status --diagnostic` now renders a `Core Image` block (installed vs.
  daemon digest) and a rebuilt-binary drift diagnosis.

### Changed

- Comment and documentation drift swept: legacy crate names (`muta-*` → `nuo-*`),
  broken ADR links, and stale module docs.
- User-facing strings aligned with the `nuo` product name (`nuo stop`,
  `nuo client` update recommendations, daemon banner and log prefixes).

### Removed

- Dead duplicate modules `nuo-server/src/{client,identity,supervisor}.rs` and the
  orphan `nuo/src/client.rs`.
- The dead `discovery_path` helper and the unused `supervise` alias.

### Fixed

- An orphaned integration test (`nuo/tests/it/daemon_spawn.rs`) that was never
  compiled; it is now declared and exercises daemon spawn isolation.

## [0.0.2] - 2026-10-04

Initial tagged release: the unified `nuo` binary, the extracted `nuo-server`
container runtime, decoupled capability tools, and the `providers/` namespace.
