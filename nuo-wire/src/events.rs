//! Wire types for the harness ↔ driver protocol: requests ([`AgentRequest`]),
//! responses ([`AgentResponse`]), live agent events ([`AgentEvent`]), and the
//! small data records they carry.

use crate::{Availability, ImagePart, Message, ToolOutput, ToolStream, TrustDomain};
use serde::{Deserialize, Serialize};

// Relocated to the tool leaf (`nuo_tool::events`) per ADR-0008 §3.
pub use nuo_tool::events::{
    AgentNotice, NoticeKind, NoticeSeverity, NoticeSource, NoticeSurface, PermissionRequest,
    StdinRequest, SubagentEvent, UserQuestion, UserQuestionOption, UserQuestionRequest,
};

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
    /// session is explicit so a frontend can keep composing in a side view
    /// (or switch views) without accidentally steering the wrong agent. The
    /// message is admitted atomically at the next safe turn boundary.
    Steer {
        session_id: String,
        #[serde(alias = "input")]
        message: QueuedMessage,
    },
    /// Cancel a not-yet-admitted [`AgentRequest::Steer`]. The agent linearizes
    /// cancellation against boundary admission: exactly one of
    /// `SteerAdmitted` or `SteerCancelled` is emitted for the id.
    CancelSteer {
        session_id: String,
        input_id: String,
    },
    /// Queue a follow-up message into an explicit live session. The
    /// **driver owns the queue** (ADR-0197 M4): when the target round is
    /// idle the message starts immediately ([`RoundEvent::FollowUpStarted`]);
    /// when it is running the driver enqueues it
    /// ([`RoundEvent::FollowUpQueued`]) and ships it automatically at the
    /// round boundary. The frontend sends one verb and renders the state
    /// the backend reports — it never decides when a follow-up ships.
    FollowUp {
        session_id: String,
        #[serde(alias = "input")]
        message: QueuedMessage,
    },
    /// Remove one queued follow-up (the queue modal's delete / destructive
    /// recall-to-composer). Idempotent: removing an unknown id is a no-op.
    QueueRemove {
        session_id: String,
        input_id: String,
    },
    /// Clear every queued follow-up for the session (the queue modal's clear).
    QueueClear {
        session_id: String,
    },
    /// Reorder one queued follow-up within its session's queue by `delta`
    /// positions (the queue modal's `K`/`J`). Clamped at the session's
    /// slice boundaries; unknown ids are a no-op.
    QueueReorder {
        session_id: String,
        input_id: String,
        delta: i32,
    },
    /// Pause or resume automatic shipping of the session's follow-up queue
    /// (`Ctrl+P` / the queue modal's block control). Enqueueing still works
    /// while paused; only the round-boundary auto-ship is gated.
    QueuePaused {
        session_id: String,
        paused: bool,
    },
    SlashCommand(String),
    /// Trust project-authored asset domains for the active workspace.
    /// Direct control-plane admission action — does not emit `/trust` into the
    /// transcript or record command history.
    TrustWorkspace {
        #[serde(default)]
        domains: Vec<TrustDomain>,
    },
    /// Ask the daemon to complete the composer input at `cursor`. Cursor and
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
    /// every sibling subagent untouched (ADR-0205 scene scope). The child's round
    /// loop observes the cancellation at its next safe boundary, returns its
    /// partial transcript, and the parent records it as an interrupted (not
    /// failed) result — identical to the drain the primary interrupt triggers when
    /// the parent turn is stopped. An unknown or already-finished `call_id`
    /// degrades to a no-op.
    InterruptSubagent {
        call_id: String,
    },
    /// The client declares this session over (ADR-0112). Sent on the paths
    /// where the operator's intent is "I am done with this session", not
    /// "I am detaching": the TUI's `/exit` and double-Ctrl+C quit, a
    /// headless run's terminal round, and the Web app's end-session
    /// action. The server intercepts it at the attach connection (it never
    /// reaches the driver queue) and tears the hosted session down through
    /// the same path as `ControlRequest::KillSession`: cancel the driver,
    /// fire SessionEnd hooks, drop it from the registry, and publish
    /// `SessionRemoved` so every dashboard drops the row. Disk history is
    /// kept — ending a session is not deleting it.
    EndSession,
    PermissionReply {
        request_id: String,
        decision: PermissionDecision,
        /// Full-duplex (ADR-0029): when the reply targets a permission
        /// request surfaced by a *subagent* (carried up as a
        /// [`RoundEvent::SubagentStep`] / [`SubagentEvent::PermissionRequest`]),
        /// this is the parent tool-call id the request was nested under. The
        /// harness looks up the live child's `crate::SubagentHandle` in the
        /// task registry by this id and resolves its parked oneshot directly.
        /// `None` means the request came from the top-level (or `/btw` side)
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
    /// identity (ADR-0201) and must be unique; the daemon rejects a duplicate
    /// with a suggested alternative instead of silently disambiguating.
    /// `provider` must name a registered model provider. `protocol`, `base_url`,
    /// and `user_agent` are optional overrides of the provider's defaults;
    /// `models` is an optional initial inclusion set, honored only for models
    /// Register or update a declarative model provider surface in `model_providers.toml` (ADR-0258).
    RegisterProvider {
        id: String,
        #[serde(default)]
        label: Option<String>,
        root_url: String,
        #[serde(default)]
        protocol: Option<crate::WireProtocol>,
        #[serde(default)]
        client_profile: Option<crate::ClientPreset>,
        #[serde(default)]
        user_agent: Option<String>,
        #[serde(default)]
        catalog_format: Option<String>,
        #[serde(default)]
        dialect: Option<String>,
    },
    /// Add a new connection instance to `connections.toml` (ADR-0201, ADR-0258).
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
        /// Client identity (impersonation/headers). Defaults to Native when unset.
        #[serde(default)]
        client_identity: Option<crate::ClientIdentity>,
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
    /// key, client identity) without touching its model scope (ADR-0258). Keyed by `name`;
    /// renaming is the separate atomic request [`Self::RenameConnection`].
    EditConnection {
        name: String,
        provider: String,
        api_key: crate::SecretString,
        #[serde(default)]
        client_identity: Option<crate::ClientIdentity>,
    },
    /// Rename a connection. The daemon rewrites every hard join key
    /// (`credentials.toml`, `auth.toml`, `default_connection`) in one
    /// transaction; historical records keep the old name (ADR-0201).
    RenameConnection {
        from: String,
        to: String,
    },
    /// Remove a model (channel) from a user-defined provider, persist, and push a
    /// fresh picker snapshot. The last remaining model is kept (a provider must
    /// serve at least one model).
    /// Include or declare a model within a scope (preset or connection) with optional capability facts (ADR-0199).
    IncludeModel {
        scope: crate::model::ModelTargetScope,
        model: crate::model::DeclaredModel,
    },
    /// Exclude/hide a model from the resolved set within a scope (ADR-0199).
    ExcludeModel {
        scope: crate::model::ModelTargetScope,
        model_id: String,
    },
    /// Clear explicit inclusion or exclusion rules for a model within a scope (ADR-0199).
    ClearModelRule {
        scope: crate::model::ModelTargetScope,
        model_id: String,
    },
    /// Set capability overrides for a specific model within a scope (ADR-0199).
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
        /// Capability overrides (ADR-0149 layer 1): `None` keeps the stored
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
        /// Capability overrides (ADR-0149 layer 1) — same semantics as
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
    /// Delete a session (active or archived) by id or short id prefix.
    DeleteSession {
        id: String,
    },
    /// Set (or clear) a session's display title — the manual title the
    /// monitor row and session pickers show (ADR-0022's AI title fills it
    /// only while no manual title exists). `title: None` clears the manual
    /// title back to the AI/first-prompt fallback. The harness republishes
    /// the monitor row so every client sees the rename.
    RenameSession {
        id: String,
        title: Option<String>,
    },
    /// Request full detail for one session (the `i` session-info sub-view).
    /// The harness replies with [`AgentResponse::SessionDetail`].
    QuerySessionDetail {
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
    /// Request the current sessions-picker rows without changing frontend
    /// navigation. The reply is [`AgentResponse::SessionsOverview`].
    QuerySessionsOverview,
    /// Request the current session DAG without changing frontend navigation.
    /// The reply is [`AgentResponse::SessionTreeSnapshot`].
    QuerySessionTree,
    /// Request the token-source report (per-round / per-turn request usage,
    /// reported vs. estimated) for one session. The harness replies with
    /// [`AgentResponse::TokenUsageReport`] carrying a snapshot of its
    /// server-side ledger. Attached frontends have no local ledger, so the
    /// context-usage modal (click on the hint-bar meter) issues this on
    /// demand — mirroring [`AgentRequest::QuerySessionDetail`].
    QueryTokenUsage {
        session_id: String,
    },
    /// Request the cross-session usage-statistics report (ADR-0122): daily
    /// token totals, per-model breakdown, and the recent terminal-request
    /// event log, aggregated over the durable day-partitioned store that
    /// survives session cleanup. The harness replies with
    /// [`AgentResponse::UsageStatsReport`]. Sent by the TUI when the
    /// `/usage` overlay opens.
    QueryUsageStats {
        /// How many recent events to include in the event-log tail.
        event_cap: usize,
    },
    /// Request a fresh session-context snapshot (model / tools / permissions /
    /// skills / mcp). The harness replies with [`AgentResponse::SessionContext`].
    /// Sent by the TUI when a manager modal opens.
    QuerySessionContext,
    /// Revoke a single cached "always allow" permission rule. The harness
    /// removes it from the in-memory allowlist and replies with an updated
    /// [`AgentResponse::SessionContext`] so the modal reflects the change.
    RevokePermission {
        tool: String,
        scope: String,
    },
    /// Clear every cached "always allow" permission rule for this process.
    /// The harness drops the whole in-memory allowlist and replies with an
    /// updated [`AgentResponse::SessionContext`] so the permissions manager
    /// modal reflects the now-empty list.
    ClearAllPermissions,
    /// Enable or disable a tool for the current session. Disabled tools are
    /// hidden from the model (their schemas are not sent) and rejected if the
    /// model still tries to call them. The harness replies with an updated
    /// [`AgentResponse::SessionContext`].
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
    /// [`AgentResponse::SessionContext`].
    ToggleMcpServer {
        name: String,
        enabled: bool,
    },
    /// Reset and re-establish one MCP server's connection, re-discovering its
    /// tools (the per-server analogue of the periodic catalog refresh). Used by
    /// the `/mcp` modal's `r` action to recover a crashed/failed server on
    /// demand. The harness replies with an updated
    /// [`AgentResponse::SessionContext`].
    ReconnectMcpServer {
        name: String,
    },
    /// Detach from the `/btw` aside view and return to the primary transcript
    /// (ADR-0103). The aside **keeps running**: its in-flight round is left
    /// alone and its session stays registered so it can be re-entered via
    /// [`AgentRequest::FocusSide`] or the asides list. The harness emits
    /// [`AgentResponse::SideViewClosed`]. Sent by the TUI when the user
    /// presses `Ctrl+C` inside an aside view. A pristine aside (no round ever
    /// started) is discarded outright instead of lingering in the list.
    ExitSideView,
    /// Jump the view into a live `/btw` aside (ADR-0103): open it if it was
    /// closed, make the composer target it, and emit
    /// [`AgentResponse::SideViewOpened`] with the aside's full transcript so
    /// the frontend rebuilds its side buffer (the inherited parent context
    /// included). Sent when the user re-enters an aside from the asides list.
    FocusSide {
        side_id: String,
    },
    /// Interrupt the in-flight round of one `/btw` aside (ADR-0103). Esc
    /// inside an aside view resolves to this — interrupting an aside never
    /// closes it. The aside's round unwinds with its own `[Interrupted]`
    /// cleanup, mirroring [`AgentRequest::Interrupt`] for the primary.
    InterruptSide {
        side_id: String,
    },
    /// Close one `/btw` aside for real: cancel any in-flight round, drop the
    /// registry entry, **and delete its session files** (ADR-0103 §4). The
    /// aside disappears from the asides list and `/sessions`. If the aside
    /// was the focused view, the harness also emits
    /// [`AgentResponse::SideViewClosed`]. Sent by the asides modal's `D`
    /// action.
    CloseSide {
        side_id: String,
    },
    /// Request the `/btw` asides list (ADR-0103). The harness replies with
    /// [`AgentResponse::BtwList`]. Sent when the asides modal opens or is
    /// refreshed, and by the event loop to keep the header's aside count
    /// truthful.
    QueryBtwList,
    /// Update the transcript layout preference.
    /// The harness writes the new value to `config.toml`'s `[tui]
    /// transcript_layout` and replies with [`AgentResponse::TuiLayoutUpdated`]
    /// carrying the persisted string so the renderer updates its state. The value
    /// is a raw config string (e.g. "turn_band"); interpretation into a [`crate`] layout
    /// `Strategy` happens in the renderer, keeping the core free of render types.
    UpdateTuiLayout(String),
    /// Request the persisted prompt input history. The daemon is the source
    /// of truth for the shared SQLite store; the frontend never opens the
    /// database directly (ADR-0197). Replies with
    /// [`AgentResponse::InputHistory`].
    QueryInputHistory,
    /// Record (lock + merge) prompt input history entries on the daemon —
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
        custom: crate::ColorSchemeConfig,
    },
    /// Query the effective singleton `[web]` configuration, readiness, and
    /// provider capability catalog. Secrets never cross the wire. Replied with
    /// [`AgentResponse::WebSearchConfigSnapshot`].
    QueryWebSearchConfig,
    /// Cross-project session history search (ADR-0208): BM25 FTS over all
    /// persisted transcript entries in the shared `nuo.db`, optionally
    /// narrowed to one workspace root. The daemon is the source of truth; the
    /// frontend never opens the database directly (ADR-0197). Replies with
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentResponse {
    /// A per-round event tagged with the session it belongs to (ADR-0017). The
    /// TUI keys its transcript buffers by `session_id` and routes `event` to
    /// the matching one, so a primary round and a live `/btw` side round can
    /// stream concurrently over the single harness↔TUI channel without
    /// clobbering each other's transcript.
    ///
    /// Global (non-session-scoped) responses — command replies, modal
    /// snapshots, provider switches — stay as dedicated top-level variants so
    /// they are handled once, regardless of which view is focused.
    Round {
        session_id: String,
        event: RoundEvent,
    },
    /// Backend-owned completion result for [`AgentRequest::CompleteComposer`].
    /// The echoed text/cursor make stale-response rejection explicit even for
    /// clients that restart their local request counter after reconnecting.
    #[serde(alias = "InputCompletions")]
    ComposerCompletions {
        request_id: u64,
        #[serde(alias = "input")]
        text: String,
        cursor: usize,
        items: Vec<crate::ComposerCompletion>,
    },
    /// Coarse status of the primary session, surfaced to a side view's banner
    /// while the user is inside a `/btw`. Emitted by the session registry's
    /// parent-status watcher; the primary round is deliberately left running, so
    /// this is how the user learns the main session hit an approval/input wall.
    ParentStatus(ParentStatus),
    /// The user entered a `/btw` aside view (ADR-0017, extended by ADR-0103).
    /// The TUI records `side_id` as the routing key for per-round events and
    /// rebuilds its side transcript buffer from `messages` + `commands` — the
    /// aside's full persisted transcript at open time, inherited parent
    /// context included — so the viewed pixels match the model's actual
    /// context window. Emitted by the harness on `/btw` (new aside) and on
    /// [`AgentRequest::FocusSide`] (re-entry).
    SideViewOpened {
        side_id: String,
        primary_id: String,
        /// The aside's persisted transcript at open time. One-shot back-fill:
        /// after this, per-round `Round` events stream into the same buffer.
        #[serde(default)]
        messages: Vec<Message>,
        /// Command-ledger rows for the aside (ADR-0091), same as
        /// [`AgentResponse::ConversationReplaced`].
        #[serde(default)]
        commands: Vec<crate::command::CommandRecord>,
        /// Round-interrupt records for the aside (C11), same as
        /// [`AgentResponse::ConversationReplaced`].
        #[serde(default)]
        round_interrupts: Vec<RoundInterrupt>,
        /// Retry-resolution records for the aside, same as
        /// [`AgentResponse::ConversationReplaced`].
        #[serde(default)]
        retry_resolutions: Vec<crate::RetryResolution>,
    },
    /// The user left the `/btw` aside view (ADR-0103). The TUI returns to the
    /// primary transcript. Detach is non-destructive by default: the aside
    /// keeps running unless it was pristine (no round ever started), in which
    /// case it was discarded. Emitted by the harness in reply to
    /// [`AgentRequest::ExitSideView`] / [`AgentRequest::CloseSide`].
    SideViewClosed,
    /// The `/btw` asides list (ADR-0103), newest first. Drives the asides
    /// modal (`F5` / `/btw list`) and the main view's header aside count.
    /// Pushed on every list mutation (open, detach-with-discard, close) and
    /// in reply to [`AgentRequest::QueryBtwList`].
    BtwList(Vec<BtwAsideSummary>),
    /// The persisted prompt input history, in stored order — reply to
    /// [`AgentRequest::QueryInputHistory`].
    InputHistory(Vec<crate::HistoryEntry>),
    /// Cross-project session search hits — reply to
    /// [`AgentRequest::SearchHistory`]. Each hit anchors a transcript snippet
    /// to its session so a dashboard/picker can jump straight to it.
    HistorySearch(Vec<crate::HistorySearchHit>),
    /// The stored capability overrides for one provider/model route — reply
    /// to [`AgentRequest::QueryRouteSettings`].
    RouteSettings {
        provider_id: String,
        model: String,
        overrides: Option<crate::model::CapabilityOverrides>,
    },
    PermissionsCleared,
    /// Lowercase provider name → whether a usable API key is configured.
    ProviderKeys(Vec<(String, bool)>),
    /// Full provider-picker state (default id + one row per provider) for the
    /// provider picker. Supersedes `ProviderKeys` for the picker's needs;
    /// `ProviderKeys` is retained for the header key-readiness summary.
    ProviderPicker(ProviderPickerSnapshot),
    /// Copy text to the client's system clipboard (used by `/export` in daemon mode).
    CopyToClipboard {
        text: String,
    },
    /// Blank the visible transcript and zero the round counter: the harness
    /// switched to a brand-new empty session (`/new`, `/session new`). The
    /// previous session is untouched on disk — nothing was deleted.
    /// `session_id` is the freshly minted id, mirroring
    /// [`AgentResponse::ConversationReplaced`]'s post-switch id: attached
    /// frontends track it so session-scoped state (the inline ↑/↓ prompt
    /// recall, on-demand queries) follows the switch instead of lingering on
    /// the retired session.
    ConversationCleared {
        session_id: String,
    },
    /// Replace the visible transcript (dialogue messages) AND the command
    /// ledger (ADR-0091) with another session's state, after `/session open`,
    /// `/resume`, `/session resume`. The frontend rebuilds the whole document
    /// from these two sources: pure dialogue from `messages`, command rows
    /// from `commands`. `session_id` identifies the session that produced the
    /// replacement (the harness emits this only as a session switch, so it is
    /// the post-switch id); attached frontends track it to keep on-demand
    /// queries (e.g. [`AgentRequest::QueryTokenUsage`]) session-correct.
    ConversationReplaced {
        session_id: String,
        messages: Vec<Message>,
        #[serde(default)]
        commands: Vec<crate::command::CommandRecord>,
        /// Round-interrupt records (C11): re-projected into the transcript at
        /// their timestamp seams so the resumed session shows which rounds
        /// were stopped, why, and when.
        #[serde(default)]
        round_interrupts: Vec<RoundInterrupt>,
        /// Retry-resolution records: re-projected into the transcript at
        /// their timestamp seams so the resumed session shows which rounds
        /// recovered from transient provider faults.
        #[serde(default)]
        retry_resolutions: Vec<crate::RetryResolution>,
    },
    /// Replace the sessions picker contents. Data responses never navigate;
    /// slash-command presentation is signalled separately.
    SessionsOverview(Vec<SessionOverview>),
    /// Presentation signal for bare `/sessions` / `/session list`.
    OpenSessionsPanel,
    /// Replace the session-tree view's data without changing navigation. The
    /// session id lets a frontend reject a reply that raced a session switch.
    SessionTreeSnapshot {
        session_id: String,
        tree: crate::SessionTree,
    },
    /// Presentation signal for `/tree`.
    OpenTreePanel,
    /// Open the session dashboard (`/dashboard`, formerly `/host`; ADR-0096).
    /// The TUI renders the monitor stream it maintains independently; this is
    /// only the open signal, carrying no data.
    OpenHostPanel,
    /// Reply to [`AgentRequest::QuerySessionDetail`]: full detail for one
    /// session (complete last prompt, title, timestamps). Consumed by the
    /// session-info sub-view.
    SessionDetail(SessionDetail),
    /// Reply to [`AgentRequest::QueryConnectionDetail`]: full detail and usage
    /// for one connection (identity, endpoint, auth, models, usage/balance).
    ConnectionDetail(crate::ConnectionDetail),
    /// Reply to [`AgentRequest::QueryTokenUsage`]: the daemon-side token-source
    /// report for one session (per-round request usage, reported vs.
    /// estimated). Attached frontends hold no local ledger, so the
    /// context-usage modal renders this snapshot; the session id lets the
    /// frontend discard a reply that raced a session switch.
    TokenUsageReport {
        session_id: String,
        report: crate::token_ledger::TokenSourceReport,
    },
    /// Reply to [`AgentRequest::QueryUsageStats`]: the cross-session usage
    /// report (per-day / per-model totals + recent event log) aggregated from
    /// the durable usage store. Unlike [`Self::TokenUsageReport`] this data is
    /// session-independent — it outlives session deletion by design
    /// (ADR-0122) — so no session id accompanies it.
    UsageStatsReport {
        report: crate::usage_stats::UsageStatsReport,
    },
    Error(String),
    Exit,
    ProviderSwitched {
        provider: String,
        model: String,
    },
    /// Progress of an OAuth connect/authorize flow (xAI SuperGrok).
    ConnectStatus(ConnectStatus),
    /// Full session-context snapshot (model + tools + permissions + skills +
    /// mcp) for the session modal. Sent in reply to [`AgentRequest::QuerySessionContext`]
    /// and re-sent after any mutation handled by the harness
    /// ([`AgentRequest::RevokePermission`] / [`AgentRequest::ToggleTool`]).
    SessionContext(SessionContextSnapshot),
    /// The transcript layout preference was updated (from the `/config` modal
    /// via [`AgentRequest::UpdateTuiLayout`]). Carries the persisted config
    /// string so the modal re-renders from the authoritative state — the TOML
    /// write is the source of truth, not the TUI's optimistic local edit.
    TuiLayoutUpdated(String),
    /// The TUI color scheme and custom palette were persisted successfully.
    TuiColorSchemeUpdated {
        name: String,
        custom: crate::ColorSchemeConfig,
    },
    /// Authoritative singleton web configuration; never contains secret text.
    WebSearchConfigSnapshot(WebConfigView),
    /// Validated, persisted and hot-applied authoritative state.
    WebSearchConfigUpdated(WebConfigView),
}


/// Session-scoped events emitted while a user round runs, carried under an
/// [`AgentResponse::Round`] envelope (ADR-0017). Splitting these off
/// `AgentResponse` makes "which session does this belong to" a first-class
/// question: every event — whether from the primary or a `/btw` side — arrives
/// tagged with its `session_id`, and global/command responses stay top-level.
/// Origin of the current model-context token count shown by frontends.
///
/// This describes the AI-visible request context, never the durable session or
/// rendered transcript size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextTokenSource {
    /// Count reported by the provider for the completed request, plus that
    /// request's completion (which becomes history for the next request).
    Api,
    /// Local estimate of the provider-visible projection of `model_window`.
    Projection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextTokenSnapshot {
    pub tokens: usize,
    pub source: ContextTokenSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overhead_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_tokens: Option<usize>,
    /// Request-local temporary-context tokens (`E_n`), reported separately from
    /// durable input so diagnostics do not conflate the two (ADR-0213 §8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporary_context_tokens: Option<usize>,
}

impl ContextTokenSnapshot {
    pub fn new(tokens: usize, source: ContextTokenSource) -> Self {
        Self {
            tokens,
            source,
            overhead_tokens: None,
            history_tokens: None,
            temporary_context_tokens: None,
        }
    }

    pub fn from_estimate(
        estimate: crate::RequestTokenEstimate,
        source: ContextTokenSource,
    ) -> Self {
        Self {
            tokens: estimate.total_tokens,
            source,
            overhead_tokens: Some(estimate.overhead_tokens),
            history_tokens: Some(estimate.history_tokens),
            temporary_context_tokens: Some(estimate.temporary_context_tokens),
        }
    }
}

/// A durable record of one round being stopped before its natural terminal
/// path (`RoundEvent::RoundCompleted`). Written by the harness whenever a
/// round unwinds through an interrupt — user-requested, superseded by newer
/// input, or killed with its host process — so a resumed session can show
/// *that and why* the round stopped, at the moment it stopped.
///
/// This is a **projection record, not a conversation message**: it never
/// enters `model_window` / `archived_transcript`, never reaches the model,
/// and never costs context tokens (the deliberate decision documented in
/// `docs/explanation/interrupt-semantics.md` — omission stays the
/// model-facing signal). It rides in the session store beside the command
/// ledger and, like the ledger, is re-projected into the transcript on
/// resume by timestamp seam.
///
/// `at_ms` lives on the payload (not the event-log envelope) because log
/// compaction rewrites the `.jsonl` and drops every envelope timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundInterrupt {
    /// The interrupt's cause, as observed at the stop site. Maps 1:1 to the
    /// user-facing vocabulary; see [`RoundInterruptReason`].
    pub reason: RoundInterruptReason,
    /// Unix-epoch milliseconds at which the stop was recorded.
    pub at_ms: u64,
    /// The 1-based round that was stopped, when known. `None` when no
    /// agent-side counter was available at the stop site — e.g. the
    /// phase-1 unsend (the round was rewound and its counter restored
    /// before the record was written) and records synthesized for a round
    /// the process abandoned at exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    /// Optional error payload or stop detail (e.g. fatal provider error message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl RoundInterrupt {
    /// The user-facing label for this record, e.g. `"Esc Esc"`,
    /// `"new message"`, `"process exited"`, `"error"`. One vocabulary shared by the
    /// TUI, the Web app, and headless output.
    pub fn label(&self) -> &'static str {
        self.reason.label()
    }
}

