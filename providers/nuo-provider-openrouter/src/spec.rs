//! The built-in `openrouter` provider: OpenRouter's normalized multi-model
//! gateway over Chat Completions, authenticated with one API key.

use nuo_inference_wire::WireProtocol;
use nuo_provider::reasoning::ReasoningSupport;
use nuo_provider::{Effort, Model};

use nuo_provider::*;

pub use nuo_provider::model_providers::OPENROUTER_BUILTIN_MODELS;

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
    protocol: WireProtocol::OpenAiChatCompletions,
    model_guidance: "",
    effort_levels: NEX_N25_EFFORTS,
    default_effort: Some(nuo_provider::Effort::High),
    wire_entities: None,
}];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::OpenRouter,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("openrouter"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://openrouter.ai/api/v1"),
    user_agent: None,
    protocol: WireProtocol::OpenAiChatCompletions,
    models: OPENROUTER_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_provider::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: Some(nuo_provider::QuotaPort::OpenRouterKey),
    display_policy: nuo_provider::DisplayPolicy::Structural,
    responses_state: nuo_provider::ResponsesState::Stateless,
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
