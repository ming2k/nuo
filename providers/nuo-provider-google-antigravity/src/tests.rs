use super::*;
use nuo_tool::{Message, Role};

/// Baseline capability view for tests that need to stamp an effort ladder:
/// `GoogleProvider::new` derives capabilities from the registry, which the
/// llm-client crate's tests see as the fallback (`effort_levels == []`).
fn p_caps(model: &str) -> nuo_protocol::ModelCapabilities {
    nuo_protocol::ModelCapabilities::for_channel(model, None)
}

#[test]
fn default_constructor_targets_official_base() {
    // `new` resolves the official versioned base; the per-call path is
    // appended at request time, not stored on the base.
    let p = GoogleProvider::new("k".to_string(), "gemini-2.5-flash".to_string());
    assert_eq!(
        p.endpoint.base_url(),
        "https://generativelanguage.googleapis.com/v1beta"
    );
    assert_eq!(p.endpoint.user_agent(), NUO_USER_AGENT);
}

#[test]
fn custom_base_url_strips_trailing_slash() {
    // A relay/中转站 base supplied with a trailing slash must not yield a
    // double slash in the appended model path.
    let p = GoogleProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "gemini-2.5-flash".to_string(),
        "https://relay.example.com/v1beta/",
        "relay-agent/1.0",
    );
    assert_eq!(p.endpoint.base_url(), "https://relay.example.com/v1beta");
    assert_eq!(p.endpoint.user_agent(), "relay-agent/1.0");
}

#[test]
fn prompt_hints_emit_no_system_guidance() {
    let p = GoogleProvider::new("k".to_string(), "gemini-2.5-flash".to_string());
    // No protocol note: native function calls are the wire default and the
    // model already follows its own functionResponse replay convention.
    assert!(p.prompt_hints().system_guidance.is_empty());
}

#[test]
fn antigravity_envelope_preserves_canonical_antigravity_wire_ids() {
    for (id, expected) in [
        // Registered per-level entity ids are the user's explicit depth
        // choice: they pass through verbatim (ADR-0074 Verbatim arm).
        ("gemini-3.8-flash-high", "gemini-3.8-flash-high"),
        ("gemini-3.8-flash-medium", "gemini-3.8-flash-medium"),
        ("gemini-3.8-flash-low", "gemini-3.8-flash-low"),
        // The bare canonical id has no registered entity of its own; the
        // baseline table resolves it to the upstream-default row
        // (`-tiered`: in-band `thinkingLevel` depth).
        ("gemini-3.8-flash", "gemini-3.8-flash-tiered"),
        ("gemini-3.8-flash-tiered", "gemini-3.8-flash-tiered"),
        ("claude-sonnet-4-6", "claude-sonnet-4-6"),
        ("claude-opus-4-6", "claude-opus-4-6-thinking"),
        ("claude-opus-4-6-thinking", "claude-opus-4-6-thinking"),
        ("gpt-oss-120b", "gpt-oss-120b-medium"),
        ("gpt-oss-120b-medium", "gpt-oss-120b-medium"),
        ("gemini-3.5-flash-lite", "gemini-3.5-flash-lite"),
        // gemini-4-argon: the bare base id has no PredictionService entity
        // (404 `Requested entity was not found`); the baseline table
        // resolves it to the paid-tier default row (`-medium`,
        // `defaultAgentModelId` on the pro surface). Per-level ids pass
        // through untouched.
        ("gemini-4-argon", "gemini-4-argon-medium"),
        ("gemini-4-argon-low", "gemini-4-argon-low"),
        ("gemini-4-argon-medium", "gemini-4-argon-medium"),
        ("gemini-4-argon-high", "gemini-4-argon-high"),
    ] {
        let p = GoogleProvider::with_base_url_and_user_agent(
            "k".to_string(),
            id.to_string(),
            "https://daily-cloudcode-pa.googleapis.com",
            "ua",
        )
        .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
        .with_project_id("proj-1");
        let (_, _, body) = p.prepare_request(&ModelRequest::new(Vec::new()), false, false);
        assert_eq!(
            body.body["model"], expected,
            "model id {id} must match expected wire id {expected}"
        );
    }

    // Test wire mapping when reasoning effort is selected on Gemini 3.8 Flash
    for effort in [
        nuo_protocol::Effort::Minimal,
        nuo_protocol::Effort::Low,
        nuo_protocol::Effort::Medium,
        nuo_protocol::Effort::High,
        nuo_protocol::Effort::Max,
    ] {
        let mut p = GoogleProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "gemini-3.8-flash".to_string(),
            "https://daily-cloudcode-pa.googleapis.com",
            "ua",
        )
        .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
        .with_project_id("proj-1");
        p.reasoning_effort = Some(effort);
        let (_, _, body) = p.prepare_request(&ModelRequest::new(Vec::new()), false, false);
        assert_eq!(body.body["model"], "gemini-3.8-flash-tiered");
        assert!(!body.body["request"]["generationConfig"]["thinkingConfig"].is_null());
    }

    // Verify Vertex Claude requests never stamp Google-native thinkingConfig
    let p_claude = GoogleProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "claude-sonnet-4-6".to_string(),
        "https://daily-cloudcode-pa.googleapis.com",
        "ua",
    )
    .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
    .with_project_id("proj-1");
    let (_, _, body_claude) =
        p_claude.prepare_request(&ModelRequest::new(Vec::new()), false, false);
    assert!(body_claude.body["request"]["generationConfig"]["thinkingConfig"].is_null());
}

