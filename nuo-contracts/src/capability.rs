//! Foundational capability traits: how the harness talks to a model
//! ([`Provider`]) and to tools ([`Tool`]), the stream events a provider emits
//! ([`ProviderStreamEvent`]).

use crate::tool_access::ToolAccesses;
use crate::usage::TokenUsage;
use crate::{Message, SubagentEvent, ToolOutput, ToolStream};
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Transient provider-owned state shared by requests in one user round.
/// Each provider namespaces its slots by route. Never serialized or included
/// in prompt fingerprints; dropping the round releases its routing tokens.
#[derive(Default)]
pub struct ProviderTurnContext {
    slots: Mutex<HashMap<String, Arc<OnceLock<String>>>>,
}

impl std::fmt::Debug for ProviderTurnContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderTurnContext")
            .finish_non_exhaustive()
    }
}

impl ProviderTurnContext {
    pub fn slot(&self, key: String) -> Arc<OnceLock<String>> {
        self.slots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(key)
            .or_default()
            .clone()
    }
}

/// Per-model (and per-subagent-profile) variant selection: a map from a
/// capability name (a [`Tool::name`]) to the [`Tool::variant`] id chosen for
/// it. When the agent resolves its toolset for the active model, a capability
/// listed here is realized by its named variant; capabilities absent from the
/// map fall back to their default variant. This is how one logical toolset can
/// hand different models a genuinely different *implementation* of a tool
/// (different description, schema, and behaviour) rather than a re-worded copy
/// of a single impl.
///
/// Configured per model id under `[tool_variants."<model-id>"]` in
/// `config.toml`; the agent selects the map matching `Provider::model()`.
/// Subagent profiles carry their own static selection (see
/// [`crate::SubAgentProfile::variant_pins`]).
pub type VariantSelection = HashMap<String, String>;

/// Narrow prompt hints exposed by a concrete provider implementation.
///
/// The provider owns protocol facts (for example, how tool results or
/// thinking replay are represented on its wire surface), while the agent owns
/// whether and where those facts are inserted into model context. Empty by
/// default for test providers and simple adapters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderPromptHints {
    pub system_guidance: &'static str,
}

/// One immutable, provider-agnostic model request.
///
/// A provider-neutral tool declaration. This is the canonical, vendor-agnostic
/// shape the harness carries: adapters translate it into each provider's wire
/// format (OpenAI `{type:"function", function:{...}}`, Anthropic
/// `{name, description, input_schema}`, Google `functionDeclarations`, etc.).
///
/// Replacing the previous OpenAI-shape `serde_json::Value` canonical form
/// removes the coupling where every adapter had to reverse-engineer the OpenAI
/// nesting (`spec["function"]["name"]`) — adapters now read typed fields.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// The JSON Schema for the tool's parameters (a draft-07 object schema).
    pub parameters: serde_json::Value,
}

impl ToolSpec {
    /// Build a neutral spec from a [`Tool`]'s name/description/parameters.
    pub fn from_tool(tool: &dyn Tool) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters(),
        }
    }
}