/// Why a round stopped before completing. The closed classifier for
/// [`RoundInterrupt::reason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundInterruptReason {
    /// The user explicitly stopped the round: double-Esc in the primary
    /// view, Esc Esc inside a `/btw` aside, or the equivalent control-plane
    /// `Interrupt` request. Interrupt semantics documentation calls this
    /// the plain "interrupt" path — the round unwinds and emits its own
    /// cleanup because the generation is not bumped.
    User,
    /// A newer round replaced this one: the user sent a new message (or a
    /// `!shell` command / scheduled follow-up) while this round was still
    /// live, or switched sessions (`/resume`, `/session open|fork|new`).
    /// The stale round's own cleanup is generation-suppressed, which before
    /// this record existed left no trace at all.
    Superseded,
    /// The host process terminated with the round still in flight — a
    /// daemon stop (signal, control verb, or kill), a TUI signal exit, or a
    /// crash. Inferred on load when a recorded interrupt's round never
    /// completed and no terminal interrupt was recorded for it.
    Terminated,
    /// The round ended in a fatal / terminal error (e.g. rate limit / network
    /// failure / provider quota exhaustion).
    Error,
}

impl RoundInterruptReason {
    /// The single user-facing word/phrase for this reason. Kept short so it
    /// fits the transcript's meta strip (`Interrupted · <label> · HH:MM`).
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "Esc Esc",
            Self::Superseded => "new message",
            Self::Terminated => "process exited",
            Self::Error => "error",
        }
    }
}

