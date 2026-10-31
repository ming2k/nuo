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
//! in `nuo-harness` (ADR-0057).
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

pub mod discovery;
#[cfg(unix)]
pub use discovery::default_uds_path;
pub use discovery::{
    Discovery, ServerInfo, default_local_endpoint, global_discovery_path, global_lock_path,
    global_spawn_lock_path, instance_dir,
};
pub mod version;
pub use version::{
    ERR_PROTOCOL_MISMATCH, ERR_SESSION_GONE, ERR_SESSION_UNRESUMABLE, ERR_VERSION_MISMATCH,
    VersionRelation, compare_versions, incompatibility_error, protocol_accepts, protocol_mismatch,
    server_image_is_current, version_mismatch, versions_compatible,
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
    ProjectionDirective, PrunedMediaOutput, PrunedToolOutput, StatePayload, SubagentRef,
    Transcript, TranscriptEntry,
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
    InputContract, InputExpectation, InputPrompt, PatchOp, ShellTermination, ToolOutput,
    ToolStream, WebSearchHit,
};
pub mod tool_access;
pub use tool_access::{ToolAccess, ToolAccesses, ToolFileAccessOperation};

/// Re-exports the pure preview shape from `nuo-tool` so the wire vocabulary (the
/// `ToolPreview` event and `PermissionRequest::preview`) can name it without a
/// second definition. A mirrored struct here would be a second thing to keep in
/// sync, and the whole point of `PreviewResult` is that a preview and an applied
/// patch are one shape.
pub mod preview {
    pub use nuo_tool::preview::*;
}
pub use preview::PreviewResult;

pub mod auth;
pub use auth::{
    ChatGptAuthMetadata, CopilotAuthMetadata, CredentialSource, ExtensionMap, GoogleAuthMetadata,
    PreflightValidator, ResolvedAuth, StaticCredentialSource, static_credential,
};

pub mod tool_validation;

pub mod capability;
pub mod catalog;
pub mod connection_auth;
pub mod connection_detail;
pub mod model_providers;
pub mod provider_state;
pub mod wire_surface;
pub use nuo_provider::NUO_USER_AGENT;
pub mod effort;
pub use effort::{COMMON_LADDER, Effort, EffortLevel};
pub mod tuning;
pub use tuning::{EFFORT_KNOB, THINKING_KNOB, TunableSetting, TunableSettings};
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
    PreFlightRouteInput, PreFlightRouteOutput, PreFlightRouterTask, StreamLoopChannel,
    StreamLoopReviewInput, StreamLoopReviewerTask, StreamLoopVerdict, ThreadDigest,
    ThreadTitleInput, ThreadTitleTask, TrajectoryLoopReviewInput, TrajectoryLoopReviewerTask,
    TrajectoryLoopVerdict,
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
    CompactionPayload, ThreadEntry, ThreadEntryId, ThreadEntryKind, ThreadTree,
};
pub mod skills_config;
pub use shared_roots::{SharedAdditionalRoots, SharedConfinement};
pub mod tool_registry;
pub mod web_config;
pub use capability::{
    BuiltinTool, InputHandler, ModelRequest, Provider, ProviderEventStream, ProviderPromptHints,
    ProviderStreamEvent, ProviderTextStream, ProviderTurnContext, ScopeTarget, Tool,
    ToolInvocation, ToolSpec, VariantSelection, empty_variant_selection,
};
pub use catalog::{
    AnthropicMessagesDialect, Channel, GoogleGenerateContentDialect, GoogleInteractionsDialect,
    OpenAiChatDialect, OpenAiResponsesDialect, ProviderDialect, ProviderEntry, Transport,
};
pub use connection_auth::{ConnectionAuth, Entitlement, LoginMethod};
pub use connection_detail::{
    BalanceQuota, ConnectionDetail, ConnectionQuotaEntry, ConnectionUsageState,
    ModelAvailabilityRule, PeriodicQuota, ProviderQuotaData, ProviderQuotaSnapshot, ProviderUsage,
    QuotaWindowBucket, QuotaWindowKind, RateLimitSpec, UsageMetric,
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
    ModelTargetScope, NamedFilterPolicy, RemoteModelMetadata, WireProtocol, baseline_models,
    model_by_id, register_fitted_models, resolve as resolve_model, sanitize_model_id,
    simple_glob_matches,
};
pub use monitor::*;
pub use nuo_provider::{CatalogDiscovery, DiscoveredModel, ModelListError};
pub use pressure::{
    CLEARED_TOOL_PREFIX, CRUISE_HIGH_WATERMARK, CRUISE_LOW_WATERMARK, CompactionPolicy,
    ContextBudget, LayeredRequestWeights, MessageContentFingerprint, MessageTokenWeights,
    PRUNE_QUANTUM_FLOOR_TOKENS, PruneOutcome, RequestTokenEstimate, TAIL_QUARANTINE_TOKENS,
    ToolSchemaWeights, estimate_bytes, estimate_draft_tokens, estimate_message_tokens,
    estimate_semantic_json_tokens, estimate_tokens, estimate_tokens_weighted, freeze_tool_output,
    layered_request_weights, prune_tool_results,
};
pub use secret::SecretString;
pub use session_title::{TITLE_MAX_LEN, ThreadTitle, clean_title};
pub use skills_config::SkillsConfig;
/// The BPE token counter ([`crate::tokenizer`]) under the name the
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
///
/// 19: Full-stack canonical thread nomenclature unification (ADR-0135).
/// Wire protocol envelopes, phases, and snapshots purified from `session_id` to
/// `thread_id`; `SessionPhase` -> `ThreadPhase`, `SessionSnapshot` -> `ThreadSnapshot`,
/// `SessionOverview` -> `ThreadOverview`, `MonitoredSession` -> `MonitoredThread`.
pub const PROTOCOL_VERSION: u32 = 19;
/// Minimum supported client protocol version.
pub const MIN_PROTOCOL_VERSION: u32 = 19;

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

