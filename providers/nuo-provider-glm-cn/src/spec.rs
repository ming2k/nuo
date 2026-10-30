//! The `glm-cn` provider channel: Zhipu's **GLM Coding Plan** on the domestic
//! BigModel platform (`open.bigmodel.cn/api/coding/paas/v4`).
//!
//! The product is Zhipu's GLM Coding Plan; `CN` names the *site*, not the
//! product — the international plan is the same product served from
//! `api.z.ai/api/coding/paas/v4`. Keeping the region in the label is a
//! transitional stand-in: region is a genuine account dimension that has no
//! `ModelProviderSpec` counterpart yet (`docs/dev/provider-extension-design.md`
//! §5), so folding it into the label is honest about what it is.

use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::*;

/// Models served by the GLM Coding Plan endpoint, in display/activation
/// order — the first entry is the initial active channel. `glm-5.3-flash`
/// joined the plan alongside the flagship (native multimodal, 1M context,
/// ~1/3 the credit burn), so it is offered ahead of the older flagships.
pub use nuo_provider::model_providers::GLM_CN_MODELS;

// GLM Coding Plan, domestic site — Zhipu BigModel
// (open.bigmodel.cn/api/coding/paas/v4). A coding-agent membership endpoint
// that serves the GLM-5 family; glm-5.3 is the current flagship. Like the Kimi
// Code platform, it expects a recognized coding-agent User-Agent. The plan key
// is a per-connection `api_key_env` or a stored credential — the spec declares
// no built-in environment variable name.

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_inference_wire`'s registry at link time (see
/// [`nuo_provider::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // GLM family (Zhipu / Z.AI / opencode-go)
    Model {
        id: "glm-5.3",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::GLM_5,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        // GLM-5.3-Flash — the GLM-5 family's first natively multimodal model
        // (vision: screenshots, rendered UI, image inputs), served on the
        // coding plan at roughly a third of GLM-5.3's credit burn. Text
        // parameters are identical to GLM-5.3: 1M context, always-on thinking
        // (`thinking.type: enabled` only) with `reasoning_effort`
        // low/high/xhigh/max, streaming, and tool calls.
        id: "glm-5.3-flash",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::GLM_5,
        default_effort: Some(nuo_provider::Effort::High),
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
        effort_levels: effort_ladders::GLM_5,
        default_effort: Some(nuo_provider::Effort::High),
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
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
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
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "glm-4.7",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "glm-4.6",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "glm-4.5",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

pub const ZCODE_USER_AGENT: &str = "ZCode/3.5.3";
pub const ZCODE_CLIENT_HEADERS: &[(&str, &str)] =
    &[("X-Title", "Z Code"), ("X-ZCode-Agent", "glm")];

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("glm-cn"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://open.bigmodel.cn/api/coding/paas/v4"),
    user_agent: Some(std::borrow::Cow::Borrowed(ZCODE_USER_AGENT)),
    default_headers: std::borrow::Cow::Borrowed(ZCODE_CLIENT_HEADERS),
    protocol: WireProtocol::OpenAiChatCompletions,
    // Live-verified (2026-08): the coding endpoint serves GET /models and
    // returns the plan's current model ids (OpenAI list shape, ids only — no
    // capability metadata). The first-party list stays authoritative for what
    // the account's plan actually offers; if it is ever unreachable/empty, the
    // upstream models.dev entry (`zai`) covers the gap so a plan refresh does not blank
    // the picker. Baselines stay the single source of capability truth either
    // way (capability overlay is unavailable here).
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Native,
    responses_state: nuo_provider::ResponsesState::Stateless,
    models: GLM_CN_MODELS,
};

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_provider::model::resolve;

    #[test]
    fn baseline_ids_are_unique() {
        let mut ids: Vec<_> = MODELS.iter().map(|m| m.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate baseline ids in glm-cn");
    }

    #[test]
    fn the_seed_stays_registered() {
        for id in GLM_CN_MODELS {
            let model = resolve(id);
            assert_eq!(model.id, *id, "seeded id {id} must be registered");
            assert_eq!(model.context_window, 1_000_000, "{id}: 1M context");
            assert!(model.tool_call, "{id}: tool calls supported");
            assert_eq!(
                MODEL_PROVIDER_SPEC.model_protocol(id),
                WireProtocol::OpenAiChatCompletions,
                "{id}: domestic BigModel coding endpoint speaks OpenAiChatCompletions"
            );
        }
    }

    #[test]
    fn spec_conforms_to_domestic_bigmodel_endpoint() {
        assert_eq!(MODEL_PROVIDER_SPEC.id, "glm-cn");
        assert_eq!(
            MODEL_PROVIDER_SPEC.root_url,
            "https://open.bigmodel.cn/api/coding/paas/v4"
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.protocol,
            WireProtocol::OpenAiChatCompletions
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.user_agent.as_deref(),
            Some(ZCODE_USER_AGENT)
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.default_headers.as_ref(),
            ZCODE_CLIENT_HEADERS
        );
    }
}