/// The durable twin of a round that *recovered* from transient provider
/// failures via the harness retry loop — the success-side mirror of
/// [`RoundInterrupt`]. Where an interrupt records "this round stopped", a
/// resolution records "this round hit N retryable provider faults and then
/// completed". Written once, when the round reaches its natural terminal
/// path after at least one retry; re-projected into the transcript on
/// resume so the recovery is auditable after the fact, exactly as an
/// interrupt is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryResolution {
    /// How many retry attempts the round consumed (1 = one fault retried
    /// once, …). Equal to the number of [`Self::faults`].
    pub attempts: u32,
    /// The per-attempt fault summaries, in order, one line each (the same
    /// public message the live retry notice carried).
    pub faults: Vec<String>,
    /// Unix-epoch milliseconds at which the final (successful) retry
    /// completed — i.e. when the round stopped being in retry backoff.
    pub at_ms: u64,
    /// The 1-based round that recovered, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
}

impl RetryResolution {
    /// The collapsed transcript line, e.g.
    /// `"Recovered after 2 provider retries — retried once"`. Kept short so
    /// it fits the transcript's meta strip; the full fault list rides in
    /// [`Self::faults`] and is rendered as the expandable detail.
    pub fn summary_line(&self) -> String {
        let mut line = format!(
            "Recovered after {} provider {}",
            self.attempts,
            if self.attempts == 1 {
                "retry"
            } else {
                "retries"
            }
        );
        if let Some(round) = self.round {
            line.push_str(&format!(" (round {round})"));
        }
        line
    }
}

