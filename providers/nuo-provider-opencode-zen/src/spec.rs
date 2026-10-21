//! The `opencode-zen` provider channel: the OpenCode Zen relay surface
//! (`opencode.ai/zen/v1`), authenticated via an OpenCode API key
//! (`OPENCODE_API_KEY`) or public keyless bearer.

use nuo_inference_wire::reasoning::ReasoningSupport;
use nuo_inference_wire::{Model, WireProtocol};

use nuo_provider::*;

/// Curated seed models offered by the OpenCode Zen preset.
pub use nuo_inference_wire::model_providers::OPENCODE_ZEN_MODELS;

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
        wire_entities: None,
    },
    Model {
        // Retired V4 Flash generation, served upstream by V4.1-Flash-1017; the
        // row is therefore the serving model's (1M, multimodal) with the
        // platform's accepted effort vocabulary (ADR-0083). Byte-identical to
        // the `nuo-provider-deepseek` and `opencode-plan` rows.
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        wire_entities: None,
    },
    Model {
        id: "glm-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::GLM_5,
        wire_entities: None,
    },
    Model {
        id: "glm-5.3-flash",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::GLM_5,
        wire_entities: None,
    },
    Model {
        id: "deepseek-v4.1-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        wire_entities: None,
    },
    // =========================================================================
    // EPHEMERAL: OpenCode Promotional Free Tier (Oct 2026 - Limited Time)
    // Upstream authority: models.opencode.ai/api.json ($p priority list)
    // Context is restricted to 200k (vs 1M paid), output 32k.
    // Tagged for easy discovery and retirement when OpenCode free tier ends.
    // =========================================================================
    Model {
        id: "deepseek-v4.1-flash-free",
        family: "deepseek",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        wire_entities: None,
    },
    Model {
        id: "glm-5.3-flash-free",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::GLM_5,
        wire_entities: None,
    },
];

inventory::submit!(nuo_inference_wire::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_inference_wire::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[
        (
            WireProtocol::AnthropicMessages,
            std::borrow::Cow::Borrowed("https://opencode.ai/inference/anthropic/v1"),
        ),
        (
            WireProtocol::GoogleGenerateContent,
            std::borrow::Cow::Borrowed("https://opencode.ai/inference/google/v1beta"),
        ),
    ]),
    catalog_root_url: Some(std::borrow::Cow::Borrowed("https://opencode.ai/zen/v1")),
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("opencode-zen"),
    baselines: MODELS,
    // Endpoints are per-model by wire format (see `route_for_model`); the
    // instance-level default is the OpenAI chat-completions surface.
    root_url: std::borrow::Cow::Borrowed("https://opencode.ai/zen/v1"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_inference_wire::client_identity::OPENCODE_USER_AGENT,
    )),
    protocol: WireProtocol::OpenAiChatCompletions,
    // Served models come from the live /zen/v1/models endpoint.
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_inference_wire::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Structural,
    models: OPENCODE_ZEN_MODELS,
};

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_inference_wire::model::{BaselineModels, resolve};

    #[test]
    fn baseline_ids_are_unique() {
        let mut ids: Vec<_> = MODELS.iter().map(|m| m.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate baseline ids in opencode-zen");
        let _ = BaselineModels(MODELS);
    }

    #[test]
    fn the_zen_seed_stays_registered() {
        for id in OPENCODE_ZEN_MODELS {
            let model = resolve(id);
            assert_eq!(model.id, *id, "seeded id {id} must be registered");
        }
    }
}
