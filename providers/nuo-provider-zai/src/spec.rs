//! The `zai-code` provider template and its legacy registry preset: Z.AI /
//! Zhipu BigModel coding-plan platform (`open.bigmodel.cn/api/coding/paas/v4`).

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use nuo_provider::*;

/// Models served by Z.AI's coding-plan endpoint, in display/activation
/// order — the first entry is the initial active channel. `glm-5.3-flash`
/// joined the plan alongside the flagship (native multimodal, 1M context,
/// ~1/3 the credit burn), so it is offered ahead of the older flagships.
pub use nuo_model_codec::model_providers::ZAI_CODE_MODELS;

// ZAI Code (CN) — Zhipu BigModel / Z.AI coding-plan platform
// (open.bigmodel.cn/api/coding/paas/v4). A coding-agent membership endpoint
// that serves the GLM-5 family; glm-5.3 is the current flagship. Like the Kimi
// Code platform, it expects a recognized coding-agent User-Agent. Shares
// the ZHIPU_API_KEY legacy name for key compatibility with the broader
// Zhipu ecosystem, while ZAI_API_KEY is the preferred alias.

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_model_codec`'s registry at link time (see
/// [`nuo_model_codec::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // GLM family (Zhipu / Z.AI / opencode-go)
    Model {
        id: "glm-5.3",
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
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::GLM_5,
    },
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
        id: "glm-4.7",
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
        id: "glm-4.6",
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
        id: "glm-4.5",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("glm-cn"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://open.bigmodel.cn/api/coding/paas/v4"),
    user_agent: Some(std::borrow::Cow::Borrowed(nuo_provider_transport::ZCODE_USER_AGENT)),
    protocol: WireProtocol::ChatCompletions,
    // Live-verified (2026-08): the coding endpoint serves GET /models and
    // returns the plan's current model ids (OpenAI list shape, ids only — no
    // capability metadata). The first-party list stays authoritative for what
    // the account's plan actually offers; if it is ever unreachable/empty, the
    // models.dev entry for zai covers the gap so a plan refresh does not blank
    // the picker. Baselines stay the single source of capability truth either
    // way (capability overlay is unavailable here).
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::ZCode,
    client_profile_sensitive: false,
    quota: None,
    models: ZAI_CODE_MODELS,
};
