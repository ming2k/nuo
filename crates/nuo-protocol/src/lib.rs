//! Canonical wire-level message framing, envelopes, and session communication
//! types for Nuo — plus the provider-neutral domain contracts (the `Provider`
//! execution trait, `ModelRequest`/`ToolSpec`, model/pricing vocabulary, the
//! context-pressure model, thread/SSOT types, policy schemas, and the events
//! exchanged by sessions and frontends).
//!
//! Conforming to ADR-0005, this crate is a **zero-I/O, zero-async-runtime**
//! contract layer: no filesystem, no network. Tool-facing vocabulary (`Tool`,
//! `ToolOutput`, `Message`, `Role`, `TodoList`, …) is owned by `nuo-tool` and
//! imported from there; provider transport and concrete specs are owned by
//! `nuo-provider` ([ADR-0180](../../docs/adr/0180-single-ownership-of-shared-vocabulary.md)).
//! Pure cognition belongs in `nuo-engine` (ADR-0057 → ADR-0066).
//!
//! Consolidated from the former `nuo-contracts` crate: ADR-0001 mandated that
//! crate's dismantling (`[INV-ARCH-01]` bans catch-all `contracts`/`common`/
//! `shared`/`core` crates), and ADR-0005 designated `nuo-protocol` as the home for
//! wire envelopes and session communication entities.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::io;

use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

pub use async_trait::async_trait;

// ── Domain & wire contract modules (consolidated from `nuo-contracts`) ──────

pub mod endpoint;
pub use endpoint::LocalEndpoint;
pub mod discovery;
pub use discovery::{Discovery, ServerInfo};
pub mod version;
pub use version::{
    ERR_PROTOCOL_MISMATCH, ERR_SERVER_DRAINING, ERR_SERVER_FAILED, ERR_SERVER_NOT_READY,
    ERR_SESSION_GONE, ERR_SESSION_UNRESUMABLE, ERR_VERSION_MISMATCH, incompatibility_error,
    protocol_accepts, protocol_mismatch, version_mismatch, versions_compatible,
};
pub mod cache;
pub use cache::{
    CachePlan, CacheResolutionError, CacheRetention, PromptCacheCapabilities, PromptCacheMode,
    PromptCacheModePreference, PromptCachePreference, PromptCacheSpec, PromptCacheUsage,
    ResolvedCachePolicy, read_prompt_cache_usage,
};
pub mod request_projection;
pub use request_projection::RequestProjection;
pub mod budget;
pub use budget::{BudgetPeriod, ConnectionBudget};
pub mod pricing;
pub use pricing::{ModelPricing, NanoUsd, PricingTier, TurnCost};

pub mod error;
pub use error::{
    HarnessError, ProviderError, ProviderErrorKind, RetryDisposition, RetryVerdict,
    TransportDiagnostic,
};

pub mod transcript;
pub use transcript::{
    CompactionMeta, DirectiveKind, DirectivePayload, EntryKind, EntryOrigin, EntryPayload,
    ImageBlobRef, MessagePayload, ProjectionDirective, PrunedMediaOutput, PrunedToolOutput,
    StatePayload, SubagentRef, Transcript, TranscriptEntry, checkpoint_message,
};

pub mod context;
pub mod instructions;
pub mod language;
pub use context::{
    Budget, ContextMeasurement, ContextNotice, ContextPlan, ContextPlanView, Elision, NoticeKind,
    Overflow, PlannedBlock, PlannedBlockRecord, PlannedBlockView, Position, PrefixDrift, Rank,
    Resume, Stability, Topic,
};
pub use language::{MAX_LANGUAGE_TAG_LEN, ResponseLanguage};
pub mod observation;
pub mod request_builder;
pub mod thread;
pub mod thread_view;
pub use instructions::{
    Block, Boundary, InstructionRoute, InstructionSelector, InstructionSource, InstructionText,
    ScopedInstruction, render_blocks,
};

pub mod action;
pub mod command;
pub use command::{
    ChangedFile, CommandRecord, CommandResult, CommandStatus, ReviewStatus, ReviewVerdict,
    SearchHit, UndoRefusal,
};
pub use observation::{
    InvalidationReason, ObservationLifecycle, ObservationMetrics, TerminationReason, Trust,
};
pub use request_builder::{RequestBuilder, RequestContext};
pub use thread::{
    BlobRef, BranchId, BranchPolicy, BranchState, FactContext, FactId, FactKind, FactPayload,
    THREAD_SCHEMA_VERSION, Thread, ThreadError, ThreadFact, ThreadGraph, ThreadId, ThreadRevision,
    ThreadState, ThreadStatus,
};
pub use thread_view::{FactSummary, FoldReason, FoldRecord, RequestView, ThreadWindow};

pub mod auth;
pub use auth::{
    ChatGptAuthMetadata, CopilotAuthMetadata, CredentialSource, ExtensionMap, GoogleAuthMetadata,
    PreflightValidator, ResolvedAuth, StaticCredentialSource, static_credential,
};

