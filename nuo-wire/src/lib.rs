//! Canonical wire-level message framing, envelopes, and session communication
//! types for Nuo — plus the shared domain contracts (capability traits,
//! conversation and tool-output types, the context-pressure model, subagent
//! profiles, skills/MCP config schemas, and the events exchanged by sessions
//! and frontends).
//!
//! Conforming to ADR-0005, this crate is a **zero-I/O, zero-async-runtime**
//! contract layer: no filesystem, no network. It owns the serialized byte
//! envelopes exchanged across local IPC and network boundaries, and the domain
//! values (`TokenUsage`, `TodoList`, the `Provider`/`Tool` capability traits,
//! …) shared by independent layers. Pure logic owned only by the agent belongs
//! in `nuo-agent` (ADR-0057).
//!
//! Consolidated from the former `nuo-contracts` crate: ADR-0001 mandated that
//! crate's dismantling (`[INV-ARCH-01]` bans catch-all `contracts`/`common`/
//! `shared`/`core` crates), and ADR-0005 designated `nuo-wire` as the home for
//! wire envelopes and session communication entities.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io;

use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

pub use async_trait::async_trait;

// ── Domain & wire contract modules (consolidated from `nuo-contracts`) ──────

pub mod color_scheme_config;
pub use color_scheme_config::{
    ColorSchemeConfig, CommandThemeConfig, ComponentThemesConfig, CrateThemeConfig,
    DialogThemeConfig, DiffThemeConfig, FeedbackThemeConfig, FeedbackToneConfig, InputThemeConfig,
    KeycapThemeConfig, OverlayThemeConfig, SheetThemeConfig, SurfacesThemeConfig, ThemeFile,
    ViewThemeConfig,
};
pub mod cache;
pub use cache::{
    CachePlan, CacheResolutionError, CacheRetention, PromptCacheCapabilities, PromptCacheMode,
    PromptCacheModePreference, PromptCachePreference, PromptCacheSpec, PromptCacheUsage,
    ResolvedCachePolicy, read_prompt_cache_usage,
};
pub mod request_projection;
pub use request_projection::RequestProjection;
pub mod usage;
pub use usage::TokenUsage;

pub mod error;
pub use error::{
    HarnessError, ProviderError, ProviderErrorKind, RetryDisposition, ToolError, ToolErrorKind,
};

pub mod message;
pub use message::{
    ImagePart, InjectionKind, InjectionOrigin, Message, Role, SubagentMeta, ToolCall, ToolResult,
};

pub mod transcript;
pub use transcript::{
    DirectiveKind, DirectivePayload, EntryKind, EntryOrigin, EntryPayload, MessagePayload,
    ProjectionDirective, PrunedMediaOutput, PrunedToolOutput, StatePayload, SubagentRef, Transcript, TranscriptEntry,
};

pub mod instructions;
pub use instructions::{InstructionBundle, InstructionSlice, InstructionTier};

pub mod command;
pub use command::*;

pub mod mention;

pub mod completion;
pub use completion::{
    CommandAlias, CommandCatalog, CommandExample, CommandSpec, CommandSubcommandSpec,
    CommandSuggestion, ComposerCompletion, ComposerCompletionKind, InputCompletion,
    InputCompletionKind,
};

pub mod tool_output;
pub use tool_output::{
    InputContract, InputExpectation, InputPrompt, PatchOp, ShellTermination, ToolOutput, ToolStream,
    WebSearchHit,
};
pub mod tool_access;
pub use tool_access::{ToolAccess, ToolAccesses, ToolFileAccessOperation};

pub mod auth;
pub use auth::{
    ChatGptAuthMetadata, CopilotAuthMetadata, CredentialSource, ExtensionMap, GoogleAuthMetadata,
    OpencodeAuthMetadata, PreflightValidator, ResolvedAuth, StaticCredentialSource,
    static_credential,
};

pub mod tool_validation;