/// Argon's wire effort surface is the per-level entity itself (no
/// `tieredModelIds` entry carries argon, so there is no canonical entity
/// that would take an in-band `thinkingLevel` budget). The registered
/// entity must track the channel's effort override.
#[test]
fn antigravity_argon_routes_effort_to_registered_per_level_entity() {
    for (effort, expected) in [
        (nuo_protocol::Effort::Minimal, "gemini-4-argon-low"),
        (nuo_protocol::Effort::Low, "gemini-4-argon-low"),
        (nuo_protocol::Effort::Medium, "gemini-4-argon-medium"),
        (nuo_protocol::Effort::High, "gemini-4-argon-high"),
        (nuo_protocol::Effort::Max, "gemini-4-argon-high"),
    ] {
        let mut p = GoogleProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "gemini-4-argon".to_string(),
            "https://daily-cloudcode-pa.googleapis.com",
            "ua",
        )
        .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
        .with_project_id("proj-1");
        p.reasoning_effort = Some(effort);
        let (_, _, body) = p.prepare_request(&ModelRequest::new(Vec::new()), false, false);
        assert_eq!(
            body.body["model"], expected,
            "effort {effort:?} must reach its registered argon entity"
        );
    }

    // No effort override: the account default entity applies.
    let p = GoogleProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "gemini-4-argon".to_string(),
        "https://daily-cloudcode-pa.googleapis.com",
        "ua",
    )
    .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
    .with_project_id("proj-1");
    let (_, _, body) = p.prepare_request(&ModelRequest::new(Vec::new()), false, false);
    assert_eq!(body.body["model"], "gemini-4-argon-medium");
}

/// The Claude 5.5 family's registered wire entities are the per-level
/// suffixes; effort ranks into them. This exercises the **live
/// observation** path: the entity evidence comes from the channel's
/// remote catalog snapshot (`effort_entities`), exactly what a real
/// `fetchAvailableModels` sync produces.
#[test]
fn antigravity_claude_5_5_routes_observed_entities() {
    let observed: Vec<String> = [
        "claude-opus-5-5-low",
        "claude-opus-5-5-medium",
        "claude-opus-5-5-high",
        "claude-opus-5-5-max",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for (effort, expected) in [
        (Some(nuo_protocol::Effort::Low), "claude-opus-5-5-low"),
        (Some(nuo_protocol::Effort::Medium), "claude-opus-5-5-medium"),
        (Some(nuo_protocol::Effort::High), "claude-opus-5-5-high"),
        (Some(nuo_protocol::Effort::Max), "claude-opus-5-5-max"),
    ] {
        let mut p = GoogleProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "claude-opus-5-5".to_string(),
            "https://daily-cloudcode-pa.googleapis.com",
            "ua",
        )
        .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
        .with_project_id("proj-1");
        p.capabilities.effort_entities = observed.clone();
        p.reasoning_effort = effort;
        let (_, _, body) = p.prepare_request(&ModelRequest::new(Vec::new()), false, false);
        assert_eq!(
            body.body["model"], expected,
            "effort {effort:?} must reach its registered 5-5 entity"
        );
    }
}

/// Offline-floor parity: with NO remote observation, the compiled
/// baseline entity table routes bare family ids identically to the live
/// path — an offline session never degrades into a bare-id 404.
#[test]
fn antigravity_baseline_table_covers_the_offline_floor() {
    for (model, effort, expected) in [
        (
            "claude-opus-5-5",
            Some(nuo_protocol::Effort::High),
            "claude-opus-5-5-high",
        ),
        (
            "claude-sonnet-5-5",
            Some(nuo_protocol::Effort::Max),
            "claude-sonnet-5-5-max",
        ),
        ("gemini-4-argon", None, "gemini-4-argon-medium"),
        ("gemini-3.8-flash", None, "gemini-3.8-flash-tiered"),
        ("claude-opus-4-6", None, "claude-opus-4-6-thinking"),
        (
            "claude-opus-4-6",
            Some(nuo_protocol::Effort::High),
            "claude-opus-4-6-thinking",
        ),
    ] {
        let mut p = GoogleProvider::with_base_url_and_user_agent(
            "k".to_string(),
            model.to_string(),
            "https://daily-cloudcode-pa.googleapis.com",
            "ua",
        )
        .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
        .with_project_id("proj-1");
        assert!(p.capabilities.effort_entities.is_empty());
        p.reasoning_effort = effort;
        let (_, _, body) = p.prepare_request(&ModelRequest::new(Vec::new()), false, false);
        assert_eq!(
            body.body["model"], expected,
            "offline floor for {model} must hit its registered entity"
        );
    }
}

