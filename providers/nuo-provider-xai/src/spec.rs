//! The `xai-oauth` provider preset: xAI Grok over OpenAI-compatible chat
//! completions (SuperGrok OAuth or `XAI_API_KEY`).

use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::*;

/// xAI Grok models over OpenAI-compatible chat completions (SuperGrok OAuth or
/// `XAI_API_KEY`).
pub use nuo_provider::model_providers::XAI_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_inference_wire`'s registry at link time (see
/// [`nuo_provider::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // xAI Grok (OpenAI-compatible; SuperGrok OAuth or XAI_API_KEY)
    Model {
        id: "grok-4.5",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::XAI_GROK,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "grok-4.20",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::XAI_GROK,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "grok-4.3",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::XAI_GROK,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "grok-build-0.1",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::XAI_GROK,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("xai"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.x.ai/v1"),
    user_agent: None,
    protocol: WireProtocol::OpenAiChatCompletions,
    models: XAI_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_provider::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Native,
    responses_state: nuo_provider::ResponsesState::Stateless,
};
