use super::*;
use crate::{PromptCacheCapabilities, PromptCachePreference};

#[test]
fn dialect_strings_parse_their_serialized_form() {
    // `FromStr` and the wire share one serde representation, so every
    // dialect's serialized name must parse back to itself. A hand-written
    // match missed `codebuddy`, which made a valid dialect unregisterable.
    for dialect in [
        ProviderDialect::Standard,
        ProviderDialect::Antigravity,
        ProviderDialect::ChatGpt,
        ProviderDialect::Copilot,
        ProviderDialect::DeepSeek,
        ProviderDialect::OpenRouter,
        ProviderDialect::Portable,
        ProviderDialect::Qoder,
        ProviderDialect::CodeBuddy,
    ] {
        let encoded = serde_json::to_string(&dialect).unwrap();
        let bare: String = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            bare.parse::<ProviderDialect>().unwrap(),
            dialect,
            "{bare} must parse back to {dialect:?}"
        );
    }
}

/// The portable gateway's contract, pinned field by field: flattened tools,
/// no `store`, dual-form reasoning replay. Each flag was established by
/// probing the live endpoint (ADR-0142), so a change here is a change to
/// the wire contract, not a preference.
#[test]
fn portable_surface_declares_its_probed_contract() {
    let portable = ProviderDialect::Portable.openai_responses();
    assert_eq!(portable, OpenAiResponsesDialect::Portable);
    // Flattened OpenAI tool shape: the gateway rejects DeepSeek's nested
    // form with `400 tools[0].function: requires a native Responses target`.
    assert!(!portable.nested_tool_definitions());
    assert!(portable.supports_tool_choice());
    assert!(portable.supports_parallel_tool_calls());
    assert!(portable.supports_developer_role());
    // No state: `store: true` and `previous_response_id` both answer 400,
    // so `store` is never emitted and the full context is replayed.
    assert!(!portable.supports_store());
    // Both reasoning part families, because the upstreams behind the one URL
    // enforce opposite replay rules.
    assert!(portable.dual_form_reasoning_replay());
    // It is a transcoding gateway, not DeepSeek's native platform, so it
    // keeps the cached-reasoning grant a translating surface understands.
    assert!(portable.encrypted_reasoning());
    assert!(portable.reasoning_summary());

    // DeepSeek's native surface keeps the opposite answers.
    let native = ProviderDialect::DeepSeek.openai_responses();
    assert!(native.nested_tool_definitions());
    assert!(!native.supports_store());
    assert!(!native.dual_form_reasoning_replay());
    assert!(!native.encrypted_reasoning());
}

#[test]
fn catalog_lookup_is_exact_match() {
    let entries = [ProviderEntry {
        id: "deepseek-v4-flash".to_string(),
        name: "DeepSeek V4 Flash".to_string(),
        description: String::new(),
        channels: vec![Channel {
            id: "default".to_string(),
            transport: Transport::OpenAiChatCompletions {
                base_url: "https://api.deepseek.com/v1/chat/completions".to_string(),
                dialect: Default::default(),
                user_agent: "agent".to_string(),
                headers: Vec::new(),
            },
            credentials: crate::static_credential("k"),
            model: "deepseek-v4-flash".to_string(),
            remote: None,
            user_overrides: None,
            tunings: Default::default(),
            prompt_cache_preference: PromptCachePreference::default(),
            prompt_cache: PromptCacheCapabilities::unsupported(),
        }],
        default_channel: 0,
        builtin: true,
    }];
    assert_eq!(
        entries
            .iter()
            .find(|e| e.id == "deepseek-v4-flash")
            .expect("exact id")
            .id,
        "deepseek-v4-flash"
    );
    // No alias mapping: stale ids do not resolve.
    assert!(entries.iter().find(|e| e.id == "deepseek").is_none());
    assert!(entries.iter().find(|e| e.id == "deepseek-flash").is_none());
    assert!(entries.iter().find(|e| e.id == "unknown").is_none());
}

