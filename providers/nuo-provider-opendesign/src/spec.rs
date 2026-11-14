//! The `opendesign` provider channel: the OpenDesign Cloud API gateway
//! (`https://amr-link.open-design.ai/v1`), authenticated with an OpenDesign
//! API key (`sk-amr-…`).
//!
//! # Surface
//!
//! The gateway exposes three API formats (Chat Completions, Responses, and
//! Messages). This channel declares **Chat Completions** as its single wire:
//! it serves the whole account-scoped model catalog, including the Claude and
//! GPT rows the gateway translates, so one protocol keeps routing and replay
//! uniform across the plan. The other formats remain reachable through a user
//! declared provider when a caller needs one of their native fields.
//!
//! Prompt caching is not declared: the gateway does not publish cache-control
//! support, and its usage payload reports cache reads only for passthrough
//! upstreams, which is not a capability the client can request.
//!
//! # Catalog and capabilities
//!
//! `GET /v1/models` is account-scoped (Go/Plus/Pro/Max receive different
//! catalogs) and authoritative for membership. It advertises context and
//! modality metadata; the compiled rows below are the offline floor and the
//! source of the reasoning ladders, because the catalog publishes no effort
//! tiers.
//!
//! Several ids are shared with first-party/sibling channels (`deepseek-*`,
//! `glm-5.3-flash`, `kimi-k2.7-code`, `mimo-*`, `hy4-preview`). Those rows MUST
//! stay byte-identical to the owning baseline: the global model registry keys
//! capability metadata by id, and the registry invariant requires one signature
//! per id. Where the gateway accepts a narrower effort set than the model's
//! home surface (GLM-5.3 Flash rejects `xhigh`), the shared row keeps the home
//! surface's ladder; the route's live catalog can still revise context and
//! modality metadata, but the picker cannot present a per-provider ladder for a
//! globally shared id yet (`docs/dev/provider-extension-design.md` §5).
//!
//! See `docs/reference/openapi/opendesign.yml` for the gateway contract.

use nuo_inference_wire::WireProtocol;
use nuo_protocol::Model;
use nuo_protocol::reasoning::ReasoningSupport;

use nuo_protocol::surface::{CatalogShape, RemoteCatalogSource};
use nuo_provider::*;

