//! Provider/channel catalog: the two-layer abstraction over LLM backends.
//!
//! A [`ProviderEntry`] is a configured provider preset (e.g. `glm-cn`,
//! `kimi-code`) that owns one or more [`Channel`]s — delivery paths
//! distinguished by transport and endpoint. Each channel references a model by
//! its wire id; intrinsic model metadata (context window, capabilities) is
//! resolved from the [`crate::model`] registry, not duplicated per provider.
//!
//! This module owns the *types* and the provider *construction* path. It is
//! deliberately decoupled from any specific config struct: a [`Channel`] already
//! carries resolved credentials and the wire model id, so constructing a
//! provider from it (see `build_provider_for_channel` in the composition root (`nuo-server::provider_registry`))
//! is a pure operation. Resolution (environment variable then config field)
//! lives in the loader, not here, so the same types serve both built-in
//! presets and future user-defined entries.
//!
//! See `docs/adr/0015-dependency-inversion-architecture-for-model-providers.md`
//! for the design (service surface / channel as the first-class provider unit).

use std::fmt;

/// Provider-specific behavior layered on the OpenAI Chat Completions protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenAiChatDialect {
    #[default]
    Standard,
    Copilot,
    /// OpenRouter's normalized Chat Completions surface. It uses the unified
    /// `reasoning` object and returns replayable `reasoning_details` sidecars.
    OpenRouter,
    /// Qoder's COSY-signed SSE surface (`api*.qoder.sh`). The request body is
    /// the standard chat-completions JSON wrapped by QoderEncoding
    /// (custom-alphabet base64 + outer-third swap); headers carry the COSY
    /// signature set; the response is plain SSE whose `data:` payloads are
    /// HTTP-like envelopes (`{"body":"<chat.completion.chunk JSON>",..}`)
    /// terminated by an `event:finish` frame.
    Qoder,
    /// DeepSeek's Chat Completions surface. Standard JSON in/out, but the
    /// thinking contract differs: past assistant turns must carry their
    /// `reasoning_content` back whenever the request carries `tools`, because
    /// upstream concatenates that CoT into the context (Thinking Mode guide,
    /// 2026-10). Dropping it silently shortens the model's reasoning context
    /// across every tool round trip (ADR-0083).
    DeepSeek,
    /// Tencent AI CodeBuddy Chat Completions surface (`copilot.tencent.com/v2`).
    /// Strictly requires streaming (`stream: true`), User-Agent validation,
    /// and client identity headers (`X-Domain`, `X-Product`, `X-IDE-Type`).
    CodeBuddy,
}

/// Provider-specific behavior layered on the OpenAI Responses protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenAiResponsesDialect {
    #[default]
    Standard,
    /// DeepSeek's OpenAI-compatible Responses surface is stateless: it
    /// ignores `store` and does not support `previous_response_id`.
    DeepSeek,
    ChatGpt,
    Copilot,
}

impl OpenAiResponsesDialect {
    /// Whether this backend implements OpenAI's `reasoning.summary` request
    /// field (a *summary* of the chain of thought, streamed as
    /// `response.reasoning_summary_text.*`).
    ///
    /// False where the backend defines `reasoning` as `{effort}` only and
    /// returns the raw CoT as `reasoning_text` content parts instead — DeepSeek
    /// (ADR-0083). Asking such a backend for a summary is a foreign field on
    /// that wire, not a capability negotiation.
    pub const fn reasoning_summary(self) -> bool {
        !matches!(self, OpenAiResponsesDialect::DeepSeek)
    }

    /// Whether this backend returns replayable encrypted reasoning state and
    /// therefore understands `include: ["reasoning.encrypted_content"]`.
    ///
    /// That field is how OpenAI hands a stateless client the ciphertext it must
    /// replay to continue a reasoning turn. A backend that returns plaintext
    /// `reasoning` output items (DeepSeek) needs no such grant, and the field is
    /// not part of its documented request schema (ADR-0083).
    pub const fn encrypted_reasoning(self) -> bool {
        !matches!(self, OpenAiResponsesDialect::DeepSeek)
    }
}

/// Provider-specific behavior layered on the Anthropic Messages protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnthropicMessagesDialect {
    #[default]
    Standard,
    Copilot,
}

/// Provider-specific behavior layered on Google's generateContent protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GoogleGenerateContentDialect {
    #[default]
    GenerativeLanguage,
    Antigravity,
}

/// Service-specific behavior inherited independently of a model's wire protocol.
/// Models select the protocol family; the provider supplies its implementation
/// of that family. Uncustomized families retain their standard wire behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderDialect {
    #[default]
    Standard,
    Antigravity,
    ChatGpt,
    Copilot,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "openrouter")]
    OpenRouter,
    Qoder,
    #[serde(rename = "codebuddy")]
    CodeBuddy,
}

/// Errors produced during remote catalog discovery.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelListError {
    #[error("bad endpoint: {0}")]
    BadEndpoint(String),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("status {0}: {1}")]
    Status(u16, String),
    #[error("parse error: {0}")]
    Parse(String),
}

impl ModelListError {
    /// Whether the upstream refused the request (401/403).
    pub const fn is_refusal(&self) -> bool {
        matches!(self, Self::Status(401 | 403, _))
    }
}

