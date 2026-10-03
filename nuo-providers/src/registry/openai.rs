//! The built-in `openai` provider preset: OpenAI's chat-completions API,
//! one key (`OPENAI_API_KEY`).

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use super::{effort_ladders, CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// The model ids the built-in `openai` provider serves over the OpenAI
/// chat-completions API, one key (`OPENAI_API_KEY`). Mirrors OpenAI's current
/// frontier chat lineup — the GPT-5.6 tier-named family (`gpt-5.6-sol`, the
/// flagship, leads) plus the GPT-5.x family; `gpt-5.6-sol` is the default.
/// The legacy `gpt-4o`/`gpt-4o-mini` ids stay registered for existing
/// configs but are no longer seeded for the official provider. Each id exists
/// in the model registry.
pub use nuo_model_codec::model_providers::OPENAI_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_model_codec`'s registry at link time (see
/// [`nuo_model_codec::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    Model {
        id: "gpt-6-astra",
        family: "gpt",
        context_window: 872_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_6,
    },
    // GPT-5.6 (OpenAI)
    // The 2026-06-26 flagship family with OpenAI's tier naming scheme:
    // Sol (flagship) / Terra (balanced) / Luna (efficient, high-volume).
    // `gpt-5.6` is an alias that routes to `gpt-5.6-sol`. All speak the
    // standard OpenAI chat-completions API and reason via `reasoning_content`.
    // GPT-5.6 honors the `max` effort level, so these carry the 5.6-specific
    // effort set rather than the xhigh-capped `OPENAI_GPT`.
    // OpenAI has not published the context window; use the GPT-5.5-class 1M
    // window conservatively for all three tiers and the alias.
    Model {
        id: "gpt-5.6",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-terra",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-luna",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    // GPT (OpenAI)
    // The current frontier chat family served over the OpenAI chat-completions
    // API. All reason (surfaced via the `reasoning_content` stream) and take
    // text+image input. Context windows and pricing per OpenAI's model docs;
    // `gpt-5.5`/`gpt-5.4` share a 1M window, `gpt-5.4-mini` a 400K window.
    Model {
        id: "gpt-5.5",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.4",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.4-mini",
        family: "gpt",
        context_window: 400_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    // Legacy GPT-4o family — no longer in OpenAI's frontier chat lineup (it
    // remains only behind the TTS/transcribe specialized models) but kept
    // registered so existing configs and older sessions still resolve metadata.
    Model {
        id: "gpt-4o",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gpt-4o-mini",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gpt-5.3-codex-spark",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2-chat-latest",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2-pro",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

const OPENAI_GPT_56_CACHE: nuo_model_codec::PromptCacheSpec = nuo_model_codec::PromptCacheSpec {
    modes: &[
        nuo_model_codec::PromptCacheMode::Implicit,
        nuo_model_codec::PromptCacheMode::Explicit,
    ],
    default_mode: Some(nuo_model_codec::PromptCacheMode::Implicit),
    supported_retentions: &[nuo_model_codec::CacheRetention::ThirtyMinutes],
    default_retention: Some(nuo_model_codec::CacheRetention::ThirtyMinutes),
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: Some(4),
    min_cacheable_tokens: Some(1024),
    reports_reads: true,
    reports_writes: true,
    reports_misses: false,
};

const OPENAI_24H_CACHE: nuo_model_codec::PromptCacheSpec = nuo_model_codec::PromptCacheSpec {
    modes: &[nuo_model_codec::PromptCacheMode::Implicit],
    default_mode: Some(nuo_model_codec::PromptCacheMode::Implicit),
    supported_retentions: &[nuo_model_codec::CacheRetention::TwentyFourHours],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: Some(2048),
    reports_reads: true,
    reports_writes: false,
    reports_misses: false,
};

const OPENAI_LEGACY_CACHE: nuo_model_codec::PromptCacheSpec = nuo_model_codec::PromptCacheSpec {
    modes: &[nuo_model_codec::PromptCacheMode::Implicit],
    default_mode: Some(nuo_model_codec::PromptCacheMode::Implicit),
    supported_retentions: &[
        nuo_model_codec::CacheRetention::InMemory,
        nuo_model_codec::CacheRetention::TwentyFourHours,
    ],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: Some(2048),
    reports_reads: true,
    reports_writes: false,
    reports_misses: false,
};

const OPENAI_IN_MEMORY_CACHE: nuo_model_codec::PromptCacheSpec = nuo_model_codec::PromptCacheSpec {
    modes: &[nuo_model_codec::PromptCacheMode::Implicit],
    default_mode: Some(nuo_model_codec::PromptCacheMode::Implicit),
    supported_retentions: &[nuo_model_codec::CacheRetention::InMemory],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: Some(2048),
    reports_reads: true,
    reports_writes: false,
    reports_misses: false,
};

pub(super) fn prompt_cache_for_model(model: &str) -> nuo_model_codec::PromptCacheSpec {
    if model == "gpt-5.6" || model.starts_with("gpt-5.6-") {
        OPENAI_GPT_56_CACHE
    } else if model == "gpt-5.5" || model.starts_with("gpt-5.5-") {
        OPENAI_24H_CACHE
    } else if matches!(
        model,
        "gpt-5.4" | "gpt-5.4-mini" | "gpt-5.2" | "gpt-5.2-chat-latest" | "gpt-5.2-pro"
    ) {
        OPENAI_LEGACY_CACHE
    } else if matches!(model, "gpt-4o" | "gpt-4o-mini" | "gpt-5.3-codex-spark") {
        OPENAI_IN_MEMORY_CACHE
    } else {
        nuo_model_codec::PromptCacheSpec::UNSUPPORTED
    }
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("openai"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.openai.com/v1"),
    user_agent: None,
    protocol: WireProtocol::ChatCompletions,
    models: OPENAI_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::Native,
    client_profile_sensitive: false,
};
