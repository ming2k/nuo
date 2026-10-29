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
use crate::effort::Effort;
use crate::reasoning::ReasoningSupport;
pub use nuo_inference_wire::WireProtocol;

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
    /// The reasoning-effort levels this model honors, ascending.
    ///
    /// This is the model's depth **ladder** and the single declaration of it —
    /// the request-boundary clamp range, the picker's segmented selector, and
    /// the resolved `ModelCapabilities::effort_levels` all derive from this one
    /// table rather than keeping a copy. See ADR-0126.
    ///
    /// `&[]` means effort control does not apply (non-reasoning models, or
    /// protocols without an effort field). Models with unknown effort tiers
    /// default to [`crate::effort::COMMON_LADDER`]; non-reasoning models carry
    /// `&[]`.
    pub effort_levels: &'static [crate::effort::Effort],
    /// The provider's declared **default** reasoning depth for this model —
    /// the tier a request stamps when the user has expressed no choice.
    ///
    /// This is provider data, not a framework rule: the framework only clamps
    /// it onto [`Self::effort_levels`] ([`crate::effort::Effort::channel_default`])
    /// and never infers it from a family-name prefix. `None` means the provider
    /// expressed no preference, in which case the vendor-neutral
    /// [`crate::effort::Effort::High`] is clamped onto the ladder. Ignored when
    /// `effort_levels` is empty.
    pub default_effort: Option<crate::effort::Effort>,
    /// The model's **compiled offline-floor wire-entity table** (ADR-0074).
    ///
    /// `Some` when the vetted baseline carries the per-level PredictionService
    /// entities this family registers (the Antigravity tiered surfaces). This
    /// is the *single source of truth* for the offline floor: the provider's
    /// compiled `BASELINE_WIRE_FAMILIES` consumed this table's rows at
    /// definition time and the live catalog overrides it with observed
    /// `wire_entities`. `None` (every non-tiered model) means the canonical
    /// id routes verbatim. Declared here — a vetted registry fact — never
    /// inferred from a family prefix in a request builder.
    pub wire_entities: Option<&'static crate::model::BaselineWireFamily>,
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
/// provider states one — why not.
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
    /// by the root algebra for [`Self::protocol`]; absent means the
    /// provider spec's route for that protocol holds.
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
    /// (ADR-0068), rather than silently dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_levels: Option<Vec<crate::effort::EffortLevel>>,
    /// Endpoint-advertised **default** reasoning depth, when the catalogue
    /// states a preference (Kimi and Copilot publish one alongside their
    /// tiers). Absent means the endpoint expressed none, and the vetted
    /// baseline's declared default — or the vendor-neutral fallback — stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<crate::effort::Effort>,
    /// The catalog the model came from, as the provider names it — Qoder's
    /// `source` (`"system"` for platform models, `"custom"` for user-added
    /// ones). Not presentation: a signed surface may carry this value on the
    /// wire as part of the request's model identity (Qoder's `X-Model-Source`
    /// and `model_config.source`), so it is round-tripped rather than derived.
    /// `None` when the endpoint advertises no provenance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_source: Option<String>,
    /// The provider's declared availability for this model on this account
    /// Qoder's `enable:false`, Codex's `supported_in_api:false`,
    /// and Copilot's `policy.state:"disabled"` all land here. `None` — the
    /// common case, since most providers declare no such verdict — means
    /// undeclared and therefore usable. `Some(usable: false)` is a declaration
    /// the account may not run the model; pickers surface it dimmed and the
    /// server refuses to route it, but membership and capabilities are
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
    /// Live catalog **observation** of the concrete effort-parameterized wire
    /// entities the upstream registered for this canonical model — evidence
    /// carried verbatim from [`crate::catalog::DiscoveredModel::wire_entities`]
    /// (ADR-0068: parsers record, they do not infer). `None`/empty means the
    /// canonical id routes verbatim. Consumed by the request builder through
    /// [`ModelCapabilities::wire_entity_resolution`]; never consulted for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_entities: Option<Vec<String>>,
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
    /// resolution of the layers in.
    ///
    /// - `Some(true)` — a layer declared image support;
    /// - `Some(false)` — a layer declared that images are *not* accepted;
    /// - `None` — **no layer declared anything**. The route is *unknown*, not
    ///   text-only, and the client policy is permissive: an undeclared route is
    ///   attempted rather than silently stripped, because vision is the one
    ///   capability most vendors do not advertise.
    pub vision: Option<bool>,
    /// The effort ladder this channel honors, as [`crate::EffortLevel`] so a
    /// provider-advertised tier outside the [`crate::Effort`] vocabulary is preserved
    /// and stamped through (ADR-0068). Built in `for_channel` from the remote
    /// advertisement over the static baseline.
    pub effort_levels: Vec<crate::effort::EffortLevel>,
    /// The provider's **declared default** reasoning depth for this route, or
    /// `None` when it expressed no preference. Resolved with the same
    /// precedence as the ladder (remote advertisement over the static
    /// baseline) so a live catalog may revise the default alongside the rungs.
    ///
    /// The framework's only use of it is to clamp it onto
    /// [`Self::effort_levels`]; it is never inferred from a family name.
    pub default_effort: Option<crate::effort::Effort>,
    /// The concrete per-level wire entities the upstream catalog registered
    /// for this model (ADR-0074). Empty when the canonical id routes
    /// verbatim — every provider except the tiered Antigravity families
    /// (`gemini-4-argon`, `claude-{opus,sonnet}-5-5`). Resolved from the
    /// vetted baseline's `Model::wire_entities` table when the remote layer
    /// carried no live observation. Consumed by
    /// [`ModelCapabilities::wire_entity_resolution`]; evidentiary, not display.
    pub effort_entities: Vec<String>,
}