#[test]
fn key_ready_is_false_for_empty_cloud_key() {
    let channel = Channel {
        id: "default".to_string(),
        transport: Transport::OpenAiChatCompletions {
            base_url: "https://api.openai.com/v1/chat/completions".to_string(),
            dialect: Default::default(),
            user_agent: "agent".to_string(),
            headers: Vec::new(),
        },
        credentials: crate::static_credential("   "),
        model: "gpt-4o".to_string(),
        remote: None,
        user_overrides: None,
        tunings: Default::default(),
        prompt_cache_preference: PromptCachePreference::default(),
        prompt_cache: PromptCacheCapabilities::unsupported(),
    };
    assert!(!channel.key_ready());
}

#[test]
fn anthropic_transport_needs_a_key() {
    // The Anthropic /messages transport is a cloud transport: it must
    // report needing a key, and an empty key must not be "ready".
    let needs_key = Transport::AnthropicMessages {
        base_url: "https://opencode.ai/inference/anthropic/v1/messages".to_string(),
        dialect: Default::default(),
        user_agent: "agent".to_string(),
        headers: Vec::new(),
    }
    .needs_api_key();
    assert!(needs_key, "Anthropic transport must require an API key");

    let channel = Channel {
        id: "default".to_string(),
        transport: Transport::AnthropicMessages {
            base_url: "https://opencode.ai/inference/anthropic/v1/messages".to_string(),
            dialect: Default::default(),
            user_agent: "agent".to_string(),
            headers: Vec::new(),
        },
        credentials: crate::static_credential("  "),
        model: "minimax-m3".to_string(),
        remote: None,
        user_overrides: None,
        tunings: Default::default(),
        prompt_cache_preference: PromptCachePreference::default(),
        prompt_cache: PromptCacheCapabilities::unsupported(),
    };
    assert!(!channel.key_ready(), "empty key must not be ready");
}

#[test]
fn multi_model_entry_resolves_channel_by_model_id() {
    // A provider like opencode-go hosts one channel per model, each with the
    // transport matching that model's wire format. Selecting a model is
    // selecting a channel.
    let entry = ProviderEntry {
        id: "opencode-go".to_string(),
        name: "OpenCode Go".to_string(),
        description: String::new(),
        channels: vec![
            Channel {
                id: "glm-5.2".to_string(),
                transport: Transport::OpenAiChatCompletions {
                    base_url: "https://opencode.ai/inference/openai/v1/chat/completions"
                        .to_string(),
                    dialect: Default::default(),
                    user_agent: "agent".to_string(),
                    headers: Vec::new(),
                },
                credentials: crate::static_credential("k"),
                model: "glm-5.2".to_string(),
                remote: None,
                user_overrides: None,
                tunings: Default::default(),
                prompt_cache_preference: PromptCachePreference::default(),
                prompt_cache: PromptCacheCapabilities::unsupported(),
            },
            Channel {
                id: "claude-opus-5".to_string(),
                transport: Transport::AnthropicMessages {
                    base_url: "https://opencode.ai/inference/anthropic/v1/messages".to_string(),
                    dialect: Default::default(),
                    user_agent: "agent".to_string(),
                    headers: Vec::new(),
                },
                credentials: crate::static_credential("k"),
                model: "claude-opus-5".to_string(),
                remote: None,
                user_overrides: None,
                tunings: Default::default(),
                prompt_cache_preference: PromptCachePreference::default(),
                prompt_cache: PromptCacheCapabilities::unsupported(),
            },
        ],
        default_channel: 0,
        builtin: true,
    };
    // OpenAI-format model resolves to the OpenAiChatCompletions channel.
    let glm = entry.channel_for_model("glm-5.2").expect("glm-5.2 channel");
    assert!(matches!(
        glm.transport,
        Transport::OpenAiChatCompletions { .. }
    ));
    // Anthropic-format model resolves to the AnthropicMessages channel.
    let mm = entry
        .channel_for_model("claude-opus-5")
        .expect("claude-opus-5 channel");
    assert!(matches!(mm.transport, Transport::AnthropicMessages { .. }));
    // An unknown model id resolves to nothing.
    assert!(entry.channel_for_model("nope").is_none());
    assert!(!entry.offers_model("nope"));
    assert!(entry.offers_model("glm-5.2"));
}

