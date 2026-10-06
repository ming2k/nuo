//! OpenAI-compatible provider registry and the `Channel` → concrete `Provider`
//! factory consumed by the orchestration layer.
//!
//! The registry is split one file per provider: each module holds the
//! provider's model constants and its [`ModelProviderSpec`] entry.
//! This file keeps the shared types, the aggregate tables, and the factory.

use nuo_model_codec::Provider;
use nuo_model_codec::catalog::{Channel, Transport};
use std::borrow::Cow;
use std::sync::Arc;

use crate::{
    AnthropicMessagesProvider, CatalogShape, GoogleProvider, OpenAiChatCompletionsProvider,
    OpenAiResponsesProvider, ThinkingConfig,
};

mod anthropic;
mod antigravity_oauth;
mod chatgpt;
mod commandcode;
mod copilot;
mod custom_baselines;
mod deepseek;
pub mod effort_ladders;
mod google;
mod kimi;
mod openai;
pub(crate) mod opencode;
pub(crate) mod opencode_go;
pub(crate) mod opencode_zen;
mod openrouter;
/// Public: the Qoder dialect's wire implementation is exercised by nuo-llm-client's
/// golden-wire tests ([INV-WIRE-01], ADR-0271), which need the pipeline builder
/// and the codec's decode half.
pub use nuo_provider_qoder as qoder;
/// The QianwenAI Token Plan ladders are reused by its model-provider tests.
pub mod qianwen;
mod xai;
mod zai;

pub use anthropic::ANTHROPIC_BUILTIN_MODELS;
pub use antigravity_oauth::ANTIGRAVITY_OAUTH_MODELS;
pub use chatgpt::CHATGPT_BUILTIN_MODELS;
pub use commandcode::COMMANDCODE_BUILTIN_MODELS;
pub use copilot::COPILOT_SEED_MODELS;
pub use deepseek::DEEPSEEK_BUILTIN_MODELS;
pub use google::GOOGLE_BUILTIN_MODELS;
pub use kimi::KIMI_CODE_MODELS;
pub use openai::OPENAI_BUILTIN_MODELS;
pub use opencode::OPENCODE_CONSOLE_MODELS;
pub use opencode_go::OPENCODE_GO_MODELS;
pub use opencode_zen::OPENCODE_ZEN_MODELS;
pub use openrouter::OPENROUTER_BUILTIN_MODELS;
pub use xai::XAI_BUILTIN_MODELS;
pub use zai::ZAI_CODE_MODELS;

pub use qoder::{QoderCatalogSigning, build_catalog_signer, catalog_root_for_connection};

use anthropic::anthropic_model_max_tokens;

// ═════════════════════════════════════════════════════════════════════════════
// OpenAI-compatible provider wrappers for popular Chinese & global services
// ═════════════════════════════════════════════════════════════════════════════

pub use nuo_model_codec::RemoteCatalogSource;

// ═════════════════════════════════════════════════════════════════════════════
// Model providers — the definition of one upstream service surface
// ═════════════════════════════════════════════════════════════════════════════

/// The reconciliation-relevant definition of one model provider.
///
/// A connection records its provider's stable [`id`](ModelProviderSpec::id).
/// At startup the catalog uses the provider protocol and baseline `models` to
/// reconcile the connection. This struct is the source of truth for that
/// mapping; it intentionally lives in `nuo-provider-adapters` (where the model
/// constants live) so the reconciliation layer in `nuo-agent` and the UI in
/// `nuo` both read one table. The UI-only fields (label / description /
pub use nuo_provider::spec::{
    ModelProviderSpec, PromptCachePolicy, unsupported_prompt_cache,
};

