//! The built-in `qianwen` provider preset: the QianwenAI Platform Token Plan
//! surface (`token-plan.maas.qianwenaiapi.com`), one key (`QIANWEN_API_KEY`).
//!
//! Token Plan personal/team subscriptions are billed in Credits and are
//! strictly interactive-use: the key (`sk-sp-…`) only authenticates against
//! the `token-plan.*` host — the generic `maas.qianwenaiapi.com` pay-as-you-go
//! surface rejects it with 401 — and the plan's model whitelist is enforced
//! upstream (`kimi-k2.7-code` answers with a model-denied error on a plan that
//! does not include it). The live `GET /models` reflects that whitelist, so it
//! is the authoritative membership source and the compiled `MODELS` list is
//! the offline seed plus the capability source (the endpoint publishes ids
//! only — see `OpenAiCatalogParser`).
//!
//! Wire: standard OpenAI Chat Completions (`/compatible-mode/v1`), live-verified
//! (2026-10): streaming with `reasoning_content` deltas, terminal `usage`
//! chunk under `stream_options`, native tool calls, and a prompt-cache hit
//! count in `prompt_tokens_details.cached_tokens`.
//!
//! Reasoning: every reasoning model on the surface is a hybrid thinker whose
//! depth control is plain `reasoning_effort` — the standard Chat Completions
//! field this crate's effort ladder already stamps — with two vendor quirks
//! verified upstream:
//! - `none` is a first-class rung on the Qwen models (and an explicit off:
//!   the server answers with zero reasoning content), while GLM-5.3 accepts
//!   only `low`/`high`/`max`.
//! - DeepSeek V4 (Pro/Pro-0813) rejects `none`/`minimal` — its floor is `low`.
//! Those tier sets are per-model ladders below; the shared clamp machinery
//! never emits a rung a model rejected. Thinking on/off rides the same knob:
//! `enable_thinking` is restricted to `true` on the always-thinking GLM-5.3
//! and pointless on the effort-gated models, so muta sends no extra field.

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};