/// Messages and tool declarations travel together so a provider never has to
/// retain request inputs in mutable side state. This is the contract exchanged
/// by the agent (which assembles model context) and provider adapters (which
/// serialize it into their protocol-specific wire shape).
#[derive(Debug, Clone, Serialize)]
pub struct ModelRequest {
    #[serde(skip)]
    pub turn_context: Arc<ProviderTurnContext>,
    /// The telemetry handle of the attempt issuing this request (ADR-0232).
    ///
    /// Created by the attempt's owner immediately before dispatch and filled by
    /// the transport that executes it. Runtime-only, like [`Self::turn_context`]:
    /// not serialized, not persisted, never on the wire — a monotonic clock
    /// reading is meaningful only inside the process that took it.
    ///
    /// A retry reuses the turn's assembled request, so the owner must stamp a
    /// *fresh* handle onto the per-attempt clone it dispatches. Sharing one
    /// across attempts would attribute one attempt's timings to another.
    #[serde(skip)]
    pub transport_telemetry: crate::TransportTelemetry,
    /// Structured instruction manifest (tiers: Base, Session, Task, Ephemeral).
    #[serde(default, skip_serializing_if = "crate::InstructionBundle::is_empty")]
    pub instructions: crate::InstructionBundle,
    pub messages: Vec<Message>,
    /// Request-local temporary context (`E_n`, ADR-0213/ADR-0217): optional,
    /// bounded, and never persisted to the durable interaction record. Provider
    /// adapters append it at the tail of the wire message sequence; it is
    /// deliberately excluded from the request envelope's revisions and from the
    /// cacheable prefix identity because it does not survive a request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub temporary_context: Vec<Message>,
    /// Tool declarations in the provider-neutral [`ToolSpec`] shape. Provider
    /// adapters translate these into their own wire format.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_specs: Vec<ToolSpec>,
    /// Whether this is a one-off request (for example title generation or
    /// summarization compaction). Prompt-cache intent is independent and must
    /// be expressed through `prompt_cache_preference`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ephemeral: bool,
    /// Explicit wire delivery plan selected from the conversation state and
    /// the concrete provider route.
    #[serde(default)]
    pub delivery: crate::RequestDelivery,
    /// Semantic context version for diagnostics and state validation.
    pub context_revision: crate::ContextRevision,
    pub context_relation: crate::ContextRelation,
    /// Request-envelope version (instructions/tools/controls).
    pub envelope_revision: crate::EnvelopeRevision,
    /// Per-request prompt-cache intent. Ephemeral requests default to disabled.
    pub prompt_cache_preference: crate::PromptCachePreference,
}

