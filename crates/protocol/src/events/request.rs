//! Request-side wire types: trust decisions, agent requests, and web config.

use super::*;

/// What a human decided when asked to review project-authored content.
///
/// A closed two-value vocabulary rather than an inferred one, because the two
/// answers have opposite effects and the request that carries them
/// ([`AgentRequest::TrustWorkspace`]) also carries the domain list. Sending the
/// refused domains in a field that means "grant" is a security inversion that no
/// type would have caught; naming the intent makes it unrepresentable.
///
/// Deliberately has **no** serde default. A frame that omits the decision is
/// rejected, because every default would have to pick a direction, and the only
/// safe direction for a malformed trust frame is to refuse it outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustDecision {
    /// Load the listed domains: persist a grant for their current content.
    Grant,
    /// Do not load the listed domains: persist a refusal for their current
    /// content, so they are not asked about again until that content changes.
    ///
    /// Domains outside the list are left untouched — a refusal is scoped, unlike
    /// [`AgentRequest`]'s revoke path, which drops every grant for the workspace.
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentRequest {
    /// Submit a user prompt to start an interactive round.
    #[serde(alias = "Chat")]
    Prompt {
        text: String,
        images: Vec<ImagePart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sent_at_ms: Option<u64>,
    },
    /// Queue steering message into a round that is already running. The target
    /// thread is explicit so a frontend can keep composing in a side view
    /// (or switch views) without accidentally steering the wrong agent. The
    /// message is admitted atomically at the next safe turn boundary.
    Steer {
        thread_id: String,
        #[serde(alias = "input")]
        message: QueuedMessage,
    },
    /// Cancel a not-yet-admitted [`AgentRequest::Steer`]. The agent linearizes
    /// cancellation against boundary admission: exactly one of
    /// `SteerAdmitted` or `SteerCancelled` is emitted for the id.
    CancelSteer {
        thread_id: String,
        input_id: String,
    },
    /// Queue a follow-up message into an explicit live thread. The
    /// **driver owns the queue** (M4): when the target round is
    /// idle the message starts immediately ([`RoundEvent::FollowUpStarted`]);
    /// when it is running the driver enqueues it
    /// ([`RoundEvent::FollowUpQueued`]) and ships it automatically at the
    /// round boundary. The frontend sends one verb and renders the state
    /// the backend reports — it never decides when a follow-up ships.
    FollowUp {
        thread_id: String,
        #[serde(alias = "input")]
        message: QueuedMessage,
    },
    /// Remove one queued follow-up (the queue modal's delete / destructive
    /// recall-to-composer). Idempotent: removing an unknown id is a no-op.
    QueueRemove {
        thread_id: String,
        input_id: String,
    },
    /// Clear every queued follow-up for the thread (the queue modal's clear).
    QueueClear {
        thread_id: String,
    },
    /// Reorder one queued follow-up within its thread's queue by `delta`
    /// positions (the queue modal's `K`/`J`). Clamped at the thread's
    /// slice boundaries; unknown ids are a no-op.
    QueueReorder {
        thread_id: String,
        input_id: String,
        delta: i32,
    },
    /// Re-read the authoritative follow-up queue for one thread.
    ///
    /// The driver pushes a [`RoundEvent::QueueUpdated`] snapshot after every
    /// queue change, but a snapshot is an *event*, not a fact that survives a
    /// disconnect: a client that attaches (or re-attaches) to a live session
    /// starts with an empty projection and no event is coming, so it would show
    /// an empty queue while the driver holds items that will ship at the next
    /// boundary — the user sees nothing pending and gets surprise prompts. A
    /// frontend asks once when it settles on a thread.
    QueueSnapshot {
        thread_id: String,
    },
    /// Pause or resume automatic shipping of the thread's follow-up queue
    /// (`Ctrl+P` / the queue modal's block control). Enqueueing still works
    /// while paused; only the round-boundary auto-ship is gated.
    QueuePaused {
        thread_id: String,
        paused: bool,
    },
    SlashCommand(String),
    /// Answer the pre-view workspace-trust decision for the active workspace.
    ///
    /// A direct control-plane admission action — it does not emit `/trust` into
    /// the transcript or record command history.
    ///
    /// `decision` states *which way* the list points, and the list is always the
    /// set the decision applies to: `Grant` authorizes these domains (the server
    /// refuses every other candidate itself), `Deny` refuses exactly these and
    /// leaves every other grant intact. The intent is a field rather than
    /// something the server infers, because a bare `Vec<TrustDomain>` does not
    /// say which direction it points — and reading a refusal as a grant is a
    /// security inversion in the one path whose purpose is to record refusals.
    TrustWorkspace {
        domains: Vec<TrustDomain>,
        decision: TrustDecision,
    },
    /// Soft reload (ADR-0034 Level 1): re-read the configuration matrix and
    /// credentials, and re-sync MCP servers and skills, without dropping any
    /// active connection.
    ReloadRuntime,
    /// Ask the server to complete the composer input at `cursor`. Cursor and
    /// response edit offsets are Unicode-scalar indices so native and browser
    /// clients share one indexing contract. `request_id` lets clients discard
    /// a response that raced newer typing.
    #[serde(alias = "CompleteInput")]
    CompleteComposer {
        request_id: u64,
        #[serde(alias = "input")]
        text: String,
        cursor: usize,
    },
    Interrupt,
    /// Interrupt one in-flight subagent spawned by the session, addressed by the
    /// parent tool-call id that spawned it (the same key the
    /// `SubagentRegistry`/`SubagentHandle` duplex path uses, ADR-0029). Esc Esc
    /// inside the Subagent scene resolves to this so the chord is *scene-scoped*:
    /// it stops only the viewed child and leaves the enclosing primary round and
    /// every sibling subagent untouched (scene scope). The child's round
    /// loop observes the cancellation at its next safe boundary, returns its
    /// partial transcript, and the parent records it as an interrupted (not
    /// failed) result — identical to the drain the primary interrupt triggers when
    /// the parent turn is stopped. An unknown or already-finished `call_id`
    /// degrades to a no-op.
    InterruptSubagent {
        call_id: String,
    },
    /// The client declares this thread over. Sent on the paths
    /// where the operator's intent is "I am done with this thread", not
    /// "I am detaching": the TUI's `/exit` and double-Ctrl+C quit, a
    /// headless run's terminal round, and the Web app's end-thread
    /// action. The server intercepts it at the attach connection (it never
    /// reaches the driver queue) and tears the hosted thread down through
    /// the same path as `ControlRequest::KillThread`: cancel the driver,
    /// fire ThreadEnd hooks, drop it from the registry, and publish
    /// `ThreadRemoved` so every dashboard drops the row. Disk history is
    /// kept — ending a thread is not deleting it.
    EndThread,
    PermissionReply {
        request_id: String,
        decision: PermissionDecision,
        /// Full-duplex (ADR-0029): when the reply targets a permission
        /// request surfaced by a *subagent* (carried up as a
        /// [`RoundEvent::SubagentStep`] / [`SubagentEvent::PermissionRequest`]),
        /// this is the parent tool-call id the request was nested under. The
        /// harness looks up the live child's `crate::SubagentHandle` in the
        /// task registry by this id and resolves its parked oneshot directly.
        /// `None` means the request came from the top-level
        /// agent and is resolved on `context.agent` as before.
        parent_call_id: Option<String>,
    },
    UserQuestionReply {
        request_id: String,
        answers: Vec<Vec<String>>,
        /// Full-duplex (ADR-0029): the parent tool-call id when the answered
        /// question came from a subagent's `ask_user`
        /// ([`SubagentEvent::UserQuestionRequest`]); `None` for a top-level /
        /// side agent question. See [`AgentRequest::PermissionReply`] for the
        /// routing contract.
        parent_call_id: Option<String>,
    },
    /// Reply to an interactive command's standard input request ([`AgentEvent::StdinRequest`]),
    /// routed back to the parked oneshot. `parent_call_id` mirrors the question/permission replies for subagent
    /// routing.
    #[serde(alias = "InputReply")]
    StdinReply {
        request_id: String,
        text: String,
        parent_call_id: Option<String>,
    },
    SwitchConnection {
        provider: String,
        model: String,
        api_key: Option<crate::SecretString>,
        base_url: Option<String>,
    },
    /// Create a connection: bind a model provider to a credential and a client
    /// identity, persist it, then activate it. `name` is the connection's
    /// identity and must be unique; the server rejects a duplicate
    /// with a suggested alternative instead of silently disambiguating.
    /// `provider` must name a registered model provider. `protocol`, `base_url`,
    /// and `user_agent` are optional overrides of the provider's defaults;
    /// `models` is an optional initial inclusion set, honored only for models
    /// Register or update a declarative model provider surface in `model_providers.toml`.
    RegisterProvider {
        id: String,
        #[serde(default)]
        label: Option<String>,
        root_url: String,
        #[serde(default)]
        protocol: Option<crate::WireProtocol>,
        #[serde(default)]
        user_agent: Option<String>,
        #[serde(default)]
        headers: Option<std::collections::BTreeMap<String, String>>,
        #[serde(default)]
        catalog_format: Option<String>,
        #[serde(default)]
        dialect: Option<String>,
    },
    /// Add a new connection instance to `connections.toml`.
    /// A connection is a credentialed pipe to a model provider.
    ///
    /// Per ADR-0046, reasoning (effort/thinking) is not set at connection
    /// creation — it is opted in per model via the stage-2 model `e` editor
    /// (`EditConnectionModel`). New channels start with thinking off.
    AddConnection {
        name: String,
        provider: String,
        api_key: crate::SecretString,
        #[serde(default)]
        models: Vec<String>,
        /// How the connection authenticates. OAuth credentials are owned by
        /// this exact connection name.
        auth: crate::ConnectionAuth,
        /// Custom HTTP headers configured for this connection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<std::collections::BTreeMap<String, String>>,
        /// Budget policy and circuit breaker posture (ADR-0065).
        #[serde(default)]
        budget: crate::ConnectionBudget,
        /// Economic entitlement and cost model (ADR-0095).
        /// When omitted, falls back to the provider's default entitlement.
        #[serde(default)]
        entitlement: Option<crate::Entitlement>,
    },
    /// Reauthenticate an existing OAuth connection. Runs the browser-loopback
    /// or device-code flow, persists directly under `name`, then activates it.
    /// Progress streams via [`AgentResponse::ConnectStatus`].
    ConnectConnection {
        name: String,
        method: crate::LoginMethod,
    },
    /// Run OAuth before a connection exists. Successful credentials remain
    /// session-local until the following `AddConnection` consumes them.
    AuthorizeOAuth {
        method: crate::LoginMethod,
        auth: crate::ConnectionAuth,
    },
    /// Cancel any in-flight OAuth authorization.
    CancelAuthorizeOAuth,
    /// Edit a connection's metadata in place (provider, API
    /// key, client identity) without touching its model scope. Keyed by `name`;
    /// renaming is the separate atomic request [`Self::RenameConnection`].
    EditConnection {
        name: String,
        provider: String,
        api_key: crate::SecretString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<std::collections::BTreeMap<String, String>>,
        /// Budget policy and circuit breaker posture (ADR-0065).
        #[serde(default)]
        budget: crate::ConnectionBudget,
        /// Economic entitlement and cost model (ADR-0095).
        #[serde(default)]
        entitlement: Option<crate::Entitlement>,
    },
    /// Rename a connection. The server rewrites every hard join key
    /// (`credentials.toml`, `auth.toml`, `default_connection`) in one
    /// transaction; historical records keep the old name.
    RenameConnection {
        from: String,
        to: String,
    },
    /// Remove a model (channel) from a user-defined provider, persist, and push a
    /// fresh picker snapshot. The last remaining model is kept (a provider must
    /// serve at least one model).
    /// Include or declare a model within a scope (preset or connection) with optional capability facts.
    IncludeModel {
        scope: crate::model::ModelTargetScope,
        model: crate::model::DeclaredModel,
    },
    /// Exclude/hide a model from the resolved set within a scope.
    ExcludeModel {
        scope: crate::model::ModelTargetScope,
        model_id: String,
    },
    /// Clear explicit inclusion or exclusion rules for a model within a scope.
    ClearModelRule {
        scope: crate::model::ModelTargetScope,
        model_id: String,
    },
    /// Set capability overrides for a specific model within a scope.
    SetModelCapabilities {
        scope: crate::model::ModelTargetScope,
        model_id: String,
        overrides: crate::model::CapabilityOverrides,
    },
    /// Edit settings for one model/channel of a connection. This is
    /// intentionally channel-scoped: OpenAI effort and Anthropic
    /// effort/thinking can vary by model even when the provider endpoint/key are
    /// shared.
    EditConnectionModel {
        connection: String,
        model: String,
        effort: Option<String>,
        thinking: Option<bool>,
        /// Capability overrides (layer 1): `None` keeps the stored
        /// overrides untouched; `Some(record)` replaces them wholesale (an
        /// empty record clears them). Persisted per (instance, model) in the
        /// route-settings **state** store, never in config.
        overrides: Option<crate::model::CapabilityOverrides>,
    },
    /// Edit the per-model reasoning settings (Anthropic effort/thinking) for a
    /// **built-in** model, persisted into the `[model_reasoning."<model-id>"]`
    /// table. This is the model-level counterpart to `EditConnectionModel`:
    /// built-in providers (e.g. `anthropic`) have no user-editable channels, so
    /// their per-model reasoning knobs live in this shared table keyed by model
    /// id rather than on a channel. ADR-0045.
    EditModelReasoning {
        model: String,
        effort: Option<String>,
        thinking: Option<bool>,
        /// Capability overrides (layer 1) — same semantics as
        /// [`AgentRequest::EditConnectionModel::overrides`].
        overrides: Option<crate::model::CapabilityOverrides>,
    },
    /// Delete a connection: drop the entry from `connections.toml`, remove its
    /// credential and OAuth tokens, and persist. If the deleted connection was
    /// the default (`default_connection`), fall back to the first remaining
    /// connection and activate it so the live selection never points at a
    /// removed entry. Unknown names are ignored.
    DeleteConnection {
        name: String,
    },
    /// Toggle the favorite flag on a model in the **Models** picker. `id` is the
    /// model wire id. Favorite is model-level (a daily-driver model is starred
    /// wherever it is served), so the Connections list has no favorite concept.
    ToggleFavorite {
        id: String,
    },
    /// Make `id` the default model and activate it. Equivalent to selecting it
    /// in the picker and pressing `d`: it both sets the persisted default and
    /// switches the live provider.
    SetDefaultModel {
        id: String,
    },
    /// Refresh available models for catalog-enabled providers from upstream.
    RefreshProviderModels,
    /// Delete a thread (active or archived) by id or short id prefix.
    DeleteThread {
        id: String,
    },
    /// Set (or clear) a thread's display title — the manual title the
    /// monitor row and thread pickers show (ADR-0022's AI title fills it
    /// only while no manual title exists). `title: None` clears the manual
    /// title back to the AI/first-prompt fallback. The harness republishes
    /// the monitor row so every client sees the rename.
    RenameThread {
        id: String,
        title: Option<String>,
    },
    /// Request full detail for one thread (the `i` thread-info sub-view).
    /// The harness replies with [`AgentResponse::ThreadDetail`].
    QueryThreadDetail {
        id: String,
    },
    /// Request connection inspection details and live provider usage.
    /// The reply is [`AgentResponse::ConnectionDetail`].
    QueryConnectionDetail {
        id: String,
        #[serde(default)]
        force_refresh: bool,
    },
    /// Request concurrent usage refresh for all configured connections.
    /// The reply streams [`AgentResponse::ConnectionDetail`] per connection.
    QueryAllConnectionsUsage {
        #[serde(default)]
        force_refresh: bool,
    },
    /// Request aggregated provider quota overview across configured connections (ADR-0036).
    /// When `provider` is Some(id), filters to connections serving that provider
    /// (e.g. "google-antigravity"); None inspects all quota-capable connections.
    /// The reply is [`AgentResponse::ProviderQuotas`].
    QueryProviderQuotas {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default)]
        force_refresh: bool,
    },
    /// Request the current threads-picker rows without changing frontend
    /// navigation. The reply is [`AgentResponse::ThreadsOverview`].
    QueryThreadsOverview,
    /// Request the current thread DAG without changing frontend navigation.
    /// The reply is [`AgentResponse::ThreadTreeSnapshot`].
    QueryThreadTree,
    /// Request the token-source report (per-round / per-turn request usage,
    /// reported vs. estimated) for one thread. The harness replies with
    /// [`AgentResponse::TokenUsageReport`] carrying a snapshot of its
    /// server-side ledger. Attached frontends have no local ledger, so the
    /// context-usage modal (click on the hint-bar meter) issues this on
    /// demand — mirroring [`AgentRequest::QueryThreadDetail`].
    QueryTokenUsage {
        thread_id: String,
    },
    /// Request the cross-thread usage-statistics report: daily
    /// token totals, per-model breakdown, and the recent terminal-request
    /// event log, aggregated over the durable day-partitioned store that
    /// survives thread cleanup. The harness replies with
    /// [`AgentResponse::UsageStatsReport`]. Sent by the TUI when the
    /// `/usage` overlay opens.
    QueryUsageStats {
        /// How many recent events to include in the event-log tail.
        event_cap: usize,
    },
    /// Request a fresh thread-context snapshot (model / tools / permissions /
    /// skills / mcp). The harness replies with [`AgentResponse::ThreadContext`].
    /// Sent by the TUI when a manager modal opens.
    QueryThreadContext,
    /// Query server endpoints (port, active, token, local socket) (ADR-0045).
    QueryServerEndpoints,
    /// Toggle TCP listener on/off or set port (ADR-0045).
    ToggleServerPort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        enable: bool,
    },
    /// Revoke a single cached "always allow" permission rule. The harness
    /// removes it from the in-memory allowlist and replies with an updated
    /// [`AgentResponse::ThreadContext`] so the modal reflects the change.
    RevokePermission {
        tool: String,
        scope: String,
    },
    /// Clear every cached "always allow" permission rule for this process.
    /// The harness drops the whole in-memory allowlist and replies with an
    /// updated [`AgentResponse::ThreadContext`] so the permissions manager
    /// modal reflects the now-empty list.
    ClearAllPermissions,
    /// Enable or disable a tool for the current session. Disabled tools are
    /// hidden from the model (their schemas are not sent) and rejected if the
    /// model still tries to call them. The harness replies with an updated
    /// [`AgentResponse::ThreadContext`].
    ToggleTool {
        name: String,
        enabled: bool,
    },
    /// Enable or disable a configured MCP server for the live session. Unlike
    /// [`AgentRequest::ToggleTool`] (which only flips a session flag on an
    /// already-installed tool), this connects/disconnects the server: disabling
    /// drops its tools from the live tool list and closes the connection;
    /// enabling reconnects it from `[mcp.<name>]` config and re-discovers its
    /// tools. Session-scoped — config.toml is not rewritten, so a restart
    /// restores the configured state. The harness replies with an updated
    /// [`AgentResponse::ThreadContext`].
    ToggleMcpServer {
        name: String,
        enabled: bool,
    },
    /// Reset and re-establish one MCP server's connection, re-discovering its
    /// tools (the per-server analogue of the periodic catalog refresh). Used by
    /// the `/mcp` modal's `r` action to recover a crashed/failed server on
    /// demand. The harness replies with an updated
    /// [`AgentResponse::ThreadContext`].
    ReconnectMcpServer {
        name: String,
    },
    /// Update the transcript layout preference.
    /// The harness writes the new value to `config.toml`'s `[tui]
    /// transcript_layout` and replies with [`AgentResponse::TuiLayoutUpdated`]
    /// carrying the persisted string so the renderer updates its state. The value
    /// is a raw config string (e.g. "turn_band"); interpretation into a [`crate`] layout
    /// `Strategy` happens in the renderer, keeping the core free of render types.
    UpdateTuiLayout(String),
    /// Request the persisted prompt input history. The server is the source
    /// of truth for the shared SQLite store; the frontend never opens the
    /// database directly. Replies with
    /// [`AgentResponse::InputHistory`].
    QueryInputHistory,
    /// Record (lock + merge) prompt input history entries on the server —
    /// one entry per recorded prompt, or the frontend's whole buffer on the
    /// exit flush. Fire-and-forget: the frontend's local list already
    /// reflects the entries.
    RecordInputHistory {
        entries: Vec<crate::HistoryEntry>,
        dedup: bool,
    },
    /// Delete one prompt input history row by content and timestamp.
    /// Fire-and-forget; the frontend has already updated its local list.
    DeleteInputHistoryEntry {
        text: String,
        created_at_ms: u64,
    },
    /// Request the stored capability overrides for one provider/model route
    /// (the model editor's prefill). Replies with
    /// [`AgentResponse::RouteSettings`].
    QueryRouteSettings {
        provider_id: String,
        model: String,
    },
    /// Update the TUI color scheme preference (from the `/config` modal).
    /// The harness persists the selected preset id and the custom semantic
    /// palette together so switching away from Custom does not discard it.
    UpdateTuiColorScheme {
        name: String,
        custom: serde_json::Value,
    },
    /// Query the effective singleton `[web]` configuration, readiness, and
    /// provider capability catalog. Secrets never cross the wire. Replied with
    /// [`AgentResponse::WebSearchConfigSnapshot`].
    QueryWebSearchConfig,
    /// Cross-project session history search: BM25 FTS over all
    /// persisted transcript entries in the shared `nuo.db`, optionally
    /// narrowed to one workspace root. The server is the source of truth; the
    /// frontend never opens the database directly. Replies with
    /// [`AgentResponse::HistorySearch`].
    SearchHistory {
        query: String,
        /// When `Some`, restrict hits to this project root; `None` searches
        /// every project the instance has ever hosted.
        workspace: Option<String>,
        /// Maximum hits (default 20, engine-clamped).
        limit: Option<usize>,
    },
    /// Update the `[web]` configuration live. Every field is optional:
    /// absent fields keep their current value, so a frontend can PATCH one
    /// setting at a time. API keys are optional and follow the credentials
    /// store discipline (persisted to `credentials.toml`, never to
    /// `config.toml`); an empty-string key **clears** it. The harness
    /// validates, persists, hot-applies the new config to the web tools, and
    /// replies with [`AgentResponse::WebSearchConfigUpdated`] carrying the
    /// effective post-update view (key presence only).
    UpdateWebSearchConfig(Box<WebConfigUpdate>),
}