use super::{effort_ladders, CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// The model ids the built-in `qianwen` provider seeds (the plan's
/// Qwen-native models; the live catalog serves the rest). Each id exists in
/// the model registry and floats with the upstream latest.
pub use nuo_model_codec::model_providers::QIANWEN_BUILTIN_MODELS;

/// Baseline capability metadata for the Qwen-native models this provider
/// serves, submitted to `nuo_model_codec`'s registry at link time (see
/// [`nuo_model_codec::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // Qwen hybrid models — thinking on by default, gated only by
    // `reasoning_effort`; the `none` rung is the documented off switch.
    Model {
        id: "qwen3.8-max",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::QWEN_MIXED,
    },
    Model {
        id: "qwen3.8-flash",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::QWEN_MIXED,
    },
    Model {
        id: "qwen3.6-flash",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::QWEN_MIXED,
    },
    // GLM (Zhipu, Alibaba-hosted), DeepSeek (V4 / V4.1), and Kimi K2.7 Code
    // are deliberately NOT re-declared here: the same ids are already owned
    // by their home providers' baselines (`glm-cn`, `deepseek`, `opencode-go`),
    // and a duplicate baseline must be field-identical — muta's capability
    // union is keyed by model id, not by route (see
    // `shared_baseline_ids_are_identical_across_provider_tables`). Their
    // cross-vendor tier sets differ from the home ladders (live-verified:
    // glm-5.3 rejects `xhigh` here while the home table advertises it;
    // deepseek-v4-pro accepts `xhigh`; `none` is a Qwen-only rung), so a
    // second declaration would either contradict the home table or shadow it
    // by link order. The conservative resolution: the qianwen routes carry no
    // invented ladder for ids another provider already vets — the live
    // `/models` catalog still serves those ids on their clamped home ladders,
    // and `none` requests on the Qwen-only ids are the explicit off switch.
];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(super::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("qianwen"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed(
        "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1",
    ),
    user_agent: None,
    protocol: WireProtocol::ChatCompletions,
    models: QIANWEN_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpenAi),
    default_client_profile: nuo_model_codec::ClientPreset::Native,
    client_profile_sensitive: false,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effort_ladders::{DEEPSEEK_V4_PLAN, QWEN_MIXED};
    use nuo_model_codec::effort::Effort;
    use nuo_model_codec::model::resolve;

    /// Every seeded id is registered (the same invariant
    /// `builtin_provider_models_resolve_with_expected_wire_formats` enforces
    /// cross-provider). The route is provider-scoped (ADR-0260): the qianwen
    /// spec resolves Chat Completions for every seeded id, including ids whose
    /// home baseline speaks Responses at another provider.
    #[test]
    fn the_seed_stays_registered() {
        for id in QIANWEN_BUILTIN_MODELS {
            let model = resolve(id);
            assert_eq!(model.id, *id, "seeded id {id} must be registered");
            assert_eq!(
                model.context_window, 1_000_000,
                "{id}: Token Plan serves a 1M window"
            );
            assert!(model.tool_call, "{id}: the agent harness requires tools");
            assert_eq!(
                MODEL_PROVIDER_SPEC.model_protocol(id),
                WireProtocol::ChatCompletions,
                "{id}: the compatible-mode surface speaks Chat Completions"
            );
        }
    }

    /// Live-verified (2026-10) effort ladder for the Qianwen-native rows:
    /// exactly the rungs the endpoint accepts, including `none` as the
    /// thinking off switch. An invented rung is a guaranteed 400 the first
    /// time the editor cycles it.
    #[test]
    fn ladders_match_the_upstream_validation() {
        for id in ["qwen3.8-max", "qwen3.8-flash", "qwen3.6-flash"] {
            assert_eq!(resolve(id).effort_levels, QWEN_MIXED, "{id}");
        }
        // The third-party ids live on their home baselines (the capability
        // union is model-id-keyed); the qianwen table does not shadow them.
        assert_eq!(
            resolve("glm-5.3").effort_levels,
            crate::effort_ladders::GLM_5
        );
        assert_eq!(
            resolve("deepseek-v4-pro").effort_levels,
            crate::effort_ladders::LOW_HIGH_MAX
        );
    }

    /// A "reasoning off" request never emits a rung the DeepSeek V4 Pro
    /// endpoint rejects outright (live-verified: `none`/`minimal` →
    /// `invalid_parameter_error`); the clamp pins it to `low`.
    #[test]
    fn deepseek_plan_ladder_has_no_off_rung() {
        assert!(!DEEPSEEK_V4_PLAN.contains(&Effort::None));
        assert!(!DEEPSEEK_V4_PLAN.contains(&Effort::Minimal));
        assert_eq!(Effort::None.clamp_to(DEEPSEEK_V4_PLAN), Effort::Low);
    }

    /// The Qwen models' `none` rung is real: the clamp keeps it, so the
    /// hybrid thinker's off switch rides the standard effort field.
    #[test]
    fn qwen_ladder_honors_none() {
        assert_eq!(Effort::None.clamp_to(QWEN_MIXED), Effort::None);
    }

    #[test]
    fn baseline_ids_are_unique() {
        let mut ids: Vec<_> = MODELS.iter().map(|m| m.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate baseline ids in qianwen");
    }

    /// Only Qianwen-native ids are declared in the baseline table. The
    /// third-party rows (GLM / DeepSeek / Kimi) stay owned by their home
    /// providers and are served here through the live catalog — a duplicate
    /// baseline would have to be field-identical and the cross-vendor tier
    /// sets genuinely differ.
    #[test]
    fn the_table_declares_only_qianwen_native_ids() {
        for model in MODELS {
            assert!(
                model.id.starts_with("qwen3."),
                "non-native id {} must not be re-declared",
                model.id
            );
        }
    }
}
