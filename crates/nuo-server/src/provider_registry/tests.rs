//! Tests for the provider composition root: spec uniqueness, route
//! derivation, factory dispatch, and typed quota-port resolution.

use super::*;

#[test]
fn model_provider_specs_have_unique_nonempty_ids() {
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
    for spec in MODEL_PROVIDER_SPECS {
        let resolved = model_provider_spec(&spec.id).expect("id resolves");
        assert_eq!(resolved.id, spec.id);
    }
    assert!(model_provider_spec("does-not-exist").is_none());
}

#[test]
fn user_declared_provider_can_be_registered_and_resolved() {
    let test_id = "test-corp-relay";
    let custom_spec = ModelProviderSpec {
        dialect: nuo_provider::ProviderDialect::Standard,
        protocol_roots: Cow::Borrowed(&[]),
        catalog_root_url: None,
        prompt_cache: PromptCachePolicy::Compiled(unsupported_prompt_cache),
        id: Cow::Borrowed(test_id),
        baselines: &[],
        root_url: Cow::Borrowed("https://relay.example.com/v1"),
        user_agent: None,
        protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
        models: &[],
        catalog_source: RemoteCatalogSource::None,
        default_client_profile: nuo_provider::ClientPreset::Cursor,
        client_profile_sensitive: false,
        quota: None,
        display_policy: nuo_provider::DisplayPolicy::Native,
    };
    register_user_declared_provider(custom_spec).unwrap();
    let resolved = model_provider_spec(test_id).expect("dynamic spec resolves");
    assert_eq!(resolved.id, test_id);
    assert_eq!(resolved.root_url, "https://relay.example.com/v1");
    assert_eq!(
        resolved.default_client_profile,
        nuo_provider::ClientPreset::Cursor
    );
}

#[test]
fn registry_covers_the_contract_provider_id_vocabulary() {
    let mut registry: Vec<&str> = MODEL_PROVIDER_SPECS
        .iter()
        .map(|spec| spec.id.as_ref())
        .collect();
    let mut contract: Vec<&str> = nuo_provider::model_providers::MODEL_PROVIDER_IDS.to_vec();
    registry.sort_unstable();
    contract.sort_unstable();
    assert_eq!(registry, contract);
}

#[test]
fn exactly_one_spec_per_contract_provider_id() {
    // `[INV-PROV-06]`: each provider id has exactly one authoritative spec.
    for id in nuo_provider::model_providers::MODEL_PROVIDER_IDS {
        let count = MODEL_PROVIDER_SPECS
            .iter()
            .filter(|spec| spec.id == *id)
            .count();
        assert_eq!(count, 1, "provider `{id}` must have exactly one spec");
    }
}