/// A model entry discovered from a provider's live models list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub id: String,
    pub availability: Option<crate::model::Availability>,
    pub advertised: Option<bool>,
    pub protocol: Option<nuo_inference_wire::WireProtocol>,
    pub endpoint: Option<String>,
    pub family: Option<String>,
    pub name: Option<String>,
    pub context_window: Option<usize>,
    pub max_output_tokens: Option<u32>,
    pub reasoning: Option<bool>,
    pub thinking: Option<crate::reasoning::ReasoningSupport>,
    pub tool_call: Option<bool>,
    pub vision: Option<bool>,
    pub effort_levels: Option<Vec<String>>,
    pub catalog_source: Option<String>,
    /// Live catalog **observation**: the concrete effort-parameterized wire
    /// entity ids the upstream registered for this canonical model, in the
    /// order upstream advertises them (shallow→deep for the known tiered
    /// Antigravity families: `gemini-4-argon-{low,medium,high}`,
    /// `claude-{opus,sonnet}-5-5-{low,medium,high,max}`).
    ///
    /// This is **evidence, not policy** (ADR-0068): the parser records what
    /// the upstream actually registered; it never infers a family prefix.
    /// `None`/empty means the canonical id routes verbatim — the common case
    /// for every provider whose wire id equals the id the user selects.
    pub wire_entities: Option<Vec<String>>,
}

/// Strip parameter/execution-mode annotations like `(Thinking)` or `(thinking)` from model labels.
///
/// Per ADR-0068 [INV-CATALOG-02], model identity and runtime execution parameters (such as
/// reasoning effort or thinking mode) must remain strictly decoupled. Suffixes like
/// `(Thinking)` or `[Thinking]` inside model names are redundant and visually noisy because
/// reasoning is modeled as an orthogonal capability with its own UI controls and tags.
/// Furthermore, hyphenated marketing names (e.g. `DeepSeek-V4.1-Flash` or `GLM-5.3-Flash`)
/// are folded to natural spaces (ADR-0081).
pub fn clean_model_display_name(raw: &str) -> String {
    let mut s = raw.trim();
    loop {
        let mut stripped = false;
        for suffix in &[
            " (Thinking)",
            " (thinking)",
            " (THINKING)",
            "(Thinking)",
            "(thinking)",
            "(THINKING)",
            " [Thinking]",
            " [thinking]",
            " [THINKING]",
            "[Thinking]",
            "[thinking]",
            "[THINKING]",
            " - Thinking",
            " - thinking",
            " - THINKING",
            " (Thinking Mode)",
            " (thinking mode)",
        ] {
            if let Some(rest) = s.strip_suffix(suffix) {
                s = rest.trim();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    if s.contains('-') && should_fold_display_hyphens(s) {
        format_structural_tokens(s)
    } else {
        s.to_string()
    }
}

/// Whether the string is a hyphenated brand name whose hyphens should be folded into spaces.
/// Preserves lowercase raw identifiers (e.g. "k3", "glm-5", "deepseek-chat") so repeating-ID
/// collapse logic remains honest unless explicitly formatted.
fn should_fold_display_hyphens(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_uppercase())
}

/// The closed set of relay service tiers (ADR-0081 [INV-CATALOG-05]).
///
/// A gateway appends `:<tier>` to select a routing *variant* of the same model
/// (`nex-n2.5-pro:free`, `grok-4.7:nitro`). The tier is routing syntax, never
/// part of a name, so it is split off exactly like a hyphen. Closedness is
/// load-bearing in the other direction: `:` also appears inside authored labels
/// as provenance (`OpenAI: GPT-5.6 Sol`), and stripping blindly would truncate
/// that label to `OpenAI`.
pub const SERVICE_TIERS: &[&str] = &["free", "nitro", "floor", "online", "beta", "extended"];

/// Brand tokens that lead a model id (ADR-0081 [INV-CATALOG-05]).
///
/// Shared by the casing pass ([`capitalize_token`]) and the namespace-redundancy
/// test in [`format_structural_tokens`], so "does the tail already name the
/// vendor?" and "how is that vendor spelled?" cannot answer differently.
pub const BRAND_LEXICON: &[(&str, &str)] = &[
    ("deepseek", "DeepSeek"),
    ("glm", "GLM"),
    ("qwen", "Qwen"),
    ("claude", "Claude"),
    ("gpt", "GPT"),
    ("openai", "OpenAI"),
    ("gemini", "Gemini"),
    ("grok", "Grok"),
    ("mimo", "MiMo"),
    ("minimax", "MiniMax"),
    ("kimi", "Kimi"),
    ("mistral", "Mistral"),
    ("llama", "Llama"),
    ("hy", "HY"),
    ("step", "Step"),
    ("longcat", "LongCat"),
    ("omen", "Omen"),
];

/// Tier keywords that are title-cased inside a model name (ADR-0081).
pub const TIER_LEXICON: &[&str] = &[
    "flash",
    "pro",
    "max",
    "plus",
    "lite",
    "turbo",
    "exp",
    "vision",
    "code",
    "chat",
    "reasoner",
    "haiku",
    "sonnet",
    "opus",
    "preview",
    "contributor",
    "free",
    "argon",
    "luna",
];

/// Split the `vendor/` namespace from a relay wire id, if it carries one.
///
/// A namespace is routing syntax: `deepseek/deepseek-v4.1-flash` names one model
/// twice, `meta/muse-spark-1.1` names a vendor the tail omits. Both are handled
/// by the caller, which decides whether to print it.
fn split_namespace(id: &str) -> (Option<&str>, &str) {
    match id.split_once('/') {
        Some((vendor, tail)) if !vendor.is_empty() && !tail.is_empty() => (Some(vendor), tail),
        _ => (None, id),
    }
}

/// Drop a recognised `:tier` suffix, leaving unrecognised colons untouched.
fn strip_service_tier(id: &str) -> &str {
    match id.rsplit_once(':') {
        Some((head, tier))
            if !head.is_empty()
                && SERVICE_TIERS
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(tier)) =>
        {
            head
        }
        _ => id,
    }
}

/// Whether `vendor` states nothing the already-lifted `tail` does not.
///
/// True when the tail's first token is a brand from [`BRAND_LEXICON`] (whether
/// or not it is the same brand), because the model name then already carries a
/// family identity and a redundant `vendor/` prefix would read as a double
/// name: `deepseek/deepseek-v4.1-flash` and `openai/gpt-5.6-sol`.
fn namespace_is_redundant(vendor: &str, tail: &str) -> bool {
    let vendor_key = vendor.to_ascii_lowercase();
    let tail_key = tail.to_ascii_lowercase();
    let tail_head = tail_key
        .split('-')
        .next()
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default();
    if vendor_key == tail_key || vendor_key == tail_head {
        return true;
    }
    BRAND_LEXICON.iter().any(|(brand, _)| *brand == tail_head)
}

/// Tokenize and structurally format an identifier or brand name into clean typography (ADR-0081).
///
/// Routing syntax is absorbed first: a recognised `:tier` suffix is dropped, and
/// a `vendor/` namespace is printed only when it states something the model name
/// does not.
fn format_structural_tokens(s: &str) -> String {
    let without_tier = strip_service_tier(s.trim());
    match split_namespace(without_tier) {
        (Some(vendor), tail) if !namespace_is_redundant(vendor, tail) => {
            format!("{} {}", lift_hyphenated(vendor), lift_hyphenated(tail))
        }
        (_, tail) => lift_hyphenated(tail),
    }
}

/// Lift one hyphen-and-dot separated identifier fragment into typography.
fn lift_hyphenated(s: &str) -> String {
    let raw_tokens: Vec<&str> = s.split('-').filter(|t| !t.is_empty()).collect();
    let normalized_tokens = normalize_adjacent_version_tokens(&raw_tokens);
    normalized_tokens
        .into_iter()
        .map(|token| capitalize_token(&token))
        .collect::<Vec<String>>()
        .join(" ")
}

fn normalize_adjacent_version_tokens(tokens: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if i + 1 < tokens.len()
            && tokens[i].chars().all(|c| c.is_ascii_digit())
            && tokens[i + 1].chars().all(|c| c.is_ascii_digit())
        {
            out.push(format!("{}.{}", tokens[i], tokens[i + 1]));
            i += 2;
        } else {
            out.push(tokens[i].to_string());
            i += 1;
        }
    }
    out
}