impl ModelRequest {
    /// Build a request without tools (title generation, summarization, tests).
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            turn_context: Arc::default(),
            transport_telemetry: crate::TransportTelemetry::default(),
            instructions: crate::InstructionBundle::default(),
            messages,
            temporary_context: Vec::new(),
            tool_specs: Vec::new(),
            ephemeral: false,
            delivery: crate::RequestDelivery::FullReplay,
            context_revision: crate::ContextRevision::empty(),
            context_relation: crate::ContextRelation::Initial,
            envelope_revision: crate::EnvelopeRevision::ephemeral(),
            prompt_cache_preference: crate::PromptCachePreference::default(),
        }
        .with_recomputed_revisions()
    }

    /// Build an ephemeral request without tools (e.g. title generation, compaction).
    pub fn ephemeral(messages: Vec<Message>) -> Self {
        Self {
            turn_context: Arc::default(),
            transport_telemetry: crate::TransportTelemetry::default(),
            instructions: crate::InstructionBundle::default(),
            messages,
            temporary_context: Vec::new(),
            tool_specs: Vec::new(),
            ephemeral: true,
            delivery: crate::RequestDelivery::FullReplay,
            context_revision: crate::ContextRevision::empty(),
            context_relation: crate::ContextRelation::Initial,
            envelope_revision: crate::EnvelopeRevision::ephemeral(),
            prompt_cache_preference: crate::PromptCachePreference::default(),
        }
        .with_recomputed_revisions()
    }

    /// Set instructions on the request.
    pub fn with_instructions(mut self, instructions: crate::InstructionBundle) -> Self {
        self.instructions = instructions;
        self.with_recomputed_revisions()
    }

    /// Set the ephemeral flag on the request.
    pub fn with_ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    /// Build a request and snapshot the supplied tool declarations atomically.
    /// Tool specifications are sorted deterministically by name to guarantee
    /// static prefix alignment for LLM prompt / KV-cache reuse across turns.
    pub fn with_tools(messages: Vec<Message>, tools: &[Arc<dyn Tool>]) -> Self {
        let mut tool_specs: Vec<ToolSpec> = tools
            .iter()
            .map(|t| ToolSpec::from_tool(t.as_ref()))
            .collect();
        tool_specs.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            turn_context: Arc::default(),
            transport_telemetry: crate::TransportTelemetry::default(),
            instructions: crate::InstructionBundle::default(),
            messages,
            temporary_context: Vec::new(),
            tool_specs,
            ephemeral: false,
            delivery: crate::RequestDelivery::FullReplay,
            context_revision: crate::ContextRevision::empty(),
            context_relation: crate::ContextRelation::Initial,
            envelope_revision: crate::EnvelopeRevision::ephemeral(),
            prompt_cache_preference: crate::PromptCachePreference::default(),
        }
        .with_recomputed_revisions()
    }

    /// Build a request with structured instructions and tool declarations.
    pub fn with_instructions_and_tools(
        instructions: crate::InstructionBundle,
        messages: Vec<Message>,
        tools: &[Arc<dyn Tool>],
    ) -> Self {
        let mut tool_specs: Vec<ToolSpec> = tools
            .iter()
            .map(|t| ToolSpec::from_tool(t.as_ref()))
            .collect();
        tool_specs.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            turn_context: Arc::default(),
            transport_telemetry: crate::TransportTelemetry::default(),
            instructions,
            messages,
            temporary_context: Vec::new(),
            tool_specs,
            ephemeral: false,
            delivery: crate::RequestDelivery::FullReplay,
            context_revision: crate::ContextRevision::empty(),
            context_relation: crate::ContextRelation::Initial,
            envelope_revision: crate::EnvelopeRevision::ephemeral(),
            prompt_cache_preference: crate::PromptCachePreference::default(),
        }
        .with_recomputed_revisions()
    }

    pub fn with_delivery(mut self, delivery: crate::RequestDelivery) -> Self {
        self.delivery = delivery;
        self
    }

    pub fn with_route_state(
        mut self,
        route: &crate::RouteFingerprint,
        mode: crate::ContinuationMode,
    ) -> Self {
        let (delivery, relation) = crate::select_request_delivery(&self.messages, route, mode);
        self.delivery = delivery;
        self.context_relation = relation;
        self
    }

    pub fn with_recomputed_revisions(mut self) -> Self {
        self.context_revision = crate::ContextRevision {
            sequence: self
                .messages
                .iter()
                .filter(|message| message.role != crate::Role::System)
                .count() as u64,
            head: Some(crate::semantic_context_head(self.messages.iter())),
        };
        self.envelope_revision = crate::EnvelopeRevision {
            sequence: 0,
            fingerprint: crate::request_envelope_fingerprint(
                &self.instructions,
                &self.messages,
                &self.tool_specs,
            ),
        };
        self
    }

    pub fn with_prompt_cache_preference(
        mut self,
        preference: crate::PromptCachePreference,
    ) -> Self {
        self.prompt_cache_preference = preference;
        self
    }

    /// Attach request-local temporary context (`E_n`). It is appended at the
    /// wire tail by adapters and excluded from the cacheable prefix identity, so
    /// it does not recompute the request revisions.
    pub fn with_temporary_context(mut self, temporary_context: Vec<Message>) -> Self {
        self.temporary_context = temporary_context;
        self
    }

    /// Borrow the request-local temporary context (`E_n`).
    pub fn temporary_context(&self) -> &[Message] {
        &self.temporary_context
    }

    /// Derive the provider-neutral cache plan: the identity of the cacheable
    /// prefix `S | H | I` plus message counts separated from `E`. See
    /// [`crate::CachePlan`] and ADR-0217.
    pub fn cache_plan(&self) -> crate::CachePlan {
        crate::CachePlan::from_request(self)
    }

    /// Borrow tool declarations in the optional form used by request builders.
    pub fn tool_specs(&self) -> Option<&[ToolSpec]> {
        (!self.tool_specs.is_empty()).then_some(self.tool_specs.as_slice())
    }

    /// Consume the request into the provider-facing `(messages, tool_specs)`
    /// pair. Request-local `E_n` is appended after the conversation so the wire
    /// tail matches the assembled request; it is never silent-dropped.
    pub fn into_parts(mut self) -> (Vec<Message>, Vec<ToolSpec>) {
        self.messages.append(&mut self.temporary_context);
        (self.messages, self.tool_specs)
    }
}

impl From<Vec<Message>> for ModelRequest {
    fn from(messages: Vec<Message>) -> Self {
        Self::new(messages)
    }
}

/// A shared empty [`VariantSelection`] map, handy as a default borrow target so
/// callers can always hand out `&VariantSelection` without an `Option`.
pub fn empty_variant_selection() -> &'static VariantSelection {
    static EMPTY: std::sync::LazyLock<VariantSelection> =
        std::sync::LazyLock::new(VariantSelection::new);
    &EMPTY
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderStreamEvent {
    /// Upstream model-catalog version advertised on an inference response.
    /// The harness consumes this control event internally; it is not content.
    ModelCatalogEtag(String),
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    /// Token usage reported by the provider at the end of a stream (e.g. from
    /// an Anthropic `message_delta` event carrying `usage`). Emitted *in
    /// addition to* the content deltas so the harness can book real
    /// `prompt_tokens` instead of estimating them. Providers that never report
    /// usage simply never emit this variant — the harness then falls back to
    /// the local char-class estimator.
    Usage(TokenUsage),
    /// Terminal metadata for this exact stream. A stream that ends without
    /// this event is incomplete and must not advance provider continuation.
    Completed(crate::ProviderCompletionMeta),
}

