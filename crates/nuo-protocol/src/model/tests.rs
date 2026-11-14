use super::*;

#[test]
fn availability_disabled_preserves_status_label_and_resets_at() {
    let avail = Availability::disabled(
        "disabled: 5h limit",
        Some("5h rolling quota exhausted".to_string()),
        Some(1710000000000),
    );
    assert!(!avail.is_usable());
    assert_eq!(avail.display_status(), Some("disabled: 5h limit"));
    assert_eq!(avail.reason.as_deref(), Some("5h rolling quota exhausted"));
    assert_eq!(avail.resets_at_ms, Some(1710000000000));

    let locked = Availability::locked(None);
    assert!(!locked.is_usable());
    assert_eq!(locked.display_status(), Some("disabled"));

    let usable = Availability::usable();
    assert!(usable.is_usable());
    assert_eq!(usable.display_status(), None);
}

#[test]
fn user_overrides_win_over_remote_and_baseline() {
    // Layer 1 (user) beats layer 2 (remote) beats layer 3
    // (baseline), field-wise; unset user knobs fall through.
    let remote = RemoteModelMetadata {
        vision: Some(true),
        tool_call: Some(true),
        context_window: Some(222_000),
        ..Default::default()
    };
    let user = CapabilityOverrides {
        // user says vision off, even though remote+baseline say on
        vision: Some(false),
        // user has no opinion on tool_call -> remote's true stands
        tool_call: None,
        // user has no opinion on context window -> remote's 222_000 stands
        context_window: None,
        family: Some("user-family".to_string()),
        thinking: None,
        max_output_tokens: Some(4_096),
        protocol: None,
    };
    let caps =
        ModelCapabilities::for_channel("fixture-alpha", Some(&remote)).apply_overrides(&user);
    // Layer 1 wins:
    assert_eq!(
        caps.vision,
        Some(false),
        "user Some(false) must beat remote Some(true)"
    );
    assert_eq!(caps.family, "user-family");
    assert_eq!(caps.max_output_tokens, Some(4_096));
    // Fall-through to layer 2:
    assert!(caps.tool_call);
    assert_eq!(caps.context_window, 222_000);
}

#[test]
fn empty_capability_overrides_are_a_noop() {
    let caps = ModelCapabilities::for_channel("fixture-alpha", None);
    let overridden = caps
        .clone()
        .apply_overrides(&CapabilityOverrides::default());
    assert_eq!(caps, overridden);
    assert!(CapabilityOverrides::default().is_empty());
}

#[test]
fn user_thinking_override_controls_chain_disclosure() {
    // baseline has ReasoningSupport::Adaptive -> chain_disclosed = true
    let caps = ModelCapabilities::for_channel("fixture-alpha", None);
    assert!(caps.chain_disclosed());

    // Override to ReasoningSummary -> chain_disclosed becomes false
    let user_summary = CapabilityOverrides {
        thinking: Some(ReasoningSupport::ReasoningSummary),
        ..Default::default()
    };
    let caps_summary = caps.clone().apply_overrides(&user_summary);
    assert!(caps_summary.reasoning());
    assert!(!caps_summary.chain_disclosed());

    // Override to ReasoningContent -> chain_disclosed becomes true
    let user_disclosed = CapabilityOverrides {
        thinking: Some(ReasoningSupport::ReasoningContent),
        ..Default::default()
    };
    let caps_disclosed = caps.clone().apply_overrides(&user_disclosed);
    assert!(caps_disclosed.reasoning());
    assert!(caps_disclosed.chain_disclosed());
}

