//! The built-in `deepseek` provider preset: DeepSeek V4 Flash, Pro, and the
//! experimental Flash Vision model over
//! the official Responses API, one key (`DEEPSEEK_API_KEY`).
//!
//! DeepSeek V4 (Flash + Pro) is served as one multi-model `deepseek` provider
//! built in the catalog layer (both models share one `DEEPSEEK_API_KEY`), not
//! as two single-model registry presets — so it has no entry in
//! the provider-scoped catalog.
//!
//! Both V4 models natively speak the OpenAI **Responses API**
//! (`https://api.deepseek.com/v1/responses`), so this preset's channels use
//! the Responses transport. Chat-completions remains available upstream, but
//! is no longer what the preset seeds.

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use super::{effort_ladders, CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// The model ids the built-in `deepseek` provider serves (V4 Flash, Pro, and
/// Flash Vision over the Responses API, one key). Each id exists in the model
/// registry and floats with the upstream latest.
pub use nuo_model_codec::model_providers::DEEPSEEK_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_model_codec`'s registry at link time (see
/// [`nuo_model_codec::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // DeepSeek (opencode-go / direct)
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-flash-vision-exp",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
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
        reports_misses: true,
    }
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::DeepSeek,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("deepseek"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.deepseek.com/v1"),
    user_agent: None,
    protocol: WireProtocol::Responses,
    models: DEEPSEEK_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::Native,
    client_profile_sensitive: false,
};

#[cfg(test)]
mod tests {
    use super::{DEEPSEEK_BUILTIN_MODELS, MODELS};

    #[test]
    fn vision_model_is_seeded_with_image_input_support() {
        let id = "deepseek-v4-flash-vision-exp";
        assert!(DEEPSEEK_BUILTIN_MODELS.contains(&id));
        let model = MODELS.iter().find(|model| model.id == id).unwrap();
        assert!(model.vision);
        assert!(model.tool_call);
        assert_eq!(model.context_window, 1_000_000);
    }
}