#[test]
fn thinking_rejected_flag_is_sticky_and_latches_once() {
    // One observation latches the channel-scoped flag; the exactly-once
    // return lets the caller log the downgrade exactly once per process.
    let p = GoogleProvider::new("k".to_string(), "gemini-3.7-flash".to_string());
    assert!(!p.thinking_was_rejected());
    assert!(p.note_thinking_rejected(), "first observation reports new");
    assert!(p.thinking_was_rejected());
    assert!(
        !p.note_thinking_rejected(),
        "later observations report already-known"
    );
    assert!(p.thinking_was_rejected());
}

#[test]
fn omit_thinking_strips_the_whole_thinking_surface() {
    // The downgrade must drop BOTH the disclosure request and the depth
    // directive: `includeThoughts` alone is what the upstream refused, but
    // `thinkingLevel`/`thinkingBudget` ride the same rejected object and
    // re-sending either would fail the retry identically.
    let capabilities = {
        let mut caps = p_caps("gemini-3.7-flash");
        caps.thinking = nuo_protocol::reasoning::ReasoningSupport::ReasoningContent;
        let levels = [
            nuo_protocol::Effort::Minimal,
            nuo_protocol::Effort::Low,
            nuo_protocol::Effort::Medium,
            nuo_protocol::Effort::High,
        ];
        caps.effort_levels = levels.iter().copied().map(Into::into).collect();
        caps
    };
    let p = GoogleProvider::new("k".to_string(), "gemini-3.7-flash".to_string())
        .with_model_capabilities(capabilities)
        .with_reasoning_effort(Some(nuo_protocol::Effort::High));
    let (_, _, with) = p.prepare_request(&ModelRequest::new(Vec::new()), true, false);
    assert_eq!(
        with.body["generationConfig"]["thinkingConfig"]["includeThoughts"],
        true
    );
    assert_eq!(
        with.body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "HIGH"
    );

    let (_, _, without) = p.prepare_request(&ModelRequest::new(Vec::new()), true, true);
    assert!(
        without.body["generationConfig"]["thinkingConfig"].is_null(),
        "the entire thinkingConfig object must be absent after downgrade"
    );
}

#[test]
fn omit_thinking_is_orthogonal_to_non_antigravity_path() {
    // The native (non-envelope) path stamps thinkingConfig on the raw
    // body; the downgrade applies there identically.
    let capabilities = {
        let mut caps = p_caps("gemini-2.5-pro");
        caps.thinking = nuo_protocol::reasoning::ReasoningSupport::ReasoningContent;
        let budget_levels = [
            nuo_protocol::Effort::Minimal,
            nuo_protocol::Effort::Low,
            nuo_protocol::Effort::Medium,
            nuo_protocol::Effort::High,
            nuo_protocol::Effort::Max,
        ];
        caps.effort_levels = budget_levels.iter().copied().map(Into::into).collect();
        caps
    };
    let p = GoogleProvider::new("k".to_string(), "gemini-2.5-pro".to_string())
        .with_model_capabilities(capabilities)
        .with_reasoning_effort(Some(nuo_protocol::Effort::High));
    let (_, _, with) = p.prepare_request(&ModelRequest::new(Vec::new()), true, false);
    assert_eq!(
        with.body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        32768
    );
    let (_, _, without) = p.prepare_request(&ModelRequest::new(Vec::new()), true, true);
    assert!(without.body["generationConfig"]["thinkingConfig"].is_null());
}

#[test]
fn plain_api_key_travels_in_the_header() {
    let p = GoogleProvider::with_base_url(
        "AIza-key".to_string(),
        "gemini-3-flash".to_string(),
        "https://generativelanguage.googleapis.com/v1beta",
    );
    let auth = nuo_protocol::ResolvedAuth::new("AIza-key");
    let (url, headers, _) =
        p.prepare_request_for_auth(&ModelRequest::new(Vec::new()), false, true, &auth);
    assert!(url.ends_with(":generateContent"), "{url}");
    assert!(
        !url.contains("key="),
        "API key must not leak into URL: {url}"
    );
    assert_eq!(
        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
        Some("AIza-key")
    );
    assert!(!headers.contains_key("x-opencode-org-id"));
}

