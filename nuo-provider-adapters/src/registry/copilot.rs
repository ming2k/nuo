//! The `copilot-oauth` provider preset: GitHub Copilot subscription models
//! over OpenAI-compatible chat completions against `api.githubcopilot.com`.

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use super::{CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// The minimal model seed for a fresh GitHub Copilot instance, before its
/// first live catalog sync completes. A Copilot instance uses a remote-catalog
/// source pointing at `api.githubcopilot.com/models` (see [`COPILOT`](crate::oauth::COPILOT)
/// / the `copilot-oauth` preset), so its real channel set is populated from
/// that endpoint at runtime — this seed only needs one universally available
/// id so a brand-new instance activates without a 400. `gpt-4o-mini` is
/// unlocked on every Copilot plan (incl. Free/Student).
pub use nuo_model_codec::model_providers::COPILOT_SEED_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_model_codec`'s registry at link time (see
/// [`nuo_model_codec::model::BaselineModels`]).
pub const MODELS: &[Model] = &[Model {
    id: "gpt-4o-mini",
    family: "gpt",
    context_window: 128_000,
    thinking: ReasoningSupport::None,
    tool_call: true,
    vision: true,
    protocol: WireProtocol::ChatCompletions,
    model_guidance: "",
    effort_levels: &[],
}];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::Copilot,
    protocol_roots: std::borrow::Cow::Borrowed(&[(
        WireProtocol::AnthropicMessages,
        std::borrow::Cow::Borrowed("https://api.githubcopilot.com/v1"),
    )]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(super::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("github-copilot"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.githubcopilot.com"),
    user_agent: None,
    // Copilot speaks the OpenAI chat-completions wire family against
    // api.githubcopilot.com. The endpoint is the preset's remote-catalog
    // source, so the instance tracks the user's actual plan-unlocked model
    // set (which varies by plan: Free/Student get only the GPT-4o chat
    // family, Pro+ unlocks GPT-5) without a hardcoded model list — every
    // advertised id the client registry does not know is overlaid with its
    // advertised capability metadata, mirroring the kimi-code flow.
    protocol: WireProtocol::ChatCompletions,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::Copilot,
    client_profile_sensitive: true,
    // Minimal seed: the id a fresh Copilot instance activates before the
    // first live catalog sync completes. `gpt-4o-mini` is universally
    // available across every Copilot plan, so the seed never 400s.
    models: COPILOT_SEED_MODELS,
};