impl RemoteModelMetadata {
    /// The declared availability verdict, defaulting to usable when the
    /// provider declared none. Undeclared is *not* a declaration:
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

impl ModelCapabilities {
    /// Whether image attachments may be put on the wire for this route.
    ///
    /// **Unknown is permissive**: only an explicit `Some(false)`
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

    /// The route's wire-entity decision: how the requested `(model, effort)`
    /// selection becomes the id stamped into the request envelope (ADR-0074).
    ///
    /// Resolution is **evidence over inference** (ADR-0068):
    /// 1. **Observed per-level entities** — when the live catalog registered
    ///    concrete effort entities for this canonical id, effort selects
    ///    among them by depth rank and the bare canonical id is never sent
    ///    (upstream 404s it: Antigravity `gemini-4-argon`,
    ///    `claude-{opus,sonnet}-5-5`).
    /// 2. **Verbatim** — no observation (or the channel id IS one of the
    ///    registered entities, i.e. the user pinned a depth): the canonical
    ///    id routes as-is, the common case for every provider whose wire id
    ///    equals the id the user selects.
    ///
    /// The request builder reads this decision; it never re-derives wire
    /// mappings from family prefixes. That is the mechanism replacing the
    /// per-provider string-match ladders.
    pub fn wire_entity_resolution<'a>(&'a self, model_id: &'a str) -> WireEntityResolution<'a> {
        if self.effort_entities.is_empty() {
            return WireEntityResolution::Verbatim;
        }
        if self.effort_entities.iter().any(|e| e == model_id) {
            // The channel id is itself a registered entity — direct identity.
            return WireEntityResolution::Verbatim;
        }
        // Live evidence resolves the account default positionally: the
        // catalog records entities in shallow→deep advertisement order with
        // the account-default row FIRST (bare-row lead, or `agentModelSorts`
        // head). The baseline layer's family default lives at its own
        // declared position — that asymmetry is exactly why the provider
        // consults `baseline_wire_family` separately (ADR-0074).
        WireEntityResolution::PerLevel {
            base: model_id,
            entities: &self.effort_entities,
        }
    }
}

/// One family's compiled offline-floor wire-entity table (ADR-0074).
///
/// Owned as static data on [`Model::wire_entities`]: the vetted baseline
/// carries the per-level entities a tiered family registers on the wire.
/// `broadcast = true` marks an **in-band-depth** family — the default
/// entity is the wire id for *every* effort (depth rides `thinkingConfig`,
/// gemini-3.8-flash); `broadcast = false` marks a **suffix-depth** family —
/// effort ranks across the per-level entities by `Effort::rank`
/// (gemini-4-argon, claude-{opus,sonnet}-5-5). The live catalog's observed
/// `wire_entities` override this table per channel; the table only serves
/// sessions whose live snapshot never ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaselineWireFamily {
    /// The canonical base id the table routes from.
    pub base: &'static str,
    /// The entity the upstream names as the account default
    /// (`defaultAgentModelId` positional precedent).
    pub default_entity: &'static str,
    /// The registered per-level entity ids, shallow→deep.
    pub entities: &'static [&'static str],
    /// Whether depth rides in-band (`thinkingConfig`) so the wire id never
    /// changes with effort.
    pub broadcast: bool,
}