pub mod capability;
pub mod connection_auth;
pub mod connection_detail;
pub mod dialects;
pub mod model_providers;
pub mod provider_state;
pub mod wire_surface;
/// Default user agent this project sends to providers.
pub const NUO_USER_AGENT: &str = concat!("nuo/", env!("CARGO_PKG_VERSION"));
pub mod effort;
pub use effort::{COMMON_LADDER, Effort, EffortLevel, ReasoningEffortControl};
pub mod tuning;
pub use tuning::{EFFORT_KNOB, THINKING_KNOB, TunableSetting, TunableSettings};
pub mod reasoning;
pub use reasoning::{ReasoningMode, ReasoningSupport};
pub mod dynamic;
pub mod events;
pub use events::{
    AgentEvent, AgentNotice, AgentOp, AgentRequest, AgentResponse, CatalogSyncFailure,
    ConnectStatus, ConnectionPickerRow, ConnectionPickerSnapshot, ContextTokenSnapshot,
    ContextTokenSource, EventClass, HarnessSnapshot, InputReply, InputRequest, LoopStatus,
    McpServerInfo, ModelInfo, NoticeSeverity, NoticeSource, PermissionDecision, PermissionRequest,
    PermissionRuleInfo, PrepareStage, ProviderModelInfo, ProviderPickerRow, ProviderPickerSnapshot,
    QueueMode, QueuedMessage, RetryPoint, RoundEvent, RoundInterrupt, RoundInterruptReason,
    RoundScope, RoundSummary, SkillInfo, StateDelta, StdinReply, StdinRequest, SubagentEvent,
    ThreadContextSnapshot, ThreadDetail, ThreadOrigin, ThreadOverview, ThreadPhase,
    ThreadPhaseUpdate, ThreadSnapshot, ToolInfo, TrustDecision, TurnContext, UserQuestion,
    UserQuestionOption, UserQuestionReply, UserQuestionRequest, WebConfigUpdate, WebConfigView,
    WebCredentialUpdate, WebSearchConfigUpdate, WebSearchConfigView,
};
pub mod agent_kind;
pub mod agent_role;
pub mod cognitive;
pub mod execution_policy;
pub mod hooks;
pub mod job;
pub mod mcp;
pub mod model;
pub mod subagent;
pub use agent_kind::AgentKind;
pub use agent_role::{
    Agent, AgentRole, AgentRoleDelegation, AgentRoleProfile, AgentRuntimeConfig, DelegationPolicy,
    MainAgent, MainAgentRole, SubAgent, SubAgentRole, ThreadRoleManifest,
};
pub use cognitive::{
    CognitiveModelPreference, CognitiveTask, HarnessTask, HarnessTaskModelPreference,
    StreamLoopChannel, StreamLoopReviewInput, StreamLoopReviewerTask, StreamLoopVerdict,
    ThreadDigest, ThreadTitleInput, ThreadTitleTask, TrajectoryLoopReviewInput,
    TrajectoryLoopReviewerTask, TrajectoryLoopVerdict,
};
pub use execution_policy::{ContextLifecycle, ExecutionPolicy, PolicyViolation};
pub mod history;
pub use history::{HISTORY_CAP, HistoryEntry, HistorySearchHit, merge_history};
pub mod human_request;
pub use human_request::{
    AutonomousFallbackPolicy, HumanChannelAccountant, HumanChannelPosture, HumanReply,
    HumanRequestKind, ReplyProvenance,
};
pub mod identity;
pub mod pressure;
pub mod token_ledger;
pub use token_ledger::{
    BeginRequestParams, MAX_PLAUSIBLE_STREAM_TPS, MIN_DEFENSIBLE_STREAM_SPAN_US,
    PerformanceTimingSource, RequestPerformance, RequestUsageKey, RequestUsageRecord,
    RequestUsageSource, RequestUsageStatus, StreamTokenSource, TokenSourceLedger,
    TokenSourceReport, TokenSourceRow, TokenSourceTotals, TokenTurn, TransportObservation,
    TransportTelemetry, TransportTimings, TurnPerformanceSnapshot, UsageStatSink,
    latest_turn_performance,
};
pub mod usage_stats;
pub use usage_stats::{
    UsageDayTotals, UsageModelRow, UsageModelTotals, UsageStatRecord, UsageStatsReport,
    aggregate_usage_records, day_key_from_epoch_ms,
};
pub mod execution;
pub mod secret;
pub mod security;
pub mod shared_roots;
pub use execution::{
    DirEntry, ExecutionEnvironment, FsError, FsMetadata, FsProvider, ProcessOutput, ProcessRunner,
    ShellIsolation, ToolMiddleware,
};
pub use security::{
    AssetLocator, AssetSpec, AttestationStatus, TrustDomain, WorkspaceSecuritySnapshot,
    WorkspaceTrustState,
};

pub mod workspace;
pub use workspace::{ThreadFilter, ThreadRemovalReport, ThreadRemovalSkip, WorkspaceBinding};

pub mod session_title;