pub type ProviderTextStream = BoxStream<'static, Result<String, crate::ProviderError>>;
pub type ProviderEventStream =
    BoxStream<'static, Result<ProviderStreamEvent, crate::ProviderError>>;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(
        &self,
        request: ModelRequest,
    ) -> Result<crate::ProviderCompletion, crate::ProviderError>;
    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<ProviderTextStream, crate::ProviderError>;
    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<ProviderEventStream, crate::ProviderError> {
        let events = self
            .stream_chat(request)
            .await?
            .filter_map(|item| async move {
                match item {
                    Ok(delta) if delta.is_empty() => None,
                    Ok(delta) => Some(Ok(ProviderStreamEvent::TextDelta(delta))),
                    Err(error) => Some(Err(error)),
                }
            });
        Ok(events
            .chain(futures::stream::once(async {
                Ok(ProviderStreamEvent::Completed(
                    crate::ProviderCompletionMeta::default(),
                ))
            }))
            .boxed())
    }

    /// Stable provider/solution identifier (e.g. `"kimi-code"`, `"google"`).
    /// The harness stamps it onto assistant messages so a session that mixes
    /// multiple models stays traceable. Defaults to an empty string for
    /// providers (mostly test doubles) that don't carry an identity.
    ///
    /// Returns an owned [`String`] because the active provider may live behind
    /// a runtime-swappable proxy that cannot lend out a borrow across its lock.
    fn provider_id(&self) -> String {
        String::new()
    }
    /// The model identifier this provider targets (e.g. `"kimi-k2.7-code"`).
    /// Companion to [`Provider::provider_id`]; defaults to an empty string.
    fn model(&self) -> String {
        String::new()
    }

    /// The baseline wire protocol this provider communicates over (ADR-0161, ADR-0297).
    fn wire_protocol(&self) -> Option<crate::WireProtocol> {
        None
    }

    /// The resolved reasoning effort (depth) this channel runs its model
    /// requests with, as the wire string (`"high"`, `"max"`, …). Companion to
    /// [`Provider::provider_id`]/[`Provider::model`]: the harness stamps it
    /// onto assistant messages next to the provider/model attribution so the
    /// transcript can show the depth each turn actually ran at. Defaults to
    /// `None` for providers (mostly test doubles and sentinel channels) that
    /// carry no effort knob — including thinking-disabled Anthropic channels.
    fn effort(&self) -> Option<crate::effort::Effort> {
        None
    }

    /// Effective model capabilities for this concrete provider channel. The
    /// default resolves the static baseline by id; providers backed by a trusted
    /// remote catalogue override it with their channel-scoped snapshot.
    fn model_capabilities(&self) -> crate::ModelCapabilities {
        crate::ModelCapabilities::for_channel(&self.model(), None)
    }

    /// Provider/protocol-specific prompt hints for the agent's system prompt.
    ///
    /// This is not the agent's behavior contract. Providers should expose only
    /// narrow facts about their wire format or replay requirements; the agent's
    /// system-prompt policy decides if and how those hints are rendered.
    fn prompt_hints(&self) -> ProviderPromptHints {
        ProviderPromptHints::default()
    }

    /// Stable identity of the concrete protocol/endpoint/model route.
    fn route_fingerprint(&self) -> crate::RouteFingerprint {
        crate::RouteFingerprint(format!("{}:{}", self.provider_id(), self.model()))
    }

    /// How this route can carry prior response state.
    fn continuation_mode(&self) -> crate::ContinuationMode {
        crate::ContinuationMode::FullReplay
    }

    /// Toggle capture for debugging. When `enabled` is true, every
    /// request flowing through this provider is serialized — request messages,
    /// the streamed/returned response, provider id, model, and a timestamp — to
    /// one JSON file under `dir` (one file per round-trip). When `enabled` is
    /// false, capture stops and `dir` is ignored. Default is a no-op; the
    /// runtime proxy (`ProxyProvider`) overrides it so capture survives
    /// mid-session `/models` swaps. See the `/debug trace` command.
    ///
    /// This lives at the semantic layer (`Vec<Message>` in / events out), not
    /// the HTTP byte layer: request URLs, headers, and transport bytes are not
    /// captured — by design, to avoid leaking API keys (e.g. providers that put
    /// the key in the query string) and to stay independent of each provider's
    /// HTTP client.
    fn set_debug_capture(&self, _enabled: bool, _dir: PathBuf) {}

    /// Whether capture is currently armed on this provider. Defaults to
    /// `false`; the runtime proxy overrides it to report the live toggle state.
    fn debug_capture_enabled(&self) -> bool {
        false
    }

    /// Whether this provider surfaces real token usage from the upstream API.
    ///
    /// The harness uses this (together with [`ProviderStreamEvent::Usage`] or
    /// [`crate::ProviderCompletionMeta::usage`]) to decide whether a turn's
    /// token accounting is **reported** (authoritative) or **estimated** (local
    /// heuristic). The token-source report modal surfaces this distinction so
    /// the user can see which turns are measured and which are guessed.
    ///
    /// Defaults to `false`; concrete providers override it once they actually
    /// parse usage from their HTTP responses.
    fn usage_supported(&self) -> bool {
        false
    }
}

