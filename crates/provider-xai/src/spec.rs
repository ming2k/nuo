//! The `xai-oauth` provider preset: xAI Grok over OpenAI-compatible chat
//! completions (SuperGrok OAuth or `XAI_API_KEY`).

use protocol::Model;
use protocol::reasoning::ReasoningSupport;

use ::provider::*;
use protocol::surface::{CatalogShape, RemoteCatalogSource};

/// xAI Grok models over OpenAI-compatible chat completions (SuperGrok OAuth or
/// `XAI_API_KEY`).
pub use protocol::model_providers::XAI_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `inference_wire`'s registry at link time (see
/// [`protocol::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // xAI Grok (OpenAI-compatible; SuperGrok OAuth or XAI_API_KEY)
    Model {
        id: "grok-4.5",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::XAI_GROK,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "grok-4.20",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::XAI_GROK,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "grok-4.3",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::XAI_GROK,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "grok-build-0.1",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::XAI_GROK,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
];

inventory::submit!(protocol::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: protocol::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: ::provider::PromptCachePolicy::Compiled(::provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed(protocol::model_providers::PROVIDER_XAI.id),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed(protocol::model_providers::PROVIDER_XAI.root_url),
    user_agent: protocol::model_providers::PROVIDER_XAI.user_agent_cow(),
    default_headers: std::borrow::Cow::Borrowed(&[]),
    protocol: protocol::model_providers::PROVIDER_XAI.protocol,
    model_protocols: &[],
    models: protocol::model_providers::PROVIDER_XAI.models,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    quota: None,
    display_policy: ::provider::DisplayPolicy::Native,
    responses_state: protocol::ResponsesState::Stateless,
};
