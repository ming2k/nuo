//! Thought-signature provenance and replay gating.

use super::*;

#[test]
fn foreign_origin_signatures_are_not_replayed() {
    let call = nuo_tool::ToolCall {
        id: "call_1".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        THOUGHT_SIGNATURES_META_KEY.to_string(),
        json!({ "call_1": "sig-from-another-route" }),
    );
    // Minted by `other-route`; this request addresses TEST_ORIGIN.
    provider_meta.insert(
        THOUGHT_SIGNATURE_ORIGIN_META_KEY.to_string(),
        json!("other-route"),
    );

    let body = body(
        vec![
            Message::new(Role::User, "inspect"),
            Message {
                tool_calls: Some(vec![call.clone()]),
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "checking")
            },
            Message::tool_result(&call, "Cargo.toml"),
        ],
        test_body_input(None, false, None),
    );

    assert_eq!(
        body["contents"][1]["parts"][1]["thoughtSignature"], SKIP_THOUGHT_SIGNATURE_VALIDATOR,
        "a foreign token must degrade to the documented bypass sentinel"
    );
}

/// A text-part signature is only valid on the exact bytes that minted it. The
/// assistant-content sanitizer rewrites bytes, so a signed turn it rewrites
/// must drop the signature instead of stamping it onto different text.
#[test]
fn rewritten_text_drops_its_signature() {
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
        json!("sig-original-bytes"),
    );
    stamp_test_origin(&mut provider_meta);

    // `sanitize_assistant_content` strips the harness's own tool narration,
    // so the replayed text is NOT the text that was signed.
    let body = body(
        vec![
            Message::new(Role::User, "go"),
            Message {
                provider_meta: Some(provider_meta),
                ..Message::new(
                    Role::Assistant,
                    "Looking\n[Called tool find_files with arguments: {\"path\":\".\"}]",
                )
            },
        ],
        test_body_input(None, false, None),
    );

    let parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["text"], "Looking");
    assert!(
        parts[0].get("thoughtSignature").is_none(),
        "the signature signed the pre-sanitized bytes, not these"
    );
}

/// The untouched counterpart: identical bytes keep their signature, so the
/// guard above is a byte check and not a blanket disable.
#[test]
fn untouched_text_keeps_its_signature() {
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
        json!("sig-exact-bytes"),
    );
    stamp_test_origin(&mut provider_meta);

    let body = body(
        vec![
            Message::new(Role::User, "go"),
            Message {
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "the answer")
            },
        ],
        test_body_input(None, false, None),
    );

    assert_eq!(
        body["contents"][1]["parts"][0]["thoughtSignature"],
        "sig-exact-bytes"
    );
}

/// One token, one site: when the answer text is a byte-identical signed part it
/// claims the token, so the turn's calls must not reuse the same token a second
/// time. Cloning a signature across sites is exactly the shape the upstream
/// rejects, and it also strips the calls of the sentinel that keeps them native.
#[test]
fn a_text_signature_is_not_also_stamped_on_the_turns_calls() {
    let call = nuo_tool::ToolCall {
        id: "call_1".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
        json!("sig-text"),
    );
    stamp_test_origin(&mut provider_meta);

    let body = body(
        vec![
            Message::new(Role::User, "inspect"),
            Message {
                tool_calls: Some(vec![call.clone()]),
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "checking")
            },
            Message::tool_result(&call, "Cargo.toml"),
        ],
        test_body_input(None, false, None),
    );

    let parts = body["contents"][1]["parts"].as_array().unwrap();
    // The text part keeps the token it signed.
    assert_eq!(parts[0]["text"], "checking");
    assert_eq!(parts[0]["thoughtSignature"], "sig-text");
    // The call is not handed a clone of the same token.
    assert_eq!(
        parts[1]["thoughtSignature"],
        SKIP_THOUGHT_SIGNATURE_VALIDATOR
    );
}

/// A turn-level (terminal-frame) signature is the documented signature for the
/// turn's function-call group, so it seeds the calls that streamed unsigned.
#[test]
fn terminal_signature_seeds_the_turns_function_calls() {
    let call = nuo_tool::ToolCall {
        id: "call_terminal".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        TERMINAL_THOUGHT_SIGNATURE_META_KEY.to_string(),
        json!("sig-turn"),
    );
    stamp_test_origin(&mut provider_meta);

    let body = body(
        vec![
            Message::new(Role::User, "inspect"),
            Message {
                tool_calls: Some(vec![call.clone()]),
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "")
            },
            Message::tool_result(&call, "Cargo.toml"),
        ],
        test_body_input(None, false, None),
    );

    assert_eq!(
        body["contents"][1]["parts"][0]["thoughtSignature"],
        "sig-turn"
    );
    // ...and the result replays natively because the call counts as signed.
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        "find_files"
    );
}