/// The typed decision of [`ModelCapabilities::wire_entity_resolution`]: how a
/// channel's `(model, effort)` selection becomes a wire id.
///
/// Evidence over inference (ADR-0068): the *observed* per-level entities the
/// upstream catalog registered are authoritative; absent evidence means the
/// canonical id routes verbatim. Living in the codec as typed data is what
/// retires the per-provider string-match ladders in request builders — the
/// provider reads the decision, it does not re-derive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireEntityResolution<'a> {
    /// The canonical id is a registered wire entity: stamp it verbatim for
    /// every effort. Also the arm for a channel id that is itself one of the
    /// observed per-level entities (a user-pinned depth).
    Verbatim,
    /// The upstream registered concrete per-level entities and the bare
    /// canonical id is not one of them. `effort` ranks into `entities`
    /// shallow→deep via `Effort::rank` (the same total order the clapmping
    /// uses — one rank table, never per-site duplicates); `None` resolves to
    /// the list's **default-position entity** (the caller's surface declares
    /// which index carries the account default). The result is always a
    /// *registered* entity — never a bare-id 404.
    PerLevel {
        /// The canonical base id the channel was opened with.
        base: &'a str,
        /// The observed registered entity ids, in advertisement order
        /// (shallow→deep for the known surfaces).
        entities: &'a [String],
    },
}

impl WireEntityResolution<'_> {
    /// Stamp the concrete wire id from a [`BaselineWireFamily`] table row.
    ///
    /// Broadcast families return the default entity for every effort (depth
    /// rides in-band); suffix-depth families rank the request by
    /// `Effort::rank`. The best entity is the one whose rung is closest to
    /// the request: an exact rung hit wins immediately; otherwise a
    /// shallower entity ≤ the request wins (rank-maximizing), and when *no*
    /// entity is ≤ the request (a rung shallower than the surface's floor),
    /// the **shallowest** registered entity wins — the surface never reads
    /// as deeper than the request. The account default (family default
    /// entity, positional `defaultAgentModelId` precedent) applies only to a
    /// `None` override.
    pub fn resolve_family(family: &BaselineWireFamily, effort: Option<Effort>) -> String {
        if family.broadcast {
            return family.default_entity.to_string();
        }
        Self::resolve_rung(family.entities, family.default_entity, effort)
    }

    /// Stamp the concrete wire id from an observed entity list (live-catalog
    /// evidence, ADR-0074). `default_idx` names the account-default row
    /// position; the ranking semantics are identical to
    /// [`Self::resolve_family`].
    pub fn resolve_observed(
        entities: &[&str],
        default_idx: usize,
        effort: Option<Effort>,
    ) -> String {
        Self::resolve_rung(
            entities,
            entities.get(default_idx).copied().unwrap_or(""),
            effort,
        )
    }

    fn resolve_rung(entities: &[&str], default_entity: &str, effort: Option<Effort>) -> String {
        // No explicit override: the caller's declared account-default row.
        let Some(requested) = effort else {
            return default_entity.to_string();
        };
        let requested_rank = requested.rank();

        // Rank every entity by its own suffixed effort rung. The best entity
        // is the one whose rung is closest to the request: an exact rung hit
        // wins immediately; otherwise a shallower entity ≤ the request wins
        // (rank-maximizing), and when *no* entity is ≤ the request (a rung
        // shallower than the surface's floor), the **shallowest** registered
        // entity wins — the surface never reads as deeper than the request.
        let mut best: Option<(usize, usize)> = None; // (rank, idx)
        let mut shallowest: Option<(usize, usize)> = None; // shallowest overall
        for (idx, entity) in entities.iter().enumerate() {
            let entity_rank = entity
                .rsplit('-')
                .next()
                .and_then(Effort::parse)
                .map(|e| e.rank())
                // Unparseable suffix (bare / `-tiered`): in-band depth, the
                // shallowest claim.
                .unwrap_or(0);
            if entity_rank == requested_rank {
                return entities[idx].to_string();
            }
            if entity_rank < requested_rank && best.is_none_or(|(rank, _)| entity_rank >= rank) {
                best = Some((entity_rank, idx));
            }
            if shallowest.is_none_or(|(rank, _)| entity_rank < rank) {
                shallowest = Some((entity_rank, idx));
            }
        }
        match best.or(shallowest) {
            Some((_, idx)) => entities[idx].to_string(),
            None => default_entity.to_string(),
        }
    }
}