pub mod session_tree;
pub use session_tree::{
    CompactionPayload, ThreadEntry, ThreadEntryId, ThreadEntryKind, ThreadTree,
};
pub use shared_roots::{SharedAdditionalRoots, SharedConfinement};
pub mod tool_registry;
pub mod web_config;
pub use capability::{
    ModelRequest, Provider, ProviderEventStream, ProviderPromptHints, ProviderStreamEvent,
    ProviderTextStream, ProviderTurnContext, ToolSpec, VariantSelection, empty_variant_selection,
};
pub use connection_auth::{ConnectionAuth, Entitlement, LoginMethod, WirePlacement};
pub use connection_detail::{
    BalanceQuota, ConnectionDetail, ConnectionQuotaEntry, ConnectionUsageState,
    ModelAvailabilityRule, PeriodicQuota, ProviderQuotaData, ProviderQuotaSnapshot, ProviderUsage,
    QuotaWindowBucket, QuotaWindowKind, RateLimitSpec, UsageMetric,
};
pub use dialects::{
    AnthropicMessagesDialect, Channel, GoogleGenerateContentDialect, GoogleInteractionsDialect,
    OpenAiChatDialect, OpenAiResponsesDialect, ProviderDialect, ProviderEntry, Transport,
};
pub use dynamic::{DynamicCatalog, DynamicToolSink};
pub use provider_state::{
    CONTINUATION_ARTIFACT_KEY, ContextRelation, ContextRevision, ContinuationCursor,
    ContinuationMode, CursorInvalidationReason, EnvelopeRevision, OPENAI_RESPONSE_ID_ARTIFACT_KEY,
    OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY, ProviderArtifacts, ProviderCompletion,
    ProviderCompletionMeta, ProviderCursorState, RequestDelivery, RouteFingerprint,
    read_continuation_cursor, request_envelope_fingerprint, request_prefix_fingerprint,
    select_request_delivery, semantic_context_head, write_continuation_cursor,
};
pub use subagent::{SubAgentProfile, ToolPolicy};
pub mod monitor;
pub use dialects::{
    DiscoveredModel, ModelListError, ResponsesState, clean_model_display_name,
    format_model_display_name,
};
pub use hooks::{Hook, HookContext, HookEvent, HookOutcome, HookStartSource, RestorePoint};
pub use identity::AgentIdentity;
pub use job::{
    AdoptionInfo, BackgroundJobInfo, BackgroundJobOutcome, BackgroundJobService, CrateChildBridge,
    JobId, JobKind, JobSpec, JobState, Readiness, RestartPolicy,
};
pub use mcp::{McpConnectionStatus, McpServerConfig};
pub use model::{
    Availability, BaselineModels, CapabilityOverrides, ConnectionFilterPolicy, DeclaredModel,
    FittedModel, Model, ModelCapabilities, ModelCapabilityPatch, ModelScopeConfig,
    ModelTargetScope, NamedFilterPolicy, RemoteModelMetadata, WireEntityResolution, WireProtocol,
    baseline_models, model_by_id, register_fitted_models, resolve as resolve_model,
    sanitize_model_id, simple_glob_matches,
};
pub use monitor::{
    MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredTask, MonitoredThread,
    PersistenceHealth, ServerInitStage, ServerPhase, StorageInitDetail, StoragePressure,
    ThreadActivity, ThreadHosting,
};
pub use pressure::{
    CLEARED_TOOL_PREFIX, CRUISE_HIGH_WATERMARK, CRUISE_LOW_WATERMARK, CompactionPolicy,
    ContextBudget, DEFAULT_REQUEST_BODY_LIMIT_BYTES, LayeredRequestWeights, MediaOffload,
    MediaOffloadOutcome, MessageContentFingerprint, MessageTokenWeights,
    PAYLOAD_KEEP_NEWEST_IMAGES, PAYLOAD_RELIEF_UTILIZATION, PRUNE_QUANTUM_FLOOR_TOKENS,
    PruneOutcome, RequestTokenEstimate, RequestWireSize, TAIL_QUARANTINE_TOKENS, ToolSchemaWeights,
    apply_media_offload, estimate_bytes, estimate_draft_tokens, estimate_message_tokens,
    estimate_semantic_json_tokens, estimate_tokens, estimate_tokens_weighted,
    layered_request_weights, media_allowance, media_bytes, message_wire_bytes,
    payload_relief_target, plan_media_offload, prune_tool_results, request_wire_size,
};
pub use secret::SecretString;
pub use session_title::{TITLE_MAX_LEN, ThreadTitle, clean_title};
/// The BPE token counter (`nuo_tool::tokenizer`) under the name the
/// heuristic estimator used to own: token prediction is BPE now, and callers
/// that imported `count_tokens` for budget-fitting (summary truncation)
/// must measure in the same unit as the projection thresholds.
pub use tool_registry::{
    Capability, CapabilityScope, ToolCapabilityAudit, ToolDeclaration, ToolFactory,
    ToolFactoryContext, ToolFactoryContextBuilder, ToolPool, ToolPoolSnapshot, ToolSelection,
    ToolSet, WorkspaceRoot, WorkspaceRoots, collect_toolset,
};
pub use web_config::{
    BOCHA_SEARCH_ENDPOINT, DUCKDUCKGO_HTML_ENDPOINT, DUCKDUCKGO_LITE_ENDPOINT, EXA_SEARCH_ENDPOINT,
    JINA_READER_ENDPOINT, PARALLEL_SEARCH_ENDPOINT, SharedWebConfig, TAVILY_SEARCH_ENDPOINT,
    WebConfig, WebCredentialRequirement, WebCredentialStatus, WebEndpointRequirement,
    WebProviderAxis, WebProviderCapability, WebReaderProvider, WebRuntimeConfig, WebSearchConfig,
    WebSearchProvider, web_provider_capabilities,
};