pub mod capability;
pub mod catalog;
pub mod client_identity;
pub mod connection_auth;
pub mod connection_detail;
pub mod model_providers;
pub mod provider_auth;
pub mod provider_state;
pub mod wire_surface;
pub use client_identity::{
    ClientCapabilities, ClientIdentity, ClientPreset, ClientProfile, ClientProfileSpec,
    NUO_USER_AGENT, OPENCODE_CLIENT_HEADERS, OPENCODE_USER_AGENT, OPENCODE_VERSION,
};
pub mod effort;
pub use effort::{COMMON_LADDER, Effort, EffortLevel};
pub mod reasoning;
pub use reasoning::{ReasoningMode, ReasoningSupport};
pub mod dynamic;
pub mod events;
pub use events::*;
pub mod hooks;
pub mod mcp;
pub mod model;
pub mod todos;
pub use todos::{MAX_TODOS, TodoId, TodoItem, TodoList, TodoStatus};
pub mod agent_kind;
pub mod agent_role;
pub mod cognitive;
pub mod execution_policy;
pub mod extension;
pub use extension::{Extension, HookPhase};
pub mod hazard;
pub use hazard::*;
pub mod job;
pub mod subagent;
pub use agent_kind::AgentKind;
pub use agent_role::{
    Agent, AgentRole, AgentRoleDelegation, AgentRoleProfile, AgentRuntimeConfig, DelegationPolicy,
    MainAgent, MainAgentRole, SessionRoleManifest, SubAgent, SubAgentRole,
};
pub use cognitive::{
    CognitiveModelPreference, CognitiveTask, EnvironmentReminderOutput, EnvironmentSensorInput,
    EnvironmentSensorTask, ExecutionTier, HarnessTask, HarnessTaskModelPreference,
    PreFlightRouteInput, PreFlightRouteOutput, PreFlightRouterTask, SessionDigest,
    SessionTitleInput, SessionTitleTask, StreamLoopChannel, StreamLoopReviewInput,
    StreamLoopReviewerTask, StreamLoopVerdict, TrajectoryLoopReviewInput,
    TrajectoryLoopReviewerTask, TrajectoryLoopVerdict,
};
pub use execution_policy::{ContextLifecycle, ExecutionPolicy, PolicyViolation};
pub mod history;
pub use history::*;
pub mod human_request;
pub use human_request::*;
pub mod identity;
pub mod pressure;
pub mod token_ledger;
pub mod tokenizer;
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
pub mod trajectory_guard_config;
pub use execution::{
    DirEntry, ExecutionEnvironment, FsError, FsMetadata, FsProvider, ProcessOutput, ProcessRunner,
    ShellIsolation, ToolMiddleware,
};
pub use hazard::{HazardLevel, HazardTier, ProcessKillSpec, ToolPermissionSubmission};
pub use security::{
    AssetLocator, AssetSpec, AttestationStatus, TrustDomain, WorkspaceSecuritySnapshot,
    WorkspaceTrustState,
};

pub mod workspace;
pub use workspace::{SessionPartition, WorkspaceBinding, WorkspaceFilter};

pub mod session_title;

pub mod context_lifecycle;

pub mod session_ir;
pub use session_ir::{
    BeliefState, BudgetPolicy, CacheBoundary, CapabilityPolicy, CausalGraph, CausalNode,
    CompilationArtifact, CompilationStats, CompilerError, CompilerOptions, ExecutionStatus,
    GuardrailPolicy, InvalidationReason, NodeId, NodeKind, NodePayload, ObservationLifecycle,
    ObservationMetrics, RuleSet, SessionDelta, SessionIR, SessionPolicy, SessionState, StateUpdate,
    SuspensionReason, SystemNoticePayload, TerminationReason, TimelineCursor, TimelineKind,
    compile_session_request,
};