/// The durable `/retry` resume point: everything a later round needs to
/// *continue* a stopped round as itself — same round number, contiguous turn
/// ordinals — rather than minting a fresh round.
///
/// `/retry`'s whole contract is "finish the round that did not finish": the
/// round counter must not advance, the turn sequence must stay unbroken, and
/// the model-visible history must be exactly the committed checkpoint the
/// stopped round left behind. A `RetryPoint` is the harness's capture of that
/// checkpoint at the moment the round stopped (terminal error after retries
/// were exhausted, or an interrupt that left committed content).
///
/// This is **projection state, not a conversation message**: like
/// [`RoundInterrupt`] it never enters `model_window`, never reaches the
/// model, and costs zero context tokens. It rides in the session store until
/// the parked round completes (then it is cleared) or the session moves on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPoint {
    /// 1-based round the point refers to. `/retry` may only fire while this
    /// still equals the session's current round counter — any newer round
    /// (or a `/new`) retires the point.
    pub round: u64,
    /// How many complete ReAct turns the stopped round committed. The resume
    /// continues numbering turns from here (`round.turn_index` starts at this
    /// value) so the transcript's `round N · turn M` sequence stays unbroken.
    pub turns_committed: usize,
    /// `model_window` length at the stopped round's last durable checkpoint.
    /// The resume seeds its working history from the window and re-checkpoints
    /// from this watermark so no partially streamed content leaks back in.
    pub history_watermark: usize,
    /// Human-decision pause time (permission prompts / `ask_user`) the
    /// stopped round had already accumulated, in milliseconds. Seeded back
    /// into the resume so a later tokens/sec stays honest across the stop.
    pub paused_ms: u64,
    /// Unix-epoch milliseconds at which the point was recorded. Lets a
    /// resumed session show *when* the round stalled.
    pub at_ms: u64,
}

/// A compact per-round accounting handed to frontends when a user round
/// completes naturally. The "active" generation time is
/// `duration_ms.saturating_sub(paused_ms)`; dividing `output_tokens` by it
/// yields an honest tokens/sec that reflects the server's real throughput,
/// unaffected by how long the user deliberated on a permission prompt or
/// `ask_user` question.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundSummary {
    /// 1-based user-round index, mirroring the transcript's round counter.
    pub round: u64,
    /// Total output (completion) tokens the model generated this round.
    pub output_tokens: u64,
    /// Full wall-clock duration of the round, including any human-decision
    /// pause time (`paused_ms`) and all tool execution.
    pub duration_ms: u64,
    /// Time within `duration_ms` the round spent parked on a human decision
    /// (a permission request or an `ask_user`). `0` when nothing blocked.
    pub paused_ms: u64,
    /// Time the model actually spent *generating* — summed across every
    /// completed provider request in the round, measured from request start
    /// to a validated response and therefore **excluding** tool execution,
    /// hooks, and human-decision pauses. This is the honest denominator for
    /// tokens/sec: `tps = output_tokens / generation_ms`. Falls back to the
    /// round `active_ms()` only when no request completed measurably.
    pub generation_ms: u64,
    /// The durable session revision this completion follows (ADR-0236 D5): the
    /// authoritative commit was acknowledged at this revision before the
    /// completion was published. A client can order the completion against
    /// later session state and discard a stale replay instead of regressing a
    /// newer view. `0` for a completion emitted before this field existed.
    #[serde(default)]
    pub session_revision: u64,
}

impl RoundSummary {
    /// Net-active generation time: the wall-clock minus the human-decision
    /// pause. Saturates at 0 so a round that somehow paused longer than it ran
    /// still yields a finite (large) TPS rather than dividing by a negative.
    pub fn active_ms(&self) -> u64 {
        self.duration_ms.saturating_sub(self.paused_ms)
    }

    /// The time `tps()` divides by: `generation_ms` when at least one request
    /// completed measurably, otherwise the round `active_ms()` fallback.
    /// Exposed so a UI can render *exactly* the denominator the throughput
    /// figure was computed from, instead of a coincidentally-larger span.
    pub fn denominator_ms(&self) -> u64 {
        if self.generation_ms > 0 {
            self.generation_ms
        } else {
            self.active_ms()
        }
    }