fn capitalize_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    if let Some((_, canonical)) = BRAND_LEXICON.iter().find(|(brand, _)| *brand == lower) {
        return (*canonical).to_string();
    }
    if let Some(canonical) = TIER_LEXICON
        .iter()
        .find(|tier| **tier == lower)
        .map(|tier| {
            let mut chars = tier.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
    {
        return canonical;
    }
    let stripped = lower
        .strip_prefix('v')
        .filter(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|rest| format!("V{rest}"));
    if let Some(model) = stripped {
        return model;
    }
    for (prefix, emitter) in [("k", "K"), ("m", "M")] {
        if let Some(rest) = lower
            .strip_prefix(prefix)
            .filter(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            return format!("{emitter}{rest}");
        }
    }
    for (prefix, emitter) in [("qwen", "Qwen"), ("glm", "GLM")] {
        if let Some(rest) = lower
            .strip_prefix(prefix)
            .filter(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            return format!("{emitter} {rest}");
        }
    }
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => {
            let mut res = first.to_uppercase().to_string();
            res.extend(chars);
            res
        }
        None => String::new(),
    }
}

/// Rule-based structural display name normalizer (ADR-0081 [INV-CATALOG-05]).
///
/// Lifts raw model identifiers or uncurated remote labels into polished,
/// human-readable typography:
/// - `"deepseek-v4.1-flash"` -> `"DeepSeek V4.1 Flash"`
/// - `"DeepSeek-V4.1-Flash"` -> `"DeepSeek V4.1 Flash"`
/// - `"glm-5.3-flash"` -> `"GLM 5.3 Flash"`
/// - `"GLM-5.3-Flash"` -> `"GLM 5.3 Flash"`
/// - `"claude-haiku-5-5"` -> `"Claude Haiku 5.5"`
/// - `"qwen3.8-max"` -> `"Qwen 3.8 Max"`
pub fn format_model_display_name(raw_name: Option<&str>, model_id: &str) -> String {
    if let Some(name) = raw_name {
        let trimmed = name.trim();
        if !trimmed.is_empty() && trimmed != model_id {
            return clean_model_display_name(trimmed);
        }
    }
    format_structural_tokens(model_id)
}

/// Whether this model identifier represents ephemeral relay noise (ADR-0081 [INV-CATALOG-06]).
///
/// Prunes ephemeral promotional tiers, unstable preview snapshots, and partner contributor
/// nodes from default listing surfaces on multi-model aggregation relays. Both tier
/// spellings are read (`-free` and `:free`) because they are the same routing fact
/// (ADR-0081 [INV-CATALOG-05]): a gateway that suffixes `:free` must not slip a
/// promotional tier past a valve that only recognises `-free`.
pub fn is_relay_promotional_noise(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    let tier = lower.rsplit_once(':').map(|(_, tier)| tier);
    let carries = |needle: &str| tier.is_some_and(|tier| tier.eq_ignore_ascii_case(needle));
    lower.ends_with("-free")
        || carries("free")
        || lower.ends_with("-preview")
        || lower.ends_with("-contributor")
        || lower.starts_with("alpha-")
        || lower.ends_with("-alpha")
        || carries("alpha")
        || lower == "test"
}