pub mod session_tree;
pub use session_tree::{
    CompactionPayload, SessionEntry, SessionEntryId, SessionEntryKind, SessionTree,
};
pub mod skills_config;
pub use shared_roots::{SharedAdditionalRoots, SharedConfinement};
pub mod tool_registry;
pub mod web_config;
pub use capability::{
    InputHandler, ModelRequest, Provider, ProviderEventStream, ProviderPromptHints,
    ProviderStreamEvent, ProviderTextStream, ProviderTurnContext, ScopeTarget, Tool,
    ToolInvocation, ToolSpec, VariantSelection, empty_variant_selection,
};
pub use catalog::{
    AnthropicMessagesDialect, Channel, GoogleGeminiDialect, GoogleGenerateContentDialect,
    OpenAiChatDialect, OpenAiResponsesDialect, ProviderDialect, ProviderEntry, Transport,
};
pub use connection_auth::{ConnectionAuth, LoginMethod};
pub use connection_detail::{
    BalanceQuota, ConnectionDetail, ConnectionUsageState, PeriodicQuota, ProviderQuotaData,
    ProviderUsage, QuotaWindowBucket, QuotaWindowKind, RateLimitSpec, UsageMetric,
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
pub use trajectory_guard_config::TrajectoryGuardConfig;
pub mod monitor;
pub use monitor::*;
pub use hooks::{
    Hook, HookContext, HookEvent, HookEventKind, HookOutcome, RestorePoint, SessionSource,
};
pub use identity::AgentIdentity;
pub use job::{
    AdoptionInfo, BackgroundJobInfo, BackgroundJobOutcome, BackgroundJobService, CrateChildBridge,
    JobId, JobKind, JobSpec, JobState, Readiness, RestartPolicy,
};
pub use mcp::{McpConnectionStatus, McpServerConfig};
pub use model::{
    Availability, BaselineModels, CapabilityOverrides, ConnectionFilterPolicy, DeclaredModel,
    FittedModel, Model, ModelCapabilities, ModelCapabilityPatch, ModelScopeConfig,
    ModelTargetScope, NamedFilterPolicy, RemoteModelMetadata, RouteCapabilities, WireProtocol,
    baseline_models, model_by_id, register_fitted_models, resolve as resolve_model,
    sanitize_model_id, simple_glob_matches,
};
pub use pressure::{
    CLEARED_TOOL_PREFIX, CRUISE_HIGH_WATERMARK, CRUISE_LOW_WATERMARK, CompactionPolicy,
    ContextBudget, LayeredRequestWeights, MessageContentFingerprint, MessageTokenWeights,
    PRUNE_QUANTUM_FLOOR_TOKENS, PruneOutcome, RequestTokenEstimate, TAIL_QUARANTINE_TOKENS,
    ToolSchemaWeights, estimate_bytes, estimate_draft_tokens, estimate_message_tokens,
    estimate_semantic_json_tokens, estimate_tokens, estimate_tokens_weighted, freeze_tool_output,
    layered_request_weights, prune_tool_results,
};
pub use secret::SecretString;
pub use session_title::{SessionTitle, TITLE_MAX_LEN, clean_title};
pub use skills_config::SkillsConfig;
/// The BPE token counter ([`crate::tokenizer`], ADR-0117) under the name the
/// heuristic estimator used to own: token prediction is BPE now, and callers
/// that imported `count_tokens` for budget-fitting (summary truncation)
/// must measure in the same unit as the projection thresholds.
pub use tokenizer::{StreamingCounter, Tokenizer, count_tokens, truncate_to_tokens};
pub use tool_output::truncate_utf8;
pub use tool_registry::{
    Capability, ToolCapabilityAudit, ToolContext, ToolContextBuilder, ToolDeclaration, ToolFactory,
    ToolPool, ToolPoolSnapshot, ToolScope, ToolSelection, ToolSet, WorkspaceRoot, WorkspaceRoots,
    collect_toolset,
};
pub use web_config::{
    BOCHA_SEARCH_ENDPOINT, DUCKDUCKGO_HTML_ENDPOINT, DUCKDUCKGO_LITE_ENDPOINT, EXA_SEARCH_ENDPOINT,
    JINA_READER_ENDPOINT, PARALLEL_SEARCH_ENDPOINT, SharedWebConfig, TAVILY_SEARCH_ENDPOINT,
    WebConfig, WebCredentialRequirement, WebCredentialStatus, WebEndpointRequirement,
    WebProviderAxis, WebProviderCapability, WebReaderProvider, WebRuntimeConfig, WebSearchConfig,
    WebSearchProvider, web_provider_capabilities,
};

pub mod provider_surface;
pub use provider_surface::{ApiRoot, CatalogShape, RemoteCatalogSource};

pub mod context_projection;
pub use context_projection::*;
pub mod policy_schema;
pub use policy_schema::*;

/// Current protocol version.
pub const PROTOCOL_VERSION: u32 = 14;
/// Minimum supported client protocol version.
pub const MIN_PROTOCOL_VERSION: u32 = 12;

/// Stable machine-readable error codes.
pub const ERR_PROTOCOL_MISMATCH: &str = "protocol_mismatch";
pub const ERR_VERSION_MISMATCH: &str = "version_mismatch";

/// Check whether the advertised client version is supported by this runtime protocol.
pub const fn protocol_accepts(client: u32) -> bool {
    matches!(client, MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION)
}

/// Initial options and postures when creating a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInitOptions {
    /// `--unattended` / unattended execution posture.
    #[serde(default)]
    pub unattended: bool,
    /// Whether workspace filesystem confinement is enforced (default true).
    #[serde(default = "default_confined")]
    pub confined: bool,
    /// Role id to staff this session with. `None` = default workspace coding principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Resume the most recent matching session instead of creating a new one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume: bool,
}

const fn default_confined() -> bool {
    true
}

impl Default for SessionInitOptions {
    fn default() -> Self {
        Self {
            unattended: false,
            confined: true,
            role: None,
            resume: false,
        }
    }
}