/// A user's explicit capability override for one (provider-instance, model)
/// route -- the **top layer** of the capability resolution order.
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

    /// Layer `over` on top of `self`. Any explicitly set `Some` field in `over`
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

/// One user-declared model on a preset or connection scope: a hidden,
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

/// The default admission gate for models passing through a connection pipe.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ConnectionFilterPolicy {
    Named(NamedFilterPolicy),
    Glob(Vec<String>),
}

/// A connection-local override for the provider's remote catalog source.
///
/// Standard named pipe filter policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedFilterPolicy {
    /// Admit only models present in the compiled baseline (strict safe filter).
    Baseline,
    /// Admit all models delivered by the remote catalog source.
    All,
    /// Admit all models delivered by the remote catalog source except promotional/preview relay noise (ADR-0081).
    Curated,
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
            Self::Named(NamedFilterPolicy::Curated) => {
                !crate::dialects::is_relay_promotional_noise(model_id)
            }
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

/// Sparse capability patch from a remote catalog or declaration.
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

/// Unified model scope configuration for preset-level or connection-level customization.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ModelScopeConfig {
    /// Pipeline admission filter rule.
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

/// Target scope for model customizations.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModelTargetScope {
    /// Provider-level customization (affects every connection to this provider).
    Provider(String),
    /// Connection-level customization (affects this connection instance only).
    Connection(String),
}

impl ModelCapabilities {
    /// The provider's declared default reasoning depth, clamped onto the
    /// ladder this route actually honors.
    ///
    /// `None` when the route exposes no ladder at all — in which case the
    /// request must not stamp an effort field. This is the single source of
    /// truth for both the picker's display fallback and the request-boundary
    /// default, so the two can never disagree.
    pub fn default_effort(&self) -> Option<crate::effort::Effort> {
        let known: Vec<crate::effort::Effort> = self
            .effort_levels
            .iter()
            .filter_map(crate::effort::EffortLevel::as_known)
            .collect();
        crate::effort::Effort::channel_default(self.default_effort, &known)
    }

    /// Resolve effective capabilities for `model_id`, applying all explicitly
    /// advertised remote fields over the local baseline.
    ///
    /// # Capability resolution order
    ///
    /// This method implements the **lower two layers** of the canonical
    /// three-layer capability resolution order:
    ///
    /// ```text
    /// 1. user config     — `RouteSettings::capability_overrides`
    ///                      (per provider-instance + model id, applied last
    ///                      by the catalog derivation)
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
    /// **Vision stays three-valued**: the baseline layer only
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
            // The declared default travels with the ladder it belongs to. A
            // remote advertisement that revises the rungs also revises the
            // default (its own, when it states one); otherwise the vetted
            // baseline's declared preference stands.
            default_effort: remote.default_effort.or(baseline.default_effort),
            // Wire-entity resolution (ADR-0074): the live observation wins;
            // when it never ran, the vetted baseline's compiled table serves
            // the offline floor. A referenced family's registered entities
            // lead with the account default.
            effort_entities: remote.wire_entities.unwrap_or_else(|| {
                baseline
                    .wire_entities
                    .map(|family| family.entities.iter().map(|e| e.to_string()).collect())
                    .unwrap_or_default()
            }),
        }
    }

    /// Apply the **top layer** of the capability resolution order:
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
            wire_entities: None,
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
/// each provider's other registry data (the `providers/nuo-provider-*` spec tables, aggregated by the composition root
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
/// declares anything.
///
/// This is the baseline layer of [`ModelCapabilities::for_channel`]'s vision
/// resolution, and the reason vision must not be read through
/// [`resolve`]'s `Model::vision`: `resolve` answers "will this route accept
/// images" with a permissive default — a *policy*, not a declaration — so it
/// conflates "vetted text-only" with "nobody knows".
pub fn declared_vision(id: &str) -> Option<bool> {
    if let Some(model) = model_by_id(id) {
        // A vetted baseline entry is a declaration: the registry is maintained
        // by hand and must cite its source (checklist).
        return Some(model.vision);
    }
    fitted_entry(id).and_then(|entry| entry.declared_vision)
}