impl DiscoveredModel {
    /// Convert live provider facts into the persisted channel-scoped snapshot.
    pub fn remote_metadata(&self) -> crate::model::RemoteModelMetadata {
        crate::model::RemoteModelMetadata {
            protocol: self.protocol,
            endpoint: self.endpoint.clone(),
            family: self.family.clone(),
            name: self
                .name
                .as_deref()
                .map(clean_model_display_name)
                .filter(|name| !name.is_empty() && *name != self.id.as_str()),
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            thinking: self.thinking.or_else(|| {
                self.reasoning.map(|reasoning| {
                    if reasoning {
                        crate::reasoning::ReasoningSupport::ReasoningContent
                    } else {
                        crate::reasoning::ReasoningSupport::None
                    }
                })
            }),
            tool_call: self.tool_call,
            vision: self.vision,
            effort_levels: self.effort_levels.as_ref().map(|levels| {
                levels
                    .iter()
                    .map(|level| crate::effort::EffortLevel::parse(level))
                    .collect()
            }),
            catalog_source: self.catalog_source.clone(),
            availability: self.availability.clone(),
            advertised: self.advertised,
            wire_entities: self
                .wire_entities
                .as_ref()
                .filter(|entities| !entities.is_empty())
                .cloned(),
        }
    }
}

impl std::str::FromStr for ProviderDialect {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Decode through the same serde representation the wire uses, so the
        // accepted strings can never drift from the serialized form. A hand
        // written match did drift: `codebuddy` deserialized but failed to parse.
        let quoted = serde_json::to_string(value).map_err(|error| error.to_string())?;
        serde_json::from_str(&quoted).map_err(|_| format!("unsupported provider dialect `{value}`"))
    }
}

impl ProviderDialect {
    /// Reject protocol families that cannot use this service's required envelope.
    pub fn supports(self, protocol: nuo_inference_wire::WireProtocol) -> bool {
        use nuo_inference_wire::WireProtocol;
        match self {
            Self::Antigravity => protocol == WireProtocol::GoogleGenerateContent,
            Self::ChatGpt => protocol == WireProtocol::OpenAiResponses,
            Self::Qoder => protocol == WireProtocol::OpenAiChatCompletions,
            Self::CodeBuddy => {
                protocol == WireProtocol::OpenAiChatCompletions
                    || protocol == WireProtocol::OpenAiResponses
            }
            Self::Copilot => {
                protocol != WireProtocol::GoogleGenerateContent
                    && protocol != WireProtocol::GoogleInteractions
            }
            Self::Standard | Self::DeepSeek | Self::OpenRouter => true,
        }
    }

    pub fn google(self) -> GoogleGenerateContentDialect {
        match self {
            Self::Antigravity => GoogleGenerateContentDialect::Antigravity,
            _ => GoogleGenerateContentDialect::GenerativeLanguage,
        }
    }

    pub fn google_generate_content(self) -> GoogleGenerateContentDialect {
        self.google()
    }

    pub fn google_interactions(self) -> GoogleInteractionsDialect {
        GoogleInteractionsDialect::Standard
    }

    pub fn openai_chat(self) -> OpenAiChatDialect {
        match self {
            Self::Copilot => OpenAiChatDialect::Copilot,
            Self::OpenRouter => OpenAiChatDialect::OpenRouter,
            Self::Qoder => OpenAiChatDialect::Qoder,
            Self::DeepSeek => OpenAiChatDialect::DeepSeek,
            Self::CodeBuddy => OpenAiChatDialect::CodeBuddy,
            _ => OpenAiChatDialect::Standard,
        }
    }

    pub fn openai_responses(self) -> OpenAiResponsesDialect {
        match self {
            Self::ChatGpt => OpenAiResponsesDialect::ChatGpt,
            Self::Copilot => OpenAiResponsesDialect::Copilot,
            Self::DeepSeek => OpenAiResponsesDialect::DeepSeek,
            _ => OpenAiResponsesDialect::Standard,
        }
    }

    pub fn anthropic(self) -> AnthropicMessagesDialect {
        match self {
            Self::Copilot => AnthropicMessagesDialect::Copilot,
            _ => AnthropicMessagesDialect::Standard,
        }
    }
}

impl OpenAiChatDialect {
    /// Whether assistant turns must carry their `reasoning_content` back on
    /// this surface.
    ///
    /// DeepSeek concatenates a replayable CoT into the context whenever the
    /// request carries `tools`, so omitting the field silently narrows the
    /// model's reasoning context on every tool round trip (ADR-0083). Other
    /// surfaces either never expose a replayable CoT (Standard/Copilot/Qoder)
    /// or replay a structured sidecar instead (OpenRouter's
    /// `reasoning_details`).
    pub const fn replays_reasoning_content(self) -> bool {
        matches!(self, OpenAiChatDialect::DeepSeek)
    }

    /// The declarative wire surface for this chat dialect, when it has one.
    ///
    /// A surface is the *data* form of a dialect: inference path, envelope,
    /// model carriers, emulated identity, and live catalog. Plain,
    /// self-describing dialects (Standard, Copilot, OpenRouter, DeepSeek) have
    /// `None` here; a dialect with a bespoke signed surface returns its table.
    /// The executor reads the table, never the enum key (ADR-0027 [INV-PROV-10]).
    pub fn surface(self) -> Option<&'static crate::wire_surface::DialectSurface> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GoogleInteractionsDialect {
    #[default]
    Standard,
}

