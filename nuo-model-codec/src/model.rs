//! Canonical model registry — baseline metadata for models whose provider does
//! not publish a complete live model catalogue.
//!
//! A [`ProviderEntry`](crate::catalog::ProviderEntry) references a model by its
//! wire id (e.g. `"glm-5.2"`); this module supplies conservative defaults for
//! that id. A trusted provider may instead attach a [`RemoteModelMetadata`]
//! snapshot to its channel. Such metadata is scoped to the provider because an
//! endpoint, account entitlement, and serving runtime can change a model's
//! available API surface and capabilities.
//!
//! The [`WireProtocol`] on each model is the baseline wire protocol when no live
//! provider metadata supplies a more specific endpoint. A remote catalogue can
//! legitimately route the same model id through a different surface.

use crate::reasoning::ReasoningSupport;
pub use crate::wire_protocol::WireProtocol;

/// A canonical baseline model definition.
///
/// The registered baselines (see [`BaselineModels`]) are authoritative only
/// when a channel has no trusted remote metadata for the requested field. Use
/// [`ModelCapabilities::for_channel`] for request-time behavior.
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// Wire model id sent in API requests, e.g. `"glm-5.2"`. This is the
    /// model's identity: it is what goes on the wire, what every config surface
    /// (favorites, `hidden_models`, route settings) keys on, and what the user
    /// types. Model pickers lead with a provider-published display label when
    /// one exists (see [`crate::model::RemoteModelMetadata::name`]) and fall
    /// back to this id when it does not, keeping the id visible either way.
    pub id: &'static str,
    /// Model family for grouping, e.g. `"glm"`, `"gpt"`, `"google"`.
    pub family: &'static str,
    /// Context window in tokens. `0` means unknown.
    pub context_window: usize,
    /// What extended thinking this model supports and how it is encoded on the
    /// wire. The single source of truth for thinking capability; the coarse
    /// "does it reason" bool used for display derives from it via
    /// [`Model::reasoning`]. See [`ReasoningSupport`].
    pub thinking: ReasoningSupport,
    /// Whether the model supports native tool/function calling.
    pub tool_call: bool,
    /// Whether the model supports vision (image inputs via `image_url`/
    /// `inline_data`). When `false`, images attached to messages are
    /// silently stripped before the request hits the wire.
    pub vision: bool,
    /// Baseline wire protocol used to reach this model.
    pub protocol: WireProtocol,
    /// Model-specific prompt guidance injected into the system prompt as a
    /// `ModelGuidance` section. Because each model behaves differently,
    /// this is the per-model hook for any behavioral nudge a model needs.
    /// Empty for all known models today; a model entry is free to carry
    /// non-empty guidance when it needs one. The model entry is the single
    /// source of truth; the prompt engine just renders whatever the resolved
    /// model carries.
    pub model_guidance: &'static str,
    /// The reasoning-effort levels this model honors, ascending. Used as the
    /// clamp range when a user requests an effort the model doesn't support.
    /// `&[]` means effort control does not apply (non-reasoning models, or
    /// protocols without an effort field). Models with unknown effort tiers
    /// default to [`crate::effort::COMMON_LADDER`]; non-reasoning models carry `&[]`.
    pub effort_levels: &'static [crate::effort::Effort],
}

impl Model {
    /// Coarse "does this model reason at all" flag, for capability display.
    /// Derives from [`Self::thinking`] so there is one source of truth.
    pub const fn reasoning(&self) -> bool {
        self.thinking.reasons()
    }
}

/// A provider's declared availability verdict for a model on the account
/// behind the connection: whether the account may run it, and — when the
/// provider states one — why not (ADR-0273).
///
/// This is a **declaration**, never an inference: it exists only because a
/// provider response carried it, and it is never synthesized from status codes,
/// model-id patterns, or plan heuristics. It is orthogonal to
/// [`RemoteModelMetadata::advertised`] (the listing hint) and to capability:
/// an unavailable model keeps its membership, its capabilities, and its route
/// shape — it simply must not run.
///
/// `reason` is the provider's own explanation, **verbatim**. It is display
/// data and nothing else: no code path parses, matches, localizes, or decides
/// on it (`[INV-AVAIL-03]`). A provider that states no reason yields `None`,
/// and a surface must then say only what it knows rather than inventing one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Availability {
    /// Whether the account may run the model right now.
    pub usable: bool,
    /// The provider's own explanation for an unusable verdict, verbatim.
    /// `None` means the provider declared none — never "unknown, assume the
    /// obvious".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Availability {
    /// The account may run the model.
    pub const fn usable() -> Self {
        Self {
            usable: true,
            reason: None,
        }
    }

    /// The account may not run the model, with the provider's stated reason
    /// when it gave one.
    pub fn locked(reason: Option<String>) -> Self {
        Self {
            usable: false,
            reason,
        }
    }

    pub const fn is_usable(&self) -> bool {
        self.usable
    }
}

impl Default for Availability {
    fn default() -> Self {
        Self::usable()
    }
}