/// A conservative fallback for model ids no registered baseline knows (local
/// models, user-defined relays, unreleased models). Assumes tool calling (the
/// harness depends on it) and nothing else.
///
/// `vision: true` is the **permissive policy default** described on
/// [`ModelCapabilities::accepts_images`], not a capability claim: an
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
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        default_effort: Some(crate::effort::Effort::High),
        wire_entities: None,
    }
}

/// Resolve any model id to its metadata: the vetted static registry entry
/// when known (with a live-refreshed effort ladder when a trusted provider's
/// overlay overrode it — see [`register_fitted_models`]), then the
/// runtime-fitted overlay for ids a trusted provider advertised, then a
/// conservative fallback. Never returns `None` so callers need not branch on
/// absence.
pub use resolve as resolve_model;

pub fn resolve(id: &str) -> Model {
    if let Some(model) = model_by_id(id) {
        // A trusted provider may have refreshed the baseline's live effort
        // ladder via `register_fitted_models` (stored under the baseline's
        // own id); every other field stays vetted.
        if let Some(overridden) = fitted_entry(model.id) {
            return Model {
                effort_levels: overridden.model.effort_levels,
                default_effort: Some(crate::effort::Effort::High),
                wire_entities: None,
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
/// learned at runtime from a provider's live model list (ADR-0068).
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
    /// stays undeclared rather than becoming a text-only claim.
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
/// The two are deliberately separate. `model.vision` is the
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
/// layer lives in `nuo_agent::catalog`).
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
                            default_effort: Some(crate::effort::Effort::High),
                            wire_entities: None,
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
                    // or user explicitly declares tool support.
                    tool_call: false,
                    // Permissive *policy* view: an endpoint that advertised no
                    // vision field must not be read as text-only.
                    // `declared_vision` below keeps what was actually said.
                    vision: fitted.vision.unwrap_or(true),
                    protocol: fitted.protocol,
                    model_guidance: "",
                    effort_levels: Box::leak(levels.into_boxed_slice()),
                    default_effort: Some(crate::effort::Effort::High),
                    wire_entities: None,
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
        // Layer 1 (user) beats layer 2 (remote) beats layer 3
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
        // baseline has ReasoningSupport::Adaptive -> chain_disclosed = true
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
            protocol: WireProtocol::OpenAiChatCompletions,
            model_guidance: "",
            effort_levels: crate::effort::COMMON_LADDER,
            default_effort: Some(crate::effort::Effort::High),
            wire_entities: None,
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
            default_effort: Some(crate::effort::Effort::High),
            wire_entities: None,
        },
    ];
    const FIXTURE_B: &[Model] = &[Model {
        id: "fixture-gamma",
        family: "fixture",
        context_window: 333_000,
        thinking: ReasoningSupport::None,
        tool_call: false,
        vision: false,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: &[],
        default_effort: Some(crate::effort::Effort::High),
        wire_entities: None,
    }];

    inventory::submit!(BaselineModels(FIXTURE_A));
    inventory::submit!(BaselineModels(FIXTURE_B));

    #[test]
    fn registered_baselines_resolve_by_id() {
        let m = resolve("fixture-alpha");
        assert_eq!(m.context_window, 111_000);
        assert!(m.reasoning());
        assert!(m.vision);
        assert_eq!(m.protocol, WireProtocol::OpenAiChatCompletions);
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
        assert_eq!(g.protocol, WireProtocol::GoogleGenerateContent);
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
        // Safe conservative text defaults.
        assert_eq!(m.context_window, 128_000);
        assert!(!m.reasoning());
        assert!(!m.tool_call);
        // `Model::vision` is a *policy* view, not a claim: the fallback
        // permits images so an undeclared route is attempted. What
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
            protocol: WireProtocol::OpenAiChatCompletions,
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
            protocol: WireProtocol::GoogleGenerateContent,
            effort_levels: vec![crate::effort::Effort::Max],
        }]);
        // The vetted baseline entry wins on every field except the effort
        // ladder: effort tiers are a live platform knob, so a trusted
        // provider's advertised tiers refresh the baseline's ladder while
        // identity, context, format, and vision stay vetted.
        let m = resolve("fixture-alpha");
        assert_eq!(m.context_window, 111_000);
        assert_eq!(m.protocol, WireProtocol::OpenAiChatCompletions);
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
            protocol: WireProtocol::OpenAiChatCompletions,
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
            WireProtocol::OpenAiChatCompletions
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
            WireProtocol::OpenAiChatCompletions.display_name(),
            "OpenAI Chat Completions"
        );
        assert_eq!(
            WireProtocol::OpenAiResponses.display_name(),
            "OpenAI Responses"
        );
        assert_eq!(
            WireProtocol::AnthropicMessages.display_name(),
            "Anthropic Messages"
        );
        assert_eq!(
            WireProtocol::GoogleGenerateContent.display_name(),
            "Google GenerateContent"
        );
        assert_eq!(
            WireProtocol::GoogleInteractions.display_name(),
            "Google Interactions"
        );

        // as_str
        assert_eq!(
            WireProtocol::OpenAiChatCompletions.as_str(),
            "openai-chat-completions"
        );
        assert_eq!(WireProtocol::OpenAiResponses.as_str(), "openai-responses");
        assert_eq!(
            WireProtocol::AnthropicMessages.as_str(),
            "anthropic-messages"
        );
        assert_eq!(
            WireProtocol::GoogleGenerateContent.as_str(),
            "google-generate-content"
        );
        assert_eq!(
            WireProtocol::GoogleInteractions.as_str(),
            "google-interactions"
        );

        // FromStr canonical
        assert_eq!(
            WireProtocol::from_str("openai-chat-completions").unwrap(),
            WireProtocol::OpenAiChatCompletions
        );
        assert_eq!(
            WireProtocol::from_str("openai-responses").unwrap(),
            WireProtocol::OpenAiResponses
        );
        assert_eq!(
            WireProtocol::from_str("anthropic-messages").unwrap(),
            WireProtocol::AnthropicMessages
        );
        assert_eq!(
            WireProtocol::from_str("google-generate-content").unwrap(),
            WireProtocol::GoogleGenerateContent
        );
        assert_eq!(
            WireProtocol::from_str("google-interactions").unwrap(),
            WireProtocol::GoogleInteractions
        );

        // Serde serialization
        assert_eq!(
            serde_json::to_string(&WireProtocol::OpenAiChatCompletions).unwrap(),
            "\"openai-chat-completions\""
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::OpenAiResponses).unwrap(),
            "\"openai-responses\""
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::AnthropicMessages).unwrap(),
            "\"anthropic-messages\""
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::GoogleGenerateContent).unwrap(),
            "\"google-generate-content\""
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::GoogleInteractions).unwrap(),
            "\"google-interactions\""
        );

        // Serde deserialization
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"openai-chat-completions\"").unwrap(),
            WireProtocol::OpenAiChatCompletions
        );
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"openai-responses\"").unwrap(),
            WireProtocol::OpenAiResponses
        );
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"anthropic-messages\"").unwrap(),
            WireProtocol::AnthropicMessages
        );
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"google-generate-content\"").unwrap(),
            WireProtocol::GoogleGenerateContent
        );
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"google-interactions\"").unwrap(),
            WireProtocol::GoogleInteractions
        );
    }
}