    /// Output tokens per second of *generation* time — the time the model
    /// actually spent streaming a response, excluding tool execution and
    /// human-decision pauses. Falls back to round `active_ms()` (wall-clock
    /// minus human pause) when no provider request completed measurably, and
    /// returns `0.0` when there is no usable denominator so the UI renders `–`
    /// rather than `inf`.
    pub fn tps(&self) -> f64 {
        let denominator_ms = self.denominator_ms();
        if denominator_ms == 0 {
            0.0
        } else {
            (self.output_tokens as f64) * 1000.0 / (denominator_ms as f64)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoundEvent {
    Notice(AgentNotice),
    /// Current AI-visible context size for this session. Frontends must use
    /// this instead of estimating from the persisted/rendered transcript.
    ContextTokens(ContextTokenSnapshot),
    /// A steering message crossed the agent's safe turn boundary and is now part
    /// of the live conversation (the agent persists that boundary before the
    /// provider observes it). Frontends append it to the transcript at this
    /// event, never when it was merely queued.
    SteerAdmitted(QueuedMessage),
    /// The addressed round stopped accepting steering messages before this id
    /// could be admitted. A frontend may safely retain it as a paused follow-up item.
    SteerUnavailable {
        input_id: String,
    },
    /// The driver accepted a follow-up into its queue while the target round
    /// was running (ADR-0197 M4). The item will ship automatically at the
    /// round boundary, unless the queue is paused; the authoritative queue
    /// snapshot rides [`RoundEvent::QueueUpdated`].
    FollowUpQueued {
        input_id: String,
    },
    /// The authoritative follow-up queue snapshot for a session, sent after
    /// every queue change (enqueue, ship, remove, clear, promote, pause).
    /// A full-replace diff: frontends rebuild their queue projection from
    /// `items` verbatim. Ships also emit [`RoundEvent::FollowUpStarted`] —
    /// the snapshot reflects the queue *after* the ship.
    QueueUpdated {
        items: Vec<QueuedMessage>,
        paused: bool,
    },
    /// A pending steering message was cancelled before admission.
    SteerCancelled {
        input_id: String,
    },
    /// Cancellation lost the race with admission (or the id was unknown).
    /// The subsequent admitted/unavailable event remains authoritative.
    SteerCancelFailed {
        input_id: String,
    },
    /// A follow-up outbox item was accepted by its exact live session and a
    /// fresh round was started. Like steering admission, this is the transcript
    /// commit point for frontends.
    FollowUpStarted(QueuedMessage),
    /// The user-driven round reached its natural, successful terminal path.
    /// Interruptions, blocked prompts, and errors deliberately emit no such
    /// event, so next-round outbox items pause instead of auto-running.
    /// Carries a small per-round summary so frontends can show an honest
    /// generation throughput (tokens/sec) that excludes the time the round
    /// spent parked on human decisions (permission prompts / ask_user).
    RoundCompleted(RoundSummary),
    /// A round stopped before its natural terminal path, with the reason and
    /// timestamp. Emitted exactly once per stopped round, after the round's
    /// own cleanup — including the generation-suppressed case (a superseded
    /// round), which previously left no visible trace. The durable twin of
    /// this event (`RoundInterrupt`) is persisted in the session store and
    /// re-projected into the transcript on resume; this live event never
    /// reaches the model context.
    ///
    /// Distinct from [`RoundEvent::UnsentInput`] (a Phase-1 interrupt is an
    /// *unsend*: the user message returned to the composer and nothing
    /// committed) and from [`RoundEvent::ToolCancelled`] (per-tool). This
    /// event covers the round as a whole on every interrupt phase.
    RoundInterrupted(RoundInterrupt),
    Text(String),
    /// A typed slash-command result (ADR-0091). Replaces the `Text` replies
    /// commands used to emit: the TUI renders it as a distinct command block
    /// (dimmed header + expandable result), never as assistant prose, and the
    /// same value is recorded in the session's command ledger for resume/
    /// export/audit.
    /// A typed slash-command result (ADR-0091). Replaces the `Text` replies
    /// commands used to emit: the TUI renders it as a distinct command block
    /// (dimmed invocation header + expandable result), never as assistant
    /// prose, and the same value is recorded in the session's command ledger
    /// for resume/export/audit.
    CommandResult {
        /// Command word without the leading slash (e.g. `"search"`), or
        /// `"shell"` for a `!command` passthrough.
        name: String,
        /// Raw argument remainder after the command word.
        args: String,
        result: crate::command::CommandResult,
    },
    /// Turn-level error (e.g. a provider failure mid-turn). Distinct from the
    /// global [`AgentResponse::Error`] only in that it belongs to a specific
    /// session's transcript and is therefore carried under the [`Round`]
    /// envelope.
    ///
    /// [`Round`]: AgentResponse::Round
    Error(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
        structured: ToolOutput,
        duration_ms: u64,
    },
    /// Incremental output streamed by a running tool (see [`ToolStream`]).
    ToolStream {
        id: String,
        stream: ToolStream,
    },
    ToolCancelled {
        id: String,
        name: String,
    },
    /// A tool call announced **before its arguments finished streaming**
    /// (ADR-0026). Mirrors [`AgentEvent::ToolCallStarted`]: `name` is known,
    /// `arguments` are still arriving, `index` keys the announced step so the
    /// later whole-argument [`RoundEvent::ToolCall`] collapses onto it.
    ToolCallStarted {
        index: usize,
        id: Option<String>,
        name: String,
    },
    /// Count-only progress for a still-streaming tool call's arguments
    /// (ADR-0026, `[INV-STREAM-TOOL-03]`): bytes received, never the bytes.
    ToolInputProgress {
        index: usize,
        id: Option<String>,
        bytes: usize,
    },
    PermissionRequest(PermissionRequest),
    UserQuestionRequest(UserQuestionRequest),
    /// Mirrors [`AgentEvent::StdinRequest`]: an interactive `bash` command
    /// needs operator stdin.
    #[serde(alias = "InputRequest")]
    StdinRequest(StdinRequest),
    /// A context projection (compaction or prune) was committed. Token
    /// samples of the active window around the projection (ADR-0120, ADR-0296).
    Compacted {
        archived_messages: usize,
        window_tokens_before: usize,
        window_tokens_after: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tracked_files: Vec<String>,
    },
    HarnessState(HarnessSnapshot),
    /// The task list changed (full-replace via `todo`, surgical update via
    /// `todo_update`). Mirrors [`AgentEvent::TodosUpdated`]. An empty list
    /// means "no active task list" and hides the sticky panel.
    TodosUpdated(crate::todos::TodoList),
    /// The unattended execution toggle changed. `true` = the agent runs in unattended
    /// mode (suppresses interactive human confirmations). Emitted by `/unattended` so
    /// the TUI can refresh its badge without waiting for the next harness snapshot.
    UnattendedChanged(bool),
    /// The workspace confinement toggle changed. `true` = confined to workspace,
    /// `false` = confinement disabled. Emitted by `/confinement` so the TUI can refresh its
    /// badge without waiting for the next harness snapshot.
    ConfinementChanged(bool),
    RetryScheduled {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        message: String,
    },
    /// The round that consumed one or more retries reached its natural
    /// terminal path — the recovery is now durable fact, not live state.
    /// Frontends must fold their transient retry entry (countdown UI) into
    /// a permanent transcript marker carrying this record, mirroring how
    /// [`RoundEvent::RoundInterrupted`] projects its durable twin. Emitted
    /// at most once per round, immediately before [`RoundEvent::RoundCompleted`].
    RetryResolved(crate::RetryResolution),
    Activity(String),
    /// A new ReAct turn started within the current user round. `turn` is the
    /// 0-indexed model-request index within the round (0 = the first request).
    /// Surfaced as structured data so the activity bar can render
    /// `round N · turn M · <status>` without parsing the turn back out of the
    /// `Activity` status string. Emitted just before the matching
    /// `Activity("waiting for model")`.
    TurnStarted {
        /// 1-indexed enclosing user round.
        round: u64,
        /// 0-indexed model-request position within `round`.
        turn: usize,
    },
    /// One concrete provider attempt completed with client-observed
    /// performance telemetry. This is pushed at the ReAct-turn boundary so
    /// hint bars update immediately; detailed history remains queryable from
    /// the request ledger.
    TurnPerformance(crate::TurnPerformanceSnapshot),
    StreamStart,
    StreamDelta(String),
    StreamReasoningDelta(String),
    StreamReasoningEnd(String),
    StreamEnd(String),
    StreamDiscard,
    /// The user interrupted the round before any model output reached the
    /// client (Phase 1: request in-flight, no content delta yet). The round's
    /// user message has been removed from the conversation context and
    /// session store, and the client should offer `prompt` (and any
    /// `images`) back for re-editing — the conversation is back to its
    /// pre-send state. Restoring into the composer is advisory: a client
    /// whose composer holds in-progress input should leave it alone and
    /// surface the prompt another way (history recall / a notice), since the
    /// unsend arrives asynchronously. The cancelled network request may
    /// still bill its input tokens on the provider side, but no assistant
    /// message, tool calls, or output tokens are produced or recorded.
    UnsentInput {
        prompt: String,
        images: Vec<crate::ImagePart>,
    },
    /// A subagent event to render nested inside the parent tool step. The
    /// variant carries the child's own [`SubagentEvent`] plus the parent
    /// tool-call id it was nested under (ADR-0183).
    SubagentStep {
        parent_call_id: String,
        event: SubagentEvent,
    },
    /// A background process or sub-subagent job started.
    BackgroundJobStarted(crate::job::BackgroundJobInfo),
    /// Incremental progress or output line from a background job.
    BackgroundJobProgress {
        job_id: crate::job::JobId,
        line: String,
    },
    /// A service task reported readiness (ADR-0190): the process is alive and
    /// its readiness condition (first output, grace, or port probe) is met.
    /// Wake-eligible.
    BackgroundJobReady {
        job_id: crate::job::JobId,
    },
    /// A background job completed.
    BackgroundJobCompleted(crate::job::BackgroundJobOutcome),
}

/// Coarse status of the primary session, reported to a `/btw` side view's
/// banner (ADR-0017). This is the codex `SideParentStatus` equivalent: the
/// whole reason the parent round is left running instead of cancelled is so the
/// user can see the main session hit an approval or input wall and jump back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParentStatus {
    Idle,
    Running,
    NeedsApproval,
    NeedsInput,
    Failed,
    Interrupted,
}

/// One row of the `/btw` asides list (ADR-0103): a live aside conversation
/// forked from the primary session. Rows are ordered newest-first (most
/// recently opened or re-entered first) by the harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtwAsideSummary {
    /// The aside's session id — the same key per-round `Round` events carry.
    pub id: String,
    /// Short label: the aside's first user prompt when it has one, else a
    /// neutral placeholder. Already truncated for one-line display.
    pub title: String,
    /// Whether the aside has an in-flight round right now.
    pub running: bool,
    /// Epoch seconds of the aside's last activity (creation or last write).
    pub updated_at: u64,
}

/// Coarse, display-level status of a session's round lifecycle, mirrored to
/// the TUI activity bar. This is a badge, not the protocol state: the round
/// lifecycle itself (`RoundLifecycle` in nuo-agent) is binary — no active
/// round, or an active round identified by a generation.
/// Awaiting-permission / awaiting-input are overlays derived from the
/// parked-request tables (see [`ParentStatus`]), not values here: they carry
/// no lifecycle meaning (interrupt behaves identically) and there is no
/// user-level pause/resume for them to describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoopStatus {
    Idle,
    Running,
}