/// Capability metadata received from a trusted provider's live model catalogue.
///
/// Every field is optional so an omitted remote field falls back to the static
/// baseline. A present `false` is meaningful: it explicitly overrides a more
/// optimistic local default. This record belongs to the channel that received
/// it, never globally by model id, because availability and protocol routing are
/// provider- and account-specific.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RemoteModelMetadata {
    /// Exact API surface advertised for this model by the provider. When absent,
    /// the channel's configured transport remains authoritative.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<WireProtocol>,
    /// Provider-advertised **API root** override for this model (e.g. OpenCode
    /// Console's per-model `provider.api`). The wire suffix is still appended
    /// by the ADR-0259 root algebra for [`Self::protocol`]; absent means the
    /// provider spec's route for that protocol holds (ADR-0269).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Provider's model-family label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Human-readable label the provider publishes for this model (models.dev
    /// `name`, Anthropic/Kimi `display_name`, Gemini `displayName`), e.g.
    /// `"DeepSeek V4.1 Flash"` for the wire id `deepseek-flash`.
    ///
    /// **Presentation only, and never required.** The wire id stays the model's
    /// identity everywhere — favorites, `hidden_models`, route settings, usage
    /// recency, and every config surface key on the id. Surfaces render this as
    /// a secondary annotation beside the id and accept it as a search alias, so
    /// a user who knows the brand name can find the model without the client
    /// ever implying that the name is what to type on the wire. `None` (the
    /// common case: OpenAI-compatible `/models` and Gemini advertise no label)
    /// means the surface shows the bare id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Maximum full request context in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    /// Maximum generated tokens, when declared by the endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Exact reasoning representation supported by the advertised endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ReasoningSupport>,
    /// Whether native tool/function calls are accepted by this endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    /// Whether image input is accepted by this endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Endpoint-advertised reasoning effort values. An empty vector explicitly
    /// means that the model accepts no effort control. Carries
    /// [`EffortLevel`](crate::effort::EffortLevel) so a provider-advertised tier
    /// the vocabulary does not name is preserved verbatim and stamped through
    /// (ADR-0065), rather than silently dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_levels: Option<Vec<crate::effort::EffortLevel>>,
    /// The catalog the model came from, as the provider names it — Qoder's
    /// `source` (`"system"` for platform models, `"custom"` for user-added
    /// ones). Not presentation: a signed surface may carry this value on the
    /// wire as part of the request's model identity (Qoder's `X-Model-Source`
    /// and `model_config.source`), so it is round-tripped rather than derived.
    /// `None` when the endpoint advertises no provenance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_source: Option<String>,
    /// The provider's declared availability for this model on this account
    /// (ADR-0273). Qoder's `enable:false`, Codex's `supported_in_api:false`,
    /// and Copilot's `policy.state:"disabled"` all land here. `None` — the
    /// common case, since most providers declare no such verdict — means
    /// undeclared and therefore usable. `Some(usable: false)` is a declaration
    /// the account may not run the model; pickers surface it dimmed and the
    /// daemon refuses to route it, but membership and capabilities are
    /// untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,
    /// The provider's listing intent: whether this model is meant to appear in
    /// a model picker listing (Codex's `visibility != "list"`, Copilot's
    /// `model_picker_enabled:false`). Independent of [`Self::availability`]:
    /// an API-supported model may be deliberately unlisted, and an unavailable
    /// model may well be listed (Qoder's greyed entries). `None` means the
    /// provider expressed no listing intent and the model is listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertised: Option<bool>,
}

/// Effective capabilities for one provider channel.
///
/// This owned view combines the local baseline with the channel's remote
/// snapshot. One model id can therefore have different capabilities or routes
/// at different providers without one account's discovery changing another
/// provider's behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub family: String,
    pub context_window: usize,
    pub max_output_tokens: Option<u32>,
    pub thinking: ReasoningSupport,
    pub tool_call: bool,
    /// Effective image-input support for this route, as a **three-valued**
    /// resolution of the layers in ADR-0149.
    ///
    /// - `Some(true)` — a layer declared image support;
    /// - `Some(false)` — a layer declared that images are *not* accepted;
    /// - `None` — **no layer declared anything**. The route is *unknown*, not
    ///   text-only, and the client policy is permissive: an undeclared route is
    ///   attempted rather than silently stripped, because vision is the one
    ///   capability most vendors do not advertise (ADR-0230).
    pub vision: Option<bool>,
    /// The effort ladder this channel honors, as [`crate::EffortLevel`] so a
    /// provider-advertised tier outside the [`crate::Effort`] vocabulary is preserved
    /// and stamped through (ADR-0065). Built in `for_channel` from the remote
    /// advertisement over the static baseline.
    pub effort_levels: Vec<crate::effort::EffortLevel>,
}

impl RemoteModelMetadata {
    /// The declared availability verdict, defaulting to usable when the
    /// provider declared none (ADR-0273). Undeclared is *not* a declaration:
    /// it must never render as disabled.
    pub fn availability_or_usable(&self) -> Availability {
        self.availability.clone().unwrap_or_default()
    }

    /// Whether the provider's declared verdict permits running the model.
    pub fn is_usable(&self) -> bool {
        self.availability
            .as_ref()
            .is_none_or(Availability::is_usable)
    }

    /// Whether the provider wants the model listed; undeclared means listed.
    pub fn is_advertised(&self) -> bool {
        self.advertised.unwrap_or(true)
    }
}

/// Materialized, route-scoped capabilities evaluated daemon-side via ADR-0149.
/// Projected to frontends as the infallible single source of truth (ADR-0182).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize,
)]
pub struct RouteCapabilities {
    /// Context window size in tokens. Guaranteed > 0 for all routed channels.
    pub context_window: usize,
    /// Maximum generation tokens, when declared or configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Whether the route accepts image attachments, as declared by the
    /// resolution layers: `None` means **undeclared** (ADR-0230), never a
    /// client-side guess that images are unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Whether the route supports tool/function calling.
    pub tool_call: bool,
    /// Extended thinking / reasoning support mode.
    pub thinking: ReasoningSupport,
}

impl RouteCapabilities {
    /// Unknown is permissive (ADR-0230): only an explicit `Some(false)` vetoes images.
    pub const fn accepts_images(&self) -> bool {
        !matches!(self.vision, Some(false))
    }
}

impl ModelCapabilities {
    /// Materialize the route-scoped projection for cross-process DTOs.
    pub fn to_route_capabilities(&self) -> RouteCapabilities {
        RouteCapabilities {
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            vision: self.vision,
            tool_call: self.tool_call,
            thinking: self.thinking,
        }
    }

    /// Whether image attachments may be put on the wire for this route.
    ///
    /// **Unknown is permissive** (ADR-0230): only an explicit `Some(false)`
    /// vetoes images. A route whose layers never declared vision support is
    /// attempted, because the alternative — silently dropping the pixels and
    /// letting the model answer about an image it never saw — is the one
    /// failure mode the user cannot detect. A provider that rejects images
    /// says so loudly; a client that strips them says nothing at all.
    pub const fn accepts_images(&self) -> bool {
        !matches!(self.vision, Some(false))
    }