/// The signature origin must distinguish **backends**, not just models.
///
/// The same model id is served by Google AI Studio, by Cloud Code's
/// `v1internal`, and by any relay configured through `with_base_url`. A token
/// minted by one and replayed to another is the cross-backend case Google
/// answers `400 "Corrupted thought signature."` to, so the identity that gates
/// replay has to carry the ingress authority.
#[test]
fn signature_origin_distinguishes_backends_not_just_models() {
    let studio = GoogleProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "gemini-3.8-flash".to_string(),
        "https://generativelanguage.googleapis.com/v1beta",
        "ua",
    );
    let cloudcode = GoogleProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "gemini-3.8-flash".to_string(),
        "https://daily-cloudcode-pa.googleapis.com",
        "ua",
    )
    .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
    .with_project_id("proj-1");
    let relay = GoogleProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "gemini-3.8-flash".to_string(),
        "https://relay.example.com/v1beta",
        "ua",
    );

    // Same channel model id, three different minting backends.
    assert_ne!(studio.signature_origin(), cloudcode.signature_origin());
    assert_ne!(studio.signature_origin(), relay.signature_origin());
    assert_ne!(cloudcode.signature_origin(), relay.signature_origin());
    assert_eq!(
        studio.signature_origin(),
        "generativelanguage.googleapis.com/gemini-3.8-flash",
        "the origin is the ingress authority plus the wire model"
    );
    // The Cloud Code envelope rewrites the wire model, and the origin follows.
    assert_eq!(
        cloudcode.signature_origin(),
        "daily-cloudcode-pa.googleapis.com/gemini-3.8-flash-tiered"
    );
}

/// A token minted by one backend must not be replayed to another, end to end:
/// build a turn stamped by AI Studio, then replay it on a Cloud Code channel.
#[test]
fn a_token_from_another_backend_is_not_replayed() {
    let call = nuo_tool::ToolCall {
        id: "call_1".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    // Minted by AI Studio.
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        response::THOUGHT_SIGNATURES_META_KEY.to_string(),
        serde_json::json!({ "call_1": "sig-from-studio" }),
    );
    provider_meta.insert(
        response::THOUGHT_SIGNATURE_ORIGIN_META_KEY.to_string(),
        serde_json::json!("generativelanguage.googleapis.com/gemini-3.8-flash"),
    );
    let messages = vec![
        Message::new(Role::User, "inspect"),
        Message {
            tool_calls: Some(vec![call.clone()]),
            provider_meta: Some(provider_meta),
            ..Message::new(Role::Assistant, "checking")
        },
        Message::tool_result(&call, "Cargo.toml"),
    ];

    // Replayed on the Cloud Code channel — a different backend.
    let p = GoogleProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "gemini-3.8-flash".to_string(),
        "https://daily-cloudcode-pa.googleapis.com",
        "ua",
    )
    .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
    .with_project_id("proj-1");
    let (_, _, body) = p.prepare_request(&ModelRequest::new(messages), false, false);
    assert_eq!(
        body.body["request"]["contents"][1]["parts"][1]["thoughtSignature"],
        request::SKIP_THOUGHT_SIGNATURE_VALIDATOR,
        "a foreign backend's token must not be replayed"
    );
}

/// The counterpart: a token minted by THIS backend replays natively. Without
/// this the gate could be vacuously passing by dropping everything.
#[test]
fn a_token_from_the_same_backend_replays() {
    let call = nuo_tool::ToolCall {
        id: "call_1".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    let p = GoogleProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "gemini-3.8-flash".to_string(),
        "https://daily-cloudcode-pa.googleapis.com",
        "ua",
    )
    .with_dialect(nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity)
    .with_project_id("proj-1");
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        response::THOUGHT_SIGNATURES_META_KEY.to_string(),
        serde_json::json!({ "call_1": "sig-same-backend" }),
    );
    provider_meta.insert(
        response::THOUGHT_SIGNATURE_ORIGIN_META_KEY.to_string(),
        serde_json::json!(p.signature_origin()),
    );
    let (_, _, body) = p.prepare_request(
        &ModelRequest::new(vec![
            Message::new(Role::User, "inspect"),
            Message {
                tool_calls: Some(vec![call.clone()]),
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "checking")
            },
            Message::tool_result(&call, "Cargo.toml"),
        ]),
        false,
        false,
    );
    assert_eq!(
        body.body["request"]["contents"][1]["parts"][1]["thoughtSignature"],
        "sig-same-backend"
    );
}