/// Runtime input supervisor for a supervised command invocation. Implemented
/// by the agent layer (which owns the human-input channel) and handed to the
/// command tool through [`Tool::input_handler`]. Kept as a trait so
/// `muta-contracts` stays free of agent/async-runtime coupling: the command
/// tool's examiner calls it, the agent fulfils it.
#[async_trait]
pub trait InputHandler: Send + Sync {
    /// Resolve one runtime input prompt. `Ok(Some(line))` — the operator's
    /// answer, written into the reported channel. `Ok(None)` — no answer
    /// (declined, or no reachable human channel); the caller kills the child
    /// with [`ShellTermination::InputUnanswered`](crate::ShellTermination::InputUnanswered).
    async fn resolve(&self, prompt: crate::tool_output::InputPrompt) -> Option<String>;
}

/// Everything a tool needs for one invocation beyond its own state: the call
/// identity, the raw arguments, the input-execution contract, and the runtime
/// input supervisor (when the dispatch layer supplied one). Bundled so the
/// trait method stays stable as per-call context grows.
pub struct ToolInvocation<'a> {
    /// The dispatch-generated call id (keys live streams and subagent views).
    pub call_id: &'a str,
    /// Raw JSON tool arguments exactly as the model emitted them.
    pub arguments: &'a str,
    /// How the child's input channels are provisioned (command tool only;
    /// other tools ignore it).
    pub input: crate::tool_output::InputContract,
    /// Runtime input supervisor for a supervised child. The dispatch layer
    /// builds it per invocation (it captures the live event channel), so it is
    /// borrowed rather than owned for the tool's lifetime.
    pub input_handler: Option<&'a dyn InputHandler>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    /// Compatibility aliases that can resolve to this tool during dispatch.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Whether this tool matches the requested dispatch name.
    fn matches_name(&self, requested: &str) -> bool {
        self.name() == requested || self.aliases().contains(&requested)
    }

    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    /// The variant id distinguishing this implementation from other variants of
    /// the same capability ([`name`](Self::name)). A capability with a single
    /// implementation uses the default; multiple variants of one capability
    /// share `name()` and differ only in `variant()`. The variant id never
    /// reaches the model — it is the selection key under
    /// `[tool_variants."<model-id>"]` config and in subagent profiles, by which
    /// a model or profile picks which implementation of a capability it sees.
    fn variant(&self) -> &str {
        "default"
    }

    /// Whether this tool is currently available/configured and should be admitted to
    /// model requests. Returning `false` hides the tool definition from prompts (saving
    /// tool schema tokens) and prevents model dispatch.
    fn is_available(&self) -> bool {
        true
    }

    /// Whether executing this tool may block awaiting a live human decision
    /// (e.g. `ask_user`, an approval-gated mode switch). Non-interactive
    /// execution contexts — subagents spawned for autonomous research — have
    /// no user reachable to answer, so a [`crate::subagent::ToolPolicy`] with
    /// `allow_user_interaction: false` excludes these. See ADR-0011.
    fn requires_user(&self) -> bool {
        false
    }

    /// Whether invoking this tool spawns a nested sub-agent (ADR-0183).
    fn spawns_subagent(&self) -> bool {
        false
    }

    /// Whether this tool cooperates with the harness's turn cancellation by
    /// observing [`Tool::request_cancel`] and draining its in-flight call to a
    /// terminal result instead of requiring the harness to drop the future.
    ///
    /// The default is `false`: most tools (bash, file I/O, web) cannot stop
    /// mid-call, so the harness keeps its fast drop-based cancellation for
    /// them. A tool that runs a nested agent (e.g. `task`) opts in because its
    /// in-flight call *owns a partial transcript* worth preserving — dropping
    /// it would discard real work the user may want to resume.
    fn supports_cooperative_cancel(&self) -> bool {
        false
    }

    /// Best-effort cooperative cancellation of an in-flight call identified by
    /// the harness-assigned `call_id`. The harness calls this when the user
    /// interrupts a turn, *then* waits a bounded grace period for the call to
    /// return a terminal result. Returns `true` if the tool accepted the
    /// request (it will stop at its next safe boundary); `false` if the call
    /// is unknown or already finished — the harness then falls back to
    /// dropping the future.
    ///
    /// Only consulted when [`Tool::supports_cooperative_cancel`] is `true`.
    /// The default rejects every request so non-cooperative tools keep their
    /// unchanged drop semantics.
    fn request_cancel(&self, _call_id: &str) -> bool {
        false
    }

    /// Whether this tool only functions on a model that can see images
    /// (vision). A vision-only tool (e.g. `read_image`, which feeds the model
    /// an image part) is useless — or actively misleading — on a text-only
    /// model, which strips image parts before the request hits the wire. This
    /// is a **model-capability requirement**, the symmetric counterpart of
    /// [`requires_user`](Self::requires_user): where that gates on whether a
    /// human is reachable, this gates on whether the model can perceive the
    /// tool's output.
    ///
    /// The pool resolver ([`crate::ToolSet::resolve_for`]) treats it as a
    /// **hard** filter: a variant whose `requires_vision()` a model cannot
    /// satisfy is never selectable for that model — it is simply absent from
    /// the resolved set, so no agent-side override can reinstate it. This is
    /// why model capability limits live on the scope/pool axis, not the soft
    /// override axis.
    fn requires_vision(&self) -> bool {
        false
    }

    /// Whether this tool exercises control over the harness itself (e.g. the
    /// abort/exit escape hatch), as opposed to the workspace/filesystem. This
    /// is orthogonal to [`Tool::scope_target`]: `scope_target` classifies *what
    /// the call touches*, while this classifies *process control*. Subagent
    /// profiles exclude control tools unconditionally — a spawned agent must
    /// never be able to tear down the whole program. A control tool bypasses
    /// the permission broker and scope gate entirely: it declares no
    /// [`ScopeTarget`] (the default [`ScopeTarget::Unspecified`]), so neither
    /// the scope gate nor the broker fires for it — it is gated solely by this
    /// flag.
    fn affects_control_flow(&self) -> bool {
        false
    }

    /// The operation target this call acts on, so the operation-scope gate can
    /// decide whether the call falls inside the agent's granted scope.
    ///
    /// Tools return a typed [`ScopeTarget`]: a file path for `write_file`/
    /// `edit_text`, the command string for `bash`, etc. The scope gate
    /// dispatches on the variant — `Path` targets are checked against the
    /// granted directory prefixes, `Command` targets against a command
    /// allowlist. [`ScopeTarget::Unspecified`] (the default) is admitted
    /// without a scope check, since the tool declares no locatable target.
    ///
    /// Like [`permission_label`](Self::permission_label), this never reaches
    /// the model.
    fn scope_target(&self, _arguments: &str) -> ScopeTarget {
        ScopeTarget::Unspecified
    }

    /// What this call touches, so the scheduler can decide whether it may run
    /// concurrently with the other calls in its batch. Returns a declarative
    /// [`ToolAccesses`] list consumed by the harness's concurrency scheduler.
    ///
    /// The **default** derives a *conservative* declaration from
    /// [`scope_target`](Self::scope_target), so existing tools get correct
    /// (if coarse) concurrency without override:
    ///
    /// | `scope_target` | derived `accesses` | concurrency effect |
    /// |---|---|---|
    /// | `Unspecified` | `none()` | freely parallelizable |
    /// | `Path(p)` | `read_write_file(p)` | serializes with any access to `p` |
    /// | `Command(_)` | `all()` | serializes with everything in the batch |
    ///
    /// Tools override this to declare a **precise** access (e.g. `read_file`
    /// for read-only tools, `search_tree` for content search, `read_tree` for a
    /// directory listing). Like [`scope_target`](Self::scope_target), this
    /// never reaches the model.
    fn accesses(&self, arguments: &str) -> ToolAccesses {
        match self.scope_target(arguments) {
            ScopeTarget::Unspecified => ToolAccesses::none(),
            ScopeTarget::Path(path) => {
                ToolAccesses::read_write_file(path.to_string_lossy().into_owned())
            }
            ScopeTarget::Command(_) => ToolAccesses::all(),
        }
    }

    /// Short, human-friendly label shown as the title of the permission
    /// prompt for `Write` tools. Defaults to the raw [`Tool::name`], which is
    /// fine when the name itself reads as a label (e.g. `bash`, `write_file`).
    /// Override when the name is a synthetic identifier whose meaning is not
    /// obvious to a user. Only consulted for tools that actually trigger a
    /// permission prompt.
    ///
    /// This is purely a UI string; it never reaches the model and is not
    /// part of the function schema sent to providers.
    fn permission_label(&self) -> String {
        self.name().to_string()
    }

    /// User-facing description shown in the body of the permission prompt
    /// (the "Details" section). Defaults to [`Tool::description`], which is
    /// appropriate when that text is written for humans. Override when
    /// [`Tool::description`] is model-facing instruction prose (constraints
    /// aimed at the model rather than a description of the call's effect)
    /// that would confuse a user reading the prompt. Keep overrides to one
    /// or two plain sentences describing *what the call does*, not *when
    /// the model should call it*.
    ///
    /// Like [`permission_label`](Self::permission_label), this never reaches
    /// the model.
    fn permission_description(&self) -> String {
        self.description().to_string()
    }

    /// Threat / hazard classification of this tool.
    ///
    /// Read-only / inspection tools return `HazardLevel::Safe` (default).
    /// Destructive or executing tools return their specific `HazardLevel`.
    fn hazard_level(&self) -> crate::hazard::HazardLevel {
        crate::hazard::HazardLevel::Safe
    }

    /// Build the tool-specific submission to the permission handler for a given set of arguments.
    ///
    /// Safe tools return `None` (no permission evaluation or prompt needed).
    /// Dangerous tools (file modification, command execution) submit their structured
    /// intent payload (file paths, command line + process kill spec).
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<crate::hazard::ToolPermissionSubmission> {
        if !self.hazard_level().requires_permission() {
            return None;
        }
        Some(crate::hazard::ToolPermissionSubmission {
            hazard_level: self.hazard_level(),
            label: self.permission_label(),
            description: self.permission_description(),
            scope: match self.scope_target(arguments) {
                crate::ScopeTarget::Command(c) => c,
                crate::ScopeTarget::Path(p) => p.to_string_lossy().into_owned(),
                crate::ScopeTarget::Unspecified => self.name().to_string(),
            },
            payload: crate::hazard::ToolPermissionPayload::Generic {
                summary: format!("Execute tool '{}'", self.name()),
                details: serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null),
            },
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String>;

    /// Structured result. Default delegates to [`call`](Self::call), wrapping
    /// the text as [`ToolOutput::Text`]. Tools override this to return richer
    /// variants (e.g. a shell exit code, a file patch) so callers render from
    /// data instead of string-sniffing. See ADR-0001. Migration is additive:
    /// unmigrated tools keep working through this default.
    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        self.call(arguments).await.map(ToolOutput::text)
    }

    /// Structured, event-emitting execution — the method the harness actually
    /// invokes so typed output reaches the transcript. Default delegates to
    /// [`call_structured`](Self::call_structured) and emits no events. Tools
    /// that spawn subagents (e.g. `task`) override this to forward child
    /// events while still returning a [`ToolOutput`] (typically [`ToolOutput::Text`]).
    ///
    /// [`ToolInvocation::input`] is the **execution contract** for how a child's
    /// input channels are provisioned ([`InputContract`]). It is decided
    /// *before* spawn by the agent dispatch layer (never from the model's
    /// arguments). The default [`InputContract::Sealed`] keeps tools that
    /// ignore input correct: a child that blocks on `read(stdin)` gets instant
    /// EOF instead of hanging silently until the wall-clock timeout.
    async fn call_structured_with_events<'a>(
        &self,
        invocation: ToolInvocation<'a>,
        _on_event: Box<dyn FnMut(SubagentEvent) + Send + 'a>,
        _on_stream: &mut (dyn FnMut(ToolStream) + Send + 'a),
    ) -> Result<ToolOutput, String> {
        self.call_structured(invocation.arguments).await
    }

    /// Execute the tool while optionally emitting events (e.g. subagent steps).
    ///
    /// The default implementation simply calls `call()` and emits no events.
    /// Tools that spawn subagents can override this to stream child events back
    /// to the parent harness.
    async fn call_with_events<'a>(
        &self,
        _call_id: &str,
        arguments: &str,
        _on_event: Box<dyn FnMut(SubagentEvent) + Send + 'a>,
    ) -> Result<String, String> {
        self.call(arguments).await
    }

    /// Generate an OpenAI-compatible function schema for this tool. This is the
    /// authoritative schema for the variant; per-model differences are expressed
    /// by selecting a different variant, not by patching this output.
    fn to_openai_function(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.description(),
                "parameters": self.parameters(),
            }
        })
    }
}