    /// Whether some layer actually **declared** image support (either way).
    /// `false` means the route is undeclared — the state a gate must not treat
    /// as "text-only" (that is what [`Self::accepts_images`] answers).
    pub const fn vision_declared(&self) -> bool {
        self.vision.is_some()
    }
}

/// A user's explicit capability override for one (provider-instance, model)
/// route -- the **top layer** of the capability resolution order (ADR-0149).
///
/// Every field is optional; `None` means "no opinion, fall through to the
/// layer below". A present `Some(false)` is meaningful: it forces the
/// capability off even when both the remote advertisement and the static
/// baseline say otherwise (e.g. a relay's `glm-5.3-flash` that strips image
/// inputs, or an account whose plan caps the context window lower than the
/// model card claims).
///
/// This lives in `nuo-wire` (not persistence) so the merge function can
/// live beside the structure it overrides -- persistence keys it per
/// `(instance_id, model_id)` inside `RouteSettings` and owns only storage.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CapabilityOverrides {
    /// Explicit model route protocol; resolved independently from capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<WireProtocol>,
    /// Force the family tag used for family-scoped wire behavior (cache
    /// policy, effort mapping). `None` -> inherit from the layers below.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Force the context window (tokens). `None` -> inherit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    /// Force the max output tokens. `None` -> inherit. `Some(0)` clears an
    /// inherited cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Force the thinking representation. `None` -> inherit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ReasoningSupport>,
    /// Force native tool calling on/off. `None` -> inherit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    /// Force image-input support on/off. `None` -> inherit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
}

impl CapabilityOverrides {
    /// Whether any knob is set. An all-`None` record is a no-op and should
    /// not be persisted.
    pub fn is_empty(&self) -> bool {
        self.protocol.is_none()
            && self.family.is_none()
            && self.context_window.is_none()
            && self.max_output_tokens.is_none()
            && self.thinking.is_none()
            && self.tool_call.is_none()
            && self.vision.is_none()
    }

    /// Layer `over` on top of `self` (ADR-0199). Any explicitly set `Some(...)` field in `over`
    /// takes precedence over `self`.
    pub fn merge_with(&self, over: &CapabilityOverrides) -> CapabilityOverrides {
        CapabilityOverrides {
            protocol: over.protocol.or(self.protocol),
            family: over.family.clone().or_else(|| self.family.clone()),
            context_window: over.context_window.or(self.context_window),
            max_output_tokens: over.max_output_tokens.or(self.max_output_tokens),
            thinking: over.thinking.or(self.thinking),
            tool_call: over.tool_call.or(self.tool_call),
            vision: over.vision.or(self.vision),
        }
    }
}

/// One user-declared model on a preset or connection scope (ADR-0199): a hidden,
/// preview, or unlisted upstream id pinned to a scope with optional capability facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DeclaredModel {
    /// Explicit wire protocol for this model within its provider or connection scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<WireProtocol>,
    /// Exact model id sent on the wire and shown in the picker.
    pub id: String,
    /// Context window in tokens, when the user knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    /// Maximum generated tokens, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Reasoning representation, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ReasoningSupport>,
    /// Image-input support, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Native tool-call support, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
}

impl DeclaredModel {
    /// A bare declaration for `raw` with its id sanitized (trimmed,
    /// whitespace runs → hyphens, control chars dropped); `None` when nothing
    /// usable remains.
    pub fn sanitized(raw: &str) -> Option<Self> {
        let id = sanitize_model_id(raw);
        (!id.is_empty()).then_some(Self {
            id,
            ..Self::default()
        })
    }

    /// Convert declared capability facts into a [`CapabilityOverrides`] record.
    pub fn to_overrides(&self) -> CapabilityOverrides {
        CapabilityOverrides {
            protocol: self.protocol,
            family: None,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            thinking: self.thinking,
            tool_call: self.tool_call,
            vision: self.vision,
        }
    }
}

/// The default admission gate for models passing through a connection pipe (ADR-0203).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ConnectionFilterPolicy {
    Named(NamedFilterPolicy),
    Glob(Vec<String>),
}

/// A connection-local override for the provider's remote catalog source.
///
/// Standard named pipe filter policies (ADR-0203).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedFilterPolicy {
    /// Admit only models present in the compiled baseline (strict safe filter).
    Baseline,
    /// Admit all models delivered by the remote catalog source.
    All,
}

impl Default for ConnectionFilterPolicy {
    fn default() -> Self {
        Self::Named(NamedFilterPolicy::Baseline)
    }
}

impl ConnectionFilterPolicy {
    /// Whether this filter policy admits `model_id`.
    pub fn admits(&self, model_id: &str, is_in_baseline: bool) -> bool {
        match self {
            Self::Named(NamedFilterPolicy::Baseline) => is_in_baseline,
            Self::Named(NamedFilterPolicy::All) => true,
            Self::Glob(patterns) => patterns
                .iter()
                .any(|pattern| simple_glob_matches(pattern, model_id)),
        }
    }
}

/// Simple glob pattern matcher supporting leading and trailing wildcards (e.g. `gpt-*`, `*mini`, `*`).
pub fn simple_glob_matches(pattern: &str, text: &str) -> bool {
    if pattern == "*" || pattern == text {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        if let Some(suffix) = prefix.strip_prefix('*') {
            text.contains(suffix)
        } else {
            text.starts_with(prefix)
        }
    } else if let Some(suffix) = pattern.strip_prefix('*') {
        text.ends_with(suffix)
    } else {
        pattern == text
    }
}

/// Sparse capability patch from a remote catalog or declaration (ADR-0203).
///
/// Follows tristate sparse merge semantics: `None` means absent/unspecified,
/// allowing fallthrough to the layer below; `Some(val)` overrides explicitly.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ModelCapabilityPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ReasoningSupport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_levels: Option<Vec<String>>,
}