impl SessionInitOptions {
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

/// What role the connection wants to assume.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachAction {
    New(Option<SessionInitOptions>),
    Attach(Option<String>),
    Picker(Option<SessionInitOptions>),
    Control(ControlRequest),
    Monitor(MonitorAction),
}

impl Serialize for AttachAction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::New(None) => serializer.serialize_str("new"),
            Self::New(Some(opts)) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("new", opts)?;
                map.end()
            }
            Self::Attach(id) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("attach", id)?;
                map.end()
            }
            Self::Picker(None) => serializer.serialize_str("picker"),
            Self::Picker(Some(opts)) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("picker", opts)?;
                map.end()
            }
            Self::Control(req) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("control", req)?;
                map.end()
            }
            Self::Monitor(act) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("monitor", act)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for AttachAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum RawAttachAction {
            New(Option<SessionInitOptions>),
            Attach(Option<String>),
            Picker(Option<SessionInitOptions>),
            Control(ControlRequest),
            Monitor(MonitorAction),
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireHelper {
            Str(String),
            Structured(RawAttachAction),
        }

        match WireHelper::deserialize(deserializer)? {
            WireHelper::Str(s) => match s.as_str() {
                "new" => Ok(AttachAction::New(None)),
                "picker" => Ok(AttachAction::Picker(None)),
                other => Err(serde::de::Error::unknown_variant(other, &["new", "picker"])),
            },
            WireHelper::Structured(raw) => Ok(match raw {
                RawAttachAction::New(opts) => AttachAction::New(opts),
                RawAttachAction::Attach(id) => AttachAction::Attach(id),
                RawAttachAction::Picker(opts) => AttachAction::Picker(opts),
                RawAttachAction::Control(c) => AttachAction::Control(c),
                RawAttachAction::Monitor(m) => AttachAction::Monitor(m),
            }),
        }
    }
}

/// Single-shot session-management verbs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum ControlRequest {
    Shutdown,
    CreateSession {
        project: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        init_options: Option<SessionInitOptions>,
    },
    SendPrompt {
        session_id: String,
        text: String,
    },
    Interrupt {
        session_id: String,
    },
    ResolvePermission {
        session_id: String,
        request_id: String,
        decision: PermissionDecision,
    },
    KillSession {
        session_id: String,
    },
    SuspendSession {
        session_id: String,
    },
    AskArchivist {
        text: String,
    },
}

/// The unified wire envelope on every connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum Wire {
    /// Handshake frame declaring role, scope, and capabilities.
    Select {
        action: AttachAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<std::path::PathBuf>,
        #[serde(default)]
        posture: human_request::HumanChannelPosture,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<u32>,
    },
    /// Daemon response welcoming an attached connection.
    Welcome {
        session_id: String,
        round_counter: u64,
        messages: Vec<Message>,
        #[serde(default)]
        provider: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        round_interrupts: Vec<RoundInterrupt>,
        #[serde(default)]
        retry_resolutions: Vec<RetryResolution>,
        #[serde(default)]
        command_catalog: nuo_tool::completion::CommandCatalog,
    },
    /// Daemon response to ambiguous attach / picker.
    Pick {
        sessions: Vec<SessionOverview>,
    },
    /// Reply to single-shot control verb.
    ControlReply {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Full-duplex client agent request envelope.
    Request {
        #[serde(flatten)]
        request: AgentRequest,
    },
    /// Full-duplex daemon agent response envelope.
    Response {
        #[serde(flatten)]
        response: AgentResponse,
    },
    /// Daemon observability event envelope.
    Monitor {
        #[serde(flatten)]
        event: MonitorEvent,
    },
    /// Connection-level error envelope.
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

/// Maximum wire frame payload length: 16 MB.
pub const MAX_WIRE_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Length-delimited JSON codec for native IPC streams.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeWireCodec;

impl Decoder for NativeWireCodec {
    type Item = Wire;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }

        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_be_bytes(length_bytes) as usize;

        if length > MAX_WIRE_FRAME_SIZE {
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

        serde_json::from_slice(&payload).map(Some).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize Wire payload: {e}"),
            )
        })
    }
}

impl Encoder<Wire> for NativeWireCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Wire, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let serialized = serde_json::to_vec(&item).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to serialize Wire payload: {e}"),
            )
        })?;

        if serialized.len() > MAX_WIRE_FRAME_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Serialized Wire payload length {} exceeds maximum limit {}",
                    serialized.len(),
                    MAX_WIRE_FRAME_SIZE
                ),
            ));
        }

        dst.reserve(4 + serialized.len());
        dst.put_u32(serialized.len() as u32);
        dst.put_slice(&serialized);
        Ok(())
    }
}
