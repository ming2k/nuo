//! The built-in `openrouter` provider: OpenRouter's normalized multi-model
//! gateway over Chat Completions, authenticated with one API key.

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Effort, Model, WireProtocol};

use super::{CatalogShape, ModelProviderSpec, RemoteCatalogSource};

pub use nuo_model_codec::model_providers::OPENROUTER_BUILTIN_MODELS;

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
    protocol: WireProtocol::ChatCompletions,
    model_guidance: "",
    effort_levels: NEX_N25_EFFORTS,
}];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::OpenRouter,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(super::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("openrouter"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://openrouter.ai/api/v1"),
    user_agent: None,
    protocol: WireProtocol::ChatCompletions,
    models: OPENROUTER_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::Native,
    client_profile_sensitive: false,
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