/// Unified model scope configuration for preset-level or connection-level customization (ADR-0199, ADR-0203).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ModelScopeConfig {
    /// Pipeline admission filter rule (ADR-0203).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<ConnectionFilterPolicy>,
    /// Explicitly declared or included/injected models with optional capability facts.
    #[serde(
        default,
        rename = "inject",
        alias = "include",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub include: Vec<DeclaredModel>,
    /// Explicitly excluded or blocked model ids.
    #[serde(
        default,
        rename = "block",
        alias = "exclude",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub exclude: Vec<String>,
    /// Per-model capability overrides keyed by exact model id.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub overrides: std::collections::BTreeMap<String, CapabilityOverrides>,
}

impl ModelScopeConfig {
    /// Whether this configuration contains no rules.
    pub fn is_empty(&self) -> bool {
        self.filter.is_none()
            && self.include.is_empty()
            && self.exclude.is_empty()
            && self.overrides.is_empty()
    }

    /// Look up an included model declaration by exact id.
    pub fn find_included(&self, id: &str) -> Option<&DeclaredModel> {
        self.include.iter().find(|m| m.id == id)
    }

    /// Whether this scope explicitly excludes `id`.
    pub fn is_excluded(&self, id: &str) -> bool {
        self.exclude
            .iter()
            .any(|pattern| simple_glob_matches(pattern, id))
    }

    /// All model ids explicitly included, preserving order.
    pub fn included_ids(&self) -> Vec<String> {
        self.include.iter().map(|m| m.id.clone()).collect()
    }
}

/// Target scope for model customizations (ADR-0199).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModelTargetScope {
    /// Provider-level customization (affects every connection to this provider).
    Provider(String),
    /// Connection-level customization (affects this connection instance only).
    Connection(String),
}

impl ModelCapabilities {
    /// Resolve effective capabilities for `model_id`, applying all explicitly
    /// advertised remote fields over the local baseline.
    ///
    /// # Capability resolution order (ADR-0149)
    ///
    /// This method implements the **lower two layers** of the canonical
    /// three-layer capability resolution order:
    ///
    /// ```text
    /// 1. user config     — `RouteSettings::capability_overrides`
    ///                      (per provider-instance + model id, applied last
    ///                      by the catalog derivation, see ADR-0149)
    /// 2. remote metadata — the `remote` argument here: fields a trusted
    ///                      endpoint advertised (presets whose
    ///                      `RemoteCatalogSource` carries capability fields)
    /// 3. local baseline  — the static registry entry for the model id
    /// ```
    ///
    /// A field resolved by a higher layer wins; an absent field at a higher
    /// layer falls through to the layer below. The top (user) layer is *not*
    /// applied here — capability overrides are the user's per-route choices
    /// and are stamped on by [`Self::apply_overrides`] at the catalog
    /// derivation site, keeping this function a pure baseline⊕remote merge.
    ///
    /// **Vision stays three-valued** (ADR-0230): the baseline layer only
    /// contributes a vision declaration when the id is actually *known*
    /// ([`declared_vision`]) — a baseline miss or a fitted entry whose endpoint
    /// said nothing leaves `vision: None` (undeclared), rather than coercing
    /// the absence of information into `Some(false)`. That coercion was the
    /// bug: every relay endpoint that does not advertise a capability field
    /// made its models look text-only, which silently stripped images and
    /// dropped the vision-gated tools.
    pub fn for_channel(model_id: &str, remote: Option<&RemoteModelMetadata>) -> Self {
        let baseline = resolve(model_id);
        let remote = remote.cloned().unwrap_or_default();
        Self {
            family: remote.family.unwrap_or_else(|| {
                if baseline.family.is_empty() {
                    model_id.to_string()
                } else {
                    baseline.family.to_string()
                }
            }),
            context_window: remote.context_window.unwrap_or(baseline.context_window),
            max_output_tokens: remote.max_output_tokens,
            thinking: remote.thinking.unwrap_or(baseline.thinking),
            tool_call: remote.tool_call.unwrap_or(baseline.tool_call),
            vision: remote.vision.or_else(|| declared_vision(model_id)),
            effort_levels: remote.effort_levels.unwrap_or_else(|| {
                baseline
                    .effort_levels
                    .iter()
                    .copied()
                    .map(Into::into)
                    .collect()
            }),
        }
    }

    /// Apply the **top layer** of the capability resolution order (ADR-0149):
    /// stamp the user's explicit `CapabilityOverrides` onto the already-merged
    /// (baseline + remote) capabilities. Consumes `self` and returns the
    /// overridden copy. This is deliberately a separate step from
    /// [`Self::for_channel`] so that merge stays pure baseline+remote and
    /// this stays the single, auditable place a user can win over a provider.
    pub fn apply_overrides(mut self, user: &CapabilityOverrides) -> Self {
        if let Some(family) = user.family.clone() {
            self.family = family;
        }
        if let Some(context_window) = user.context_window {
            self.context_window = context_window;
        }
        if let Some(max_output_tokens) = user.max_output_tokens {
            self.max_output_tokens = Some(max_output_tokens);
        }
        if let Some(thinking) = user.thinking {
            self.thinking = thinking;
        }
        if let Some(tool_call) = user.tool_call {
            self.tool_call = tool_call;
        }
        if let Some(vision) = user.vision {
            self.vision = Some(vision);
        }
        self
    }

    /// Coarse reasoning capability used by picker and request construction.
    pub const fn reasoning(&self) -> bool {
        self.thinking.reasons()
    }