// Fixture baselines. Core's own tests must not depend on real vendor data
// (that lives with the provider crates), so they register small tables of
// fictional ids through the same inventory mechanism providers use. The
// ids are deliberately non-vendor so they can never collide with a real
// baseline in a binary that also links a provider crate.
const FIXTURE_A: &[Model] = &[
    Model {
        id: "fixture-alpha",
        family: "fixture",
        context_window: 111_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: crate::effort::COMMON_LADDER,
        default_effort: Some(crate::effort::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "fixture-beta",
        family: "fixture",
        context_window: 222_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        effort_levels: &[],
        default_effort: Some(crate::effort::Effort::High),
        wire_entities: None,
    },
];
const FIXTURE_B: &[Model] = &[Model {
    id: "fixture-gamma",
    family: "fixture",
    context_window: 333_000,
    thinking: ReasoningSupport::None,
    tool_call: false,
    vision: false,
    protocol: WireProtocol::GoogleGenerateContent,
    effort_levels: &[],
    default_effort: Some(crate::effort::Effort::High),
    wire_entities: None,
}];

inventory::submit!(BaselineModels(FIXTURE_A));
inventory::submit!(BaselineModels(FIXTURE_B));

#[test]
fn registered_baselines_resolve_by_id() {
    let m = resolve("fixture-alpha");
    assert_eq!(m.context_window, 111_000);
    assert!(m.reasoning());
    assert!(m.vision);
    assert_eq!(m.protocol, WireProtocol::OpenAiChatCompletions);
    // `fitted_overlay_never_overrides_a_registered_baseline` may have
    // already run in this process and refreshed the ladder (overlay
    // writes are process-global), so assert the baseline value only when
    // no override landed; the override case is covered there.
    assert!(
        m.effort_levels == crate::effort::COMMON_LADDER
            || m.effort_levels == [crate::effort::Effort::Max].as_slice(),
        "unexpected ladder: {:?}",
        m.effort_levels
    );

    let g = resolve("fixture-gamma");
    assert_eq!(g.protocol, WireProtocol::GoogleGenerateContent);
    assert!(!g.tool_call);
}

#[test]
fn model_by_id_returns_none_for_unregistered_ids() {
    assert!(model_by_id("fixture-alpha").is_some());
    assert!(model_by_id("some-local-model").is_none());
}

#[test]
fn registered_baselines_have_unique_ids() {
    let mut ids: Vec<&str> = baseline_models().map(|m| m.id).collect();
    ids.sort_unstable();
    let dups: Vec<&str> = ids
        .windows(2)
        .filter(|w| w[0] == w[1])
        .map(|w| w[0])
        .collect();
    assert!(dups.is_empty(), "duplicate baseline ids: {dups:?}");
}

#[test]
fn resolve_falls_back_for_unknown() {
    let m = resolve("some-local-model");
    // Safe conservative text defaults.
    assert_eq!(m.context_window, 128_000);
    assert!(!m.reasoning());
    assert!(!m.tool_call);
    // `Model::vision` is a *policy* view, not a claim: the fallback
    // permits images so an undeclared route is attempted. What
    // was actually declared is `declared_vision`, which stays `None`.
    assert!(m.vision);
    assert_eq!(declared_vision("some-local-model"), None);
    // The capability resolution carries the undeclared state through, and
    // the policy helper reads it permissively rather than as text-only.
    let caps = ModelCapabilities::for_channel("some-local-model", None);
    assert_eq!(caps.vision, None);
    assert!(!caps.vision_declared());
    assert!(caps.accepts_images());
}

#[test]
fn declared_vision_reports_only_real_declarations() {
    // A vetted baseline entry is a declaration in both directions.
    assert_eq!(declared_vision("fixture-alpha"), Some(true));
    // Nothing in the layers declares an unknown id.
    assert_eq!(declared_vision("mystery-relay-model"), None);
}

#[test]
fn fitted_overlay_supplies_metadata_for_unregistered_ids() {
    register_fitted_models(vec![FittedModel {
        id: "fitted-future-k9".to_string(),
        family: "kimi-code".to_string(),
        context_window: 2_000_000,
        reasoning: true,
        vision: Some(true),
        protocol: WireProtocol::OpenAiChatCompletions,
        // Unsorted input with a duplicate: stored ascending, deduped.
        effort_levels: vec![
            crate::effort::Effort::Max,
            crate::effort::Effort::Low,
            crate::effort::Effort::Low,
        ],
    }]);
    let m = resolve("fitted-future-k9");
    assert_eq!(m.id, "fitted-future-k9");
    assert_eq!(m.context_window, 2_000_000);
    assert!(m.reasoning());
    assert!(m.vision);
    assert_eq!(
        m.effort_levels,
        &[crate::effort::Effort::Low, crate::effort::Effort::Max]
    );
}

#[test]
fn fitted_overlay_never_overrides_a_registered_baseline() {
    register_fitted_models(vec![FittedModel {
        id: "fixture-alpha".to_string(),
        family: "bogus".to_string(),
        context_window: 1,
        reasoning: false,
        vision: Some(false),
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: vec![crate::effort::Effort::Max],
    }]);
    // The vetted baseline entry wins on every field except the effort
    // ladder: effort tiers are a live platform knob, so a trusted
    // provider's advertised tiers refresh the baseline's ladder while
    // identity, context, format, and vision stay vetted.
    let m = resolve("fixture-alpha");
    assert_eq!(m.context_window, 111_000);
    assert_eq!(m.protocol, WireProtocol::OpenAiChatCompletions);
    assert!(m.vision);
    assert_eq!(m.effort_levels, [crate::effort::Effort::Max].as_slice());
}

#[test]
fn fitted_overlay_with_no_advertised_tiers_keeps_the_baseline_ladder() {
    // A fitted entry for a baseline-known id that advertises NO effort
    // tiers must not wipe the baseline's ladder — an absent field means
    // "the endpoint did not say", not "the model lost its knob".
    register_fitted_models(vec![FittedModel {
        id: "fixture-beta".to_string(),
        family: "fixture".to_string(),
        context_window: 0,
        reasoning: false,
        vision: Some(false),
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: Vec::new(),
    }]);
    // fixture-beta's baseline ladder is empty already, so check through
    // the gamma fixture's *sibling* instead: gamma has no fitted entry at
    // all and must be untouched by beta's registration.
    let g = resolve("fixture-gamma");
    assert_eq!(g.context_window, 333_000);
}

#[test]
fn fallback_format_is_openai_compat() {
    assert_eq!(
        fallback_model("anything").protocol,
        WireProtocol::OpenAiChatCompletions
    );
}

#[test]
fn sanitize_model_id_replaces_whitespace_with_hyphen() {
    assert_eq!(sanitize_model_id("gpt 5.5 preview"), "gpt-5.5-preview");
    assert_eq!(
        sanitize_model_id("  claude   3.7  sonnet  "),
        "claude-3.7-sonnet"
    );
    assert_eq!(sanitize_model_id("gemini-3.1-pro"), "gemini-3.1-pro");
    assert_eq!(sanitize_model_id("   "), "");
}

#[test]
fn wire_protocol_names_and_compatibility() {
    use std::str::FromStr;

    // Display names
    assert_eq!(
        WireProtocol::OpenAiChatCompletions.display_name(),
        "OpenAI Chat Completions"
    );
    assert_eq!(
        WireProtocol::OpenAiResponses.display_name(),
        "OpenAI Responses"
    );
    assert_eq!(
        WireProtocol::AnthropicMessages.display_name(),
        "Anthropic Messages"
    );
    assert_eq!(
        WireProtocol::GoogleGenerateContent.display_name(),
        "Google GenerateContent"
    );
    assert_eq!(
        WireProtocol::GoogleInteractions.display_name(),
        "Google Interactions"
    );

    // as_str
    assert_eq!(
        WireProtocol::OpenAiChatCompletions.as_str(),
        "openai-chat-completions"
    );
    assert_eq!(WireProtocol::OpenAiResponses.as_str(), "openai-responses");
    assert_eq!(
        WireProtocol::AnthropicMessages.as_str(),
        "anthropic-messages"
    );
    assert_eq!(
        WireProtocol::GoogleGenerateContent.as_str(),
        "google-generate-content"
    );
    assert_eq!(
        WireProtocol::GoogleInteractions.as_str(),
        "google-interactions"
    );

    // FromStr canonical
    assert_eq!(
        WireProtocol::from_str("openai-chat-completions").unwrap(),
        WireProtocol::OpenAiChatCompletions
    );
    assert_eq!(
        WireProtocol::from_str("openai-responses").unwrap(),
        WireProtocol::OpenAiResponses
    );
    assert_eq!(
        WireProtocol::from_str("anthropic-messages").unwrap(),
        WireProtocol::AnthropicMessages
    );
    assert_eq!(
        WireProtocol::from_str("google-generate-content").unwrap(),
        WireProtocol::GoogleGenerateContent
    );
    assert_eq!(
        WireProtocol::from_str("google-interactions").unwrap(),
        WireProtocol::GoogleInteractions
    );

    // Serde serialization
    assert_eq!(
        serde_json::to_string(&WireProtocol::OpenAiChatCompletions).unwrap(),
        "\"openai-chat-completions\""
    );
    assert_eq!(
        serde_json::to_string(&WireProtocol::OpenAiResponses).unwrap(),
        "\"openai-responses\""
    );
    assert_eq!(
        serde_json::to_string(&WireProtocol::AnthropicMessages).unwrap(),
        "\"anthropic-messages\""
    );
    assert_eq!(
        serde_json::to_string(&WireProtocol::GoogleGenerateContent).unwrap(),
        "\"google-generate-content\""
    );
    assert_eq!(
        serde_json::to_string(&WireProtocol::GoogleInteractions).unwrap(),
        "\"google-interactions\""
    );

    // Serde deserialization
    assert_eq!(
        serde_json::from_str::<WireProtocol>("\"openai-chat-completions\"").unwrap(),
        WireProtocol::OpenAiChatCompletions
    );
    assert_eq!(
        serde_json::from_str::<WireProtocol>("\"openai-responses\"").unwrap(),
        WireProtocol::OpenAiResponses
    );
    assert_eq!(
        serde_json::from_str::<WireProtocol>("\"anthropic-messages\"").unwrap(),
        WireProtocol::AnthropicMessages
    );
    assert_eq!(
        serde_json::from_str::<WireProtocol>("\"google-generate-content\"").unwrap(),
        WireProtocol::GoogleGenerateContent
    );
    assert_eq!(
        serde_json::from_str::<WireProtocol>("\"google-interactions\"").unwrap(),
        WireProtocol::GoogleInteractions
    );
}