/// The single registry of model providers.
///
/// Each entry's `id` MUST be unique. The set is the source of truth shared by
/// the add-connection UI and the catalog's model reconciliation — the
/// `provider` recorded on a connection resolves back to its entry here. Each
/// entry lives beside its provider's model constants in the per-provider
/// modules.
pub const MODEL_PROVIDER_SPECS: &[ModelProviderSpec] = &[
    openai::MODEL_PROVIDER_SPEC,
    openrouter::MODEL_PROVIDER_SPEC,
    commandcode::MODEL_PROVIDER_SPEC,
    anthropic::MODEL_PROVIDER_SPEC,
    google::MODEL_PROVIDER_SPEC,
    deepseek::MODEL_PROVIDER_SPEC,
    xai::MODEL_PROVIDER_SPEC,
    chatgpt::MODEL_PROVIDER_SPEC,
    copilot::MODEL_PROVIDER_SPEC,
    kimi::MODEL_PROVIDER_SPEC,
    zai::MODEL_PROVIDER_SPEC,
    qianwen::MODEL_PROVIDER_SPEC,
    qoder::MODEL_PROVIDER_SPEC,
    opencode::MODEL_PROVIDER_SPEC,
    opencode_go::MODEL_PROVIDER_SPEC,
    opencode_zen::MODEL_PROVIDER_SPEC,
    antigravity_oauth::MODEL_PROVIDER_SPEC,
];

static USER_DECLARED_SPECS: std::sync::RwLock<
    std::collections::BTreeMap<String, Arc<ModelProviderSpec>>,
> = std::sync::RwLock::new(std::collections::BTreeMap::new());

pub fn user_declared_provider_spec(id: &str) -> Option<Arc<ModelProviderSpec>> {
    USER_DECLARED_SPECS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .cloned()
}

pub fn register_user_declared_provider(
    spec: ModelProviderSpec,
) -> Result<Arc<ModelProviderSpec>, String> {
    if MODEL_PROVIDER_SPECS
        .iter()
        .any(|builtin| builtin.id == spec.id)
    {
        return Err(format!(
            "provider `{}` collides with a built-in provider",
            spec.id
        ));
    }
    spec.validate()?;
    let spec = Arc::new(spec);
    USER_DECLARED_SPECS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(spec.id.to_string(), spec.clone());
    Ok(spec)
}

/// Replace the dynamic snapshot atomically; removed entries are released once readers finish.
///
/// The declarations arrive as a value: reading `model_providers.toml` is the
/// host's job, this crate only serves the registry.
/// Invalid input preserves the last valid snapshot and propagates the error.
pub fn sync_user_declared_providers(
    declared: &impl AsRef<std::collections::BTreeMap<String, nuo_model_codec::model_providers::UserDeclaredProvider>>,
) -> Result<(), String> {
    let mut next = std::collections::BTreeMap::new();
    for (id, provider) in declared.as_ref() {
        provider.validate(id)?;
        let wire = provider
            .default_protocol
            .unwrap_or(nuo_model_codec::WireProtocol::ChatCompletions);
        let dialect = provider.dialect.unwrap_or_default();
        let catalog_source = provider.catalog.clone().unwrap_or_else(|| {
            RemoteCatalogSource::Endpoint(match dialect {
                nuo_model_codec::ProviderDialect::Antigravity => CatalogShape::GoogleCloudCode,
                nuo_model_codec::ProviderDialect::ChatGpt => CatalogShape::Codex,
                _ => CatalogShape::from_wire_protocol(wire),
            })
        });
        let spec = ModelProviderSpec {
            id: Cow::Owned(id.clone()),
            baselines: &[],
            root_url: Cow::Owned(provider.root_url.clone()),
            user_agent: provider.user_agent.clone().map(Cow::Owned),
            protocol: wire,
            dialect,
            protocol_roots: Cow::Owned(
                provider
                    .protocol_roots
                    .iter()
                    .map(|(wire, root)| (*wire, Cow::Owned(root.clone())))
                    .collect(),
            ),
            catalog_root_url: provider.catalog_root_url.clone().map(Cow::Owned),
            models: &[],
            catalog_source,
            default_client_profile: provider
                .client_profile
                .unwrap_or(nuo_model_codec::ClientPreset::Native),
            client_profile_sensitive: provider.client_profile_sensitive,
            prompt_cache: PromptCachePolicy::Declared(provider.prompt_cache.clone().unwrap_or_default()),
        };
        spec.validate()?;
        next.insert(id.clone(), Arc::new(spec));
    }
    *USER_DECLARED_SPECS
        .write()
        .unwrap_or_else(|e| e.into_inner()) = next;
    Ok(())
}