/// How a [`Channel`] speaks to its model. Determines which `Provider`
/// implementation is constructed for it (in the composition root).
///
/// Variants carry only the endpoint shape intrinsic to the transport.
/// Per-call credentials and the wire model id live on the [`Channel`] itself,
/// so the same transport serves a built-in preset and a user-defined relay.
#[derive(Debug, Clone)]
pub enum Transport {
    /// OpenAI-compatible chat-completions endpoint at `base_url`.
    OpenAiChatCompletions {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        dialect: OpenAiChatDialect,
    },
    /// Anthropic-compatible `/messages` endpoint at `base_url`.
    AnthropicMessages {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        thinking: Option<crate::ReasoningMode>,
        dialect: AnthropicMessagesDialect,
    },
    /// Google native GenerateContent API (`generativelanguage.googleapis.com` or Antigravity).
    GoogleGenerateContent {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        dialect: GoogleGenerateContentDialect,
    },
    /// Google native Interactions API (`generativelanguage.googleapis.com/v1beta/interactions`).
    GoogleInteractions {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        dialect: GoogleInteractionsDialect,
    },
    /// OpenAI **Responses** API (`/responses` endpoint).
    OpenAiResponses {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        dialect: OpenAiResponsesDialect,
    },
}

impl Transport {
    /// The wire-protocol name for this transport.
    pub fn protocol_label(&self) -> &'static str {
        match self {
            Transport::OpenAiChatCompletions { .. } => "openai-chat-completions",
            Transport::OpenAiResponses { .. } => "openai-responses",
            Transport::AnthropicMessages { .. } => "anthropic-messages",
            Transport::GoogleGenerateContent { .. } => "google-generate-content",
            Transport::GoogleInteractions { .. } => "google-interactions",
        }
    }

    /// The base URL endpoint for this transport.
    pub fn base_url(&self) -> &str {
        match self {
            Transport::OpenAiChatCompletions { base_url, .. }
            | Transport::AnthropicMessages { base_url, .. }
            | Transport::GoogleGenerateContent { base_url, .. }
            | Transport::GoogleInteractions { base_url, .. }
            | Transport::OpenAiResponses { base_url, .. } => base_url,
        }
    }

    /// The resolved client profile for this transport.
    pub fn client_profile(&self) -> &crate::ClientProfile {
        match self {
            Transport::OpenAiChatCompletions { client_profile, .. }
            | Transport::AnthropicMessages { client_profile, .. }
            | Transport::GoogleGenerateContent { client_profile, .. }
            | Transport::GoogleInteractions { client_profile, .. }
            | Transport::OpenAiResponses { client_profile, .. } => client_profile,
        }
    }

    /// The User-Agent header string for this transport.
    pub fn user_agent(&self) -> String {
        self.client_profile().user_agent().to_string()
    }

    /// Whether this transport needs an API key at all.
    pub fn needs_api_key(&self) -> bool {
        match self {
            Transport::OpenAiChatCompletions { .. }
            | Transport::AnthropicMessages { .. }
            | Transport::GoogleGenerateContent { .. }
            | Transport::GoogleInteractions { .. }
            | Transport::OpenAiResponses { .. } => true,
        }
    }
}

/// One delivery path for a [`ProviderEntry`].
///
/// A channel pairs a [`Transport`] with a [`crate::CredentialSource`] and
/// the wire `model` id. Built-in presets materialize exactly one channel per
/// entry (id `"default"`); user-defined entries may declare several channels
/// per model (e.g. Gemini via Studio, Vertex, or a relay), with the entry's
/// `default_channel` selecting one. See ADR-0002.
#[derive(Clone)]
pub struct Channel {
    /// Stable identifier within the model (e.g. `"studio"`, `"vertex"`).
    /// Built-in presets use `"default"`.
    pub id: String,
    /// Endpoint shape and provider implementation selector.
    pub transport: Transport,
    /// The dynamic or static credential source for this channel.
    pub credentials: std::sync::Arc<dyn crate::CredentialSource>,
    /// Resolved wire model id sent to the provider.
    pub model: String,
    /// Provider-scoped live capability metadata. A trusted provider's remote
    /// catalogue owns the fields it explicitly supplies; the static model
    /// registry remains the fallback for omitted or offline data.
    pub remote: Option<crate::RemoteModelMetadata>,
    /// The user's explicit capability overrides for this route — the top
    /// layer of the capability resolution order (ADR-0068), applied after
    /// the remote overlay in [`Channel::capabilities`]. `None` means the
    /// user has no opinion and the two lower layers decide.
    pub user_overrides: Option<crate::model::CapabilityOverrides>,
    /// Prompt-cache controls and telemetry declared by this concrete route.
    /// This is intentionally independent of model-family capabilities: a
    /// relay speaking the same wire protocol may expose none of them.
    pub prompt_cache: crate::PromptCacheCapabilities,
    /// User-selected cache intent for this route.
    pub prompt_cache_preference: crate::PromptCachePreference,
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("id", &self.id)
            .field("transport", &self.transport)
            .field("credentials", &self.credentials)
            .field("model", &self.model)
            .field("remote", &self.remote)
            .field("user_overrides", &self.user_overrides)
            .field("prompt_cache", &self.prompt_cache)
            .field("prompt_cache_preference", &self.prompt_cache_preference)
            .finish()
    }
}

impl Channel {
    /// Return the active credential source for this channel.
    pub fn credentials_source(&self) -> std::sync::Arc<dyn crate::CredentialSource> {
        self.credentials.clone()
    }

    /// The model's catalog provenance: `(source, display_name)`.
    ///
    /// `source` is the provider's own label for the catalog the model came from
    /// (Qoder's `system` / `custom`); `display_name` is the presentation label.
    /// Both are empty when the catalog advertises none. A dialect whose surface
    /// declares a matching `CatalogSource`/`DisplayName` carrier stamps them
    /// into the request; the wire id stays the identity (ADR-0027).
    pub fn catalog_provenance(&self) -> (String, String) {
        let remote = self.remote.as_ref();
        (
            remote
                .and_then(|metadata| metadata.catalog_source.clone())
                .unwrap_or_default(),
            remote
                .and_then(|metadata| metadata.name.clone())
                .unwrap_or_default(),
        )
    }