pub mod surface;
pub use surface::{ApiRoot, CatalogShape, RemoteCatalogSource};

pub mod context_projection;
pub use context_projection::{
    ContextProjectionCheckpoint, ContextProjectionKind, ContextProjectionResult,
};
pub mod policy_config;
pub mod policy_schema;
pub use policy_schema::{
    BashPolicyActionConfig, BashPolicyConfig, BashPolicyMatcherConfig, BashPolicyRuleConfig,
    CustomRole, PermissionRuleConfig,
};

/// Current protocol version.
///
/// 20: Command effect classes (ADR-0140). Every command declares whether it is an
/// act (`Mutation`, recorded) or a reading (`Reading`, ephemeral); the class
/// rides both `CommandSpec` and `RoundEvent::CommandResult`, so a client files a
/// reply where the server says it belongs instead of inferring it. A mixed pair
/// would disagree about which replies are durable rows, which is the exact
/// asymmetry the field removes — hence a hard floor rather than a tolerant field.
///
/// 23: Tool announcement retraction (`RoundEvent::ToolAnnouncementsRetracted`).
/// When a streamed attempt ends before its announced calls are dispatched, the
/// server names the provisional announcements the frontend must retract. A peer
/// that does not model the frame leaves phantom tool steps in the transcript,
/// so the field is a hard floor: every client at 21 must move with it.
///
/// 24: One call identity (ADR-0166). `ToolCallStarted` / `ToolInputProgress`
/// stop carrying a separate announcement token and provider slot; they carry
/// the call id that every later event about the call already used.
/// `ToolAnnouncementsRetracted { announcements: [u64] }` becomes
/// `ToolCallsRetracted { ids: [String] }`, and `ToolCall` loses its
/// `announcement` field. A v23 peer cannot deserialize the new shapes, so the
/// floor moves with the ceiling: this is a clean break, not a compatibility
/// layer.
///
/// 25: A discarded attempt carries its own position (ADR-0167).
/// `RoundEvent::StreamDiscard` changes from a unit variant to
/// `{ round: u64, turn: u64 }`. A v24 peer cannot decode the struct variant,
/// and a peer that somehow ignored the fields would sweep by a side channel
/// that re-attachment does not populate. The floor moves with the ceiling.
///
/// 26: Turn-scoped events carry their own context (ADR-0168). `Round` gains a
/// `RoundScope::Turn { context, event }` envelope carrying round, 0-indexed
/// turn and the turn's send time; session/round events use
/// `RoundScope::Session`. `TurnStarted` and `StreamDiscard` become unit
/// markers, and the client keeps no per-session position or timestamp map.
/// A v25 peer cannot decode the envelope shape; there is no compatibility
/// lane.
///
/// 27: Role-scoped thread control (ADR-0171). `ControlReply` gains an optional
/// `payload` carrying the JSON-encoded result of data-bearing verbs
/// (`ListThreads`, `StopThreads`, `PurgeThreads`, usage/endpoint reports).
/// The envelope shape changes, so the floor moves with the ceiling.
///
/// 30: `AgentResponse` and `RoundEvent` wire shapes updated (background job
/// outbox / measured-usage telemetry).
///
/// 31: Permission negotiation carries a suggested prefix scope and an exact
/// `Always` variant (ADR-0212), so a client can choose between a generalized
/// `cargo test *` grant and the literal command. The variant is not decodable
/// by a v30 peer and the new server must not interpret a v30 client's `Always`
/// as the generalized grant it never saw, so the floor moves with the ceiling.
///
/// 32: The measured context plan is a wire surface (ADR-0220).
/// `AgentRequest` gains `QueryContextPlan` and `AgentResponse` gains
/// `ContextPlan { thread_id, plan }` carrying the rendered body of every active
/// block; `RequestTokenEstimate` names `instructions_tokens` and
/// `tool_schema_tokens`, and `ContextTokenSnapshot` carries both. A v31 peer
/// cannot decode the new variants and would read the unnamed layers as absent,
/// so the floor moves with the ceiling.
/// 33: Multiplexed thread sessions on a single connection (ADR-0251).
/// `Wire::Request` gains an optional `thread_id` target; `Wire` gains
/// `Subscribe`, `Unsubscribe`, and `Subscribed`; `AgentResponse` gains
/// `ThreadSubscribed`. Floor moves with the ceiling.
/// 34: Server-scoped draft catalog reconciliation (ADR-0250).
/// `AgentRequest` gains `CheckThreadsExist`; `AgentResponse` gains
/// `ThreadsExistence` and `ThreadDeleted`. Floor moves with the ceiling.
/// 35: Monotonic per-thread generation gating (`ThreadKey { id, generation }`,
/// ADR-0253).
/// 36: Server lifecycle contract and session-free unified handshake (ADR-0260).
/// `Wire::Select`/`Welcome` and `AttachAction` are replaced by `Wire::Hello` ->
/// `Wire::ServerHello` and `ThreadTarget`; `ServerPhase` and
/// `MonitorEvent::ServerPhaseChanged` replace `MonitorEvent::ServerDraining`;
/// `ThreadPhase` retires `Binding` and unproduced `Loading`; `Subscribe`,
/// `Subscribed`, `Pick`, `Control`, `ControlReply`, and `Error` carry
/// `request_id` for unified single-connection multiplexing. Clean break.
/// 37: Observable storage migration progress (ADR-0266).
/// `ServerPhase::Initializing` gains optional `detail: Option<StorageInitDetail>`
/// (`BackingUp`, `Migrating`, `Verifying`) and `started_at_ms: Option<u64>`.
/// Clean break.
pub const PROTOCOL_VERSION: u32 = 37;

