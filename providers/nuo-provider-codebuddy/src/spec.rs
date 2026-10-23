//! The built-in `codebuddy` provider preset: Tencent AI CodeBuddy
//! (`copilot.tencent.com/v2`), authenticated via Bearer token or CodeBuddy CLI credentials.
//!
//! # Models and Capabilities
//!
//! CodeBuddy provides access to frontier reasoning models:
//! - `hy4-preview`: Hunyuan 4 Thinking Model (混元思考模型) with 1M context, streaming thinking
//!   via `reasoning_content`, native tool calling, and multimodal vision.
//! - `hy3`: Hunyuan 3 Thinking Model (混元思考模型).
//! - `deepseek-v4-flash`: DeepSeek V4.1 Flash, 1M context, vision, reasoning.
//! - `deepseek-v4-pro`: DeepSeek V4 Pro, 1M context, text reasoning.
//! - `kimi-k3-1`: Kimi K3.1, 1M context, tool calling, vision.
//! - `glm-5.3-flash`: GLM 5.3 Flash, 1M context, tool calling, vision.
//! - `minimax-m2.5`: MiniMax M2.5, 1M context.
//! - `default`: Automatic model routing.
//!
//! # Wire Protocol
//!
//! CodeBuddy speaks standard OpenAI-compatible Chat Completions SSE streams
//! with incremental `reasoning_content` deltas. Streaming is mandatory (`stream: true`).

use nuo_inference_wire::WireProtocol;
use nuo_provider::reasoning::ReasoningSupport;
use nuo_provider::*;

pub use nuo_provider::model_providers::CODEBUDDY_BUILTIN_MODELS;

/// Baseline capability metadata for models served by Tencent AI CodeBuddy.
pub const MODELS: &[Model] = &[
    Model {
        id: "hy4-preview",
        family: "hunyuan",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        wire_entities: None,
    },
    Model {
        id: "hy3",
        family: "hunyuan",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        wire_entities: None,
    },
    Model {
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
        id: "deepseek-v4-pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        wire_entities: None,
    },
    Model {
        id: "kimi-k3-1",
        family: "kimi",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
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
        id: "minimax-m2.5",
        family: "minimax",
        context_window: 204_800,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    Model {
        id: "default",
        family: "auto",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
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
        reports_misses: false,
    }
}

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::CodeBuddy,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("codebuddy"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://copilot.tencent.com/v2"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_provider::client_identity::CODEBUDDY_USER_AGENT,
    )),
    protocol: WireProtocol::OpenAiChatCompletions,
    catalog_source: RemoteCatalogSource::None,
    default_client_profile: nuo_provider::ClientPreset::CodeBuddy,
    client_profile_sensitive: true,
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Native,
    models: CODEBUDDY_BUILTIN_MODELS,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebuddy_spec_invariants() {
        assert_eq!(MODEL_PROVIDER_SPEC.id, "codebuddy");
        assert_eq!(
            MODEL_PROVIDER_SPEC.protocol,
            WireProtocol::OpenAiChatCompletions
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.dialect,
            nuo_provider::ProviderDialect::CodeBuddy
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.default_client_profile,
            nuo_provider::ClientPreset::CodeBuddy
        );
        const { assert!(MODEL_PROVIDER_SPEC.client_profile_sensitive) };
    }

    #[test]
    fn hy4_preview_supports_reasoning_and_vision() {
        let hy4 = MODELS
            .iter()
            .find(|m| m.id == "hy4-preview")
            .expect("hy4-preview");
        assert!(hy4.vision);
        assert!(hy4.tool_call);
        assert_eq!(hy4.thinking, ReasoningSupport::ReasoningContent);
        assert_eq!(hy4.context_window, 1_000_000);
    }

    #[test]
    fn seeded_models_exist() {
        for id in CODEBUDDY_BUILTIN_MODELS {
            assert!(
                MODELS.iter().any(|m| m.id == *id),
                "model {id} must exist in MODELS"
            );
        }
    }
}