    /// Whether this channel has a usable API key or valid credential.
    pub fn key_ready(&self) -> bool {
        if !self.transport.needs_api_key() {
            return true;
        }
        self.credentials.is_ready()
    }

    /// Resolve effective capabilities for this delivery path. The provider's
    /// remote snapshot overlays only its explicit fields onto the static model
    /// baseline, preventing a global model id from overwriting account-specific
    /// routes or capabilities.
    pub fn capabilities(&self) -> crate::ModelCapabilities {
        let merged = crate::ModelCapabilities::for_channel(&self.model, self.remote.as_ref());
        match &self.user_overrides {
            Some(user) => merged.apply_overrides(user),
            None => merged,
        }
    }
}

/// A catalog entry: a provider preset with one or more channels. Each channel
/// references a model by wire id; model metadata (context window, capabilities)
/// is resolved from the [`crate::model`] registry.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    /// Canonical stable identifier — the provider/preset id
    /// (`"glm-cn"`, `"kimi-code"`, ...).
    pub id: String,
    /// Display name (e.g. `"GLM Coding Plan CN"`).
    pub name: String,
    /// Short human-readable description.
    pub description: String,
    /// Delivery paths for this provider. Phase 1: exactly one per entry.
    pub channels: Vec<Channel>,
    /// Index into `channels` of the preferred path.
    pub default_channel: usize,
    /// `true` for the built-in presets; `false` for user-defined entries.
    pub builtin: bool,
}

impl ProviderEntry {
    /// The preferred channel, or `None` if the entry has no channels.
    pub fn default_channel(&self) -> Option<&Channel> {
        self.channels.get(self.default_channel)
    }

    /// Whether the entry has a usable API key on its default channel. Built-in
    /// keyless entries (local server) always report ready.
    pub fn key_ready(&self) -> bool {
        self.default_channel()
            .map(Channel::key_ready)
            .unwrap_or(true)
    }

    /// The context window (in tokens) of the model on the default channel,
    /// resolved from the model registry. Returns `0` when the entry has no
    /// default channel or the model is not in the registry.
    pub fn context_window(&self) -> usize {
        self.default_channel()
            .map(|ch| ch.capabilities().context_window)
            .unwrap_or(0)
    }

    /// The channel carrying `model_id`, if any. A multi-model provider (e.g.
    /// `opencode-plan`) exposes one channel per model — each with the transport
    /// matching that model's [`crate::model::WireProtocol`] — so selecting a
    /// model is selecting a channel. Returns `None` when the entry does not
    /// serve that model id.
    pub fn channel_for_model(&self, model_id: &str) -> Option<&Channel> {
        self.channels.iter().find(|ch| ch.model == model_id)
    }