/// Optimistic, partial mutation of the singleton web configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebConfigUpdate {
    /// Required compare-and-swap precondition. Callers must query the current
    /// view before mutating it; stale writers are rejected rather than merged.
    pub expected_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::WebSearchProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader: Option<crate::WebReaderProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searxng_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<WebCredentialUpdate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCredentialUpdate {
    pub axis: crate::WebProviderAxis,
    pub provider_id: String,
    /// Empty clears the stored value. Secrets are never echoed in responses.
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfigView {
    pub revision: u64,
    pub provider: crate::WebSearchProvider,
    pub reader: crate::WebReaderProvider,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searxng_url: Option<String>,
    pub search_credential: crate::WebCredentialStatus,
    pub reader_credential: crate::WebCredentialStatus,
    pub capabilities: Vec<crate::WebProviderCapability>,
}

/// Stable wire-name aliases retained across the internal `[web]` schema migration.
pub type WebSearchConfigUpdate = WebConfigUpdate;
pub type WebSearchConfigView = WebConfigView;

/// Controls how many queued messages are injected when the agent reaches a queue drain point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QueueMode {
    /// Drain and inject only the oldest queued message, leaving the rest queued for later drain points.
    #[default]
    OneAtATime,
    /// Drain and inject every queued message at that point.
    All,
}

/// A queued user message waiting to be admitted as steering or follow-up.
///
/// `text` is the provider-facing payload. `display_text` preserves compact
/// attachment chips for the transcript when the provider-facing form expanded
/// a large paste. The stable id is generated by the submitting frontend and
/// makes admission/cancellation races deterministic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImagePart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at_ms: Option<u64>,
}

/// Which piece of local work a warming session is currently performing.
///
/// Display-oriented but closed: a frontend renders one line per stage, and an
/// unknown stage must not be invented by a peer. The set is the assembly work
/// that is allowed to delay `SessionPhase::Ready` (ADR-0133); network work is
/// deliberately absent because it never gates readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrepareStage {
    /// Opening or resuming the durable thread store.
    Thread,
    /// Discovering local skill sources.
    Skills,
    /// Running ThreadStart hooks (bounded).
    Hooks,
    /// Materialising the provider/model route (no credential I/O).
    Provider,
}
