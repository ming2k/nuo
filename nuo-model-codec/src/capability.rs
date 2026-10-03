//! Foundational capability traits: how the harness talks to a model
//! ([`Provider`]) and to tools ([`Tool`]), the stream events a provider emits
//! ([`ProviderStreamEvent`]).

use crate::endpoint::TransportTelemetry;
use crate::message::Message;
use crate::usage::TokenUsage;
pub use nuo_tool::Tool;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

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
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// Build a neutral spec from any tool's name/description/parameters.
    pub fn from_tool(tool: &dyn Tool) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters_schema(),
        }
    }

    pub fn from_parts(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
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
    pub transport_telemetry: TransportTelemetry,
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
            transport_telemetry: TransportTelemetry::default(),
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
            transport_telemetry: TransportTelemetry::default(),
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
            transport_telemetry: TransportTelemetry::default(),
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

    /// Build a request with structured instructions and pre-computed tool declarations.
    pub fn with_instructions_and_tool_specs(
        instructions: crate::InstructionBundle,
        messages: Vec<Message>,
        mut tool_specs: Vec<ToolSpec>,
    ) -> Self {
        tool_specs.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            turn_context: Arc::default(),
            transport_telemetry: TransportTelemetry::default(),
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
            transport_telemetry: TransportTelemetry::default(),
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
        crate::CachePlan::new(
            crate::request_prefix_fingerprint(
                &self.instructions,
                &self.messages,
                &self.tool_specs,
            ),
            self.messages.len(),
            self.temporary_context.len(),
        )
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

// `InputHandler` / `ToolInvocation` were relocated to `nuo-wire::capability`
// (the session-adapter layer) when `nuo-contracts` was consolidated. The
// duplicates that used to live here had zero referents and are removed per
// ADR-0008 (single tool contract; no duplicate vocabulary).
