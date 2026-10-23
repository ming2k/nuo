//! The built-in `commandcode` provider preset: Command Code Provider API
//! (`api.commandcode.ai/provider/v1`), authenticated via a Command Code API key.

use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::*;

pub use nuo_provider::model_providers::COMMANDCODE_BUILTIN_MODELS;

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
        protocol: WireProtocol::OpenAiResponses,
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
        vision: true,
        protocol: WireProtocol::OpenAiResponses,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        wire_entities: None,
    },
    // CommandCode names its models `vendor/model`, and the server resolves the
    // upstream family per turn (ADR-0014): a declared `deepseek/deepseek-v4-flash`
    // is served as `deepseek-v4.1-flash` (`x-cli-constants.featureLaneModels`
    // lists both `deepseek-v4.1-flash` and `deepseek-v4-pro` as real ids).
    // CommandCode's live /provider/v1/models endpoint advertises
    // `supported_endpoints: ["/chat/completions", "/responses"]` for all
    // DeepSeek and modern GPT models, and mounts /responses natively.
    Model {
        id: "deepseek/deepseek-v4.1-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiResponses,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        wire_entities: None,
    },
    Model {
        id: "deepseek/deepseek-v4.1-flash-fast",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiResponses,
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
        protocol: WireProtocol::OpenAiResponses,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("commandcode-plan"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.commandcode.ai/provider/v1"),
    user_agent: None,
    protocol: WireProtocol::OpenAiResponses,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_provider::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: Some(nuo_provider::QuotaPort::CommandCodeCredits),
    display_policy: nuo_provider::DisplayPolicy::Structural,
    models: COMMANDCODE_BUILTIN_MODELS,
};

#[cfg(test)]
mod tests {
    use super::{COMMANDCODE_BUILTIN_MODELS, MODEL_PROVIDER_SPEC, MODELS};
    use nuo_inference_wire::WireProtocol;
    use nuo_provider::DisplayPolicy;

    #[test]
    fn labels_are_lifted_structurally_not_echoed_from_the_wire_id() {
        assert_eq!(
            MODEL_PROVIDER_SPEC.display_policy,
            DisplayPolicy::Structural
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.resolve_display_label(None, "deepseek/deepseek-v4.1-flash"),
            Some("DeepSeek V4.1 Flash".to_string())
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.resolve_display_label(None, "deepseek/deepseek-v4.1-flash-fast"),
            Some("DeepSeek V4.1 Flash Fast".to_string())
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.resolve_display_label(None, "deepseek/deepseek-v4-pro"),
            Some("DeepSeek V4 Pro".to_string())
        );
    }

    #[test]
    fn deepseek_and_gpt_models_route_via_responses_api() {
        for id in [
            "deepseek/deepseek-v4.1-flash",
            "deepseek/deepseek-v4.1-flash-fast",
            "deepseek/deepseek-v4-pro",
            "gpt-5.6-sol",
        ] {
            assert_eq!(
                MODEL_PROVIDER_SPEC.model_protocol(id),
                WireProtocol::OpenAiResponses,
                "{id} must route through the Responses API"
            );
        }
        assert_eq!(
            MODEL_PROVIDER_SPEC.model_protocol("claude-sonnet-5-5"),
            WireProtocol::AnthropicMessages,
            "Claude must route through Messages API"
        );
    }

    #[test]
    fn every_seeded_namespaced_id_gets_a_label() {
        for id in COMMANDCODE_BUILTIN_MODELS {
            let label = MODEL_PROVIDER_SPEC.resolve_display_label(None, id);
            assert!(label.is_some(), "{id} must resolve a display label");
            let label = label.unwrap_or_default();
            assert!(
                !label.contains('/'),
                "{id} resolved to {label:?}: a namespace separator is not a name"
            );
        }
    }

    #[test]
    fn deepseek_flash_models_support_vision() {
        for id in [
            "deepseek/deepseek-v4-flash",
            "deepseek/deepseek-v4.1-flash",
            "deepseek/deepseek-v4.1-flash-fast",
        ] {
            let model = MODELS.iter().find(|m| m.id == id).expect("seeded model");
            assert!(model.vision, "{id} must support vision");
        }
        let pro = MODELS
            .iter()
            .find(|m| m.id == "deepseek/deepseek-v4-pro")
            .expect("seeded model");
        assert!(!pro.vision, "deepseek-v4-pro must remain text-only");
    }
}