/// The spec for a provider id: built-in first, then whatever the host last
/// declared.
///
/// A pure lookup. It reads no file and refreshes nothing: the host populates the
/// declared-provider registry through
/// [`sync_user_declared_providers`] (the daemon does so at startup and again
/// whenever it writes a declaration), and a lookup that silently did I/O would
/// make a derivation's result depend on when it happened to run.
pub fn model_provider_spec(id: &str) -> Option<Arc<ModelProviderSpec>> {
    let canonical = nuo_model_codec::model_providers::canonical_provider_id(id)
        .unwrap_or_else(|| id.to_string());
    if let Some(spec) = MODEL_PROVIDER_SPECS
        .iter()
        .find(|spec| spec.id == id || spec.id == canonical.as_str())
    {
        return Some(Arc::new(spec.clone()));
    }
    user_declared_provider_spec(id).or_else(|| user_declared_provider_spec(&canonical))
}

/// Resolve the transport endpoint for **one model** of a model provider — the
/// route the catalog materializes at runtime (routes are derived, never
/// persisted).
///
/// Returns `(protocol, base_url, user_agent)` where `protocol` is one of the
/// wire-protocol labels `"chat-completions"` / `"responses"` / `"anthropic-messages"` /
/// `"google-gemini"`. Most providers serve every model over one endpoint; the
/// `opencode-go` Console surface routes models across several inference roots
/// (OpenAI chat / Responses / Anthropic `/messages` / Google `/v1beta`), so its
/// base URL and protocol vary per model. `None` means the provider id is
/// unknown.
pub fn route_for_model(
    provider_id: &str,
    model_id: &str,
) -> Option<(
    nuo_model_codec::WireProtocol,
    String,
    Option<Cow<'static, str>>,
)> {
    let spec = model_provider_spec(provider_id)?;
    let protocol = spec.model_protocol(model_id);
    Some((
        protocol,
        spec.endpoint(protocol).ok()?,
        spec.user_agent.clone(),
    ))
}

pub use nuo_provider::spec::endpoint_for;

/// Construct the concrete `Provider` for a [`nuo_model_codec::catalog::Channel`].
///
/// This is the construction layer that knows about every concrete `Provider`
/// implementation; it lives in `nuo-provider-adapters` (not `nuo-wire`) so the
/// domain crate stays free of HTTP I/O. `entry_id` becomes the provider's
/// attribution id (`Provider::provider_id`) so assistant responses are
/// attributed to the logical model even after a mid-session switch.
///
/// `session_id` is offered as a routing key only when this exact channel
/// declares routing-key support. A protocol or model-family resemblance never
/// grants that capability to a relay.
pub fn build_provider_for_channel(
    channel: &Channel,
    entry_id: &str,
    session_id: Option<&str>,
) -> Arc<dyn Provider> {
    let credentials = channel.credentials_source();
    let prompt_cache = crate::PromptCacheConfig::new(
        channel.prompt_cache.clone(),
        channel.prompt_cache_preference,
        session_id.map(str::to_string),
    );
    match &channel.transport {
        Transport::Google {
            base_url,
            client_profile,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            let mut provider = GoogleProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                client_profile.clone(),
            )
            .with_reasoning_effort(*effort)
            .with_model_capabilities(capabilities)
            .with_prompt_cache(prompt_cache)
            .with_dialect(*dialect)
            .with_id(entry_id.to_string());
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            Arc::new(provider)
        }
        Transport::Anthropic {
            base_url,
            client_profile,
            effort,
            thinking,
            dialect,
        } => {
            let mut provider = AnthropicMessagesProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                client_profile.clone(),
            )
            .with_id(entry_id.to_string());
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            // Cap the response length at the model's registered output limit so
            // high-output models (MiniMax M3) are not truncated by the default.
            let capabilities = channel.capabilities();
            if let Some(max_tokens) = capabilities
                .max_output_tokens
                .or_else(|| anthropic_model_max_tokens(&channel.model))
            {
                provider = provider.with_max_tokens(max_tokens);
            }
            // Apply the two reasoning knobs INDEPENDENTLY. effort (depth) and
            // thinking (on/off) are orthogonal on the wire, so we never couple
            // them: setting effort must not implicitly turn thinking on, and an
            // explicit thinking override must not change effort. Each is an
            // optional override layered onto the model-derived default
            // (`for_model`: thinking **off** unless the user opts in — ADR-0046);
            // anything unset keeps that default. Effort is clamped to the
            // model's registered levels at request-build time.
            let mut cfg =
                ThinkingConfig::for_model(&nuo_model_codec::model::resolve(&channel.model));
            if let Some(mode) = thinking {
                cfg = cfg.with_mode(*mode);
            }
            if let Some(effort) = effort {
                cfg = cfg.with_effort(*effort);
            }
            provider = provider
                .with_thinking(cfg)
                .with_model_capabilities(capabilities)
                .with_prompt_cache(prompt_cache)
                .with_dialect(*dialect);
            Arc::new(provider)
        }
        Transport::OpenAi {
            base_url,
            client_profile,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            // For OpenAI-family transports the effort knob IS the reasoning
            // control: a model that advertises a ladder (GLM-5.x — always-on
            // thinking gated only by `reasoning_effort`) must never send an
            // absent effort, or the endpoint falls back to its server-side
            // default, which can reason far deeper than the tier the picker
            // displays. Default the wire to the same `Effort::channel_default`
            // the picker shows (GPT→medium, others→high clamped to the
            // ladder); an explicit channel override still wins.
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            let (catalog_source, display_name) = channel.catalog_provenance();
            let mut provider = OpenAiChatCompletionsProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                client_profile.clone(),
            )
            .with_reasoning_effort(effective_effort)
            .with_prompt_cache(prompt_cache)
            .with_model_capabilities(capabilities)
            .with_dialect(*dialect)
            .with_catalog_provenance(catalog_source, display_name)
            .with_id(entry_id.to_string());
            if *dialect == nuo_model_codec::OpenAiChatDialect::Qoder {
                provider = provider.with_pipeline(qoder::build_qoder_pipeline());
            }
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            Arc::new(provider)
        }
        Transport::OpenAiResponses {
            base_url,
            client_profile,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            // Same wire-level default as the chat-completions arm above —
            // see the comment there.
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            let mut provider = OpenAiResponsesProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
            )
            .with_client_profile(client_profile.clone())
            .with_reasoning_effort(effective_effort)
            .with_model_capabilities(capabilities)
            .with_prompt_cache(prompt_cache)
            .with_dialect(*dialect)
            .with_id(entry_id.to_string());
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            Arc::new(provider)
        }
    }
}