impl LoopStatus {
    pub fn is_idle(self) -> bool {
        matches!(self, Self::Idle)
    }

    /// The wire string, also used directly by the TUI's activity bar.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
        }
    }
}

impl std::fmt::Display for LoopStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSnapshot {
    pub loop_status: LoopStatus,
    /// Monotonic session round counter. For a running snapshot this is the
    /// admitted active round; for an idle snapshot it is the most recently
    /// admitted round. Frontends must use this instead of counting visible
    /// transcript messages, which may have been compacted.
    #[serde(default)]
    pub round_counter: u64,
    /// Whether the agent runs in unattended execution mode
    /// (`--unattended` / `/unattended on`). The TUI mirrors this into an `UNATTENDED` badge.
    #[serde(default)]
    pub unattended: bool,
    /// Whether workspace filesystem confinement is enforced this session (default true).
    /// (`--no-confinement` / `/confinement off`). The TUI mirrors false into an `UNCONFINED` badge.
    #[serde(default = "default_confined")]
    pub confined: bool,
    /// Workspace authority is independent from the attended/unattended posture.
    /// Frontends surface this state continuously so authority is never implicit.
    #[serde(default)]
    pub workspace_security: crate::WorkspaceSecuritySnapshot,
    /// Whether a stopped round is parked for `/retry`: the previous round
    /// ended before completing (terminal provider error or an interrupt that
    /// left committed content) and its durable resume point is still armed.
    /// A round that completed naturally leaves this `false` forever — `/retry`
    /// is a no-op for it. Frontends use this to offer the `/retry` affordance
    /// instead of scanning the transcript for error notices.
    #[serde(default)]
    pub retry_pending: bool,
    /// Active staffing role for this session (e.g. "developer", "philosophist").
    #[serde(default)]
    pub role: Option<String>,
    /// Bound workspace path for this session, or None for workspace-free sessions.
    #[serde(default)]
    pub workspace: Option<String>,
}

const fn default_confined() -> bool {
    true
}

/// A row in the sessions picker: enough to identify, describe and order a past
/// session without leaking the full transcript to the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOverview {
    pub id: String,
    pub overview: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub active: bool,
    /// The session this one was forked from, when it is a branch rather
    /// than a trunk (`/fork`, `/btw` aside). `None` for a trunk session
    /// (`/new` or the very first session). Lineage is what the dashboard
    /// groups by: one trunk row per conversation, its branches nested
    /// beneath — there is always exactly one *main* line, the trunk the
    /// user is driving; branches are derived views that never replace it.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// How this session came to exist, so the dashboard can badge rows
    /// without string-matching ids: `fork` (an explicit `/fork` branch that
    /// *replaced* the active pointer), `aside` (a `/btw` background
    /// conversation forked off the trunk), or `trunk` (no parent).
    #[serde(default)]
    pub fork_kind: SessionForkKind,
    /// Structured digest (intent + history checklist), if
    /// the session has generated one.
    #[serde(default)]
    pub digest: Option<crate::cognitive::SessionDigest>,
}

/// The provenance of a session relative to its lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionForkKind {
    /// A root session: `/new` or the first-ever session. The main line.
    #[default]
    Trunk,
    /// An explicit `/fork` branch (the active pointer moved here).
    Fork,
    /// A `/btw` aside: forked from the trunk, running alongside it.
    Aside,
    /// A subagent run's own durable session (ADR-0186 §6): spawned by a
    /// subagent tool call in a parent session, never surfaced in the picker.
    Subagent,
}

/// Full detail for one session, requested on demand (the session-info
/// sub-view, `i` from the sessions picker). Unlike [`SessionOverview`], which
/// carries a truncated preview, this carries the *complete* last effective user
/// prompt so the info view can show it in full. Built from the same deferred
/// header parse as the picker rows (no full-transcript deserialize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionDetail {
    pub id: String,
    /// Stored title (AI or manual), if any.
    pub title: Option<String>,
    /// Structured digest (intent + history checklist), if
    /// the session has generated one — the resume-time working-memory view.
    #[serde(default)]
    pub digest: Option<crate::cognitive::SessionDigest>,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub active: bool,
    /// The complete, untruncated text of the last non-echo user prompt, or
    /// `None` when the session has no real user turn yet.
    pub last_prompt: Option<String>,
}

/// One row of provider-picker state sent from the harness to the TUI. Carries
/// everything the picker renders for a provider — display name, the served model
/// ids and the active one, plus the dynamic signals (key readiness, favorite,
/// last-used) — keyed by canonical provider id. The TUI renders directly from
/// these rows (built-in and user-defined providers share one path), so no static
/// per-provider table is consulted. See ADR-0002. Recency exists at BOTH
/// levels: the provider row's `last_used_ms` orders the Connections list, and
/// each `model_info` entry's `last_used_ms` orders the flat Models picker's
/// "recent" section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPickerRow {
    pub id: String,
    /// Display name (e.g. `"OpenAI"`, `"Anthropic"`, or a custom connection's name).
    pub name: String,
    /// Wire id of the currently-active model on this connection.
    pub model: String,
    /// Every model id this connection serves, in catalog order.
    pub models: Vec<String>,
    /// Per-model/channel settings in the same order as `models`.
    #[serde(default)]
    pub model_info: Vec<ProviderModelInfo>,
    /// `true` for built-in presets, `false` for user-defined connections.
    pub builtin: bool,
    /// Wire protocol id of the default channel (`"openai"` | `"anthropic"` |
    /// `"google"`), used to pre-fill the edit form.
    pub protocol: String,
    /// Base URL of the default channel, used to pre-fill the edit form.
    pub base_url: String,
    pub key_ready: bool,
    /// The model provider this connection points at (`"openai"`,
    /// `"anthropic"`, `"deepseek"`, …).
    #[serde(default)]
    pub provider: String,
    /// Client identity configured for this connection.
    #[serde(default)]
    pub client_identity: crate::ClientIdentity,
    /// Unix epoch milliseconds of the last activation. `None` if never activated.
    pub last_used_ms: Option<u64>,
    /// How the connection authenticates.
    #[serde(default)]
    pub auth: crate::ConnectionAuth,
}

pub type ConnectionPickerRow = ProviderPickerRow;

/// Why a live catalog refresh did not deliver a fresh model list (ADR-0273).
///
/// The distinction is what the user can *do*: a refusal means the account was
/// told it may not use the provider, so retrying is pointless and the remedy is
/// an entitlement change; a transient failure means retrying is the remedy. The
/// default is [`Self::Transient`] because it claims less.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSyncFailure {
    /// The upstream failed to serve the list: network, timeout, `429`, `5xx`.
    #[default]
    Transient,
    /// The upstream refused the request as unauthorized or forbidden
    /// (`401`/`403`): the connection is not usable with these credentials or
    /// this plan.
    Refused,
}

