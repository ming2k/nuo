//! The `opencode-go` provider preset: OpenCode Go subscription surface
//! (`opencode.ai/zen/go/v1`), authenticated via OpenCode Go API key
//! (`OPENCODE_API_KEY`).

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use super::{effort_ladders, CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// Curated seed models offered by the OpenCode Go preset.
pub use nuo_model_codec::model_providers::OPENCODE_GO_MODELS;

/// Baseline capability metadata for the models this provider serves.
///
/// The relay's `/models` payload carries ids only
/// (`docs/explanation/opencode-provider-integration.md` §4.2), so this table is
/// the only capability source — every id the relay serves that muta intends to
/// present as a first-class model needs an entry here. The Go endpoint table
/// (`opencode.ai/docs/go`) is the upstream authority for what exists; ids the
/// table has dropped (the bare `deepseek-flash` residue of the V4.1-Flash
/// rename) are deliberately **not** registered — a discovered-but-unregistered
/// id falls back to the conservative capability default and vanishes from the
/// curated surface the day the relay retires it.
pub const MODELS: &[Model] = &[
    // DeepSeek (opencode-go / direct)
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
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
        protocol: WireProtocol::ChatCompletions,
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
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    Model {
        id: "glm-5",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "glm-5.1",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // GLM family (Zhipu / Z.AI / opencode-go)
    Model {
        id: "glm-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::GLM_5,
    },
    Model {
        id: "kimi-k2.5",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.6",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.7-code",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-omni",
        family: "mimo",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // MiMo (Xiaomi / opencode-go, OpenAI format)
    Model {
        id: "mimo-v2.5",
        family: "mimo",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2.5-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
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
    },
    // Qwen (opencode-go)
    Model {
        id: "qwen3.7-max",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
    Model {
        id: "qwen3.7-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

#[cfg(test)]
mod tests {
    use super::{MODELS, OPENCODE_GO_MODELS};
    use nuo_model_codec::effort::Effort;
    use nuo_model_codec::model::{resolve, BaselineModels};

    /// A discovered id with no baseline entry falls back to the 128k/no-effort
    /// conservative default (`nuo_model_codec::model::fallback_model`), which
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
                &[Effort::Low, Effort::High, Effort::Max],
                "{id}: DeepSeek's Go relay ladder"
            );
            assert_eq!(model.context_window, 1_000_000, "{id}: 1M context");
            assert!(model.tool_call, "{id}: agent harness requires tool calls");
        }
    }

    /// Ids the upstream endpoint table has retired stay unregistered: the
    /// compiled baseline never outlives its source. A relay that still leaks
    /// such an id from `/models` surfaces it capability-less (the conservative
    /// fallback) instead of a hand-maintained shadow entry — the registry does
    /// not curate aliases for spellings upstream dropped.
    #[test]
    fn retired_relay_ids_stay_unregistered() {
        let model = resolve("deepseek-flash");
        assert_eq!(model.id, "", "a retired id must not resolve to a name");
        assert_eq!(model.context_window, 128_000, "retired id: fallback window");
        assert!(
            model.effort_levels.is_empty(),
            "retired id: no invented ladder"
        );
    }

    /// The offline seed is what the picker offers before the first catalog
    /// refresh; every seeded id must be registered (the same invariant the
    /// `builtin_provider_models_resolve_with_expected_wire_formats` test
    /// enforces across all built-ins).
    #[test]
    fn the_go_seed_stays_registered() {
        for id in OPENCODE_GO_MODELS {
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
        assert_eq!(ids.len(), count, "duplicate baseline ids in opencode-go");
        let _ = BaselineModels(MODELS); // keep the import tied to the real type
    }
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: Some(std::borrow::Cow::Borrowed("https://opencode.ai/zen/go/v1")),
    prompt_cache: super::PromptCachePolicy::Compiled(super::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("opencode-plan"),
    baselines: MODELS,
    // Endpoints are per-model by wire format (see `route_for_model`); the
    // instance-level default is the OpenAI chat-completions surface.
    root_url: std::borrow::Cow::Borrowed("https://opencode.ai/zen/go/v1"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_model_codec::client_identity::OPENCODE_USER_AGENT,
    )),
    protocol: WireProtocol::ChatCompletions,
    // Served models come from the live /zen/go/v1/models endpoint.
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::Native,
    client_profile_sensitive: false,
    models: OPENCODE_GO_MODELS,
};
