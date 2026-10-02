//! The `anthropic` and `anthropic-sub2api` provider presets: a configurable
//! Anthropic `/messages` relay (the official API or any compatible relay),
//! plus the per-model `max_tokens` table every Anthropic-format build
//! consults.

use nuo_contracts::reasoning::ReasoningSupport;
use nuo_contracts::{Model, WireProtocol};

use super::{effort_ladders, CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// Per-model `max_tokens` for the Anthropic `/messages` surface. The Messages
/// API requires `max_tokens`; capping the response at the model's registered
/// output limit (rather than a flat 8192) lets long agent turns from
/// high-output models (MiniMax M3: 131072) run untruncated. Values mirror
/// models.dev's opencode-go entries. Unknown models fall back to the default
/// inside [`AnthropicMessagesProvider`](crate::AnthropicMessagesProvider).
const ANTHROPIC_MODEL_MAX_TOKENS: &[(&str, u32)] = &[
    ("minimax-m3", 131072),
    ("minimax-m2.7", 131072),
    ("minimax-m2.5", 65536),
    ("qwen3.7-max", 65536),
    ("qwen3.7-plus", 65536),
    ("qwen3.6-plus", 65536),
    ("qwen3.5-plus", 65536),
    // Claude family served via Anthropic-compatible relays.
    // Claude 4.6+ Opus/Sonnet support a 128K synchronous output limit (1M
    // context); Haiku 4.5 supports 64K. Cap there so long agent turns are not
    // truncated by the provider's flat 8192 default.
    ("claude-opus-4-8", 128000),
    ("claude-fable-5", 128000),
    ("claude-sonnet-5", 128000),
    ("claude-sonnet-4-6", 128000),
    ("claude-haiku-4-5-20251001", 64000),
];

/// Look up the `max_tokens` for an Anthropic-format model id. `None` lets the
/// provider fall back to its built-in default.
pub(crate) fn anthropic_model_max_tokens(model_id: &str) -> Option<u32> {
    ANTHROPIC_MODEL_MAX_TOKENS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, tokens)| *tokens)
}

/// The Claude model ids the built-in `anthropic` provider serves, in display
/// order. The provider is a *configurable* Anthropic `/messages` relay: the
/// endpoint URL is supplied by config (defaulting to Anthropic's official API),
/// so the same preset serves the official API or any Anthropic-compatible relay.
/// Each id exists in the model registry, so its metadata (context window, output
/// limit, capabilities) resolves there.
pub use nuo_contracts::model_providers::ANTHROPIC_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_contracts`'s registry at link time (see
/// [`nuo_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // Claude (Anthropic, via Anthropic-compatible relays)
    // Served over the Anthropic Messages wire format. Relays forward to
    // Anthropic's own `/messages` surface, so these carry
    // `WireProtocol::AnthropicMessages`.
    Model {
        id: "claude-opus-4-8",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        // Opus 4.8 honors the full effort range including `xhigh`/`max`.
        effort_levels: effort_ladders::CLAUDE_FULL,
    },
    Model {
        id: "claude-sonnet-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        // Sonnet 4.6 honors `max` but NOT `xhigh` (xhigh is Opus 4.8/4.7 only).
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
    },
    Model {
        id: "claude-fable-5",
        family: "claude",
        context_window: 1_000_000,
        // Fable 5 thinking is ALWAYS ON; an explicit `{type:"disabled"}` is
        // rejected with 400. `AnthropicAdaptiveAlwaysOn` makes the transport
        // emit `thinking:{type:"adaptive"}` regardless of the user's on/off
        // choice (an opt-out is a no-op on this model). Manual `type:"enabled"`
        // also returns 400.
        thinking: ReasoningSupport::AnthropicAdaptiveAlwaysOn,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        // Fable 5 honors the full effort range including `xhigh`/`max`.
        effort_levels: effort_ladders::CLAUDE_FULL,
    },
    Model {
        id: "claude-sonnet-5",
        family: "claude",
        context_window: 1_000_000,
        // Sonnet 5: omitting the `thinking` field RUNS adaptive thinking; to
        // actually disable it you must send `{type:"disabled"}`. This is neither
        // `AnthropicAdaptive` (omit disables) nor `AnthropicAdaptiveAlwaysOn`
        // (cannot disable) — so the transport emits an explicit `disabled` on
        // opt-out to honor ADR-0046. Manual `type:"enabled"` returns 400.
        thinking: ReasoningSupport::AnthropicAdaptiveOnByDefault,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        // Sonnet 5 honors the full range INCLUDING `xhigh` — the key difference
        // from Sonnet 4.6, which rejects `xhigh` (see CLAUDE_NO_XHIGH).
        effort_levels: effort_ladders::CLAUDE_FULL,
    },
    Model {
        id: "claude-haiku-4-5-20251001",
        family: "claude",
        context_window: 200_000,
        // Haiku 4.5 supports only MANUAL extended thinking
        // (`thinking:{type:"enabled",budget_tokens}`); it has no adaptive mode
        // and rejects the `effort` parameter (400), hence empty `effort_levels`.
        thinking: ReasoningSupport::AnthropicManual,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(nuo_contracts::model::BaselineModels(MODELS));

fn prompt_cache_for_model(_: &str) -> nuo_contracts::PromptCacheSpec {
    nuo_contracts::PromptCacheSpec {
        modes: &[
            nuo_contracts::PromptCacheMode::Automatic,
            nuo_contracts::PromptCacheMode::Explicit,
        ],
        default_mode: Some(nuo_contracts::PromptCacheMode::Automatic),
        supported_retentions: &[
            nuo_contracts::CacheRetention::FiveMinutes,
            nuo_contracts::CacheRetention::OneHour,
        ],
        default_retention: Some(nuo_contracts::CacheRetention::FiveMinutes),
        disable_supported: true,
        routing_key_supported: false,
        max_breakpoints: Some(4),
        min_cacheable_tokens: None,
        reports_reads: true,
        reports_writes: true,
        reports_misses: false,
    }
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_contracts::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("anthropic"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api.anthropic.com/v1"),
    user_agent: None,
    protocol: WireProtocol::AnthropicMessages,
    models: ANTHROPIC_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::Anthropic),
    default_client_profile: nuo_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
};

#[cfg(test)]
mod tests {
    use crate::AnthropicMessagesProvider;

    use super::*;

    #[test]
    fn anthropic_max_tokens_derives_from_model_output_limit() {
        // minimax-m3's registered output limit (131072) must cap the request's
        // max_tokens, not the provider's flat 8192 default. Construct directly
        // so the typed field is readable (the trait object returned by
        // build_provider_for_channel is not downcastable).
        let provider = AnthropicMessagesProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "minimax-m3".to_string(),
            "https://opencode.ai/inference/anthropic/v1/messages",
            "agent",
        )
        .with_max_tokens(anthropic_model_max_tokens("minimax-m3").unwrap());
        assert_eq!(provider.max_tokens, 131072);
        // An unknown model id falls back to None (the provider keeps its
        // default), proving the lookup does not invent a limit.
        assert!(anthropic_model_max_tokens("not-a-model").is_none());
    }

    #[test]
    fn claude_models_cap_max_tokens_above_the_flat_default() {
        // Claude's registered output limit must lift the request cap above the
        // provider's flat 8192 default so long agent turns are not truncated.
        let opus = AnthropicMessagesProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "claude-opus-4-8".to_string(),
            "https://relay.example.com/v1/messages",
            "agent",
        )
        .with_max_tokens(anthropic_model_max_tokens("claude-opus-4-8").unwrap());
        assert_eq!(opus.max_tokens, 128000);
    }
}
