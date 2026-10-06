//! The `opencode-zen` provider preset: the OpenCode Zen relay surface
//! (`opencode.ai/zen/v1`), authenticated via an OpenCode API key
//! (`OPENCODE_API_KEY`).
//!
//! This is a distinct service surface from the account-scoped Console provider
//! (`opencode`, ADR-0269): the Zen relay authenticates with a plain key and
//! serves a public `/zen/v1/models` catalog, whereas the Console surface is
//! account-scoped and requires an OAuth credential plus a workspace header. The
//! endpoint family and model universe differ, so they are separate model
//! providers (ADR-0201 INV-1); authentication mode never selects the route.

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use nuo_provider::*;

/// Curated seed models offered by the OpenCode Zen preset.
pub use nuo_model_codec::model_providers::OPENCODE_ZEN_MODELS;

/// Baseline capability metadata for the models this provider serves.
pub const MODELS: &[Model] = &[
    Model {
        id: "claude-sonnet-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
    },
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    Model {
        id: "glm-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::GLM_5,
    },
];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: Some(std::borrow::Cow::Borrowed("https://opencode.ai/zen/v1")),
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("opencode-zen"),
    baselines: MODELS,
    // Endpoints are per-model by wire format (see `route_for_model`); the
    // instance-level default is the OpenAI chat-completions surface.
    root_url: std::borrow::Cow::Borrowed("https://opencode.ai/zen/v1"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_model_codec::client_identity::OPENCODE_USER_AGENT,
    )),
    protocol: WireProtocol::ChatCompletions,
    // Served models come from the live /zen/v1/models endpoint.
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: None,
    models: OPENCODE_ZEN_MODELS,
};