    /// Whether this entry serves `model_id` on any of its channels.
    pub fn offers_model(&self, model_id: &str) -> bool {
        self.channel_for_model(model_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PromptCacheCapabilities, PromptCachePreference};

    #[test]
    fn dialect_strings_parse_their_serialized_form() {
        // `FromStr` and the wire share one serde representation, so every
        // dialect's serialized name must parse back to itself. A hand-written
        // match missed `codebuddy`, which made a valid dialect unregisterable.
        for dialect in [
            ProviderDialect::Standard,
            ProviderDialect::Antigravity,
            ProviderDialect::ChatGpt,
            ProviderDialect::Copilot,
            ProviderDialect::DeepSeek,
            ProviderDialect::OpenRouter,
            ProviderDialect::Qoder,
            ProviderDialect::CodeBuddy,
        ] {
            let encoded = serde_json::to_string(&dialect).unwrap();
            let bare: String = serde_json::from_str(&encoded).unwrap();
            assert_eq!(
                bare.parse::<ProviderDialect>().unwrap(),
                dialect,
                "{bare} must parse back to {dialect:?}"
            );
        }
    }

    #[test]
    fn catalog_lookup_is_exact_match() {
        let entries = [ProviderEntry {
            id: "deepseek-v4-flash".to_string(),
            name: "DeepSeek V4 Flash".to_string(),
            description: String::new(),
            channels: vec![Channel {
                id: "default".to_string(),
                transport: Transport::OpenAiChatCompletions {
                    base_url: "https://api.deepseek.com/v1/chat/completions".to_string(),
                    client_profile: crate::ClientProfile::from("agent"),
                    effort: None,
                    dialect: Default::default(),
                },
                credentials: crate::static_credential("k"),
                model: "deepseek-v4-flash".to_string(),
                remote: None,
                user_overrides: None,
                prompt_cache_preference: PromptCachePreference::default(),
                prompt_cache: PromptCacheCapabilities::unsupported(),
            }],
            default_channel: 0,
            builtin: true,
        }];
        assert_eq!(
            entries
                .iter()
                .find(|e| e.id == "deepseek-v4-flash")
                .expect("exact id")
                .id,
            "deepseek-v4-flash"
        );
        // No alias mapping: stale ids do not resolve.
        assert!(entries.iter().find(|e| e.id == "deepseek").is_none());
        assert!(entries.iter().find(|e| e.id == "deepseek-flash").is_none());
        assert!(entries.iter().find(|e| e.id == "unknown").is_none());
    }

    #[test]
    fn key_ready_is_false_for_empty_cloud_key() {
        let channel = Channel {
            id: "default".to_string(),
            transport: Transport::OpenAiChatCompletions {
                base_url: "https://api.openai.com/v1/chat/completions".to_string(),
                client_profile: crate::ClientProfile::from("agent"),
                effort: None,
                dialect: Default::default(),
            },
            credentials: crate::static_credential("   "),
            model: "gpt-4o".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: PromptCachePreference::default(),
            prompt_cache: PromptCacheCapabilities::unsupported(),
        };
        assert!(!channel.key_ready());
    }

    #[test]
    fn anthropic_transport_needs_a_key() {
        // The Anthropic /messages transport is a cloud transport: it must
        // report needing a key, and an empty key must not be "ready".
        let needs_key = Transport::AnthropicMessages {
            base_url: "https://opencode.ai/inference/anthropic/v1/messages".to_string(),
            client_profile: crate::ClientProfile::from("agent"),
            effort: None,
            thinking: None,
            dialect: Default::default(),
        }
        .needs_api_key();
        assert!(needs_key, "Anthropic transport must require an API key");

        let channel = Channel {
            id: "default".to_string(),
            transport: Transport::AnthropicMessages {
                base_url: "https://opencode.ai/inference/anthropic/v1/messages".to_string(),
                client_profile: crate::ClientProfile::from("agent"),
                effort: None,
                thinking: None,
                dialect: Default::default(),
            },
            credentials: crate::static_credential("  "),
            model: "minimax-m3".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: PromptCachePreference::default(),
            prompt_cache: PromptCacheCapabilities::unsupported(),
        };
        assert!(!channel.key_ready(), "empty key must not be ready");
    }

    #[test]
    fn multi_model_entry_resolves_channel_by_model_id() {
        // A provider like opencode-plan hosts one channel per model, each with the
        // transport matching that model's wire format. Selecting a model is
        // selecting a channel.
        let entry = ProviderEntry {
            id: "opencode-plan".to_string(),
            name: "OpenCode Plan".to_string(),
            description: String::new(),
            channels: vec![
                Channel {
                    id: "glm-5.2".to_string(),
                    transport: Transport::OpenAiChatCompletions {
                        base_url: "https://opencode.ai/inference/openai/v1/chat/completions"
                            .to_string(),
                        client_profile: crate::ClientProfile::from("agent"),
                        effort: None,
                        dialect: Default::default(),
                    },
                    credentials: crate::static_credential("k"),
                    model: "glm-5.2".to_string(),
                    remote: None,
                    user_overrides: None,
                    prompt_cache_preference: PromptCachePreference::default(),
                    prompt_cache: PromptCacheCapabilities::unsupported(),
                },
                Channel {
                    id: "claude-opus-5".to_string(),
                    transport: Transport::AnthropicMessages {
                        base_url: "https://opencode.ai/inference/anthropic/v1/messages".to_string(),
                        client_profile: crate::ClientProfile::from("agent"),
                        effort: None,
                        thinking: None,
                        dialect: Default::default(),
                    },
                    credentials: crate::static_credential("k"),
                    model: "claude-opus-5".to_string(),
                    remote: None,
                    user_overrides: None,
                    prompt_cache_preference: PromptCachePreference::default(),
                    prompt_cache: PromptCacheCapabilities::unsupported(),
                },
            ],
            default_channel: 0,
            builtin: true,
        };
        // OpenAI-format model resolves to the OpenAiChatCompletions channel.
        let glm = entry.channel_for_model("glm-5.2").expect("glm-5.2 channel");
        assert!(matches!(
            glm.transport,
            Transport::OpenAiChatCompletions { .. }
        ));
        // Anthropic-format model resolves to the AnthropicMessages channel.
        let mm = entry
            .channel_for_model("claude-opus-5")
            .expect("claude-opus-5 channel");
        assert!(matches!(mm.transport, Transport::AnthropicMessages { .. }));
        // An unknown model id resolves to nothing.
        assert!(entry.channel_for_model("nope").is_none());
        assert!(!entry.offers_model("nope"));
        assert!(entry.offers_model("glm-5.2"));
    }

    #[test]
    fn context_window_resolves_from_model_registry() {
        // Uses the `fixture-alpha` baseline registered in `model::tests` (core's
        // own test binary links no provider crate, so real vendor ids fall back).
        let entry = ProviderEntry {
            id: "fixture-provider".to_string(),
            name: "Fixture".to_string(),
            description: String::new(),
            channels: vec![Channel {
                id: "default".to_string(),
                transport: Transport::OpenAiChatCompletions {
                    base_url: "https://example.com/v1/chat/completions".to_string(),
                    client_profile: crate::ClientProfile::from("agent"),
                    effort: None,
                    dialect: Default::default(),
                },
                credentials: crate::static_credential("k"),
                model: "fixture-alpha".to_string(),
                remote: None,
                user_overrides: None,
                prompt_cache_preference: PromptCachePreference::default(),
                prompt_cache: PromptCacheCapabilities::unsupported(),
            }],
            default_channel: 0,
            builtin: true,
        };
        assert_eq!(entry.context_window(), 111_000);
    }

    #[test]
    fn clean_model_display_name_strips_thinking_annotations() {
        assert_eq!(
            clean_model_display_name("Claude Opus 4.6 (Thinking)"),
            "Claude Opus 4.6"
        );
        assert_eq!(
            clean_model_display_name("Claude Sonnet 4.6 (thinking)"),
            "Claude Sonnet 4.6"
        );
        assert_eq!(
            clean_model_display_name("Claude Opus 4.6 [Thinking]"),
            "Claude Opus 4.6"
        );
        assert_eq!(
            clean_model_display_name("Claude Opus 4.6 - Thinking"),
            "Claude Opus 4.6"
        );
        assert_eq!(
            clean_model_display_name("Claude Opus 4.6 (Thinking Mode)"),
            "Claude Opus 4.6"
        );
        assert_eq!(
            clean_model_display_name("Claude Opus 4.6 (Thinking) (thinking)"),
            "Claude Opus 4.6"
        );

        let discovered = DiscoveredModel {
            id: "claude-opus-4-6".to_string(),
            name: Some("Claude Opus 4.6 (Thinking)".to_string()),
            ..DiscoveredModel::default()
        };
        let meta = discovered.remote_metadata();
        assert_eq!(meta.name.as_deref(), Some("Claude Opus 4.6"));
    }

    #[test]
    fn format_model_display_name_folds_hyphens_and_capitalizes_brands() {
        assert_eq!(
            format_model_display_name(None, "deepseek-v4.1-flash"),
            "DeepSeek V4.1 Flash"
        );
        assert_eq!(
            format_model_display_name(Some("DeepSeek-V4.1-Flash"), "deepseek-v4.1-flash"),
            "DeepSeek V4.1 Flash"
        );
        assert_eq!(
            format_model_display_name(None, "glm-5.3-flash"),
            "GLM 5.3 Flash"
        );
        assert_eq!(
            format_model_display_name(Some("GLM-5.3-Flash (Thinking)"), "glm-5.3-flash"),
            "GLM 5.3 Flash"
        );
        assert_eq!(
            format_model_display_name(None, "claude-haiku-5-5"),
            "Claude Haiku 5.5"
        );
        assert_eq!(
            format_model_display_name(None, "qwen3.8-max"),
            "Qwen 3.8 Max"
        );
        assert_eq!(format_model_display_name(None, "minimax-m3"), "MiniMax M3");
        assert_eq!(format_model_display_name(None, "grok-4.7"), "Grok 4.7");
    }

    /// ADR-0081 [INV-CATALOG-05]: a relay id carries two routing separators that
    /// are never part of a model's name. The tier is always dropped (closed set
    /// only), the namespace only when the tail already names a brand.
    #[test]
    fn structural_lift_absorbs_routing_syntax() {
        // A redundant `vendor/` namespace is absorbed: the tail already leads
        // with the family brand, so printing it twice would be a double name.
        assert_eq!(
            format_model_display_name(None, "deepseek/deepseek-v4.1-flash"),
            "DeepSeek V4.1 Flash"
        );
        assert_eq!(
            format_model_display_name(None, "openai/gpt-5.6-sol"),
            "GPT 5.6 Sol"
        );
        // A namespace the tail does not state is kept, because it is the only
        // place the vendor is named at all.
        assert_eq!(
            format_model_display_name(None, "meta/muse-spark-1.1"),
            "Meta Muse Spark 1.1"
        );
        // A recognised `:tier` is routing syntax, not a name.
        assert_eq!(
            format_model_display_name(None, "nex-agi/nex-n2.5-pro:free"),
            "Nex Agi Nex N2.5 Pro"
        );
        assert_eq!(
            format_model_display_name(None, "x-ai/grok-4.7:nitro"),
            "Grok 4.7"
        );
        // An unrecognised colon belongs to the name and survives untouched.
        assert_eq!(
            format_model_display_name(None, "vendor/model:v2"),
            "Vendor Model:v2"
        );
    }

    /// An authored label is prose, not routing syntax: it is cleaned and
    /// hyphen-folded, never restructured (ADR-0081 [INV-CATALOG-05]).
    #[test]
    fn authored_labels_are_never_restructured() {
        assert_eq!(
            format_model_display_name(Some("OpenAI: GPT-5.6 Sol"), "gpt-5.6-sol"),
            "OpenAI: GPT 5.6 Sol"
        );
        assert_eq!(
            format_model_display_name(Some("Mixtral 8x7B / Instruct"), "mixtral-8x7b"),
            "Mixtral 8x7B / Instruct"
        );
    }

    /// The casing pass and the namespace-redundancy test read one pair of
    /// tables, so they cannot disagree about a brand's identity or spelling.
    #[test]
    fn lexicons_are_the_single_answer_for_brand_identity() {
        for (brand, canonical) in BRAND_LEXICON {
            assert_eq!(&capitalize_token(brand), canonical, "{brand}");
            assert!(
                namespace_is_redundant("some-vendor", &format!("{brand}-v9")),
                "{brand} must lead a family, so a namespace in front of it is redundant"
            );
        }
        for tier in TIER_LEXICON {
            let cased = capitalize_token(tier);
            assert!(
                cased.chars().next().is_some_and(char::is_uppercase),
                "{tier} must be title-cased, got {cased}"
            );
        }
    }

    #[test]
    fn is_relay_promotional_noise_filters_transient_models() {
        assert!(is_relay_promotional_noise("beam-free"));
        assert!(is_relay_promotional_noise("deepseek-v4.1-flash-free"));
        assert!(is_relay_promotional_noise("hy4-preview"));
        assert!(is_relay_promotional_noise("step-5-preview"));
        assert!(is_relay_promotional_noise("gemini-4-argon-contributor"));
        assert!(is_relay_promotional_noise("muse-spark-1.2-contributor"));
        assert!(is_relay_promotional_noise("alpha-test-model"));
        assert!(is_relay_promotional_noise("omen-alpha"));
        assert!(is_relay_promotional_noise("test"));

        assert!(!is_relay_promotional_noise("deepseek-v4.1-flash"));
        assert!(!is_relay_promotional_noise("glm-5.3-flash"));
        assert!(!is_relay_promotional_noise("claude-haiku-5-5"));
        assert!(!is_relay_promotional_noise("qwen3.8-max"));

        // The same routing fact in its colon spelling (ADR-0081).
        assert!(is_relay_promotional_noise("nex-agi/nex-n2.5-pro:free"));
        assert!(is_relay_promotional_noise("vendor/model:alpha"));
        // A colon that belongs to a name is not a tier.
        assert!(!is_relay_promotional_noise("vendor/model:v2"));
    }
}