/// Progress / outcome of an OAuth connect flow (xAI SuperGrok).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectStatus {
    /// User must complete authorization out-of-band. `url` is the authorize /
    /// verification URL; `user_code` is set for device-code (empty for browser).
    Pending {
        provider: String,
        url: String,
        user_code: String,
        message: String,
    },
    /// Authorization succeeded; tokens persisted (and provider activated when
    /// this followed [`AgentRequest::ConnectConnection`]).
    Done { provider: String },
    /// Authorization succeeded but the follow-up live catalog sync failed,
    /// so the provider keeps its previous (often seed-only) model list. The
    /// UI surfaces this as a warning so the user does not mistake a stale list
    /// for the account's real entitlements. `kind` says whether the upstream
    /// refused the account or merely failed to answer (ADR-0273).
    CatalogSyncWarning {
        provider: String,
        message: String,
        #[serde(default)]
        kind: CatalogSyncFailure,
    },
    /// Authorization failed or was denied.
    Failed { provider: String, message: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModelInfo {
    /// Wire model id. Mirrors an entry in [`ProviderPickerRow::models`].
    pub model: String,
    /// Human-readable label the provider advertises for this model (models.dev
    /// `name`, Anthropic/Kimi `display_name`), when it advertises one. This is
    /// the label the model pickers lead with, falling back to `model` when it is
    /// absent. Purely presentational: it is never the identity, so frontends
    /// must keep the wire id visible beside it and key every config surface on
    /// `model`. `None` — the common case — means show the bare wire id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Wire protocol id of the channel serving this model (`"openai"` |
    /// `"anthropic"` | `"google"`).
    pub protocol: String,
    /// Effective reasoning effort for channels whose model exposes an effort
    /// knob. `None` for protocols/models that do not expose one.
    pub effort: Option<String>,
    /// Effective extended-thinking state for channels that expose a separate
    /// thinking on/off knob. `None` for protocols that do not expose one.
    pub thinking: Option<bool>,
    /// Reasoning effort tiers this channel/model supports, in ascending order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_levels: Vec<String>,
    /// Whether this model is favorited in the **Models** picker (ADR-0046 moved
    /// favorite from provider-level to per-model). A starred daily-driver model
    /// sorts into the second priority tier of the flat list wherever it is
    /// served. Added late, so it defaults to `false` on deserialize for older
    /// snapshots.
    #[serde(default)]
    pub favorite: bool,
    /// Unix epoch milliseconds of this model's last activation, or `None` if
    /// never activated. Drives the flat Models picker's "recent" section
    /// (recency-desc, most recently used first). `None`-defaulted so older
    /// snapshots deserialize as "never used".
    #[serde(default)]
    pub last_used_ms: Option<u64>,
    /// Effective image-input support for this route — the **full** ADR-0149
    /// capability resolution (user overrides over the remote advertisement
    /// over the static baseline), resolved daemon-side in
    /// `Channel::capabilities()`. Frontends gate image affordances (composer
    /// paste, vision-only tools) on this field instead of re-resolving the
    /// model in their own process: the client's static registry cannot see
    /// the daemon's fitted-model overlay or per-route overrides, so a
    /// client-side resolution would disagree with what the daemon actually
    /// routes.
    ///
    /// **Three-valued** (ADR-0230): `Some(false)` means a layer declared that
    /// this route rejects images — the only value a frontend may gate on;
    /// `None` means *undeclared*, which older snapshots (and every route whose
    /// vendor advertises no vision field) deserialize to, so a frontend must
    /// treat it as "try it", never as text-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Effective context window in tokens for this route — the **full**
    /// ADR-0149 capability resolution (ADR-0182). Guaranteed > 0 for valid
    /// channels; 0 indicates an unresolved fallback.
    #[serde(default)]
    pub context_window: usize,
    /// Maximum output generation tokens for this route when declared or overridden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// The **effective** availability verdict for this route (ADR-0273):
    /// the provider's declaration, after any sovereign user override has been
    /// applied daemon-side. `None` means undeclared — a frontend must treat it
    /// as usable, never as disabled. `Some(usable:false)` is the one value a
    /// picker may dim and refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,
    /// Set when the provider declared the model unusable but the user's own
    /// scope (inject) overrode it. The row is usable yet must disclose the
    /// upstream verdict it contradicts (`[INV-AVAIL-05]`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub availability_overridden: bool,
    /// The provider's listing intent (ADR-0273). `Some(false)` means the
    /// provider does not want this model offered in a listing; `None` means
    /// undeclared and therefore listed. Independent of `availability`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertised: Option<bool>,
    /// Set when this row's `availability` was observed **before a refresh that
    /// has since failed**, so the verdict may already have been reversed
    /// upstream. The declaration is still enforced (a failed refresh never
    /// widens access), but the surface must not present it as freshly
    /// confirmed (ADR-0273).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub availability_stale: bool,
}

impl ProviderModelInfo {
    /// Extract the materialized route capabilities from this model info row (ADR-0182).
    pub fn route_capabilities(&self) -> crate::RouteCapabilities {
        crate::RouteCapabilities {
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            vision: self.vision,
            tool_call: true,
            thinking: if self.thinking == Some(true) {
                crate::ReasoningSupport::AnthropicAdaptive
            } else {
                crate::ReasoningSupport::None
            },
        }
    }
}

/// Full snapshot of provider-picker state: which provider is the current
/// default plus one row per known provider. Sent on startup and after any
/// mutation (favorite toggle, default change, provider switch) so the TUI
/// always renders from a fresh, consistent picture rather than merging
/// incremental updates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderPickerSnapshot {
    /// Canonical id of the active/default connection. Matches
    /// `config.default_connection`.
    pub default_id: String,
    pub rows: Vec<ProviderPickerRow>,
}

pub type ConnectionPickerSnapshot = ProviderPickerSnapshot;

/// Complete state snapshot of a live or persisted session for client hydration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub round_counter: u64,
    pub messages: Vec<Message>,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub todos: Vec<crate::todos::TodoItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<ContextTokenSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_picker: Option<ProviderPickerSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_keys: Option<Vec<(String, bool)>>,
}

/// Steering operations a parent can submit into a running agent's inbox — the
/// down-direction of full-duplex (ADR-0029). Distinct from the request/reply
/// class ([`PermissionRequest`] / [`UserQuestionRequest`]), which resolve
/// instantly via the agent's shared-state oneshots (`reply_permission` /
/// `reply_user_question`) and therefore do **not** flow through this queue: a
/// reply must unblock a tool that is parked mid-turn, so it cannot wait for
/// the driver loop to drain. This enum covers only the "new input / control"
/// class that is safe to apply at the next ReAct-turn boundary.
///
/// Modeled on codex's `Op` (`codex-rs/protocol/src/protocol.rs`), trimmed to
/// nuo's driver shape: the agent owns an `mpsc` inbox whose receiver is
/// drained at the top of every ReAct turn (and, for `Interrupt`, raced against
/// the live stream). The top-level agent and spawned subagents share the same
/// `Op` vocabulary — a subagent is just an agent whose inbox sender the
/// parent holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentOp {
    /// Append a visible user message to the live transcript before the next
    /// model request, as if the user typed it. Lets a parent (or, for a
    /// subagent, the orchestrating agent) steer a running round with new
    /// information without restarting it. codex `inject_if_running` analogue.
    Steer(String),
    /// Append a hidden (system-level) steering note — like
    /// [`AgentOp::Steer`] but recorded as a hidden user message so
    /// it informs the model without polluting the visible transcript. codex
    /// `InterAgentCommunication` analogue.
    InterAgentMessage { msg: String },
    /// Abort the current round at the next boundary. Coarser than the parent's
    /// `CancellationToken` (which cancels instantly): this is the
    /// handle-addressable path for a caller that owns the inbox but not the
    /// cancel token. codex `Op::Interrupt` analogue.
    Interrupt,
    /// Tear the agent down (interrupt + signal that the shutdown was
    /// requested rather than cancelled). codex `Op::Shutdown` analogue.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    Notice(AgentNotice),
    ModelRequestStarted {
        /// 1-indexed enclosing user round.
        round: u64,
        /// 0-indexed model-request position within `round`.
        turn: usize,
        /// Semantic estimate of the exact request projection after turn-start
        /// hooks and immediately before it is sent to the provider.
        context_tokens: usize,
    },
    /// A provider request completed and its high-resolution performance
    /// sample was sealed before any tool dispatch.
    TurnPerformance(crate::TurnPerformanceSnapshot),
    /// Provider-reported context after a completed request. This supersedes the
    /// pre-request projection for that session until its context mutates again.
    ContextTokens(ContextTokenSnapshot),
    /// A steering message was atomically admitted at a safe turn boundary.
    SteerAdmitted(QueuedMessage),
    AssistantDelta {
        delta: String,
        start: bool,
    },
    AssistantEnd(String),
    AssistantDiscard,
    ReasoningDelta {
        delta: String,
        start: bool,
    },
    ReasoningEnd(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
        structured: ToolOutput,
        duration_ms: u64,
    },
    /// Incremental output streamed by a running tool (see [`ToolStream`]).
    ToolStream {
        id: String,
        stream: ToolStream,
    },
    ToolCancelled {
        id: String,
        name: String,
    },
    /// A tool call whose **name is known but whose arguments are still
    /// streaming** (ADR-0026). Announced as soon as the provider names the
    /// call, strictly before the arguments finish, so a frontend can create a
    /// running step and move the activity phase to the tool verb instead of
    /// showing the misleading `answering` phase while the model plans a call.
    /// `index` is the provider's tool-call slot; the whole-argument
    /// [`AgentEvent::ToolCall`] for the same slot later collapses onto the step
    /// this announced. `id` is the provider-supplied id once it has arrived
    /// (may be `None` — the harness mints the dispatch id only at dispatch).
    ToolCallStarted {
        index: usize,
        id: Option<String>,
        name: String,
    },
    /// Count-only progress for a still-streaming tool call's arguments
    /// (ADR-0023, `[INV-STREAM-TOOL-03]`). `bytes` is the number of argument
    /// bytes received so far — **never the bytes themselves**; a frontend
    /// renders it as a static, non-animated clause (ADR-0008: the activity bar
    /// is the single breathing anchor).
    ToolInputProgress {
        index: usize,
        id: Option<String>,
        bytes: usize,
    },
    /// The task list changed (`todo` / `todo_update`). The TUI uses this to refresh the
    /// unified sticky panel above the input box.
    TodosUpdated(crate::todos::TodoList),
    /// The unattended execution toggle changed (via `/unattended`).
    UnattendedChanged(bool),
    /// The workspace confinement toggle changed (via `/confinement`).
    ConfinementChanged(bool),
    PermissionRequest(PermissionRequest),
    UserQuestionRequest(UserQuestionRequest),
    /// An interactive `bash` command needs a line of stdin from the operator.
    /// The TUI shows an inline input panel; the reply travels back
    /// as [`AgentRequest::StdinReply`].
    #[serde(alias = "InputRequest")]
    StdinRequest(StdinRequest),
    /// An subagent spawned by a tool (e.g. `task`) emitted an event.
    Subagent {
        parent_call_id: String,
        event: SubagentEvent,
    },
    /// A background process or sub-subagent job started.
    BackgroundJobStarted(crate::job::BackgroundJobInfo),
    /// Incremental progress or output line from a background job.
    BackgroundJobProgress {
        job_id: crate::job::JobId,
        line: String,
    },
    /// A service task reported readiness (ADR-0190). Wake-eligible.
    BackgroundJobReady {
        job_id: crate::job::JobId,
    },
    /// A background job completed.
    BackgroundJobCompleted(crate::job::BackgroundJobOutcome),
    /// Remote catalog was updated; attached frontends should refresh provider picker.
    CatalogInvalidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// Allow this single invocation (one-off).
    Once,
    /// Allow for the duration of the current session (in-memory).
    Session,
    /// Allow permanently for this workspace (persisted).
    Always,
    /// Deny / reject this execution.
    Reject,
}

