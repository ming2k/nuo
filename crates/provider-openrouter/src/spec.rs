//! The built-in `openrouter` provider: OpenRouter's normalized multi-model
//! gateway over Chat Completions, authenticated with one API key.

use protocol::reasoning::ReasoningSupport;
use protocol::{Effort, Model};

use ::provider::*;
use protocol::surface::{CatalogShape, RemoteCatalogSource};

pub use protocol::model_providers::OPENROUTER_BUILTIN_MODELS;

const NEX_N25_EFFORTS: &[Effort] = &[Effort::None, Effort::Medium, Effort::High];

/// The live OpenRouter catalog replaces this seed after the first sync. Keep a
/// complete baseline for the requested daily-driver model so it is fully
/// capable even while offline or before a key has been entered.
pub const MODELS: &[Model] = &[Model {
    id: "nex-agi/nex-n2.5-pro:free",
    family: "nex",
    context_window: 262_144,
    thinking: ReasoningSupport::ReasoningContent,
    tool_call: true,
    vision: true,
    effort_levels: NEX_N25_EFFORTS,
    default_effort: Some(protocol::Effort::High),
    wire_entities: None,
}];

inventory::submit!(protocol::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: protocol::ProviderDialect::OpenRouter,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: ::provider::PromptCachePolicy::Compiled(::provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed(protocol::model_providers::PROVIDER_OPENROUTER.id),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed(protocol::model_providers::PROVIDER_OPENROUTER.root_url),
    user_agent: protocol::model_providers::PROVIDER_OPENROUTER.user_agent_cow(),
    default_headers: std::borrow::Cow::Borrowed(&[]),
    protocol: protocol::model_providers::PROVIDER_OPENROUTER.protocol,
    model_protocols: &[],
    models: protocol::model_providers::PROVIDER_OPENROUTER.models,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    quota: Some(::provider::QuotaPort::OpenRouterKey),
    display_policy: ::provider::DisplayPolicy::Structural,
    responses_state: protocol::ResponsesState::Stateless,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nex_pro_seed_matches_openrouter_catalog_contract() {
        let model = &MODELS[0];
        assert_eq!(OPENROUTER_BUILTIN_MODELS, &[model.id]);
        assert_eq!(model.context_window, 262_144);
        assert!(model.tool_call);
        assert!(model.vision);
        assert_eq!(model.effort_levels, NEX_N25_EFFORTS);
    }
}
