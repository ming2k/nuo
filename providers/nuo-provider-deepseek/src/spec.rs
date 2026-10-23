//! The built-in `deepseek` provider preset: DeepSeek V4.1 Flash and V4 Pro over
//! the official Responses API, one key (`DEEPSEEK_API_KEY`).
//!
//! # Model ids
//!
//! The official platform serves exactly two wire ids (ADR-0083):
//!
//! - `deepseek-flash` — DeepSeek-V4.1-Flash-1017, 1M context, native multimodal
//!   vision. This is the id the vendor tells every caller to use.
//! - `deepseek-v4-pro` — DeepSeek-V4-Pro-0813, 1M context, text only.
//!
//! The V4 Flash generation is retired: `deepseek-v4-flash` and
//! `deepseek-v4-flash-vision-exp` are still *accepted* as legacy aliases, but
//! their requests are served by DeepSeek-V4.1-Flash-1017 at the Flash price.
//! They stay registered as baselines (never seeded) so a config written by an
//! older Nuo keeps resolving its capabilities instead of falling back to the
//! conservative default.
//!
//! `deepseek-v4.1-flash` is **not** a model id on this platform — it is a
//! Novita/OpenRouter *relay* id (ADR-0014) that Nuo previously mistook for the
//! official one; the provider stopped registering it in ADR-0083.
//!
//! # Wire
//!
//! All models natively speak the OpenAI **Responses API**
//! (`https://api.deepseek.com/v1/responses`), so this preset's channels use the
//! Responses transport. The surface is stateless — responses are not stored and
//! `previous_response_id` does not exist — so continuation rides Nuo's own
//! opaque output-artifact replay. Chat Completions remains available upstream
//! but is no longer what the preset seeds.
//!
//! # Reasoning depth
//!
//! Thinking is **on by default** at effort `high`. Depth is driven on the wire
//! by `reasoning.effort` (Responses) / `reasoning_effort` (Chat Completions),
//! and `none` is the off switch. The official backend accepts the entire closed
//! [`nuo_provider::effort_ladders::DEEPSEEK_NATIVE`] vocabulary and maps the
//! tiers it does not distinguish itself, so the ladder is declared in full.
//!
//! See `docs/reference/openapi/deepseek.yml` for the full OpenAPI 3.1
//! specification.

use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::*;

/// The model ids the built-in `deepseek` provider seeds. Each id exists in the
/// model registry and floats with the upstream latest.
pub use nuo_provider::model_providers::DEEPSEEK_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_inference_wire`'s registry at link time (see
/// [`nuo_provider::model::BaselineModels`]).
///
/// The seeded ids come first; the retired V4 Flash aliases follow so an older
/// config keeps its capabilities without being offered to new users.
pub const MODELS: &[Model] = &[
    // DeepSeek V4.1 Flash — DeepSeek-V4.1-Flash-1017, the current Flash line
    // with multimodal vision integrated (ADR-0080).
    Model {
        id: "deepseek-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiResponses,
        model_guidance: "",
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        wire_entities: None,
    },
    // DeepSeek V4 Pro — DeepSeek-V4-Pro-0813, the pure-text reasoning
    // flagship. Vision is not supported on this model.
    Model {
        id: "deepseek-v4-pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiResponses,
        model_guidance: "",
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        wire_entities: None,
    },
    // Retired V4 Flash generation. The name is still accepted upstream and is
    // served by DeepSeek-V4.1-Flash-1017, so the capability row is V4.1
    // Flash's (1M, vision); registered for existing configs only.
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiResponses,
        model_guidance: "",
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        wire_entities: None,
    },
    // The retired experimental vision name, likewise served by V4.1 Flash.
    Model {
        id: "deepseek-v4-flash-vision-exp",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiResponses,
        model_guidance: "",
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

fn prompt_cache_for_model(_: &str) -> nuo_provider::PromptCacheSpec {
    nuo_provider::PromptCacheSpec {
        modes: &[nuo_provider::PromptCacheMode::Implicit],
        default_mode: Some(nuo_provider::PromptCacheMode::Implicit),
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

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::DeepSeek,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("deepseek"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.deepseek.com/v1"),
    user_agent: None,
    protocol: WireProtocol::OpenAiResponses,
    models: DEEPSEEK_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_provider::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: Some(nuo_provider::QuotaPort::DeepSeekBalance),
    display_policy: nuo_provider::DisplayPolicy::Native,
};

#[cfg(test)]
mod tests {
    use super::{DEEPSEEK_BUILTIN_MODELS, MODELS};
    use nuo_provider::effort_ladders;

    fn model(id: &str) -> &'static nuo_provider::Model {
        MODELS
            .iter()
            .find(|model| model.id == id)
            .unwrap_or_else(|| panic!("{id} must be a registered baseline"))
    }

    #[test]
    fn seeds_the_two_official_ids_only() {
        assert_eq!(
            DEEPSEEK_BUILTIN_MODELS,
            &["deepseek-flash", "deepseek-v4-pro"],
            "the platform serves exactly these two wire ids (ADR-0083)"
        );
        assert!(MODELS.iter().any(|model| model.id == "deepseek-flash"));
        assert!(MODELS.iter().any(|model| model.id == "deepseek-v4-pro"));
    }

    #[test]
    fn relay_id_is_not_registered_as_an_official_model() {
        // `deepseek-v4.1-flash` is a Novita/OpenRouter relay id, never an
        // `api.deepseek.com` model id (ADR-0083).
        assert!(!MODELS.iter().any(|model| model.id == "deepseek-v4.1-flash"));
        assert!(!DEEPSEEK_BUILTIN_MODELS.contains(&"deepseek-v4.1-flash"));
    }

    #[test]
    fn canonical_flash_is_seeded_with_multimodal_vision() {
        let model = model("deepseek-flash");
        assert!(model.vision, "V4.1 Flash accepts image input");
        assert!(model.tool_call);
        assert_eq!(model.context_window, 1_000_000);
        assert_eq!(
            model.protocol,
            nuo_inference_wire::WireProtocol::OpenAiResponses
        );
    }

    #[test]
    fn retired_aliases_keep_the_capabilities_of_the_model_that_serves_them() {
        // Both retired names are served by DeepSeek-V4.1-Flash-1017, so they
        // inherit its 1M window and vision — the earlier text-only metadata
        // predates the retirement.
        for id in ["deepseek-v4-flash", "deepseek-v4-flash-vision-exp"] {
            let model = model(id);
            assert!(model.vision, "{id} is served by the multimodal Flash model");
            assert_eq!(model.context_window, 1_000_000);
            assert!(!DEEPSEEK_BUILTIN_MODELS.contains(&id), "{id} is not seeded");
        }
    }

    #[test]
    fn pro_model_has_no_vision() {
        assert!(!model("deepseek-v4-pro").vision);
    }

    #[test]
    fn reasoning_depth_declares_the_whole_upstream_vocabulary() {
        for model in MODELS {
            assert_eq!(
                model.effort_levels,
                effort_ladders::DEEPSEEK_NATIVE,
                "{} must declare every rung the platform accepts, including `none`",
                model.id
            );
        }
        assert_eq!(
            effort_ladders::DEEPSEEK_NATIVE.first(),
            Some(&nuo_provider::Effort::None),
            "`none` is the thinking off switch and must survive the clamp"
        );
    }
}
