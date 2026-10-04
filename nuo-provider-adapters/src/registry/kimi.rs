//! The `kimi-code` provider template and its legacy registry preset:
//! Moonshot AI's Kimi Code coding platform (`api.kimi.com/coding/v1`).

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use super::{effort_ladders, CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// Models served by Moonshot's Kimi Code endpoint, in display/activation
/// order — the first entry is the initial active channel. `k3` is the
/// platform's current flagship; `kimi-k2.7-code` remains as the previous
/// pinned alias.
pub use nuo_model_codec::model_providers::KIMI_CODE_MODELS;

// Kimi Code — Moonshot AI's coding platform (api.kimi.com/coding/v1).
// The platform pins the model id to the fixed `k3` alias (Kimi K3, 1M
// context, always-on thinking); its live `GET /models` also lists the
// legacy `kimi-for-coding` (K2.7) ids, kept selectable via
// [`KIMI_CODE_MODELS`]. API key env still uses the MOONSHOT_API_KEY
// legacy name for config compatibility. The [`OPENCODE_USER_AGENT`]
// is borrowed on purpose: the endpoint was live-tested (2026-07) to
// accept any UA — including none — under OAuth auth, but whether the
// API-key path gates on a recognized coding-agent UA is unknown, so the
// recognized default stays as the zero-risk choice.

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_model_codec`'s registry at link time (see
/// [`nuo_model_codec::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // Kimi (Moonshot / opencode-go)
    Model {
        // The Kimi Code platform's current flagship. The platform's live
        // `GET /models` advertises `k3` with a 1M context window, image/video
        // inputs, and always-on thinking (`supports_thinking_type: "only"`) —
        // over the OpenAI-compatible wire the always-on reasoning simply streams
        // back as `reasoning_content`, so there is no thinking switch to model.
        // The effort ladder is tunable: `reasoning_effort` accepts
        // `low`/`high`/`max` (platform default `high`), advertised so the
        // pickers/hint bar can show the effective level and the editor can cycle
        // it; the fitted overlay refreshes it from the live `/models` list.
        id: "k3",
        family: "kimi",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    Model {
        id: "kimi-k2.7-code",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.6",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.5",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

fn prompt_cache_for_model(_: &str) -> nuo_model_codec::PromptCacheSpec {
    nuo_model_codec::PromptCacheSpec {
        modes: &[nuo_model_codec::PromptCacheMode::Implicit],
        default_mode: Some(nuo_model_codec::PromptCacheMode::Implicit),
        supported_retentions: &[],
        default_retention: None,
        disable_supported: false,
        routing_key_supported: false,
        max_breakpoints: None,
        min_cacheable_tokens: None,
        reports_reads: true,
        reports_writes: false,
        reports_misses: false,
    }
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("kimi-code"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.kimi.com/coding/v1"),
    user_agent: Some(std::borrow::Cow::Borrowed(crate::OPENCODE_USER_AGENT)),
    protocol: WireProtocol::ChatCompletions,
    // The Kimi Code platform exposes a live /models endpoint, so instances
    // created from this preset track the platform's actual model list.
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::Native,
    client_profile_sensitive: false,
    models: KIMI_CODE_MODELS,
};
