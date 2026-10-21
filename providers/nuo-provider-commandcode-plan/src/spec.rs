//! The built-in `commandcode` provider preset: Command Code Provider API
//! (`api.commandcode.ai/provider/v1`), authenticated via a Command Code API key.

use nuo_inference_wire::reasoning::ReasoningSupport;
use nuo_inference_wire::{Model, WireProtocol};

use nuo_provider::*;

pub use nuo_inference_wire::model_providers::COMMANDCODE_BUILTIN_MODELS;

/// Seed models offered by the Command Code Provider preset before the first sync.
pub const MODELS: &[Model] = &[
    Model {
        id: "claude-sonnet-5-5",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
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
        id: "deepseek/deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        wire_entities: None,
    },
    // CommandCode names its models `vendor/model`, and the server resolves the
    // upstream family per turn (ADR-0014): a declared `deepseek/deepseek-v4-flash`
    // is served as `deepseek-v4.1-flash` (`x-cli-constants.featureLaneModels`
    // lists both `deepseek-v4.1-flash` and `deepseek-v4-pro` as real ids). These
    // two siblings therefore need their own baseline entries — without one, an id
    // nothing else registers falls through to `fallback_model`, whose empty
    // `effort_levels` collapses the picker's effort slider (the id is
    // selectable but its depth is not adjustable).
    Model {
        id: "deepseek/deepseek-v4.1-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        wire_entities: None,
    },
    Model {
        id: "deepseek/deepseek-v4-pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        wire_entities: None,
    },
];

inventory::submit!(nuo_inference_wire::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_inference_wire::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("commandcode-plan"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.commandcode.ai/provider/v1"),
    user_agent: None,
    protocol: WireProtocol::OpenAiChatCompletions,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_inference_wire::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: Some(nuo_provider::QuotaPort::CommandCodeCredits),
    display_policy: nuo_provider::DisplayPolicy::Native,
    models: COMMANDCODE_BUILTIN_MODELS,
};