/// What role the connection wants to assume.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachAction {
    New(Option<ThreadInitOptions>),
    Attach(Option<String>),
    Picker(Option<ThreadInitOptions>),
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
            New(Option<ThreadInitOptions>),
            Attach(Option<String>),
            Picker(Option<ThreadInitOptions>),
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
    /// Server acknowledgement of an attached connection.
    ///
    /// `[INV-ATTACH-03]` (ADR-0133): a handle, not a payload. The server sends
    /// this as soon as it has claimed a thread identity — before opening the
    /// thread store, building the agent, loading skills, running hooks, or
    /// touching any provider credential. Hydration follows on the same
    /// connection as an authoritative [`crate::AgentResponse::ThreadSnapshot`],
    /// and readiness transitions arrive as
    /// [`crate::AgentResponse::ThreadPhase`].
    Welcome {
        thread_id: String,
        phase: ThreadPhase,
    },
    /// Server response to ambiguous attach / picker.
    Pick { threads: Vec<ThreadOverview> },
    /// Reply to single-shot control verb.
    ControlReply {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Full-duplex client agent request envelope.
    Request {
        #[serde(flatten)]
        request: AgentRequest,
    },
    /// Full-duplex server agent response envelope.
    Response {
        #[serde(flatten)]
        response: AgentResponse,
    },
    /// Server observability event envelope.
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

#[cfg(test)]
mod readiness_protocol_tests {
    use super::*;

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
        let payload = br#"{"type":"Response","RetryResolved":{"attempt":1}}"#;
        let frame = decode_wire_payload(payload);
        match frame {
            WireFrame::Undecodable { envelope, detail } => {
                assert_eq!(
                    envelope.as_deref(),
                    Some("Response"),
                    "the disclosure must name the envelope direction, got: {detail}"
                );
                assert!(
                    detail.contains("AgentResponse"),
                    "the disclosure must name the enum that failed, got: {detail}"
                );
            }
            WireFrame::Wire(other) => {
                panic!("an unknown variant must not decode as a known envelope: {other:?}")
            }
        }
    }

    /// The frame is consumed either way, so the *next* frame on the same
    /// length-prefixed stream must decode normally. This is what makes
    /// "continue reading" a sound recovery instead of a desynchronized guess.
    #[test]
    fn the_stream_stays_aligned_after_an_undecodable_frame() {
        let unknown = br#"{"type":"Response","NoSuchVariant":{}}"#;
        let good = serde_json::to_vec(&Wire::Error {
            message: "still here".to_string(),
            code: None,
        })
        .unwrap();

        let mut buf = BytesMut::new();
        NativeWireCodec
            .encode(
                Wire::Error {
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

    /// ADR-0133 / ADR-0135: Welcome is a handle, not a hydration payload.
    #[test]
    fn welcome_carries_identity_and_phase_only() {
        let welcome = Wire::Welcome {
            thread_id: "thread-1".to_string(),
            phase: ThreadPhase::Binding,
        };
        let json = serde_json::to_string(&welcome).unwrap();
        assert!(json.contains(r#""type":"Welcome""#));
        assert!(json.contains(r#""phase":"binding""#));
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
            messages: vec![Message::new(Role::User, "hello")],
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