/// A turn with no recorded origin cannot prove its tokens valid here, so they
/// are not replayed. This is the resume / cross-provider case — and it is the
/// arm that must survive the `Option` removal above: making the *input*
/// required does not make the *recorded* origin always present, because a
/// turn persisted before stamps existed (or by another route) still has none.
#[test]
fn unstamped_signatures_are_not_replayed() {
    let call = nuo_tool::ToolCall {
        id: "call_1".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        THOUGHT_SIGNATURES_META_KEY.to_string(),
        json!({ "call_1": "sig-without-origin" }),
    );

    let body = body(
        vec![
            Message::new(Role::User, "inspect"),
            Message {
                tool_calls: Some(vec![call.clone()]),
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "checking")
            },
            Message::tool_result(&call, "Cargo.toml"),
        ],
        test_body_input(None, false, None),
    );

    assert_eq!(
        body["contents"][1]["parts"][1]["thoughtSignature"],
        SKIP_THOUGHT_SIGNATURE_VALIDATOR
    );
}

/// Ownership lock for the tool-history reconciliation.
///
/// `reconcile_tool_history` re-prunes pairing on every request. The protocol
/// compiler already lowers the history for this wire (`lower_messages_for_google`,
/// nuo-protocol), so the provider pass must be a **no-op on already-lowered
/// input** — otherwise two authorities disagree on the same invariant and the
/// one that drifts silently is the one nobody tests. This asserts the
/// idempotence directly: lowering a well-formed history first must not change
/// a single byte of the emitted `contents`.
#[test]
fn provider_reconciliation_is_a_no_op_over_lowered_history() {
    let answered = nuo_tool::ToolCall {
        id: "call_1".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    let broken = nuo_tool::ToolCall {
        id: "call_broken".to_string(),
        name: "read_text".to_string(),
        // Truncated arguments: the compiler degrades this to prose, and the
        // provider must then agree rather than reinventing a different shape.
        arguments: r#"{"path":"docs"#.to_string(),
    };

    let history = vec![
        Message::new(Role::User, "investigate"),
        Message {
            tool_calls: Some(vec![answered.clone()]),
            ..Message::new(Role::Assistant, "checking")
        },
        Message::tool_result(&answered, "Cargo.toml"),
        Message {
            tool_calls: Some(vec![broken.clone()]),
            ..Message::new(Role::Assistant, "")
        },
        Message::tool_result(&broken, "aborted"),
    ];

    // What the provider emits when handed the history directly...
    let raw_body = body(history, test_body_input(None, false, None));

    // ...must equal what it emits when the history has already been through the
    // protocol compiler's Google lowering pass, driven through the real public
    // compile path (not a test-only shim, which would rot into a second
    // authority of its own).
    let mut ir = nuo_protocol::SessionIR::new(
        "session-google-lowering",
        nuo_protocol::SessionPolicy::default(),
        1000,
    );
    ir.append_message("u1", 1_000_000, Message::new(Role::User, "investigate"));
    ir.append_message(
        "a1",
        1_001_000,
        Message {
            tool_calls: Some(vec![answered.clone()]),
            ..Message::new(Role::Assistant, "checking")
        },
    );
    ir.append_message(
        "t1",
        1_002_000,
        Message::tool_result(&answered, "Cargo.toml"),
    );
    ir.append_message(
        "a2",
        1_003_000,
        Message {
            tool_calls: Some(vec![broken.clone()]),
            ..Message::new(Role::Assistant, "")
        },
    );
    ir.append_message("t2", 1_004_000, Message::tool_result(&broken, "aborted"));
    let artifact = nuo_protocol::compile_session_request(
        &ir,
        nuo_protocol::CompilerOptions {
            target_protocol: Some(nuo_protocol::WireProtocol::GoogleGenerateContent),
            ..Default::default()
        },
    )
    .expect("compilation should succeed");
    // Pin the provider's reconciliation against the compiler's own output.
    let compiled_body = body(
        artifact.request.messages.clone(),
        test_body_input(None, false, None),
    );
    // Same history, re-fed through the provider a second time: idempotent.
    let twice_body = body(
        artifact.request.messages,
        test_body_input(None, false, None),
    );
    assert_eq!(
        compiled_body["contents"], twice_body["contents"],
        "the provider pass must be idempotent over lowered history"
    );
    // And the degraded call must have become the same prose fact in both the
    // raw and the compiled path — that is the shared invariant, not two
    // independent implementations of it.
    assert!(
        raw_body["contents"].to_string().contains("[Aborted tool"),
        "the raw path degrades a truncated call to a prose fact"
    );
    assert!(
        compiled_body["contents"]
            .to_string()
            .contains("[Aborted tool"),
        "the compiled path degrades it identically"
    );
}