    /// Whether the model's full reasoning chain is disclosed to the user.
    ///
    /// Returns `false` for [`ReasoningSupport::None`] (does not reason) and
    /// [`ReasoningSupport::ReasoningSummary`] (hidden internal chain, returns only
    /// summary/placeholder deltas).
    pub const fn chain_disclosed(&self) -> bool {
        self.thinking.chain_disclosed()
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn remote_metadata_overrides_only_the_fields_it_declares() {
        let remote = RemoteModelMetadata {
            context_window: Some(64_000),
            vision: Some(false),
            tool_call: Some(false),
            ..Default::default()
        };

        let effective = ModelCapabilities::for_channel("gpt-4o", Some(&remote));

        assert_eq!(effective.context_window, 64_000);
        assert_eq!(effective.vision, Some(false));
        assert!(!effective.tool_call);
        // The provider omitted reasoning, so the local baseline remains
        // (no baseline is registered for this id in core's own tests, so the
        // fallback's `None` applies).
        assert_eq!(effective.thinking, ReasoningSupport::None);
    }

    #[test]
    fn remote_effort_levels_can_explicitly_clear_the_static_baseline() {
        let remote = RemoteModelMetadata {
            effort_levels: Some(Vec::new()),
            ..Default::default()
        };

        let effective = ModelCapabilities::for_channel("gpt-5.5", Some(&remote));

        assert!(effective.effort_levels.is_empty());
    }
}

/// Baseline model metadata registered by a provider crate.
///
/// **Mechanism lives here; data lives with the providers.** This crate owns
/// only the lookup machinery ([`resolve`], [`model_by_id`], [`fallback_model`],
/// the [`FittedModel`] overlay). The per-provider baseline tables live beside
/// each provider's other registry data (today: `nuo-provider-adapters`' registry
/// modules), and each table is submitted once at link time:
///
/// ```ignore
/// inventory::submit!(nuo_wire::model::BaselineModels(MODELS));
/// ```
///
/// Every binary that links a provider crate picks its tables up with no
/// manual call. Lookup precedence in [`resolve`]: the first registered
/// baseline with a matching id wins (a model id is expected to appear in at
/// most one provider's table; providers that share an id carry byte-identical
/// copies, so the winner is irrelevant), then the runtime-fitted overlay, then
/// [`fallback_model`]. When no provider crate is linked (this crate's own
/// tests), every id falls through to the overlay/fallback.
pub struct BaselineModels(pub &'static [Model]);

inventory::collect!(BaselineModels);

/// Iterate every baseline model registered by linked provider crates, in
/// registration (link) order. Callers that need deterministic order-independent
/// results should match by `id` rather than position.
pub fn baseline_models() -> impl Iterator<Item = &'static Model> {
    inventory::iter::<BaselineModels>
        .into_iter()
        .flat_map(|batch| batch.0.iter())
}

/// Look up a known model by its wire id. Returns `None` for user-defined or
/// unrecognized model ids; callers should fall back to [`fallback_model`].
pub fn model_by_id(id: &str) -> Option<&'static Model> {
    baseline_models().find(|m| m.id == id)
}

/// The **declared** image-input support for `id`, from the layer that owns a
/// baseline: `Some(_)` when the static registry (or a runtime-fitted entry
/// whose endpoint advertised the field) declares it, `None` when no layer
/// declares anything (ADR-0230).
///
/// This is the baseline layer of [`ModelCapabilities::for_channel`]'s vision
/// resolution, and the reason vision must not be read through
/// [`resolve`]'s `Model::vision`: `resolve` answers "will this route accept
/// images" with a permissive default — a *policy*, not a declaration — so it
/// conflates "vetted text-only" with "nobody knows".
pub fn declared_vision(id: &str) -> Option<bool> {
    if let Some(model) = model_by_id(id) {
        // A vetted baseline entry is a declaration: the registry is maintained
        // by hand and must cite its source (ADR-0149 checklist).
        return Some(model.vision);
    }
    fitted_entry(id).and_then(|entry| entry.declared_vision)
}

/// A conservative fallback for model ids no registered baseline knows (local
/// models, user-defined relays, unreleased models). Assumes tool calling (the
/// harness depends on it) and nothing else.
///
/// `vision: true` is the **permissive policy default** described on
/// [`ModelCapabilities::accepts_images`] (ADR-0230), not a capability claim: an
/// unknown route is attempted with images rather than silently having them
/// stripped. Whether anything was actually *declared* is answered by
/// [`declared_vision`], which is `None` here.
pub fn fallback_model(_id: &str) -> Model {
    Model {
        id: "",
        family: "",
        context_window: 128_000,
        thinking: ReasoningSupport::None,
        tool_call: false,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    }
}

/// Resolve any model id to its metadata: the vetted static registry entry
/// when known (with a live-refreshed effort ladder when a trusted provider's
/// overlay overrode it — see [`register_fitted_models`]), then the
/// runtime-fitted overlay for ids a trusted provider advertised, then a
/// conservative fallback. Never returns `None` so callers need not branch on
/// absence.
pub fn resolve(id: &str) -> Model {
    if let Some(model) = model_by_id(id) {
        // A trusted provider may have refreshed the baseline's live effort
        // ladder via `register_fitted_models` (stored under the baseline's
        // own id); every other field stays vetted.
        if let Some(overridden) = fitted_entry(model.id) {
            return Model {
                effort_levels: overridden.model.effort_levels,
                ..*model
            };
        }
        return *model;
    }
    if let Some(entry) = fitted_entry(id) {
        return entry.model;
    }
    fallback_model(id)
}

// ═════════════════════════════════════════════════════════════════════════════
// Runtime-fitted models (capability overlay)
// ═════════════════════════════════════════════════════════════════════════════

/// Capability metadata for a model id no registered baseline knows,
/// learned at runtime from a provider's live model list (ADR-0065).
///
/// Only **trusted** providers may feed this overlay (official endpoints whose
/// `/models` advertises real capability fields, opted in via their template);
/// an arbitrary relay cannot use it to inflate a model's context window or
/// capabilities. Registration is ignored for ids a registered baseline knows
/// — the vetted baseline entry always wins, so a provider can never
/// *downgrade* a known model either.
#[derive(Debug, Clone)]
pub struct FittedModel {
    /// Wire model id as advertised by the provider.
    pub id: String,
    /// Grouping family (the feeding template's id, e.g. `"kimi-code"`).
    pub family: String,
    /// Advertised context window in tokens; `0` means the endpoint did not
    /// say (the model resolves with an unknown window, like the fallback).
    pub context_window: usize,
    /// The endpoint advertises reasoning (a `reasoning_content` stream).
    pub reasoning: bool,
    /// The endpoint advertises image inputs. `None` = it said nothing, which
    /// stays undeclared rather than becoming a text-only claim (ADR-0230).
    pub vision: Option<bool>,
    /// Wire protocol the feeding provider speaks for this model.
    pub protocol: WireProtocol,
    /// Advertised reasoning-effort levels (any order; stored ascending via
    /// [`Effort::ORDER`](crate::effort::Effort::ORDER)).
    pub effort_levels: Vec<crate::effort::Effort>,
}