/// Initial options and postures when creating a thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadInitOptions {
    /// `--unattended` / unattended execution posture.
    #[serde(default)]
    pub unattended: bool,
    /// Whether workspace filesystem confinement is enforced (default true).
    #[serde(default = "default_confined")]
    pub confined: bool,
    /// Role id to staff this thread with. `None` = default workspace coding principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Resume the most recent matching thread instead of creating a new one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume: bool,
}

const fn default_confined() -> bool {
    true
}

impl Default for ThreadInitOptions {
    fn default() -> Self {
        Self {
            unattended: false,
            confined: true,
            role: None,
            resume: false,
        }
    }
}

impl ThreadInitOptions {
    pub fn new(unattended: bool, confined: bool) -> Self {
        Self {
            unattended,
            confined,
            role: None,
            resume: false,
        }
    }

    pub fn with_role(mut self, role: Option<String>) -> Self {
        self.role = role;
        self
    }

    pub fn with_resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    pub fn is_default(&self) -> bool {
        !self.unattended && self.confined && self.role.is_none() && !self.resume
    }
}

/// Target for subscribing a connection to a thread (`Wire::Subscribe`, ADR-0260).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadTarget {
    /// Create a fresh thread in the caller's project.
    New {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<ThreadInitOptions>,
    },
    /// Attach to a specific thread by ID, or auto-resolve the caller's project
    /// thread when `id` is `None`.
    Attach {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// Open a lightweight picker carrier thread for the threads browser.
    Picker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<ThreadInitOptions>,
    },
}

/// Single-shot session-management verbs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum ControlRequest {
    Shutdown,
    /// ADR-0034 Level 1: re-read configuration and re-sync MCP + skills
    /// without dropping connections.
    Reload,
    CreateThread {
        project: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        init_options: Option<ThreadInitOptions>,
    },
    SendPrompt {
        thread_id: String,
        text: String,
    },
    Interrupt {
        thread_id: String,
    },
    ResolvePermission {
        thread_id: String,
        request_id: String,
        decision: PermissionDecision,
    },
    KillThread {
        thread_id: String,
    },
    /// ADR-0171: the role-scoped durable history listing. Durable rows merge
    /// with resident drivers; records with no live driver come back as
    /// `ThreadHosting::Dormant`.
    ListThreads {
        filter: ThreadFilter,
    },
    /// ADR-0171: stop hosted threads (runtime only). The durable transcript
    /// survives and the next attach lazy-resumes it.
    StopThreads {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<ThreadFilter>,
        ids: Vec<String>,
    },
    /// Fork a branch cursor in a live thread. The branch shares the immutable
    /// fact prefix; no conversation bytes are copied.
    ForkBranch {
        thread_id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_fact_id: Option<String>,
    },
    /// List every branch cursor of a thread.
    ListBranches {
        thread_id: String,
    },
    /// Switch the thread's active branch.
    SwitchBranch {
        thread_id: String,
        branch_id: String,
    },
    /// Delete a branch cursor. Facts reachable from another branch survive.
    DeleteBranch {
        thread_id: String,
        branch_id: String,
    },
    /// ADR-0171: permanently delete durable thread history. The ids are the
    /// exact snapshot the operator confirmed; `filter` guards the scope.
    PurgeThreads {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<ThreadFilter>,
        ids: Vec<String>,
    },
    SuspendThread {
        thread_id: String,
    },
    AskArchivist {
        text: String,
    },
    /// ADR-0045: Dynamically open or bind a TCP listener on the running server.
    OpenTcpListener {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
    /// ADR-0045: Dynamically close the TCP listener on the running server.
    CloseTcpListener,
    /// ADR-0045: Query runtime endpoint status (port, token, UDS path).
    GetServerEndpoints,
    /// ADR-0129 [INV-USAGE-01]: Read the cross-session model usage report on
    /// the session-free control plane, independent of any active thread.
    ///
    /// The report aggregates the durable day-partitioned store under
    /// `data/usage/`, which outlives every session that ever wrote to it, so
    /// routing the question through a session's duplex made a global statistic
    /// look thread-scoped: the answer arrived on whichever thread happened to
    /// be attached, and no thread at all meant no answer. The reply's
    /// free-string channel carries the JSON-encoded
    /// [`crate::usage_stats::UsageStatsReport`], the same convention
    /// [`ControlRequest::AskArchivist`] and
    /// [`ControlRequest::GetServerEndpoints`] use for their results.
    GetUsageStats {
        /// How many recent terminal requests to include in the event tail.
        event_cap: usize,
    },
}

