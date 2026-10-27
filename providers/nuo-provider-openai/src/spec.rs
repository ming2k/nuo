//! The built-in `openai` provider preset: OpenAI's chat-completions API,
//! one key (`OPENAI_API_KEY`).

use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::*;

/// The model ids the built-in `openai` provider serves over the OpenAI
/// chat-completions API, one key (`OPENAI_API_KEY`). Mirrors OpenAI's current
/// frontier chat lineup — the GPT-5.6 tier-named family (`gpt-5.6-sol`, the
/// flagship, leads) plus the GPT-5.x family; `gpt-5.6-sol` is the default.
/// The legacy `gpt-4o`/`gpt-4o-mini` ids stay registered for existing
/// configs but are no longer seeded for the official provider. Each id exists
/// in the model registry.
pub use nuo_provider::model_providers::OPENAI_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_inference_wire`'s registry at link time (see
/// [`nuo_provider::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    Model {
        id: "gpt-6-astra",
        family: "gpt",
        context_window: 872_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_6,
        wire_entities: None,
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
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
        wire_entities: None,
    },
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
        wire_entities: None,
    },
    Model {
        id: "gpt-5.6-terra",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
        wire_entities: None,
    },
    Model {
        id: "gpt-5.6-luna",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
        wire_entities: None,
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
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
        wire_entities: None,
    },
    Model {
        id: "gpt-5.4",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
        wire_entities: None,
    },
    Model {
        id: "gpt-5.4-mini",
        family: "gpt",
        context_window: 400_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
        wire_entities: None,
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
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "gpt-4o-mini",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "gpt-5.3-codex-spark",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
        wire_entities: None,
    },
    Model {
        id: "gpt-5.2",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
        wire_entities: None,
    },
    Model {
        id: "gpt-5.2-chat-latest",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
        wire_entities: None,
    },
    Model {
        id: "gpt-5.2-pro",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

const OPENAI_GPT_56_CACHE: nuo_provider::PromptCacheSpec = nuo_provider::PromptCacheSpec {
    modes: &[
        nuo_provider::PromptCacheMode::Implicit,
        nuo_provider::PromptCacheMode::Explicit,
    ],
    default_mode: Some(nuo_provider::PromptCacheMode::Implicit),
    supported_retentions: &[nuo_provider::CacheRetention::ThirtyMinutes],
    default_retention: Some(nuo_provider::CacheRetention::ThirtyMinutes),
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: Some(4),
    min_cacheable_tokens: Some(1024),
    reports_reads: true,
    reports_writes: true,
    reports_misses: false,
};

const OPENAI_24H_CACHE: nuo_provider::PromptCacheSpec = nuo_provider::PromptCacheSpec {
    modes: &[nuo_provider::PromptCacheMode::Implicit],
    default_mode: Some(nuo_provider::PromptCacheMode::Implicit),
    supported_retentions: &[nuo_provider::CacheRetention::TwentyFourHours],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: Some(2048),
    reports_reads: true,
    reports_writes: false,
    reports_misses: false,
};

const OPENAI_LEGACY_CACHE: nuo_provider::PromptCacheSpec = nuo_provider::PromptCacheSpec {
    modes: &[nuo_provider::PromptCacheMode::Implicit],
    default_mode: Some(nuo_provider::PromptCacheMode::Implicit),
    supported_retentions: &[
        nuo_provider::CacheRetention::InMemory,
        nuo_provider::CacheRetention::TwentyFourHours,
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

const OPENAI_IN_MEMORY_CACHE: nuo_provider::PromptCacheSpec = nuo_provider::PromptCacheSpec {
    modes: &[nuo_provider::PromptCacheMode::Implicit],
    default_mode: Some(nuo_provider::PromptCacheMode::Implicit),
    supported_retentions: &[nuo_provider::CacheRetention::InMemory],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: Some(2048),
    reports_reads: true,
    reports_writes: false,
    reports_misses: false,
};

pub(super) fn prompt_cache_for_model(model: &str) -> nuo_provider::PromptCacheSpec {
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
        nuo_provider::PromptCacheSpec::UNSUPPORTED
    }
}

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("openai"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.openai.com/v1"),
    user_agent: None,
    protocol: WireProtocol::OpenAiChatCompletions,
    models: OPENAI_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_provider::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Native,
    responses_state: nuo_provider::ResponsesState::Stored,
};
