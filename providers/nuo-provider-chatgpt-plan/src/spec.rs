//! The `chatgpt-oauth` provider preset: GPT-5.x over the ChatGPT
//! Subscription backend (the Codex Responses API).
use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::effort_ladders;
use nuo_provider::spec::ModelProviderSpec;
use nuo_provider::{CatalogShape, RemoteCatalogSource};

/// Empty seed: the ChatGPT Subscription backend's model set is fully
/// catalog-derived from the account's live Codex catalog
/// (`/backend-api/codex/models`). The static seed never guesses
/// plan-specific access — the entitlement-aware endpoint is the single source
/// of truth, and the picker is intentionally empty until that first fetch
/// completes. Baseline capability metadata for ids the catalog returns still
/// resolves through the model registry (`MODELS` below).
pub use nuo_provider::model_providers::CHATGPT_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_inference_wire`'s registry at link time (see
/// [`nuo_provider::model::BaselineModels`]).
///
/// These entries record the subscription's Responses protocol per model
/// (protocol is provider-scoped). The global baseline registry is a
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
        protocol: WireProtocol::OpenAiResponses,
        effort_levels: effort_ladders::OPENAI_GPT_6,
        default_effort: Some(nuo_provider::Effort::Medium),
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
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
        default_effort: Some(nuo_provider::Effort::Medium),
        wire_entities: None,
    },
    Model {
        id: "gpt-5.6-terra",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiResponses,
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
        default_effort: Some(nuo_provider::Effort::Medium),
        wire_entities: None,
    },
    Model {
        id: "gpt-5.6-luna",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiResponses,
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
        default_effort: Some(nuo_provider::Effort::Medium),
        wire_entities: None,
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
        protocol: WireProtocol::OpenAiResponses,
        effort_levels: effort_ladders::OPENAI_GPT,
        default_effort: Some(nuo_provider::Effort::Medium),
        wire_entities: None,
    },
    Model {
        id: "gpt-5.3-codex-spark",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiResponses,
        effort_levels: effort_ladders::OPENAI_GPT,
        default_effort: Some(nuo_provider::Effort::Medium),
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

// The ChatGPT subscription endpoint is Responses-shaped, but it is not the
// public OpenAI Responses API. It accepts Codex's stable `prompt_cache_key`
// while rejecting the GPT-5.6 platform-only `prompt_cache_options` control.
// Keep this route implicit and retention-neutral so the shared encoder emits
// only the affinity key.
const CHATGPT_IMPLICIT_CACHE: nuo_provider::PromptCacheSpec = nuo_provider::PromptCacheSpec {
    modes: &[nuo_provider::PromptCacheMode::Implicit],
    default_mode: Some(nuo_provider::PromptCacheMode::Implicit),
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

const fn prompt_cache_for_model(_: &str) -> nuo_provider::PromptCacheSpec {
    CHATGPT_IMPLICIT_CACHE
}

pub const CODEX_VERSION: &str = "0.153.4";
pub const CODEX_USER_AGENT: &str = "codex_cli_rs/0.153.4";
pub const CODEX_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Openai-Intent", "conversation-edits"),
    ("x-initiator", "user"),
];

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::ChatGpt,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("chatgpt-plan"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://chatgpt.com/backend-api/codex"),
    user_agent: Some(std::borrow::Cow::Borrowed(CODEX_USER_AGENT)),
    default_headers: std::borrow::Cow::Borrowed(CODEX_CLIENT_HEADERS),
    // The Responses transport is the OpenAI wire family. Catalog fetch uses the
    // subscription-only `/backend-api/codex/models` catalog rather than the
    // public OpenAI `{data:[...]}` shape; the remote catalog is authoritative
    // for each account and its capability metadata is trusted.
    protocol: WireProtocol::OpenAiResponses,
    models: CHATGPT_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::Codex),
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Native,
    responses_state: nuo_provider::ResponsesState::Stateless,
};

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_provider::PromptCacheMode;

    #[test]
    fn seed_is_empty_and_uses_responses() {
        // The picker seed is intentionally empty: Codex /backend-api/codex/models
        // (the entitlement-aware endpoint) is authoritative, so nothing is
        // hardcoded — see the module doc for why.
        assert_eq!(CHATGPT_BUILTIN_MODELS, &[] as &[&str]);
        assert_eq!(MODEL_PROVIDER_SPEC.protocol, WireProtocol::OpenAiResponses);
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