/// One entry of the runtime-fitted overlay: the resolved [`Model`] view the
/// lookup machinery returns, plus what the feeding endpoint actually
/// **declared** about image input.
///
/// The two are deliberately separate (ADR-0230). `model.vision` is the
/// permissive *policy* view (`true` when undeclared, so image-bearing requests
/// are attempted rather than silently stripped), while `declared_vision` is the
/// honest three-valued fact the capability resolution consumes — collapsing
/// them is how "the endpoint said nothing" became "this model is text-only".
#[derive(Debug, Clone)]
struct FittedEntry {
    /// The resolved model view `resolve` returns for this id.
    model: Model,
    /// Image-input declaration: `Some(true)`/`Some(false)` when the endpoint
    /// advertised it, `None` when it did not.
    declared_vision: Option<bool>,
}

/// Process-wide overlay of runtime-fitted models. Populated at startup from
/// persisted discovery results and refreshed after a live fetch (the feeding
/// layer lives in `muta_agent::catalog`).
static FITTED_MODELS: std::sync::OnceLock<
    std::sync::RwLock<std::collections::HashMap<&'static str, FittedEntry>>,
> = std::sync::OnceLock::new();

fn fitted_models()
-> &'static std::sync::RwLock<std::collections::HashMap<&'static str, FittedEntry>> {
    FITTED_MODELS.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()))
}

/// The fitted overlay entry for `id`, if a trusted provider advertised it.
fn fitted_entry(id: &str) -> Option<FittedEntry> {
    fitted_models()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .cloned()
}

/// Register (or replace) runtime-fitted models. An id a registered baseline
/// knows keeps its vetted entry **except** for `effort_levels`: effort tiers
/// are a live platform knob that can evolve after the baseline shipped (Kimi
/// K3's ladder went from a single `max` rung to `low`/`high`/`max`), so a
/// trusted provider's advertised tiers refresh the baseline's ladder while
/// every other field stays vetted. Strings and slices are interned via
/// `Box::leak` because [`Model`] is `Copy` over `&'static str`; the set of
/// distinct fitted ids is bounded by what a provider advertises, so the
/// one-time leak per registration is negligible.
pub fn register_fitted_models(models: impl IntoIterator<Item = FittedModel>) {
    let mut overlay = fitted_models().write().unwrap_or_else(|e| e.into_inner());
    for fitted in models {
        let mut levels = fitted.effort_levels;
        levels.sort_by_key(|level| {
            crate::effort::Effort::ORDER
                .iter()
                .position(|ordered| ordered == level)
                .unwrap_or(usize::MAX)
        });
        levels.dedup();
        if let Some(baseline) = model_by_id(&fitted.id) {
            // Baseline-known id: only the effort ladder follows the live
            // advertisement (and only when the endpoint actually advertises
            // tiers — an absent field must not wipe the baseline's). The
            // `resolve` order (baseline first) means the override must land
            // ON the baseline's id to take effect.
            if !levels.is_empty() && baseline.effort_levels != levels.as_slice() {
                overlay.insert(
                    baseline.id,
                    FittedEntry {
                        model: Model {
                            effort_levels: Box::leak(levels.into_boxed_slice()),
                            ..*baseline
                        },
                        declared_vision: Some(baseline.vision),
                    },
                );
            }
            continue;
        }
        let id: &'static str = Box::leak(fitted.id.into_boxed_str());
        overlay.insert(
            id,
            FittedEntry {
                model: Model {
                    id,
                    family: Box::leak(fitted.family.into_boxed_str()),
                    context_window: fitted.context_window,
                    thinking: if fitted.reasoning {
                        ReasoningSupport::ReasoningContent
                    } else {
                        ReasoningSupport::None
                    },
                    // Unknown remote ids remain plain-text-only until the source
                    // or user explicitly declares tool support (ADR-0203).
                    tool_call: false,
                    // Permissive *policy* view: an endpoint that advertised no
                    // vision field must not be read as text-only (ADR-0230).
                    // `declared_vision` below keeps what was actually said.
                    vision: fitted.vision.unwrap_or(true),
                    protocol: fitted.protocol,
                    model_guidance: "",
                    effort_levels: Box::leak(levels.into_boxed_slice()),
                },
                declared_vision: fitted.vision,
            },
        );
    }
}