/// Dynamic server endpoint state report (ADR-0045).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEndpointsReport {
    pub tcp_port: u16,
    pub tcp_active: bool,
    pub token: Option<String>,
    pub local_endpoint: Option<String>,
}

/// The unified wire envelope on every connection.
///
/// Payloads are **nested** (`{"type":"Response","response":{…}}`), never
/// flattened into the envelope. `#[serde(flatten)]` forces the decoder to
/// buffer the frame into an intermediate map before it can name the inner
/// enum, which is a per-frame allocation and copy on the stream hot path —
/// the one place a frame is measured in tens of microseconds. A nested field
/// decodes straight into its type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum Wire {
    /// Session-free handshake frame declaring protocol version, posture, and
    /// optional server-observability subscription (ADR-0260 `[INV-SRVPHASE-02]`).
    Hello {
        /// The wire protocol this client speaks. **Required.**
        protocol: u32,
        /// The product build, advisory identity only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default)]
        posture: human_request::HumanChannelPosture,
        /// When `Some`, includes an initial [`MonitorSnapshot`] in
        /// [`Wire::ServerHello`] and (if `watch: true`) streams live
        /// [`MonitorEvent`] updates over this same connection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        monitor: Option<MonitorAction>,
    },
    /// Immediate server-level handshake acknowledgement (ADR-0260 `[INV-SRVPHASE-02]`).
    ///
    /// Sent as soon as protocol compatibility is verified — before any thread
    /// is claimed and regardless of whether `phase` is `Initializing`, `Ready`,
    /// `Reloading`, `Draining`, or `Failed`.
    ServerHello {
        server_id: String,
        protocol: u32,
        version: String,
        started_at: u64,
        phase: ServerPhase,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persistence_health: Option<PersistenceHealth>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        monitor: Option<MonitorSnapshot>,
    },
    /// Subscribe this connection to a thread (create new, attach, or picker)
    /// without tearing down existing subscriptions (ADR-0251, ADR-0260).
    Subscribe {
        request_id: u64,
        target: ThreadTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<std::path::PathBuf>,
    },
    /// Server acknowledgement for [`Wire::Subscribe`], preceding that thread's
    /// authoritative [`AgentResponse::ThreadSnapshot`] and `ThreadPhase::Ready`.
    Subscribed {
        request_id: u64,
        thread_id: String,
        #[serde(default)]
        generation: u64,
        phase: ThreadPhase,
    },
    /// Server response to an ambiguous auto-attach [`Wire::Subscribe`].
    Pick {
        request_id: u64,
        threads: Vec<ThreadOverview>,
    },
    /// Detach a subscribed thread from this connection, releasing its
    /// `HumanChannel` hold and stopping event forwarding (ADR-0251).
    Unsubscribe { thread_id: String },
    /// Full-duplex client agent request envelope.
    ///
    /// On a multiplexed connection (ADR-0251), `thread_id` names the target
    /// subscribed thread explicitly. `None` targets the connection's primary
    /// thread (single-thread clients).
    Request {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        request: AgentRequest,
    },
    /// Full-duplex server agent response envelope.
    Response {
        response: AgentResponse,
        #[serde(default, skip_serializing_if = "is_zero_u64")]
        generation: u64,
    },
    /// Session-free control-plane verb over the unified connection (ADR-0260).
    Control {
        request_id: u64,
        request: ControlRequest,
    },
    /// Reply to a [`Wire::Control`] verb.
    ControlReply {
        request_id: u64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// ADR-0171: the verb's JSON-encoded result. Data-bearing verbs
        /// (`ListThreads`, `StopThreads`, `PurgeThreads`, usage/endpoint
        /// reports, Archivist answers) carry their answer here; verbs without
        /// a result leave it `None`. The `error` field is errors only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<String>,
    },
    /// Server lifecycle & observability event envelope.
    Monitor { event: MonitorEvent },
    /// Connection-level or request-correlated error envelope.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u64>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// Maximum wire frame payload length: 64 MiB (aligned with WebSocket's 64 MiB
/// default ceiling so long hydrated transcripts never fail native IPC framing).
pub const MAX_WIRE_FRAME_SIZE: usize = 64 * 1024 * 1024;

/// One successfully de-framed item read off a wire stream.
///
/// The distinction this type exists for: **an undecodable payload is a fact
/// about one frame, not about the connection.**
///
/// Both transports are self-delimiting — native IPC carries a 4-byte length
/// prefix, WebSocket carries message boundaries — so by the time a payload
/// fails to parse, its bytes have already been consumed and the stream is
/// still *aligned* on the next frame. A peer one protocol revision away (a
/// server binary replaced on disk under a running process, a variant deleted
/// here but still emitted there) sends exactly this: a well-framed envelope
/// naming a variant this build does not model.
///
/// Encoding that as `Err` made it indistinguishable from a dead socket, so
/// every consumer was forced to treat "I don't know this frame" as "the link
/// is gone" — and the TUI, lacking any other terminal signal, closed the
/// window [ADR-0136].
///
/// So the decoder reports it here and keeps going. `Err` from the codec now
/// means *only* that framing itself broke (an impossible length prefix), i.e.
/// something that genuinely cannot be resynchronized.
// A `Wire` payload is inherently large; the same exemption the envelope itself
// carries, for the same reason.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum WireFrame {
    /// A fully modelled envelope.
    Wire(Wire),
    /// A frame whose bytes were consumed but whose payload names something
    /// this build does not model. The stream stays usable; the caller's job is
    /// to *disclose* the skew, never to interpret it as a disconnect.
    Undecodable {
        /// The envelope's own `type` discriminator, when the payload is
        /// well-formed JSON (`"Request"`, `"Response"`, …). Named separately
        /// because serde's message for a `#[serde(flatten)]`ed variant reports
        /// only the *inner* enum, which leaves an operator unable to tell which
        /// direction of the connection drifted.
        envelope: Option<String>,
        /// The decoder's message, carrying the offending tag and the variants
        /// this build does expect.
        detail: String,
    },
}

