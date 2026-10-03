//! The `chatgpt-oauth` provider preset: GPT-5.x over the ChatGPT
//! Subscription backend (the Codex Responses API).

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use super::{effort_ladders, CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// Empty seed: the ChatGPT Subscription backend's model set is fully
/// catalog-derived from the account's live Codex catalog
/// (`/backend-api/codex/models`). The static seed never guesses
/// plan-specific access — the entitlement-aware endpoint is the single source
/// of truth, and the picker is intentionally empty until that first fetch
/// completes. Baseline capability metadata for ids the catalog returns still
/// resolves through the model registry (`MODELS` below).
pub use nuo_model_codec::model_providers::CHATGPT_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_model_codec`'s registry at link time (see
/// [`nuo_model_codec::model::BaselineModels`]).
///
/// These entries record the subscription's Responses protocol per model
/// (ADR-0260: protocol is provider-scoped). The global baseline registry is a
/// capability union, so the same ids may carry a different protocol in another
/// provider table; provider routers resolve protocol through
/// [`ModelProviderSpec::model_protocol`], never through the global union.
pub const MODELS: &[Model] = &[
    Model {
        id: "gpt-6-astra",
        family: "gpt",
        context_window: 872_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_6,
    },
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-terra",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-luna",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    // Non-seeded models retained solely as metadata for ids returned by the
    // account-specific live Codex catalog.
    Model {
        id: "gpt-5.5",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.3-codex-spark",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::Responses,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

// The ChatGPT subscription endpoint is Responses-shaped, but it is not the
// public OpenAI Responses API. It accepts Codex's stable `prompt_cache_key`
// while rejecting the GPT-5.6 platform-only `prompt_cache_options` control.
// Keep this route implicit and retention-neutral so the shared encoder emits
// only the affinity key.
const CHATGPT_IMPLICIT_CACHE: nuo_model_codec::PromptCacheSpec = nuo_model_codec::PromptCacheSpec {
    modes: &[nuo_model_codec::PromptCacheMode::Implicit],
    default_mode: Some(nuo_model_codec::PromptCacheMode::Implicit),
    supported_retentions: &[],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: None,
    reports_reads: true,
    reports_writes: true,
    reports_misses: false,
};

const fn prompt_cache_for_model(_: &str) -> nuo_model_codec::PromptCacheSpec {
    CHATGPT_IMPLICIT_CACHE
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::ChatGpt,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("openai-subscription"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://chatgpt.com/backend-api/codex"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_model_codec::client_identity::CODEX_USER_AGENT,
    )),
    // The Responses transport is the OpenAI wire family. Catalog fetch uses the
    // subscription-only `/backend-api/codex/models` catalog rather than the
    // public OpenAI `{data:[...]}` shape; the remote catalog is authoritative
    // for each account and its capability metadata is trusted.
    protocol: WireProtocol::Responses,
    models: CHATGPT_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::Codex),
    default_client_profile: nuo_model_codec::ClientPreset::Codex,
    client_profile_sensitive: true,
};

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_model_codec::PromptCacheMode;

    #[test]
    fn seed_is_empty_and_uses_responses() {
        // The picker seed is intentionally empty: Codex /backend-api/codex/models
        // (the entitlement-aware endpoint) is authoritative, so nothing is
        // hardcoded — see the module doc for why.
        assert_eq!(CHATGPT_BUILTIN_MODELS, &[] as &[&str]);
        assert_eq!(MODEL_PROVIDER_SPEC.protocol, WireProtocol::Responses);
    }

    #[test]
    fn chatgpt_uses_affinity_without_platform_cache_options() {
        let capabilities = MODEL_PROVIDER_SPEC.prompt_cache.resolve("gpt-5.6-sol");
        assert_eq!(capabilities.modes, vec![PromptCacheMode::Implicit]);
        assert_eq!(capabilities.default_mode, Some(PromptCacheMode::Implicit));
        assert!(capabilities.supported_retentions.is_empty());
        assert_eq!(capabilities.default_retention, None);
        assert!(capabilities.routing_key_supported);
    }
}