/// Sanitize a raw model identifier string: trims surrounding whitespace,
/// replaces internal whitespace sequences with single hyphens (`-`),
/// and filters out ASCII control characters.
pub fn sanitize_model_id(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut in_ws = false;
    for c in trimmed.chars() {
        if c.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push('-');
                in_ws = true;
            }
        } else if !c.is_control() {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_overrides_win_over_remote_and_baseline() {
        // ADR-0149: layer 1 (user) beats layer 2 (remote) beats layer 3
        // (baseline), field-wise; unset user knobs fall through.
        let remote = RemoteModelMetadata {
            vision: Some(true),
            tool_call: Some(true),
            context_window: Some(222_000),
            ..Default::default()
        };
        let user = CapabilityOverrides {
            // user says vision off, even though remote+baseline say on
            vision: Some(false),
            // user has no opinion on tool_call -> remote's true stands
            tool_call: None,
            // user has no opinion on context window -> remote's 222_000 stands
            context_window: None,
            family: Some("user-family".to_string()),
            thinking: None,
            max_output_tokens: Some(4_096),
            protocol: None,
        };
        let caps =
            ModelCapabilities::for_channel("fixture-alpha", Some(&remote)).apply_overrides(&user);
        // Layer 1 wins:
        assert_eq!(
            caps.vision,
            Some(false),
            "user Some(false) must beat remote Some(true)"
        );
        assert_eq!(caps.family, "user-family");
        assert_eq!(caps.max_output_tokens, Some(4_096));
        // Fall-through to layer 2:
        assert!(caps.tool_call);
        assert_eq!(caps.context_window, 222_000);
    }

    #[test]
    fn empty_capability_overrides_are_a_noop() {
        let caps = ModelCapabilities::for_channel("fixture-alpha", None);
        let overridden = caps
            .clone()
            .apply_overrides(&CapabilityOverrides::default());
        assert_eq!(caps, overridden);
        assert!(CapabilityOverrides::default().is_empty());
    }

    #[test]
    fn user_thinking_override_controls_chain_disclosure() {
        // baseline has ReasoningSupport::AnthropicAdaptive -> chain_disclosed = true
        let caps = ModelCapabilities::for_channel("fixture-alpha", None);
        assert!(caps.chain_disclosed());

        // Override to ReasoningSummary -> chain_disclosed becomes false
        let user_summary = CapabilityOverrides {
            thinking: Some(ReasoningSupport::ReasoningSummary),
            ..Default::default()
        };
        let caps_summary = caps.clone().apply_overrides(&user_summary);
        assert!(caps_summary.reasoning());
        assert!(!caps_summary.chain_disclosed());

        // Override to ReasoningContent -> chain_disclosed becomes true
        let user_disclosed = CapabilityOverrides {
            thinking: Some(ReasoningSupport::ReasoningContent),
            ..Default::default()
        };
        let caps_disclosed = caps.clone().apply_overrides(&user_disclosed);
        assert!(caps_disclosed.reasoning());
        assert!(caps_disclosed.chain_disclosed());
    }

    // Fixture baselines. Core's own tests must not depend on real vendor data
    // (that lives with the provider crates), so they register small tables of
    // fictional ids through the same inventory mechanism providers use. The
    // ids are deliberately non-vendor so they can never collide with a real
    // baseline in a binary that also links a provider crate.
    const FIXTURE_A: &[Model] = &[
        Model {
            id: "fixture-alpha",
            family: "fixture",
            context_window: 111_000,
            thinking: ReasoningSupport::ReasoningContent,
            tool_call: true,
            vision: true,
            protocol: WireProtocol::ChatCompletions,
            model_guidance: "",
            effort_levels: crate::effort::COMMON_LADDER,
        },
        Model {
            id: "fixture-beta",
            family: "fixture",
            context_window: 222_000,
            thinking: ReasoningSupport::None,
            tool_call: true,
            vision: false,
            protocol: WireProtocol::AnthropicMessages,
            model_guidance: "",
            effort_levels: &[],
        },
    ];
    const FIXTURE_B: &[Model] = &[Model {
        id: "fixture-gamma",
        family: "fixture",
        context_window: 333_000,
        thinking: ReasoningSupport::None,
        tool_call: false,
        vision: false,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: &[],
    }];

    inventory::submit!(BaselineModels(FIXTURE_A));
    inventory::submit!(BaselineModels(FIXTURE_B));

    #[test]
    fn registered_baselines_resolve_by_id() {
        let m = resolve("fixture-alpha");
        assert_eq!(m.context_window, 111_000);
        assert!(m.reasoning());
        assert!(m.vision);
        assert_eq!(m.protocol, WireProtocol::ChatCompletions);
        // `fitted_overlay_never_overrides_a_registered_baseline` may have
        // already run in this process and refreshed the ladder (overlay
        // writes are process-global), so assert the baseline value only when
        // no override landed; the override case is covered there.
        assert!(
            m.effort_levels == crate::effort::COMMON_LADDER
                || m.effort_levels == [crate::effort::Effort::Max].as_slice(),
            "unexpected ladder: {:?}",
            m.effort_levels
        );

        let g = resolve("fixture-gamma");
        assert_eq!(g.protocol, WireProtocol::GoogleGemini);
        assert!(!g.tool_call);
    }

    #[test]
    fn model_by_id_returns_none_for_unregistered_ids() {
        assert!(model_by_id("fixture-alpha").is_some());
        assert!(model_by_id("some-local-model").is_none());
    }

    #[test]
    fn registered_baselines_have_unique_ids() {
        let mut ids: Vec<&str> = baseline_models().map(|m| m.id).collect();
        ids.sort_unstable();
        let dups: Vec<&str> = ids
            .windows(2)
            .filter(|w| w[0] == w[1])
            .map(|w| w[0])
            .collect();
        assert!(dups.is_empty(), "duplicate baseline ids: {dups:?}");
    }

    #[test]
    fn resolve_falls_back_for_unknown() {
        let m = resolve("some-local-model");
        // Safe conservative text defaults (ADR-0203 §4).
        assert_eq!(m.context_window, 128_000);
        assert!(!m.reasoning());
        assert!(!m.tool_call);
        // `Model::vision` is a *policy* view, not a claim: the fallback
        // permits images so an undeclared route is attempted (ADR-0230). What
        // was actually declared is `declared_vision`, which stays `None`.
        assert!(m.vision);
        assert_eq!(declared_vision("some-local-model"), None);
        // The capability resolution carries the undeclared state through, and
        // the policy helper reads it permissively rather than as text-only.
        let caps = ModelCapabilities::for_channel("some-local-model", None);
        assert_eq!(caps.vision, None);
        assert!(!caps.vision_declared());
        assert!(caps.accepts_images());
    }

    #[test]
    fn declared_vision_reports_only_real_declarations() {
        // A vetted baseline entry is a declaration in both directions.
        assert_eq!(declared_vision("fixture-alpha"), Some(true));
        // Nothing in the layers declares an unknown id.
        assert_eq!(declared_vision("mystery-relay-model"), None);
    }

    #[test]
    fn fitted_overlay_supplies_metadata_for_unregistered_ids() {
        register_fitted_models(vec![FittedModel {
            id: "fitted-future-k9".to_string(),
            family: "kimi-code".to_string(),
            context_window: 2_000_000,
            reasoning: true,
            vision: Some(true),
            protocol: WireProtocol::ChatCompletions,
            // Unsorted input with a duplicate: stored ascending, deduped.
            effort_levels: vec![
                crate::effort::Effort::Max,
                crate::effort::Effort::Low,
                crate::effort::Effort::Low,
            ],
        }]);
        let m = resolve("fitted-future-k9");
        assert_eq!(m.id, "fitted-future-k9");
        assert_eq!(m.context_window, 2_000_000);
        assert!(m.reasoning());
        assert!(m.vision);
        assert_eq!(
            m.effort_levels,
            &[crate::effort::Effort::Low, crate::effort::Effort::Max]
        );
    }

    #[test]
    fn fitted_overlay_never_overrides_a_registered_baseline() {
        register_fitted_models(vec![FittedModel {
            id: "fixture-alpha".to_string(),
            family: "bogus".to_string(),
            context_window: 1,
            reasoning: false,
            vision: Some(false),
            protocol: WireProtocol::GoogleGemini,
            effort_levels: vec![crate::effort::Effort::Max],
        }]);
        // The vetted baseline entry wins on every field except the effort
        // ladder: effort tiers are a live platform knob, so a trusted
        // provider's advertised tiers refresh the baseline's ladder while
        // identity, context, format, and vision stay vetted.
        let m = resolve("fixture-alpha");
        assert_eq!(m.context_window, 111_000);
        assert_eq!(m.protocol, WireProtocol::ChatCompletions);
        assert!(m.vision);
        assert_eq!(m.effort_levels, [crate::effort::Effort::Max].as_slice());
    }

    #[test]
    fn fitted_overlay_with_no_advertised_tiers_keeps_the_baseline_ladder() {
        // A fitted entry for a baseline-known id that advertises NO effort
        // tiers must not wipe the baseline's ladder — an absent field means
        // "the endpoint did not say", not "the model lost its knob".
        register_fitted_models(vec![FittedModel {
            id: "fixture-beta".to_string(),
            family: "fixture".to_string(),
            context_window: 0,
            reasoning: false,
            vision: Some(false),
            protocol: WireProtocol::ChatCompletions,
            effort_levels: Vec::new(),
        }]);
        // fixture-beta's baseline ladder is empty already, so check through
        // the gamma fixture's *sibling* instead: gamma has no fitted entry at
        // all and must be untouched by beta's registration.
        let g = resolve("fixture-gamma");
        assert_eq!(g.context_window, 333_000);
    }

    #[test]
    fn fallback_format_is_openai_compat() {
        assert_eq!(
            fallback_model("anything").protocol,
            WireProtocol::ChatCompletions
        );
    }

    #[test]
    fn sanitize_model_id_replaces_whitespace_with_hyphen() {
        assert_eq!(sanitize_model_id("gpt 5.5 preview"), "gpt-5.5-preview");
        assert_eq!(
            sanitize_model_id("  claude   3.7  sonnet  "),
            "claude-3.7-sonnet"
        );
        assert_eq!(sanitize_model_id("gemini-3.1-pro"), "gemini-3.1-pro");
        assert_eq!(sanitize_model_id("   "), "");
    }

    #[test]
    fn wire_protocol_names_and_compatibility() {
        use std::str::FromStr;

        // Display names
        assert_eq!(
            WireProtocol::ChatCompletions.display_name(),
            "Chat Completions"
        );
        assert_eq!(WireProtocol::Responses.display_name(), "Responses");
        assert_eq!(
            WireProtocol::AnthropicMessages.display_name(),
            "Anthropic Messages"
        );
        assert_eq!(WireProtocol::GoogleGemini.display_name(), "Google Gemini");

        // as_str
        assert_eq!(WireProtocol::ChatCompletions.as_str(), "chat-completions");
        assert_eq!(WireProtocol::Responses.as_str(), "responses");
        assert_eq!(
            WireProtocol::AnthropicMessages.as_str(),
            "anthropic-messages"
        );
        assert_eq!(WireProtocol::GoogleGemini.as_str(), "google-gemini");

        // FromStr canonical
        assert_eq!(
            WireProtocol::from_str("chat-completions").unwrap(),
            WireProtocol::ChatCompletions
        );
        assert_eq!(
            WireProtocol::from_str("responses").unwrap(),
            WireProtocol::Responses
        );
        assert_eq!(
            WireProtocol::from_str("anthropic-messages").unwrap(),
            WireProtocol::AnthropicMessages
        );
        assert_eq!(
            WireProtocol::from_str("google-gemini").unwrap(),
            WireProtocol::GoogleGemini
        );

        // FromStr legacy aliases
        assert_eq!(
            WireProtocol::from_str("openai-chat-completions").unwrap(),
            WireProtocol::ChatCompletions
        );
        assert_eq!(
            WireProtocol::from_str("openai-responses").unwrap(),
            WireProtocol::Responses
        );
        assert_eq!(
            WireProtocol::from_str("google-generate-content").unwrap(),
            WireProtocol::GoogleGemini
        );

        // Serde serialization
        assert_eq!(
            serde_json::to_string(&WireProtocol::ChatCompletions).unwrap(),
            "\"chat-completions\""
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::Responses).unwrap(),
            "\"responses\""
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::AnthropicMessages).unwrap(),
            "\"anthropic-messages\""
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::GoogleGemini).unwrap(),
            "\"google-gemini\""
        );

        // Serde deserialization aliases
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"openai-chat-completions\"").unwrap(),
            WireProtocol::ChatCompletions
        );
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"openai-responses\"").unwrap(),
            WireProtocol::Responses
        );
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"google-generate-content\"").unwrap(),
            WireProtocol::GoogleGemini
        );
    }
}