/// Parse one payload (already de-framed) into a [`WireFrame`].
///
/// Shared by every transport so the "undecodable is not fatal" rule has
/// exactly one implementation rather than one per codec.
pub fn decode_wire_payload(payload: &[u8]) -> WireFrame {
    match serde_json::from_slice::<Wire>(payload) {
        Ok(wire) => WireFrame::Wire(wire),
        Err(error) => WireFrame::Undecodable {
            // Best effort, and only ever used for the disclosure: the payload
            // is valid JSON whenever the failure is a *shape* mismatch, which
            // is the whole case this arm exists for.
            envelope: serde_json::from_slice::<serde_json::Value>(payload)
                .ok()
                .and_then(|value| {
                    value
                        .get("type")
                        .and_then(|tag| tag.as_str())
                        .map(str::to_string)
                }),
            detail: error.to_string(),
        },
    }
}

/// Length-delimited JSON codec for native IPC streams.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeWireCodec;

impl Decoder for NativeWireCodec {
    type Item = WireFrame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }

        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_be_bytes(length_bytes) as usize;

        if length > MAX_WIRE_FRAME_SIZE {
            // Framing itself is unusable: there is no way to resynchronize a
            // stream whose next length prefix cannot be trusted. This is the
            // *only* remaining fatal decode outcome.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Wire frame length {length} exceeds maximum limit {MAX_WIRE_FRAME_SIZE}"),
            ));
        }

        if src.len() < 4 + length {
            src.reserve(4 + length - src.len());
            return Ok(None);
        }

        src.advance(4);
        let payload = src.split_to(length);

        // `split_to` already removed the frame, so the stream is aligned on
        // the next one whatever the payload contains — an unknown variant is
        // reported per frame, not as a dead connection.
        Ok(Some(decode_wire_payload(&payload)))
    }
}