/// Resolve the effort a channel's request actually carries: the explicit
/// override when set, otherwise the shared [`nuo_model_codec::Effort::channel_default`]
/// for a model that advertises an effort ladder. Used by the OpenAI-family
/// factory arms so the wire can never omit the reasoning control the picker
/// already promises (`None` remains `None` for ladder-less models — no
/// `reasoning_effort` field is stamped for them at request-build time).
fn effective_channel_effort(
    override_effort: Option<nuo_model_codec::Effort>,
    capabilities: &nuo_model_codec::ModelCapabilities,
) -> Option<nuo_model_codec::Effort> {
    override_effort.or_else(|| {
        let known: Vec<nuo_model_codec::Effort> = capabilities
            .effort_levels
            .iter()
            .filter_map(nuo_model_codec::EffortLevel::as_known)
            .collect();
        nuo_model_codec::Effort::channel_default(&capabilities.family, &known)
    })
}

#[cfg(test)]
mod baseline_fidelity_tests;

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn model_provider_specs_have_unique_nonempty_ids() {
        // Provider ids are the durable join key between a connection and its
        // model provider, so they must be unique and non-empty.
        let mut ids: Vec<&str> = MODEL_PROVIDER_SPECS
            .iter()
            .map(|spec| spec.id.as_ref())
            .collect();
        ids.sort_unstable();
        assert!(
            ids.iter().all(|id| !id.is_empty()),
            "provider ids must be non-empty"
        );
        let dups: Vec<&[&str]> = ids.windows(2).filter(|pair| pair[0] == pair[1]).collect();
        assert!(dups.is_empty(), "duplicate provider ids: {dups:?}");
    }

    #[test]
    fn model_provider_spec_resolves_each_known_id() {
        // The reconciliation layer resolves a connection's provider back to a
        // spec here; every id in the table must round-trip.
        for spec in MODEL_PROVIDER_SPECS {
            let resolved = model_provider_spec(&spec.id).expect("id resolves");
            assert_eq!(resolved.id, spec.id);
        }
        // Unknown ids resolve to None; the loader rejects them (ADR-0201 INV-2).
        assert!(model_provider_spec("does-not-exist").is_none());
    }

    #[test]
    fn user_declared_provider_can_be_registered_and_resolved() {
        let test_id = "test-corp-relay";
        let custom_spec = ModelProviderSpec {
            dialect: nuo_model_codec::ProviderDialect::Standard,
            protocol_roots: Cow::Borrowed(&[]),
            catalog_root_url: None,
            prompt_cache: PromptCachePolicy::Compiled(unsupported_prompt_cache),
            id: Cow::Borrowed(test_id),
            baselines: &[],
            root_url: Cow::Borrowed("https://relay.example.com/v1"),
            user_agent: None,
            protocol: nuo_model_codec::WireProtocol::ChatCompletions,
            models: &[],
            catalog_source: RemoteCatalogSource::None,
            default_client_profile: nuo_model_codec::ClientPreset::Cursor,
            client_profile_sensitive: false,
        };
        register_user_declared_provider(custom_spec).unwrap();
        let resolved = model_provider_spec(test_id).expect("dynamic spec resolves");
        assert_eq!(resolved.id, test_id);
        assert_eq!(resolved.root_url, "https://relay.example.com/v1");
        assert_eq!(
            resolved.default_client_profile,
            nuo_model_codec::ClientPreset::Cursor
        );
    }

    #[test]
    fn registry_covers_the_contract_provider_id_vocabulary() {
        // The persisted provider vocabulary is contract data (ADR-0201); the
        // registry must cover it exactly, or a stored connection could name a
        // provider this build cannot drive.
        let mut registry: Vec<&str> = MODEL_PROVIDER_SPECS
            .iter()
            .map(|spec| spec.id.as_ref())
            .collect();
        let mut contract: Vec<&str> = nuo_model_codec::model_providers::MODEL_PROVIDER_IDS.to_vec();
        registry.sort_unstable();
        contract.sort_unstable();
        assert_eq!(registry, contract);
    }

    #[test]
    fn shared_baseline_ids_are_identical_across_provider_tables() {
        // `resolve_model` (baseline_models().find) returns the first table that
        // declares an id, so when several provider files carry the same id
        // (zai/opencode-go both list glm-5.2; kimi/opencode-go both list
        // kimi-k2.7-code) the copies MUST be field-identical -- otherwise which
        // copy wins depends on link order, an invisible behavior change.
        // Protocol is intentionally excluded: it is provider-scoped (ADR-0260),
        // so the same id legitimately speaks different wires on different
        // service surfaces (deepseek Responses vs. opencode-go ChatCompletions).
        // Provider routing resolves protocol through `ModelProviderSpec`, never
        // through this global capability union.
        // `Model` is not `PartialEq`, so compare a derived signature instead.
        use std::collections::HashMap;

        fn signature(m: &nuo_model_codec::Model) -> String {
            format!(
                "{:?}|{:?}|{}|{}|{:?}|{:?}",
                m.context_window,
                m.thinking,
                m.tool_call,
                m.vision,
                m.model_guidance,
                m.effort_levels,
            )
        }

        let mut seen: HashMap<&str, (&str, String)> = HashMap::new();
        for spec in MODEL_PROVIDER_SPECS {
            for m in spec.baselines {
                let sig = signature(m);
                if let Some((first_provider, first_sig)) =
                    seen.insert(m.id, (spec.id.as_ref(), sig))
                {
                    assert_eq!(
                        first_sig,
                        seen[&m.id].1,
                        "{id}: baseline declared by {first_provider} and {} disagree \
                         (context_window/thinking/tool_call/vision/format/guidance/effort)",
                        spec.id,
                        id = m.id
                    );
                }
            }
        }
    }

    #[test]
    fn provider_models_are_covered_by_the_local_baseline_table() {
        // Every id a provider seeds must have baseline metadata in the same
        // provider file's local table — that table is what the reconciliation
        // layer intersects the live catalog against.
        for spec in MODEL_PROVIDER_SPECS {
            let baseline_ids: std::collections::HashSet<&str> =
                spec.baselines.iter().map(|m| m.id).collect();
            for id in spec.models {
                assert!(
                    baseline_ids.contains(id),
                    "{} preset model {id} has no entry in the local baseline table",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn provider_dialect_protocol_baselines_are_service_scoped() {
        let google = model_provider_spec("google-antigravity").unwrap();
        // Another provider's known model ID cannot inject a protocol here.
        assert_eq!(
            google.model_protocol("claude-sonnet-4-6"),
            nuo_model_codec::WireProtocol::GoogleGemini
        );
        for (id, model) in [
            ("deepseek", "deepseek-v4-pro"),
            ("openai-subscription", "gpt-5.6-sol"),
        ] {
            let spec = model_provider_spec(id).unwrap();
            assert_eq!(
                spec.model_protocol(model),
                nuo_model_codec::WireProtocol::Responses
            );
        }
        for spec in MODEL_PROVIDER_SPECS {
            assert!(spec.dialect.supports(spec.protocol), "{} default", spec.id);
            for model in spec.baselines {
                assert!(
                    spec.dialect.supports(spec.model_protocol(model.id)),
                    "{}/{}",
                    spec.id,
                    model.id
                );
            }
        }
    }

    #[test]
    fn opencode_baselines_route_by_console_surface() {
        let (wire, endpoint, _) = route_for_model("opencode", "deepseek-v4-flash").unwrap();
        assert_eq!(wire, nuo_model_codec::WireProtocol::ChatCompletions);
        assert_eq!(
            endpoint,
            "https://opencode.ai/inference/openai/v1/chat/completions"
        );
    }

    #[test]
    fn opencode_go_baselines_route_by_zen_go_surface() {
        let (wire, endpoint, _) = route_for_model("opencode-go", "minimax-m3").unwrap();
        assert_eq!(wire, nuo_model_codec::WireProtocol::AnthropicMessages);
        assert_eq!(
            endpoint,
            "https://opencode.ai/zen/go/v1/messages"
        );
        let (wire, endpoint, _) = route_for_model("opencode-go", "qwen3.6-plus").unwrap();
        assert_eq!(wire, nuo_model_codec::WireProtocol::AnthropicMessages);
        assert_eq!(
            endpoint,
            "https://opencode.ai/zen/go/v1/messages"
        );
        let (wire, endpoint, _) = route_for_model("opencode-go", "glm-5.2").unwrap();
        assert_eq!(wire, nuo_model_codec::WireProtocol::ChatCompletions);
        assert_eq!(
            endpoint,
            "https://opencode.ai/zen/go/v1/chat/completions"
        );
    }

    #[test]
    fn commandcode_baselines_route_by_surface() {
        let (wire, endpoint, _) = route_for_model("commandcode", "claude-sonnet-5-5").unwrap();
        assert_eq!(wire, nuo_model_codec::WireProtocol::AnthropicMessages);
        assert_eq!(endpoint, "https://api.commandcode.ai/provider/v1/messages");
        let (wire, endpoint, _) =
            route_for_model("commandcode", "deepseek/deepseek-v4-flash").unwrap();
        assert_eq!(wire, nuo_model_codec::WireProtocol::ChatCompletions);
        assert_eq!(
            endpoint,
            "https://api.commandcode.ai/provider/v1/chat/completions"
        );
    }

    #[test]
    fn commandcode_deepseek_siblings_have_baselines_and_effort_ladders() {
        // The `vendor/model` DeepSeek siblings are registered baselines, not
        // fallbacks: a missing entry would resolve to `fallback_model`, whose
        // empty `effort_levels` collapses the picker's effort slider. Each must
        // route over chat-completions and expose the DeepSeek effort ladder.
        for id in [
            "deepseek/deepseek-v4-flash",
            "deepseek/deepseek-v4.1-flash",
            "deepseek/deepseek-v4-pro",
        ] {
            let baseline = nuo_model_codec::model::model_by_id(id)
                .unwrap_or_else(|| panic!("{id} is not a registered baseline"));
            assert_eq!(
                baseline.family, "deepseek",
                "{id}: family must group under deepseek"
            );
            assert_eq!(
                baseline.effort_levels,
                effort_ladders::LOW_HIGH_MAX,
                "{id}: the effort ladder is what makes depth adjustable"
            );
            let (wire, endpoint, _) =
                route_for_model("commandcode-plan", id).unwrap_or_else(|| panic!("{id} routes"));
            assert_eq!(wire, nuo_model_codec::WireProtocol::ChatCompletions);
            assert_eq!(
                endpoint,
                "https://api.commandcode.ai/provider/v1/chat/completions"
            );
        }
    }

    #[test]
    fn opencode_zen_baselines_route_by_zen_surface() {
        let (wire, endpoint, _) = route_for_model("opencode-zen", "claude-sonnet-4-6").unwrap();
        assert_eq!(wire, nuo_model_codec::WireProtocol::AnthropicMessages);
        assert_eq!(endpoint, "https://opencode.ai/zen/v1/messages");
        let (wire, endpoint, _) = route_for_model("opencode-zen", "glm-5.2").unwrap();
        assert_eq!(wire, nuo_model_codec::WireProtocol::ChatCompletions);
        assert_eq!(endpoint, "https://opencode.ai/zen/v1/chat/completions");
    }

    #[test]
    fn endpoint_for_applies_root_algebra_per_protocol() {
        // The catalog-advertised root override shares one algebra with the
        // compiled spec roots (ADR-0259 + ADR-0269).
        use nuo_model_codec::{ApiRoot, ProviderDialect, WireProtocol};
        let root = ApiRoot::parse("https://opencode.ai/inference/anthropic/v1").unwrap();
        assert_eq!(
            endpoint_for(
                ProviderDialect::Standard,
                &root,
                WireProtocol::AnthropicMessages
            ),
            "https://opencode.ai/inference/anthropic/v1/messages"
        );
        let google_root = ApiRoot::parse("https://opencode.ai/inference/google/v1beta").unwrap();
        assert_eq!(
            endpoint_for(
                ProviderDialect::Standard,
                &google_root,
                WireProtocol::GoogleGemini
            ),
            "https://opencode.ai/inference/google/v1beta"
        );
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;

    #[test]
    fn build_provider_stamps_entry_id_on_openai_compat() {
        let channel = Channel {
            id: "default".to_string(),
            label: "OpenAI".to_string(),
            transport: Transport::OpenAi {
                base_url: "https://api.openai.com/v1/chat/completions".to_string(),
                client_profile: nuo_model_codec::ClientProfile::from("agent"),
                effort: None,
                dialect: Default::default(),
            },
            credentials: nuo_model_codec::static_credential("k"),
            model: "gpt-4o".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
            prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
        };
        let provider = build_provider_for_channel(&channel, "openai", None);
        assert_eq!(provider.provider_id(), "openai");
        assert_eq!(provider.model(), "gpt-4o");
    }

    #[test]
    fn openai_channel_without_override_defaults_effort_on_the_wire() {
        // GLM-5.3 advertises a ladder (low/high/xhigh/max) and its endpoint
        // runs always-on thinking gated only by `reasoning_effort`. A channel
        // with no explicit override must default to `high` — the same tier
        // the picker displays — instead of omitting the field and eating the
        // server's (much deeper) default.
        let channel = Channel {
            id: "default".to_string(),
            label: "ZAI Code".to_string(),
            transport: Transport::OpenAi {
                base_url: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"
                    .to_string(),
                client_profile: nuo_model_codec::ClientProfile::from("agent"),
                effort: None,
                dialect: Default::default(),
            },
            credentials: nuo_model_codec::static_credential("k"),
            model: "glm-5.3".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
            prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
        };
        let provider = build_provider_for_channel(&channel, "zai-code", None);
        assert_eq!(provider.effort(), Some(nuo_model_codec::Effort::High));

        // An explicit override still wins verbatim.
        let mut pinned = channel;
        if let Transport::OpenAi { effort, .. } = &mut pinned.transport {
            *effort = Some(nuo_model_codec::Effort::Low);
        }
        let provider = build_provider_for_channel(&pinned, "zai-code", None);
        assert_eq!(provider.effort(), Some(nuo_model_codec::Effort::Low));
    }

    #[test]
    fn ladderless_model_keeps_absent_effort_on_the_wire() {
        // A model with NO effort ladder must keep `None`: stamping a
        // `reasoning_effort` the endpoint never advertised would be noise at
        // best and a 400 at worst.
        let channel = Channel {
            id: "default".to_string(),
            label: "OpenAI".to_string(),
            transport: Transport::OpenAi {
                base_url: "https://api.openai.com/v1/chat/completions".to_string(),
                client_profile: nuo_model_codec::ClientProfile::from("agent"),
                effort: None,
                dialect: Default::default(),
            },
            credentials: nuo_model_codec::static_credential("k"),
            model: "gpt-4o".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
            prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
        };
        let provider = build_provider_for_channel(&channel, "openai", None);
        assert_eq!(provider.effort(), None);
    }

    #[test]
    fn build_provider_dispatches_anthropic_transport() {
        // opencode-go's Claude/Qwen models reach an Anthropic /messages
        // endpoint; the catalog builds an Anthropic transport for them, and
        // build_provider_for_channel must dispatch it to the messages provider.
        let channel = Channel {
            id: "qwen3.6-plus".to_string(),
            label: "Qwen3.6 Plus".to_string(),
            transport: Transport::Anthropic {
                base_url: "https://opencode.ai/inference/anthropic/v1/messages".to_string(),
                client_profile: nuo_model_codec::ClientProfile::from("agent"),
                effort: None,
                thinking: None,
                dialect: Default::default(),
            },
            credentials: nuo_model_codec::static_credential("go-key"),
            model: "qwen3.6-plus".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
            prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
        };
        let provider = build_provider_for_channel(&channel, "opencode-go", None);
        assert_eq!(provider.provider_id(), "opencode-go");
        assert_eq!(provider.model(), "qwen3.6-plus");
    }

    #[test]
    fn builtin_provider_models_resolve_with_expected_wire_formats() {
        use nuo_model_codec::WireProtocol;
        // Every model a multi-model built-in serves must exist in the model
        // registry (so metadata resolves) and carry the wire format its own
        // provider speaks. ADR-0260: protocol selection is provider-scoped, so
        // the same id may legitimately resolve to different protocols on
        // different service surfaces.
        let cases: &[(&str, &[&str], WireProtocol)] = &[
            (
                "anthropic",
                crate::ANTHROPIC_BUILTIN_MODELS,
                WireProtocol::AnthropicMessages,
            ),
            (
                "google",
                crate::GOOGLE_BUILTIN_MODELS,
                WireProtocol::GoogleGemini,
            ),
            (
                "deepseek",
                crate::DEEPSEEK_BUILTIN_MODELS,
                WireProtocol::Responses,
            ),
            (
                "openai",
                crate::OPENAI_BUILTIN_MODELS,
                WireProtocol::ChatCompletions,
            ),
            (
                "openrouter",
                crate::OPENROUTER_BUILTIN_MODELS,
                WireProtocol::ChatCompletions,
            ),
            (
                "xai",
                crate::XAI_BUILTIN_MODELS,
                WireProtocol::ChatCompletions,
            ),
            (
                "openai-subscription",
                crate::CHATGPT_BUILTIN_MODELS,
                WireProtocol::Responses,
            ),
            (
                "github-copilot",
                crate::COPILOT_SEED_MODELS,
                WireProtocol::ChatCompletions,
            ),
            (
                "kimi-code",
                crate::KIMI_CODE_MODELS,
                WireProtocol::ChatCompletions,
            ),
            (
                "glm-cn",
                crate::ZAI_CODE_MODELS,
                WireProtocol::ChatCompletions,
            ),
            (
                "opencode-go",
                crate::OPENCODE_GO_MODELS,
                WireProtocol::ChatCompletions,
            ),
            (
                "google-antigravity",
                crate::ANTIGRAVITY_OAUTH_MODELS,
                WireProtocol::GoogleGemini,
            ),
        ];
        for (provider_id, ids, expected) in cases {
            let spec = model_provider_spec(provider_id).expect("provider resolves");
            for id in ids.iter() {
                let model = nuo_model_codec::model::resolve(id);
                assert_eq!(model.id, *id, "model {id} must be registered");
                assert_eq!(
                    spec.model_protocol(id),
                    *expected,
                    "{provider_id}/{id} wire format"
                );
            }
        }
    }
}