/// Reply sent from the TUI back to the agent after the user answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestionReply {
    pub request_id: String,
    /// One array of selected option labels per question.
    pub answers: Vec<Vec<String>>,
}

pub type InputRequest = StdinRequest;

/// Reply sent from the TUI back to the agent carrying the operator's stdin input.
/// An empty `text` signals cancellation (the command runs with closed stdin
/// and fails fast with a non-interactive remedy hint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdinReply {
    pub request_id: String,
    pub text: String,
}

pub type InputReply = StdinReply;

/// A complete, render-ready picture of the live session, sent from the harness
/// to the TUI for the session-context modal. Every pane in that modal reads
/// from this one snapshot, so opening the modal and any mutation
/// (revoke / toggle) only needs a single request/response round-trip rather
/// than one per pane. Built by the harness from its own state (provider,
/// tools, permissions, skills) plus the MCP load result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContextSnapshot {
    pub model: ModelInfo,
    pub tools: Vec<ToolInfo>,
    pub permissions: Vec<PermissionRuleInfo>,
    pub skills: Vec<SkillInfo>,
    pub mcp: Vec<McpServerInfo>,
}

/// Model-side pane of [`SessionContextSnapshot`]. `capabilities` carries
/// heuristic hints (e.g. "tool calling", "reasoning") since per-model
/// capability data is not yet modeled in the catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub model: String,
    pub display_name: String,
    pub context_window: usize,
    pub api_key_ready: bool,
    pub description: String,
    pub capabilities: Vec<String>,
}

/// One tool in the session, as seen by the modal's Tools pane. `source`
/// classifies origin: `builtin`, `mcp:<server>`, or `plan`. `enabled`
/// reflects the session-level enable/disable flag (toggled via
/// [`AgentRequest::ToggleTool`]); disabled tools stay installed but are hidden
/// from the model and rejected if invoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub source: String,
}

/// One cached "always allow" permission rule, shown in the modal's Permissions
/// pane where it can be revoked individually via [`AgentRequest::RevokePermission`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PermissionRuleInfo {
    pub tool: String,
    pub scope: String,
}

/// One skill in the registry, shown in the modal's Skills pane. `source` is the
/// [`SkillScope`](../nuo_skills/enum.SkillScope.html) display string
/// (system / remote / user / extra / repo).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub enabled: bool,
    pub source: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub quarantined: bool,
}

/// One MCP server, shown in the modal's MCP pane. The connection tri-state
/// (connected / disabled / failed) is unpacked from
/// [`crate::mcp::McpConnectionStatus`] so the DTO stays decoupled from the
/// enum, and `tool_names` carries the per-server tool list that the hint bar
/// collapses to a mere count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub connected: bool,
    pub disabled: bool,
    pub failure: Option<String>,
    pub tool_names: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_summary_tps_excludes_human_pause() {
        // Fallback path (no generation_ms recorded): 500 output tokens over
        // 10s wall-clock, 8s of which the user spent deliberating on a
        // permission prompt → only 2s of active time. TPS = 250.
        let summary = RoundSummary {
            round: 3,
            output_tokens: 500,
            duration_ms: 10_000,
            paused_ms: 8_000,
            generation_ms: 0,
            ..Default::default()
        };
        assert_eq!(summary.active_ms(), 2_000);
        assert!(
            (summary.tps() - 250.0).abs() < 0.01,
            "got {}",
            summary.tps()
        );
    }

    #[test]
    fn round_summary_tps_uses_generation_time_excluding_tools() {
        // A round that streamed 500 output tokens in 2s of real generation,
        // then spent 30s executing tools, then 8s parked on a permission.
        // Only the 2s of generation counts: TPS = 250, NOT ~12.5 (round
        // active_ms) and NOT ~3.1 (wall-clock).
        let summary = RoundSummary {
            round: 3,
            output_tokens: 500,
            duration_ms: 40_000,
            paused_ms: 8_000,
            generation_ms: 2_000,
            ..Default::default()
        };
        assert_eq!(summary.active_ms(), 32_000);
        assert!(
            (summary.tps() - 250.0).abs() < 0.01,
            "got {}",
            summary.tps()
        );
    }

    #[test]
    fn round_summary_tps_is_zero_when_round_had_no_active_time() {
        let summary = RoundSummary {
            round: 1,
            output_tokens: 0,
            duration_ms: 0,
            paused_ms: 0,
            generation_ms: 0,
            ..Default::default()
        };
        assert_eq!(summary.active_ms(), 0);
        assert_eq!(summary.tps(), 0.0);
    }

    #[test]
    fn round_summary_active_time_saturates_when_pause_exceeds_duration() {
        // Defensive: a round whose recorded pause exceeds its wall-clock
        // (shouldn't happen, but must never panic on subtraction) yields zero
        // active time rather than a negative.
        let summary = RoundSummary {
            round: 1,
            output_tokens: 100,
            duration_ms: 1_000,
            paused_ms: 2_000,
            generation_ms: 0,
            ..Default::default()
        };
        assert_eq!(summary.active_ms(), 0);
        assert_eq!(summary.tps(), 0.0);
    }

    #[test]
    fn command_ack_notice_is_toast_surfaced_info_from_harness() {
        // A slash-command reply (the *acknowledgment*, not the invocation) is
        // stamped uniformly so frontends can branch on kind + surface:
        //   - severity Info (it is a status confirmation, not an error),
        //   - surface Toast (transient bubble, never appended to transcript),
        //   - source Harness (not the agent / a tool).
        let notice = AgentNotice::command_ack("Delegated mode ON: …");
        assert_eq!(notice.kind, NoticeKind::CommandAck);
        assert_eq!(notice.severity, NoticeSeverity::Info);
        assert_eq!(notice.surface, NoticeSurface::Toast);
        assert_eq!(notice.source, NoticeSource::Harness);
        assert_eq!(notice.title, "Delegated mode ON: …");
        assert!(notice.body.is_none());
    }

    #[test]
    fn command_ack_kind_serialises_as_snake_case() {
        // The closed NoticeKind classifier must serialise the new variant
        // distinctly so frontends (and persisted/forwarded notices) cannot
        // confuse it with the existing kinds.
        let notice = AgentNotice::command_ack("x");
        let json = serde_json::to_string(&notice.kind).expect("serialise");
        assert_eq!(json, "\"command_ack\"");
    }

    #[test]
    fn trust_changed_kind_serialises_as_snake_case_and_round_trips() {
        // ADR-0155: TrustChanged is first-class, stamped uniformly by
        // `trust_changed()` — Warning severity, Harness source — and must stay
        // distinct from the generic ReviewAlert on the wire so frontends can
        // branch on it exactly.
        let notice = AgentNotice::trust_changed("Workspace configurations changed");
        assert_eq!(notice.kind, NoticeKind::TrustChanged);
        assert_eq!(notice.severity, NoticeSeverity::Warning);
        assert_eq!(notice.source, NoticeSource::Harness);
        let json = serde_json::to_string(&notice.kind).expect("serialise");
        assert_eq!(json, "\"trust_changed\"");
        let back: AgentNotice =
            serde_json::from_str(&serde_json::to_string(&notice).unwrap()).expect("round-trip");
        assert_eq!(back.kind, NoticeKind::TrustChanged);
    }

    #[test]
    fn command_ack_kind_round_trips() {
        let notice = AgentNotice::command_ack("x");
        let json = serde_json::to_string(&notice).expect("serialise");
        let back: AgentNotice = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.kind, NoticeKind::CommandAck);
        assert_eq!(back.surface, NoticeSurface::Toast);
    }
}
