//! The `codebuddy-intl` spec: the international ingress at
//! `www.codebuddy.ai/v2`.
//!
//! Model baseline = the **external-line** universe served by the vendor's
//! international gateway (`product.json` external environment of the npm
//! `@tencent-ai/codebuddy-code` CLI). The domestic line's Hunyuan/GLM/MiniMax
//! catalog is a *different universe* and lives in `nuo-provider-codebuddy-cn`
//! — the two baselines never share a const ([ADR-0097],
//! `[INV-PROV-CHANNEL-01]`).
//!
//! The upstream catalog is server-published (`/v3/config`, `product.json
//! models[]`): treat this list as the verified floor, extended at runtime by
//! catalog discovery rather than a hard contract.

use nuo_inference_wire::WireProtocol;
use nuo_provider::reasoning::ReasoningSupport;
use nuo_provider::*;

/// Seed model ids for the international line, in activation order.
pub const CODEBUDDY_INTL_BUILTIN_MODELS: &[&str] = &["gpt-5.6-sol", "default"];

/// Baseline capability metadata for the international line's models.
pub const MODELS: &[Model] = &[
    Model {
        // Verified in the wire reference snapshot carried over from the
        // pre-bifurcation baseline: the international gateway serves the
        // global-class reasoning flagship with native summaries.
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
        default_effort: Some(nuo_provider::Effort::Medium),
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
        default_effort: Some(nuo_provider::Effort::High),
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

pub const INTL_MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::CodeBuddy,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("codebuddy-intl"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://www.codebuddy.ai/v2"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_provider::client_identity::CODEBUDDY_USER_AGENT,
    )),
    protocol: WireProtocol::OpenAiChatCompletions,
    catalog_source: RemoteCatalogSource::None,
    default_client_profile: nuo_provider::ClientPreset::CodeBuddy,
    client_profile_sensitive: true,
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Native,
    responses_state: nuo_provider::ResponsesState::Stateless,
    models: CODEBUDDY_INTL_BUILTIN_MODELS,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_is_bound_to_the_international_realm() {
        assert_eq!(INTL_MODEL_PROVIDER_SPEC.id, "codebuddy-intl");
        assert_eq!(
            INTL_MODEL_PROVIDER_SPEC.root_url,
            "https://www.codebuddy.ai/v2"
        );
        assert_eq!(
            INTL_MODEL_PROVIDER_SPEC.protocol,
            WireProtocol::OpenAiChatCompletions
        );
        assert_eq!(
            INTL_MODEL_PROVIDER_SPEC.dialect,
            nuo_provider::ProviderDialect::CodeBuddy
        );
        assert_eq!(
            INTL_MODEL_PROVIDER_SPEC.default_client_profile,
            nuo_provider::ClientPreset::CodeBuddy
        );
        const { assert!(INTL_MODEL_PROVIDER_SPEC.client_profile_sensitive) };
    }

    #[test]
    fn seeded_models_exist() {
        for id in CODEBUDDY_INTL_BUILTIN_MODELS {
            assert!(
                MODELS.iter().any(|m| m.id == *id),
                "model {id} must exist in MODELS"
            );
        }
    }

    #[test]
    fn international_baseline_shares_no_model_with_the_domestic_catalog() {
        // The two channel crates must never advertise the same model universe:
        // the realms serve disjoint server-published catalogs ([ADR-0097]).
        let domestic_ids: &[&str] = &[
            "hy4-preview",
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "kimi-k3-1",
            "glm-5.3-flash",
            "minimax-m2.5",
        ];
        for id in domestic_ids {
            assert!(
                !CODEBUDDY_INTL_BUILTIN_MODELS.contains(id),
                "model {id} belongs to the domestic catalog, not the international line"
            );
        }
    }
}