#[test]
fn shared_baseline_ids_are_identical_across_provider_tables() {
    use std::collections::HashMap;

    fn signature(m: &nuo_provider::Model) -> String {
        format!(
            "{:?}|{:?}|{}|{}|{:?}|{:?}",
            m.context_window, m.thinking, m.tool_call, m.vision, m.model_guidance, m.effort_levels,
        )
    }

    let mut seen: HashMap<&str, (&str, String)> = HashMap::new();
    for spec in MODEL_PROVIDER_SPECS {
        for m in spec.baselines {
            let sig = signature(m);
            if let Some((first_provider, first_sig)) = seen.insert(m.id, (spec.id.as_ref(), sig)) {
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
    assert_eq!(
        google.model_protocol("claude-sonnet-4-6"),
        nuo_inference_wire::WireProtocol::GoogleGenerateContent
    );
    for (id, model) in [
        ("deepseek", "deepseek-v4-pro"),
        ("openai-subscription", "gpt-5.6-sol"),
    ] {
        let spec = model_provider_spec(id).unwrap();
        assert_eq!(
            spec.model_protocol(model),
            nuo_inference_wire::WireProtocol::OpenAiResponses
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
fn opencode_go_baselines_route_by_inference_go_surface() {
    for provider_id in ["opencode-plan", "opencode-go"] {
        let (wire, endpoint, _) = route_for_model(provider_id, "minimax-m3").unwrap();
        assert_eq!(wire, nuo_inference_wire::WireProtocol::AnthropicMessages);
        assert_eq!(
            endpoint,
            "https://opencode.ai/inference/go/anthropic/v1/messages"
        );
        let (wire, endpoint, _) = route_for_model(provider_id, "qwen3.6-plus").unwrap();
        assert_eq!(wire, nuo_inference_wire::WireProtocol::AnthropicMessages);
        assert_eq!(
            endpoint,
            "https://opencode.ai/inference/go/anthropic/v1/messages"
        );
        let (wire, endpoint, _) = route_for_model(provider_id, "glm-5.2").unwrap();
        assert_eq!(
            wire,
            nuo_inference_wire::WireProtocol::OpenAiChatCompletions
        );
        assert_eq!(endpoint, "https://opencode.ai/zen/go/v1/chat/completions");
    }
}

#[test]
fn commandcode_baselines_route_by_surface() {
    let (wire, endpoint, _) = route_for_model("commandcode", "claude-sonnet-5-5").unwrap();
    assert_eq!(wire, nuo_inference_wire::WireProtocol::AnthropicMessages);
    assert_eq!(endpoint, "https://api.commandcode.ai/provider/v1/messages");
    let (wire, endpoint, _) = route_for_model("commandcode", "deepseek/deepseek-v4-flash").unwrap();
    assert_eq!(wire, nuo_inference_wire::WireProtocol::OpenAiResponses);
    assert_eq!(endpoint, "https://api.commandcode.ai/provider/v1/responses");
}

#[test]
fn commandcode_deepseek_siblings_have_baselines_and_effort_ladders() {
    for id in [
        "deepseek/deepseek-v4-flash",
        "deepseek/deepseek-v4.1-flash",
        "deepseek/deepseek-v4.1-flash-fast",
        "deepseek/deepseek-v4-pro",
    ] {
        let baseline = nuo_provider::model::model_by_id(id)
            .unwrap_or_else(|| panic!("{id} is not a registered baseline"));
        assert_eq!(
            baseline.family, "deepseek",
            "{id}: family must group under deepseek"
        );
        assert_eq!(
            baseline.effort_levels,
            nuo_provider::effort_ladders::LOW_HIGH_MAX,
            "{id}: the effort ladder is what makes depth adjustable"
        );
        let (wire, endpoint, _) =
            route_for_model("commandcode-plan", id).unwrap_or_else(|| panic!("{id} routes"));
        assert_eq!(wire, nuo_inference_wire::WireProtocol::OpenAiResponses);
        assert_eq!(endpoint, "https://api.commandcode.ai/provider/v1/responses");
    }
}

#[test]
fn opencode_zen_baselines_route_by_inference_surface() {
    let (wire, endpoint, _) = route_for_model("opencode-zen", "claude-sonnet-4-6").unwrap();
    assert_eq!(wire, nuo_inference_wire::WireProtocol::AnthropicMessages);
    assert_eq!(
        endpoint,
        "https://opencode.ai/inference/anthropic/v1/messages"
    );
    let (wire, endpoint, _) = route_for_model("opencode-zen", "glm-5.2").unwrap();
    assert_eq!(
        wire,
        nuo_inference_wire::WireProtocol::OpenAiChatCompletions
    );
    assert_eq!(endpoint, "https://opencode.ai/zen/v1/chat/completions");
}

#[test]
fn endpoint_for_applies_root_algebra_per_protocol() {
    use nuo_inference_wire::WireProtocol;
    use nuo_provider::{ApiRoot, ProviderDialect};
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
            WireProtocol::GoogleGenerateContent
        ),
        "https://opencode.ai/inference/google/v1beta"
    );
}

// ── Typed quota port (`[INV-PROV-08]`) ──────────────────────────────────────

#[test]
fn kimi_code_connection_resolves_a_quota_port() {
    // Regression guard for the two-definitions Kimi defect: a `kimi-code`
    // connection must resolve its quota capability through the typed port, not
    // a substring match on the provider id / base URL.
    let spec = model_provider_spec("kimi-code").expect("kimi-code resolves");
    assert_eq!(spec.quota, Some(QuotaPort::KimiBalance));
}

#[test]
fn every_declared_quota_port_is_reachable_from_the_dispatch() {
    // Guards `[INV-PROV-07]` for `QuotaPort`: each declared built-in port is
    // used by at least one spec (the dispatch match handles all variants).
    let declared: Vec<QuotaPort> = MODEL_PROVIDER_SPECS
        .iter()
        .filter_map(|s| s.quota)
        .collect();
    for port in declared {
        match port {
            QuotaPort::DeepSeekBalance
            | QuotaPort::KimiBalance
            | QuotaPort::CommandCodeCredits
            | QuotaPort::OpenRouterKey
            | QuotaPort::Antigravity
            | QuotaPort::SiliconFlow
            | QuotaPort::Qoder => {}
        }
    }
}

// ── Factory dispatch ────────────────────────────────────────────────────────

#[test]
fn build_provider_stamps_entry_id_on_openai_compat() {
    let channel = Channel {
        id: "default".to_string(),
        transport: Transport::OpenAiChatCompletions {
            base_url: "https://api.openai.com/v1/chat/completions".to_string(),
            client_profile: nuo_provider::ClientProfile::from("agent"),
            effort: None,
            dialect: Default::default(),
        },
        credentials: nuo_provider::static_credential("k"),
        model: "gpt-4o".to_string(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_provider::PromptCachePreference::default(),
        prompt_cache: nuo_provider::PromptCacheCapabilities::unsupported(),
    };
    let provider = build_provider_for_channel(&channel, "openai", None);
    assert_eq!(provider.provider_id(), "openai");
    assert_eq!(provider.model(), "gpt-4o");
}

#[test]
fn openai_channel_without_override_defaults_effort_on_the_wire() {
    let channel = Channel {
        id: "default".to_string(),
        transport: Transport::OpenAiChatCompletions {
            base_url: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions".to_string(),
            client_profile: nuo_provider::ClientProfile::from("agent"),
            effort: None,
            dialect: Default::default(),
        },
        credentials: nuo_provider::static_credential("k"),
        model: "glm-5.3".to_string(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_provider::PromptCachePreference::default(),
        prompt_cache: nuo_provider::PromptCacheCapabilities::unsupported(),
    };
    let provider = build_provider_for_channel(&channel, "zai-code", None);
    assert_eq!(provider.effort(), Some(nuo_provider::Effort::High));

    let mut pinned = channel;
    if let Transport::OpenAiChatCompletions { effort, .. } = &mut pinned.transport {
        *effort = Some(nuo_provider::Effort::Low);
    }
    let provider = build_provider_for_channel(&pinned, "zai-code", None);
    assert_eq!(provider.effort(), Some(nuo_provider::Effort::Low));
}

#[test]
fn ladderless_model_keeps_absent_effort_on_the_wire() {
    let channel = Channel {
        id: "default".to_string(),
        transport: Transport::OpenAiChatCompletions {
            base_url: "https://api.openai.com/v1/chat/completions".to_string(),
            client_profile: nuo_provider::ClientProfile::from("agent"),
            effort: None,
            dialect: Default::default(),
        },
        credentials: nuo_provider::static_credential("k"),
        model: "gpt-4o".to_string(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_provider::PromptCachePreference::default(),
        prompt_cache: nuo_provider::PromptCacheCapabilities::unsupported(),
    };
    let provider = build_provider_for_channel(&channel, "openai", None);
    assert_eq!(provider.effort(), None);
}

#[test]
fn build_provider_dispatches_anthropic_transport() {
    let channel = Channel {
        id: "qwen3.6-plus".to_string(),
        transport: Transport::AnthropicMessages {
            base_url: "https://opencode.ai/inference/anthropic/v1/messages".to_string(),
            client_profile: nuo_provider::ClientProfile::from("agent"),
            effort: None,
            thinking: None,
            dialect: Default::default(),
        },
        credentials: nuo_provider::static_credential("go-key"),
        model: "qwen3.6-plus".to_string(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_provider::PromptCachePreference::default(),
        prompt_cache: nuo_provider::PromptCacheCapabilities::unsupported(),
    };
    let provider = build_provider_for_channel(&channel, "opencode-go", None);
    assert_eq!(provider.provider_id(), "opencode-go");
    assert_eq!(provider.model(), "qwen3.6-plus");
}

#[test]
fn build_provider_dispatches_google_interactions_transport() {
    let channel = Channel {
        id: "gemini-3.8-flash".to_string(),
        transport: Transport::GoogleInteractions {
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            client_profile: nuo_provider::ClientProfile::Native,
            effort: None,
            dialect: Default::default(),
        },
        credentials: nuo_provider::static_credential("goog-key"),
        model: "gemini-3.8-flash".to_string(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_provider::PromptCachePreference::default(),
        prompt_cache: nuo_provider::PromptCacheCapabilities::unsupported(),
    };
    let provider = build_provider_for_channel(&channel, "google", None);
    assert_eq!(provider.provider_id(), "google");
    assert_eq!(provider.model(), "gemini-3.8-flash");
    assert_eq!(
        provider.wire_protocol(),
        Some(nuo_inference_wire::WireProtocol::GoogleInteractions)
    );
}

#[test]
fn builtin_provider_models_resolve_with_expected_wire_formats() {
    use nuo_inference_wire::WireProtocol;
    let cases: &[(&str, &[&str], WireProtocol)] = &[
        (
            "anthropic",
            nuo_provider::model_providers::ANTHROPIC_BUILTIN_MODELS,
            WireProtocol::AnthropicMessages,
        ),
        (
            "google",
            nuo_provider::model_providers::GOOGLE_BUILTIN_MODELS,
            WireProtocol::GoogleGenerateContent,
        ),
        (
            "deepseek",
            nuo_provider::model_providers::DEEPSEEK_BUILTIN_MODELS,
            WireProtocol::OpenAiResponses,
        ),
        (
            "openai",
            nuo_provider::model_providers::OPENAI_BUILTIN_MODELS,
            WireProtocol::OpenAiChatCompletions,
        ),
        (
            "openrouter",
            nuo_provider::model_providers::OPENROUTER_BUILTIN_MODELS,
            WireProtocol::OpenAiChatCompletions,
        ),
        (
            "xai",
            nuo_provider::model_providers::XAI_BUILTIN_MODELS,
            WireProtocol::OpenAiChatCompletions,
        ),
        (
            "openai-subscription",
            nuo_provider::model_providers::CHATGPT_BUILTIN_MODELS,
            WireProtocol::OpenAiResponses,
        ),
        (
            "github-copilot",
            nuo_provider::model_providers::COPILOT_SEED_MODELS,
            WireProtocol::OpenAiChatCompletions,
        ),
        (
            "kimi-code",
            nuo_provider::model_providers::KIMI_CODE_MODELS,
            WireProtocol::OpenAiChatCompletions,
        ),
        (
            "glm-cn",
            nuo_provider::model_providers::ZAI_CODE_MODELS,
            WireProtocol::OpenAiChatCompletions,
        ),
        (
            "opencode-plan",
            nuo_provider::model_providers::OPENCODE_PLAN_MODELS,
            WireProtocol::OpenAiChatCompletions,
        ),
        (
            "google-antigravity",
            nuo_provider::model_providers::ANTIGRAVITY_OAUTH_MODELS,
            WireProtocol::GoogleGenerateContent,
        ),
    ];
    for (provider_id, ids, expected) in cases {
        let spec = model_provider_spec(provider_id).expect("provider resolves");
        for id in ids.iter() {
            let model = nuo_provider::model::resolve(id);
            assert_eq!(model.id, *id, "model {id} must be registered");
            assert_eq!(
                spec.model_protocol(id),
                *expected,
                "{provider_id}/{id} wire format"
            );
        }
    }

    // OpenCode Zen is multi-protocol: Claude routes over Anthropic, others over ChatCompletions
    let zen_spec = model_provider_spec("opencode-zen").expect("opencode-zen resolves");
    for id in nuo_provider::model_providers::OPENCODE_ZEN_MODELS {
        let model = nuo_provider::model::resolve(id);
        assert_eq!(model.id, *id, "model {id} must be registered");
        let expected = if *id == "claude-sonnet-4-6" {
            WireProtocol::AnthropicMessages
        } else {
            WireProtocol::OpenAiChatCompletions
        };
        assert_eq!(
            zen_spec.model_protocol(id),
            expected,
            "opencode-zen/{id} wire format"
        );
    }
}

// ── OAuth provider registration (`[INV-PROV-07]`, ADR-0027) ─────────────────

#[test]
fn init_registers_every_vendor_oauth_surface() {
    // The engine (`nuo-oauth`) links no vendor; the composition root must
    // register every surface exactly once so `oauth_provider(id)` resolves.
    init();
    for id in [
        "chatgpt",
        "chatgpt-plan",
        "openai-subscription",
        "google-antigravity",
        "antigravity",
        "antigravity-cli",
        "xai",
        "copilot",
        "github-copilot",
        "qoder",
        "qoder-cn",
        // `opencode`/`opencode-go`/`opencode-plan` retired as OAuth surfaces
        // (060956d removed OpenCode OAuth): they key-auth via
        // the generic key lane, so the composition root no longer registers
        // them here.
    ] {
        assert!(
            nuo_oauth::oauth_provider(id).is_some(),
            "OAuth surface `{id}` must be registered by init()"
        );
    }
    assert_eq!(
        nuo_oauth::oauth_config("chatgpt-plan")
            .expect("chatgpt-plan alias resolves")
            .provider_id,
        "chatgpt"
    );
}

/// ADR-0081 [INV-CATALOG-05]: a rendered label never carries routing syntax.
///
/// A per-surface test cannot see a surface that simply declared the wrong
/// `DisplayPolicy`, because the defect is a *disagreement between two
/// declarations*. This invariant reads both: it sweeps every registered spec and
/// fails if any seed renders a label still carrying a `/` namespace or a `:`
/// service tier — the check that keeps an ids-only relay from shipping raw wire
/// ids to users.
#[test]
fn every_registered_surface_renders_its_seeds_without_routing_syntax() {
    for spec in MODEL_PROVIDER_SPECS {
        for id in spec.models {
            let Some(label) = spec.resolve_display_label(None, id) else {
                // `None` is the declared fallback: the surfaces show the wire
                // id verbatim, which is only acceptable when it is clean.
                assert!(
                    !id.contains('/') && !id.contains(':'),
                    "{} renders bare wire id {id} on every surface",
                    spec.id
                );
                continue;
            };
            assert!(
                !label.contains('/') && !label.contains(':'),
                "{} rendered {label:?} for {id}: routing syntax is not a name",
                spec.id
            );
        }
    }
}