/// What a tool call acts on, so the operation-scope gate can match it against
/// the agent's granted scope. Tools report this via [`Tool::scope_target`];
/// each variant names a locatable target a tool may report. [`ScopeTarget::Unspecified`] is the default for tools with no locatable target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeTarget {
    /// A filesystem path the tool writes or reads (e.g. `write_file`, `edit_text`).
    /// Checked against the scope's granted directory prefixes.
    Path(std::path::PathBuf),
    /// A shell command string (e.g. `bash`). Checked against the scope's command
    /// allowlist, when one is set.
    Command(String),
    /// The tool declares no locatable target (e.g. `search_text`, `list_dir`). Admitted
    /// by the scope gate without a dimension check.
    Unspecified,
}

#[cfg(test)]
mod tests {
    use super::Tool;

    #[test]
    fn provider_turn_context_is_transient_and_not_prompt_content() {
        let request = super::ModelRequest::new(Vec::new());
        let before = serde_json::to_value(&request).unwrap();
        let token = request.turn_context.slot("route".into());
        token.set("opaque-token".into()).unwrap();
        let retry = request.clone();
        assert!(std::sync::Arc::ptr_eq(
            &request.turn_context,
            &retry.turn_context
        ));
        assert_eq!(serde_json::to_value(&retry).unwrap(), before);
        assert!(!format!("{request:?}").contains("opaque-token"));
        assert!(
            super::ModelRequest::ephemeral(Vec::new())
                .turn_context
                .slot("route".into())
                .get()
                .is_none()
        );
    }

    /// A minimal [`Tool`] stand-in so the schema tests can run without pulling
    /// in the whole tool crate.
    struct DummyTool {
        name: &'static str,
        variant: &'static str,
        desc: &'static str,
    }

    #[async_trait::async_trait]
    impl super::Tool for DummyTool {
        fn name(&self) -> &str {
            self.name
        }
        fn variant(&self) -> &str {
            self.variant
        }
        fn description(&self) -> &str {
            self.desc
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn desc_of(schema: &serde_json::Value) -> &str {
        schema["function"]["description"].as_str().unwrap_or("")
    }

    #[test]
    fn variant_defaults_to_default() {
        let tool = DummyTool {
            name: "read_text",
            variant: "default",
            desc: "built-in",
        };
        assert_eq!(tool.variant(), "default");
    }

    #[test]
    fn function_schema_uses_the_variant_own_description() {
        // A variant's own description is authoritative: the function schema
        // carries it verbatim, keyed by the shared capability name.
        let terse = DummyTool {
            name: "read_text",
            variant: "terse",
            desc: "terse wording",
        };
        let schema = terse.to_openai_function();
        assert_eq!(schema["function"]["name"], "read_text");
        assert_eq!(desc_of(&schema), "terse wording");
    }
}