impl Encoder<Wire> for NativeWireCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Wire, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // Serialize straight into the frame buffer behind a reserved length
        // prefix. The previous shape allocated a complete `Vec` per frame and
        // copied it in — two allocations and a full copy for every streamed
        // delta.
        let start = dst.len();
        dst.put_u32(0);
        let serialized = {
            let mut writer = dst.writer();
            serde_json::to_writer(&mut writer, &item)
        };
        if let Err(error) = serialized {
            dst.truncate(start);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to serialize Wire payload: {error}"),
            ));
        }

        let length = dst.len() - start - 4;
        if length > MAX_WIRE_FRAME_SIZE {
            dst.truncate(start);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Serialized Wire payload length {length} exceeds maximum limit {MAX_WIRE_FRAME_SIZE}"
                ),
            ));
        }
        dst[start..start + 4].copy_from_slice(&(length as u32).to_be_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod readiness_protocol_tests {
    use super::*;
    use nuo_tool::{CommandCatalog, Role};

    /// ADR-0136 `[INV-WIRE-01]`: an envelope naming a variant this build does
    /// not model is a *per-frame* fact, never a dead stream.
    ///
    /// The regression this pins: `RetryResolved` was deleted from the wire
    /// enum without a protocol bump, so a still-running server kept emitting
    /// it. The client's decoder returned `Err`, every consumer read `Err` as
    /// "the link died", and the TUI closed itself mid-round. Because both
    /// transports are self-delimiting the frame had already been consumed —
    /// the stream was still perfectly aligned, and the only thing that broke
    /// was the *typing* of the outcome.
    #[test]
    fn undecodable_frame_is_a_frame_outcome_not_a_stream_error() {
        // A well-framed payload naming a variant that no longer exists here.
        let payload = br#"{"type":"Response","response":{"RetryResolved":{"attempt":1}}}"#;
        let frame = decode_wire_payload(payload);
        match frame {
            WireFrame::Undecodable { envelope, detail } => {
                assert_eq!(
                    envelope.as_deref(),
                    Some("Response"),
                    "the disclosure must name the envelope direction, got: {detail}"
                );
                assert!(
                    detail.contains("RetryResolved"),
                    "the disclosure must name the variant that failed, got: {detail}"
                );
            }
            WireFrame::Wire(other) => {
                panic!("an unknown variant must not decode as a known envelope: {other:?}")
            }
        }
    }

    /// `[INV-WIRE-07]`: a frame that fails to encode leaves the buffer exactly
    /// as it was. A half-written frame would shift every subsequent length
    /// prefix, turning one rejected payload into a dead stream.
    #[test]
    fn an_oversize_frame_is_not_left_in_the_buffer() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(b"keep");
        let before = buf.len();

        let error = NativeWireCodec
            .encode(
                Wire::Error {
                    request_id: None,
                    message: "x".repeat(MAX_WIRE_FRAME_SIZE + 1),
                    code: None,
                },
                &mut buf,
            )
            .expect_err("an oversize frame must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            buf.len(),
            before,
            "a failed encode must leave the frame buffer untouched"
        );
        assert_eq!(&buf[..], b"keep");
    }

    /// The frame is consumed either way, so the *next* frame on the same
    /// length-prefixed stream must decode normally. This is what makes
    /// "continue reading" a sound recovery instead of a desynchronized guess.
    #[test]
    fn the_stream_stays_aligned_after_an_undecodable_frame() {
        let unknown = br#"{"type":"Response","response":{"NoSuchVariant":{}}}"#;
        let good = serde_json::to_vec(&Wire::Error {
            request_id: None,
            message: "still here".to_string(),
            code: None,
        })
        .unwrap();

        let mut buf = BytesMut::new();
        NativeWireCodec
            .encode(
                Wire::Error {
                    request_id: None,
                    message: "first".to_string(),
                    code: None,
                },
                &mut buf,
            )
            .unwrap();
        // Hand-frame an undecodable payload exactly as a skewed peer would.
        buf.extend_from_slice(&(unknown.len() as u32).to_be_bytes());
        buf.extend_from_slice(unknown);
        buf.extend_from_slice(&(good.len() as u32).to_be_bytes());
        buf.extend_from_slice(&good);

        let first = NativeWireCodec.decode(&mut buf).unwrap();
        assert!(matches!(first, Some(WireFrame::Wire(Wire::Error { .. }))));

        let second = NativeWireCodec.decode(&mut buf).unwrap();
        assert!(
            matches!(second, Some(WireFrame::Undecodable { .. })),
            "the skewed frame reports itself without killing the codec"
        );

        let third = NativeWireCodec.decode(&mut buf).unwrap();
        assert!(
            matches!(third, Some(WireFrame::Wire(Wire::Error { .. }))),
            "the frame after the skewed one must still decode: alignment survives"
        );
    }

    /// ADR-0260: ServerHello is session-free and carries server lifecycle state.
    #[test]
    fn server_hello_is_session_free_and_carries_server_phase() {
        let hello = Wire::ServerHello {
            server_id: "srv-1".to_string(),
            protocol: PROTOCOL_VERSION,
            version: "1.0.0".to_string(),
            started_at: 1_700_000_000,
            phase: ServerPhase::initializing(ServerInitStage::Storage),
            persistence_health: None,
            monitor: None,
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains(r#""type":"ServerHello""#));
        assert!(json.contains(r#""phase":"initializing""#));
        assert!(json.contains(r#""stage":"storage""#));
        assert!(!json.contains("thread_id"));
    }

    /// ADR-0133 / ADR-0260: Subscribed carries identity and preparing phase only.
    #[test]
    fn subscribed_carries_identity_and_phase_only() {
        let sub = Wire::Subscribed {
            request_id: 7,
            thread_id: "thread-1".to_string(),
            generation: 1,
            phase: ThreadPhase::Preparing {
                stage: PrepareStage::Thread,
            },
        };
        let json = serde_json::to_string(&sub).unwrap();
        assert!(json.contains(r#""type":"Subscribed""#));
        assert!(json.contains(r#""phase":"preparing""#));
        assert!(json.contains(r#""thread_id":"thread-1""#));
        assert!(!json.contains("messages"));
        assert!(!json.contains("command_catalog"));
    }

    /// `[INV-ATTACH-01]` survives into the phase taxonomy: a post-Welcome
    /// failure carries its machine-readable code.
    #[test]
    fn thread_phase_failure_carries_its_code() {
        let phase = ThreadPhase::Failed {
            code: Some(ERR_SESSION_UNRESUMABLE.to_string()),
            message: "could not load".to_string(),
        };
        let json = serde_json::to_string(&phase).unwrap();
        assert!(json.contains(r#""phase":"failed""#));
        assert!(json.contains(r#""code":"session_unresumable""#));
    }

    #[test]
    fn thread_snapshot_is_the_complete_hydration_payload() {
        let snapshot = ThreadSnapshot {
            thread_id: "t".into(),
            round_counter: 3,
            messages: vec![nuo_tool::Message::new(Role::User, "hello")],
            provider: "p".into(),
            model: "m".into(),
            todos: Vec::new(),
            context_tokens: None,
            harness_state: None,
            activity: None,
            provider_picker: None,
            provider_keys: None,
            commands: Vec::new(),
            round_interrupts: Vec::new(),
            command_catalog: Some(CommandCatalog::default()),
            watermark_seq: 0,
        };
        let back: ThreadSnapshot =
            serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        assert_eq!(back.thread_id, "t");
        assert_eq!(back.round_counter, 3);
        assert_eq!(back.messages.len(), 1);
        assert!(back.command_catalog.is_some());
    }
}