#[test]
fn context_window_resolves_from_model_registry() {
    // Uses the `fixture-alpha` baseline registered in `model::tests` (core's
    // own test binary links no provider crate, so real vendor ids fall back).
    let entry = ProviderEntry {
        id: "fixture-provider".to_string(),
        name: "Fixture".to_string(),
        description: String::new(),
        channels: vec![Channel {
            id: "default".to_string(),
            transport: Transport::OpenAiChatCompletions {
                base_url: "https://example.com/v1/chat/completions".to_string(),
                dialect: Default::default(),
                user_agent: "agent".to_string(),
                headers: Vec::new(),
            },
            credentials: crate::static_credential("k"),
            model: "fixture-alpha".to_string(),
            remote: None,
            user_overrides: None,
            tunings: Default::default(),
            prompt_cache_preference: PromptCachePreference::default(),
            prompt_cache: PromptCacheCapabilities::unsupported(),
        }],
        default_channel: 0,
        builtin: true,
    };
    assert_eq!(entry.context_window(), 111_000);
}

#[test]
fn clean_model_display_name_strips_thinking_annotations() {
    assert_eq!(
        clean_model_display_name("Claude Opus 4.6 (Thinking)"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_model_display_name("Claude Sonnet 4.6 (thinking)"),
        "Claude Sonnet 4.6"
    );
    assert_eq!(
        clean_model_display_name("Claude Opus 4.6 [Thinking]"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_model_display_name("Claude Opus 4.6 - Thinking"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_model_display_name("Claude Opus 4.6 (Thinking Mode)"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_model_display_name("Claude Opus 4.6 (Thinking) (thinking)"),
        "Claude Opus 4.6"
    );

    let discovered = DiscoveredModel {
        id: "claude-opus-4-6".to_string(),
        name: Some("Claude Opus 4.6 (Thinking)".to_string()),
        ..DiscoveredModel::default()
    };
    let meta = discovered.remote_metadata();
    assert_eq!(meta.name.as_deref(), Some("Claude Opus 4.6"));
}

#[test]
fn format_model_display_name_folds_hyphens_and_capitalizes_brands() {
    assert_eq!(
        format_model_display_name(None, "deepseek-v4.1-flash"),
        "DeepSeek V4.1 Flash"
    );
    assert_eq!(
        format_model_display_name(Some("DeepSeek-V4.1-Flash"), "deepseek-v4.1-flash"),
        "DeepSeek V4.1 Flash"
    );
    assert_eq!(
        format_model_display_name(None, "glm-5.3-flash"),
        "GLM 5.3 Flash"
    );
    assert_eq!(
        format_model_display_name(Some("GLM-5.3-Flash (Thinking)"), "glm-5.3-flash"),
        "GLM 5.3 Flash"
    );
    assert_eq!(
        format_model_display_name(None, "claude-haiku-5-5"),
        "Claude Haiku 5.5"
    );
    assert_eq!(
        format_model_display_name(None, "qwen3.8-max"),
        "Qwen 3.8 Max"
    );
    assert_eq!(format_model_display_name(None, "minimax-m3"), "MiniMax M3");
    assert_eq!(format_model_display_name(None, "grok-4.7"), "Grok 4.7");
}

/// ADR-0081 [INV-CATALOG-05]: a relay id carries two routing separators that
/// are never part of a model's name. The tier is always dropped (closed set
/// only), the namespace only when the tail already names a brand.
#[test]
fn structural_lift_absorbs_routing_syntax() {
    // A redundant `vendor/` namespace is absorbed: the tail already leads
    // with the family brand, so printing it twice would be a double name.
    assert_eq!(
        format_model_display_name(None, "deepseek/deepseek-v4.1-flash"),
        "DeepSeek V4.1 Flash"
    );
    assert_eq!(
        format_model_display_name(None, "openai/gpt-5.6-sol"),
        "GPT 5.6 Sol"
    );
    // A namespace the tail does not state is kept, because it is the only
    // place the vendor is named at all.
    assert_eq!(
        format_model_display_name(None, "meta/muse-spark-1.1"),
        "Meta Muse Spark 1.1"
    );
    // A recognised `:tier` is routing syntax, not a name.
    assert_eq!(
        format_model_display_name(None, "nex-agi/nex-n2.5-pro:free"),
        "Nex Agi Nex N2.5 Pro"
    );
    assert_eq!(
        format_model_display_name(None, "x-ai/grok-4.7:nitro"),
        "Grok 4.7"
    );
    // An unrecognised colon belongs to the name and survives untouched.
    assert_eq!(
        format_model_display_name(None, "vendor/model:v2"),
        "Vendor Model:v2"
    );
}

/// An authored label is prose, not routing syntax: it is cleaned and
/// hyphen-folded, never restructured (ADR-0081 [INV-CATALOG-05]).
#[test]
fn authored_labels_are_never_restructured() {
    assert_eq!(
        format_model_display_name(Some("OpenAI: GPT-5.6 Sol"), "gpt-5.6-sol"),
        "OpenAI: GPT 5.6 Sol"
    );
    assert_eq!(
        format_model_display_name(Some("Mixtral 8x7B / Instruct"), "mixtral-8x7b"),
        "Mixtral 8x7B / Instruct"
    );
}

/// The casing pass and the namespace-redundancy test read one pair of
/// tables, so they cannot disagree about a brand's identity or spelling.
#[test]
fn lexicons_are_the_single_answer_for_brand_identity() {
    for (brand, canonical) in BRAND_LEXICON {
        assert_eq!(&capitalize_token(brand), canonical, "{brand}");
        assert!(
            namespace_is_redundant("some-vendor", &format!("{brand}-v9")),
            "{brand} must lead a family, so a namespace in front of it is redundant"
        );
    }
    for tier in TIER_LEXICON {
        let cased = capitalize_token(tier);
        assert!(
            cased.chars().next().is_some_and(char::is_uppercase),
            "{tier} must be title-cased, got {cased}"
        );
    }
}

#[test]
fn is_relay_promotional_noise_filters_transient_models() {
    assert!(is_relay_promotional_noise("beam-free"));
    assert!(is_relay_promotional_noise("deepseek-v4.1-flash-free"));
    assert!(is_relay_promotional_noise("hy4-preview"));
    assert!(is_relay_promotional_noise("step-5-preview"));
    assert!(is_relay_promotional_noise("gemini-4-argon-contributor"));
    assert!(is_relay_promotional_noise("muse-spark-1.2-contributor"));
    assert!(is_relay_promotional_noise("alpha-test-model"));
    assert!(is_relay_promotional_noise("omen-alpha"));
    assert!(is_relay_promotional_noise("test"));

    assert!(!is_relay_promotional_noise("deepseek-v4.1-flash"));
    assert!(!is_relay_promotional_noise("glm-5.3-flash"));
    assert!(!is_relay_promotional_noise("claude-haiku-5-5"));
    assert!(!is_relay_promotional_noise("qwen3.8-max"));

    // The same routing fact in its colon spelling (ADR-0081).
    assert!(is_relay_promotional_noise("nex-agi/nex-n2.5-pro:free"));
    assert!(is_relay_promotional_noise("vendor/model:alpha"));
    // A colon that belongs to a name is not a tier.
    assert!(!is_relay_promotional_noise("vendor/model:v2"));
}