/// Curated seed models offered by the OpenDesign Cloud API channel.
pub use nuo_protocol::model_providers::OPENDESIGN_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves.
///
/// Context windows and input modalities come from the vendor model cards
/// (recorded per row); reasoning ladders come from gateway probes. Output
/// ceilings are deliberately not declared here — `Model` carries no output cap,
/// and the gateway advertises one per account in `/v1/models` metadata.
pub const MODELS: &[Model] = &[
    // DeepSeek V4.1 Flash — 1M context, native image input, all eight named
    // effort levels. Shared verbatim with the OpenCode Go/Zen rows.
    Model {
        id: "deepseek-v4.1-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // OpenDesign's slower tier of V4.1 Flash. The vendor has no such id; its
    // limits match V4.1 Flash and it accepts every effort except `ultra`
    // (gateway probe: `ultra` answers 400 deterministically).
    Model {
        id: "deepseek-v4.1-flash-eco",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::ALL_EFFORTS_EXCEPT_ULTRA,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // Retired V4 Flash ids, still accepted upstream and served by
    // DeepSeek-V4.1-Flash-1017, so they inherit its 1M window and vision.
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "deepseek-v4-flash-vision-exp",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        default_effort: Some(nuo_protocol::Effort::High),
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
        effort_levels: effort_ladders::DEEPSEEK_NATIVE,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // Claude Haiku 5.5 — the gateway's id uses a dot. 1M context, 128K output,
    // image input; probes accept every named effort except `ultra`.
    Model {
        id: "claude-haiku-5.5",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::ALL_EFFORTS_EXCEPT_ULTRA,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // GPT-6 Luna — 1,050,000 total context (922K input + 128K output), image
    // input, and the full eight-level effort vocabulary including `ultra`.
    Model {
        id: "gpt-6-luna",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::ALL_EFFORTS,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // GLM-5.3 Flash — shared id; the home baseline's GLM ladder is kept (see
    // the module note). The gateway itself accepts `low`/`high`/`max` only.
    Model {
        id: "glm-5.3-flash",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::GLM_5,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // GLM-5.3 Flash-X — the higher-throughput GLM-5.3 variant, unique to this
    // catalog. Probes accept only `low`/`high`/`max`; 128K output, image input.
    Model {
        id: "glm-5.3-flashx",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // Kimi K2.7 Code — shared id; keep the home baseline's row so the global
    // registry stays single-signature. The live catalog supplies the gateway's
    // own context/modality declaration when it differs.
    Model {
        id: "kimi-k2.7-code",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: &[],
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // MiMo V2.6 — shared ids with the OpenCode Go rows.
    Model {
        id: "mimo-v2.6-flash",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: &[],
        default_effort: Some(nuo_protocol::Effort::High),
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
        effort_levels: &[],
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // MiniMax M3.1 Flash — always-thinking: the gateway rejects `none` and
    // `ultra`, so the ladder starts at `minimal` and stops at `max`.
    Model {
        id: "minimax-m3.1-flash",
        family: "minimax",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::MINIMAL_TO_MAX,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
    // Hunyuan H4 Preview — shared id; the home baseline's row is retained.
    Model {
        id: "hy4-preview",
        family: "hunyuan",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        default_effort: Some(nuo_protocol::Effort::High),
        wire_entities: None,
    },
];

inventory::submit!(nuo_protocol::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_protocol::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("opendesign"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://amr-link.open-design.ai/v1"),
    user_agent: None,
    default_headers: std::borrow::Cow::Borrowed(&[]),
    protocol: WireProtocol::OpenAiChatCompletions,
    models: OPENDESIGN_BUILTIN_MODELS,
    // The gateway publishes bare ids and coarse account-scoped metadata, so
    // labels are lifted structurally (`deepseek-v4.1-flash` → "DeepSeek V4.1
    // Flash") rather than trusting a vendor display name.
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Structural,
    responses_state: nuo_protocol::ResponsesState::Stateless,
};

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::model::resolve;

    fn model(id: &str) -> &'static Model {
        MODELS
            .iter()
            .find(|model| model.id == id)
            .unwrap_or_else(|| panic!("{id} must be a registered baseline"))
    }

    #[test]
    fn every_seed_is_registered_with_a_chat_route() {
        for id in OPENDESIGN_BUILTIN_MODELS {
            let baseline = model(id);
            assert_eq!(baseline.protocol, WireProtocol::OpenAiChatCompletions);
            assert!(baseline.tool_call, "{id}: tool calls are supported");
            assert_eq!(
                resolve(id).id,
                *id,
                "{id}: the baseline must be visible to the global registry"
            );
        }
    }

    #[test]
    fn spec_targets_the_cloud_gateway_chat_surface() {
        assert_eq!(MODEL_PROVIDER_SPEC.id, "opendesign");
        assert_eq!(
            MODEL_PROVIDER_SPEC.root_url,
            "https://amr-link.open-design.ai/v1"
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.protocol,
            WireProtocol::OpenAiChatCompletions
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.endpoint(WireProtocol::OpenAiChatCompletions),
            Ok("https://amr-link.open-design.ai/v1/chat/completions".to_string())
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.display_policy,
            nuo_provider::DisplayPolicy::Structural
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.catalog_source,
            RemoteCatalogSource::Endpoint(CatalogShape::OpenAi)
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC.catalog_root(),
            "https://amr-link.open-design.ai/v1"
        );
    }

    #[test]
    fn unique_gateway_rows_carry_the_probed_effort_ladders() {
        assert_eq!(
            model("deepseek-v4.1-flash-eco").effort_levels,
            effort_ladders::ALL_EFFORTS_EXCEPT_ULTRA
        );
        assert_eq!(
            model("gpt-6-luna").effort_levels,
            effort_ladders::ALL_EFFORTS
        );
        assert_eq!(
            model("minimax-m3.1-flash").effort_levels,
            effort_ladders::MINIMAL_TO_MAX
        );
        assert!(!effort_ladders::MINIMAL_TO_MAX.contains(&nuo_protocol::Effort::None));
        assert_eq!(model("claude-haiku-5.5").context_window, 1_000_000);
        assert!(model("claude-haiku-5.5").vision);
        assert_eq!(model("gpt-6-luna").context_window, 1_050_000);
    }
}
