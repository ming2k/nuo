//! The `opencode-plan` provider channel: OpenCode Plan subscription surface
//! (`opencode.ai/zen/go/v1`), authenticated via OpenCode Go API key
//! (`OPENCODE_API_KEY`).

use nuo_inference_wire::reasoning::ReasoningSupport;
use nuo_inference_wire::{Model, WireProtocol};

use nuo_provider::*;

/// Curated seed models offered by the OpenCode Plan channel.
pub use nuo_inference_wire::model_providers::OPENCODE_PLAN_MODELS;

/// Baseline capability metadata for the models this provider serves.
///
/// The relay's `/models` payload carries ids only
/// (`docs/explanation/opencode-provider-integration.md` §4.2), so this table is
/// the only capability source — every id the relay serves that nuo intends to
/// present as a first-class model needs an entry here. The Go endpoint table
/// (`opencode.ai/docs/go`) is the upstream authority for what exists; ids the
/// table has dropped (the bare `deepseek-flash` residue of the V4.1-Flash
/// rename) are deliberately **not** registered — a discovered-but-unregistered
/// id falls back to the conservative capability default and vanishes from the
/// curated surface the day the relay retires it.
pub const MODELS: &[Model] = &[
    // DeepSeek (opencode-go / direct)
    Model {
        // The retired V4 Flash generation: still accepted upstream and served by
        // DeepSeek-V4.1-Flash-1017, so the capability row is the *serving*
        // model's (1M, multimodal) and the effort ladder is the platform's
        // accepted set (ADR-0083). Shared with `nuo-provider-deepseek`, which
        // must stay byte-identical (the registry invariant).
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
        // DeepSeek V4.1 Flash. Verified against the Go endpoint table
        // (opencode.ai/docs/go, 2026-09) and DeepSeek's V4.1-Flash release:
        // Chat Completions via `@ai-sdk/openai-compatible`, 1M context, native
        // multimodal (image inputs), continuous reasoning effort served on the
        // Go relay as the `low`/`high`/`max` presets (integer efforts are
        // rejected upstream with HTTP 400).
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
        id: "glm-5",
        family: "glm",
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
        id: "glm-5.1",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    // Anthropic Claude (via opencode-go Anthropic messages route)
    Model {
        id: "claude-haiku-5-5",
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
    // Google Gemini (via opencode-go Google Gemini route)
    Model {
        id: "gemini-4-argon-contributor",
        family: "gemini",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    // GLM family (Zhipu / Z.AI / opencode-go)
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
        id: "glm-5.3",
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
        id: "grok-4.7",
        family: "grok",
        context_window: 500_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    Model {
        id: "kimi-k3",
        family: "kimi",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "mimo-v2.6-flash",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "mimo-v2.6-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "kimi-k2.5",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "kimi-k2.6",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "kimi-k2.7-code",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "mimo-v2-omni",
        family: "mimo",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "mimo-v2-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    // MiMo (Xiaomi / opencode-go, OpenAI format)
    Model {
        id: "mimo-v2.5",
        family: "mimo",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "mimo-v2.5-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
    Model {
        id: "minimax-m2.5",
        family: "minimax",
        context_window: 204_800,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    Model {
        id: "minimax-m2.7",
        family: "minimax",
        context_window: 204_800,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    // MiniMax (opencode-go, Anthropic /messages format)
    Model {
        id: "minimax-m3",
        family: "minimax",
        context_window: 512_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    Model {
        id: "qwen3.5-plus",
        family: "qwen",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    Model {
        id: "qwen3.6-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    // Qwen (opencode-go)
    Model {
        id: "qwen3.7-max",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    Model {
        id: "qwen3.7-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
        wire_entities: None,
    },
    Model {
        id: "qwen3.8-max",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::QWEN_MIXED,
        wire_entities: None,
    },
    Model {
        id: "qwen3.8-flash",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::QWEN_MIXED,
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
            std::borrow::Cow::Borrowed("https://opencode.ai/inference/go/anthropic/v1"),
        ),
        (
            WireProtocol::GoogleGenerateContent,
            std::borrow::Cow::Borrowed("https://opencode.ai/inference/go/google/v1beta"),
        ),
    ]),
    catalog_root_url: Some(std::borrow::Cow::Borrowed("https://opencode.ai/zen/go/v1")),
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("opencode-plan"),
    baselines: MODELS,
    // Endpoints are per-model by wire format (see `route_for_model`); the
    // instance-level default is the OpenAI chat-completions surface.
    root_url: std::borrow::Cow::Borrowed("https://opencode.ai/zen/go/v1"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_inference_wire::client_identity::OPENCODE_USER_AGENT,
    )),
    protocol: WireProtocol::OpenAiChatCompletions,
    // Served models come from the live /zen/go/v1/models endpoint.
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_inference_wire::ClientPreset::Native,
    client_profile_sensitive: false,
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Structural,
    models: OPENCODE_PLAN_MODELS,
};

#[cfg(test)]
mod tests {
    use super::{MODELS, OPENCODE_PLAN_MODELS};
    use nuo_inference_wire::model::{BaselineModels, resolve};
    use nuo_provider::effort_ladders;

    /// A discovered id with no baseline entry falls back to the 128k/no-effort
    /// conservative default (`nuo_inference_wire::model::fallback_model`), which
    /// silently strips the effort control and under-reports the context window
    /// in the pickers (the `/zen/go/v1/models` payload carries ids only, so the
    /// compiled baseline is the only capability source — see
    /// `docs/explanation/opencode-provider-integration.md` §4.2).
    #[test]
    fn every_deepseek_catalog_id_has_a_ladder_carrying_baseline() {
        // The ids the Go endpoint table lists (opencode.ai/docs/go, 2026-09).
        // The bare `deepseek-flash` residue of the V4.1-Flash rename is
        // deliberately absent: the table no longer lists it, so no baseline
        // carries it (see `retired_relay_ids_stay_unregistered`).
        let relay_ids = [
            "deepseek-v4-flash",
            "deepseek-v4.1-flash",
            "deepseek-v4-pro",
            "deepseek-v4-flash-vision-exp",
        ];
        for id in relay_ids {
            let model = resolve(id);
            assert_eq!(
                model.id, id,
                "{id} must resolve to itself, not the anonymous fallback"
            );
            assert!(
                !model.effort_levels.is_empty(),
                "{id} must carry a reasoning-effort ladder"
            );
            assert_eq!(
                model.effort_levels,
                effort_ladders::DEEPSEEK_NATIVE,
                "{id}: the platform's accepted vocabulary, `none` (thinking off) included"
            );
            assert_eq!(model.context_window, 1_000_000, "{id}: 1M context");
            assert!(model.tool_call, "{id}: agent harness requires tool calls");
        }
    }

    /// Ids the upstream endpoint table has retired stay out of *this* table: the
    /// compiled baseline never outlives its source, so a relay that still leaks
    /// such an id from `/models` surfaces it capability-less (the conservative
    /// fallback) instead of from a hand-maintained shadow entry.
    ///
    /// `deepseek-flash` is the *official* platform id — `nuo-provider-deepseek`
    /// registers it, and this relay simply does not serve it (ADR-0083). The
    /// assertion is therefore table-scoped: the global registry is a union of
    /// every linked provider's declarations, not this relay's private view.
    #[test]
    fn relay_table_does_not_carry_ids_the_endpoint_table_dropped() {
        assert!(
            !MODELS.iter().any(|model| model.id == "deepseek-flash"),
            "the Go endpoint table no longer lists the bare `deepseek-flash`"
        );
    }

    /// The offline seed is what the picker offers before the first catalog
    /// refresh; every seeded id must be registered (the same invariant the
    /// `builtin_provider_models_resolve_with_expected_wire_formats` test
    /// enforces across all built-ins).
    #[test]
    fn the_plan_seed_stays_registered() {
        for id in OPENCODE_PLAN_MODELS {
            let model = resolve(id);
            assert_eq!(model.id, *id, "seeded id {id} must be registered");
        }
    }

    /// Guard against duplicate baseline entries silently shadowing each other
    /// (`model_by_id` returns the first match).
    #[test]
    fn baseline_ids_are_unique() {
        let mut ids: Vec<_> = MODELS.iter().map(|m| m.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate baseline ids in opencode-plan");
        let _ = BaselineModels(MODELS); // keep the import tied to the real type
    }
}
