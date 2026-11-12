//! Tool-call replay, degradation, and prose sanitation.

use super::*;

#[test]
fn replays_tool_calls_and_results_as_function_parts() {
    let call = nuo_tool::ToolCall {
        id: "call_1".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        THOUGHT_SIGNATURES_META_KEY.to_string(),
        json!({ "call_1": "sig-1" }),
    );
    provider_meta.insert(
        TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
        json!("text-sig"),
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

    assert_eq!(
        body["contents"][1]["parts"][1]["functionCall"]["name"],
        "find_files"
    );
    assert_eq!(
        body["contents"][1]["parts"][1]["functionCall"]["args"]["path"],
        "."
    );
    assert_eq!(
        body["contents"][1]["parts"][0]["thoughtSignature"],
        "text-sig"
    );
    assert_eq!(body["contents"][1]["parts"][1]["thoughtSignature"], "sig-1");
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        "find_files"
    );
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["response"]["content"],
        "Cargo.toml"
    );
}

#[test]
fn strips_unanswered_tool_calls_and_orphan_results() {
    let answered = nuo_tool::ToolCall {
        id: "answered".to_string(),
        name: "find_files".to_string(),
        arguments: "{}".to_string(),
    };
    let unanswered = nuo_tool::ToolCall {
        id: "unanswered".to_string(),
        name: "grep".to_string(),
        arguments: "{}".to_string(),
    };
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        THOUGHT_SIGNATURES_META_KEY.to_string(),
        json!({ "answered": "sig-answered" }),
    );
    stamp_test_origin(&mut provider_meta);
    let body = body(
        vec![
            Message::new(Role::User, "go"),
            Message {
                tool_calls: Some(vec![answered.clone(), unanswered]),
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "")
            },
            Message::tool_result(&answered, "{}"),
            Message {
                tool_call_id: Some("orphan".to_string()),
                ..Message::new(Role::Tool, "ignored")
            },
        ],
        test_body_input(None, false, None),
    );

    let parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["functionCall"]["id"], "answered");
    assert_eq!(parts[0]["thoughtSignature"], "sig-answered");
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["id"],
        "answered"
    );
}

#[test]
fn replays_unsigned_assistant_turn_tool_results_with_skip_thought_signature_validator() {
    let lost_call = nuo_tool::ToolCall {
        id: "toolu_vrtx_01LostSignature".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"docs"}"#.to_string(),
    };
    let kept_call = nuo_tool::ToolCall {
        id: "toolu_vrtx_01KeptSignature".to_string(),
        name: "code_query".to_string(),
        arguments: "{}".to_string(),
    };
    // Turn 1 keeps its signature material; turn 2 lost provider_meta.
    let mut signed_meta = serde_json::Map::new();
    signed_meta.insert(
        THOUGHT_SIGNATURES_META_KEY.to_string(),
        json!({ "toolu_vrtx_01KeptSignature": "sig-kept" }),
    );
    stamp_test_origin(&mut signed_meta);
    let body = body(
        vec![
            Message::new(Role::User, "investigate"),
            Message {
                tool_calls: Some(vec![kept_call.clone()]),
                provider_meta: Some(signed_meta),
                ..Message::new(Role::Assistant, "planning")
            },
            Message::tool_result(&kept_call, "outline text"),
            Message {
                tool_calls: Some(vec![lost_call.clone()]),
                provider_meta: None,
                ..Message::new(Role::Assistant, "")
            },
            Message::tool_result(&lost_call, "file list"),
            Message::new(Role::User, "continue"),
        ],
        test_body_input(None, false, None),
    );

    // Turn 1's signed pair replays natively
    let kept_parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(
        kept_parts[1]["functionCall"]["id"],
        "toolu_vrtx_01KeptSignature"
    );
    assert_eq!(kept_parts[1]["thoughtSignature"], "sig-kept");

    // Turn 2's unsigned call replays natively with the documented Google bypass sentinel
    let lost_parts = body["contents"][3]["parts"].as_array().unwrap();
    assert_eq!(
        lost_parts[0]["functionCall"]["id"],
        "toolu_vrtx_01LostSignature"
    );
    assert_eq!(
        lost_parts[0]["thoughtSignature"],
        SKIP_THOUGHT_SIGNATURE_VALIDATOR
    );

    // Turn 2's tool result replays natively as functionResponse
    let tail = body["contents"][4]["parts"].as_array().unwrap();
    assert_eq!(
        tail[0]["functionResponse"]["id"],
        "toolu_vrtx_01LostSignature"
    );
    assert_eq!(
        tail[0]["functionResponse"]["response"]["content"],
        "file list"
    );
    assert_eq!(tail[1]["text"], "continue");
}

#[test]
fn text_signature_alone_still_replays_calls_and_results() {
    // A turn with only a text-part thought signature (no per-call map)
    // falls back to signing every call on the turn, so its results stay
    // native and must not be degraded.
    let call = nuo_tool::ToolCall {
        id: "toolu_vrtx_01TextSigOnly".to_string(),
        name: "read_text".to_string(),
        arguments: "{}".to_string(),
    };
    let mut text_sig_meta = serde_json::Map::new();
    text_sig_meta.insert(
        TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
        json!("sig-text-fallback"),
    );
    stamp_test_origin(&mut text_sig_meta);
    let body = body(
        vec![
            Message::new(Role::User, "read it"),
            Message {
                tool_calls: Some(vec![call.clone()]),
                provider_meta: Some(text_sig_meta),
                ..Message::new(Role::Assistant, "")
            },
            Message::tool_result(&call, "bytes"),
        ],
        test_body_input(None, false, None),
    );

    let parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["functionCall"]["id"], "toolu_vrtx_01TextSigOnly");
    let result = body["contents"][2]["parts"][0]["functionResponse"].clone();
    assert_eq!(result["id"], "toolu_vrtx_01TextSigOnly");
}

#[test]
fn unsigned_tool_call_uses_skip_sentinel_and_does_not_borrow_earlier_signature_by_name() {
    // ADR-0046: Earlier turn signed `read_text`. A later turn ran `read_text`
    // without signatures. The later result must NOT borrow the earlier turn's
    // signature by name; it must use the documented skip sentinel.
    let unsigned_dup = nuo_tool::ToolCall {
        id: "call_unsigned_dup".to_string(),
        name: "read_text".to_string(), // same name as the signed earlier call
        arguments: "{}".to_string(),
    };
    let signed_dup = nuo_tool::ToolCall {
        id: "call_signed_dup".to_string(),
        name: "read_text".to_string(),
        arguments: "{}".to_string(),
    };
    let mut signed_meta = serde_json::Map::new();
    signed_meta.insert(
        THOUGHT_SIGNATURES_META_KEY.to_string(),
        json!({ "call_signed_dup": "sig-earlier" }),
    );
    stamp_test_origin(&mut signed_meta);
    let body = body(
        vec![
            Message::new(Role::User, "go"),
            Message {
                tool_calls: Some(vec![signed_dup.clone()]),
                provider_meta: Some(signed_meta),
                ..Message::new(Role::Assistant, "")
            },
            Message::tool_result(&signed_dup, "earlier"),
            Message {
                tool_calls: Some(vec![unsigned_dup.clone()]),
                provider_meta: None, // this turn lost its signatures
                ..Message::new(Role::Assistant, "")
            },
            Message::tool_result(&unsigned_dup, "later output"),
        ],
        test_body_input(None, false, None),
    );

    // Turn 1 carries the earlier signature
    let earlier_parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(earlier_parts[0]["thoughtSignature"], "sig-earlier");

    // The unsigned later call replays natively with the skip sentinel rather than borrowing sig-earlier
    let later_parts = body["contents"][3]["parts"].as_array().unwrap();
    assert_eq!(later_parts[0]["functionCall"]["id"], "call_unsigned_dup");
    assert_eq!(
        later_parts[0]["thoughtSignature"],
        SKIP_THOUGHT_SIGNATURE_VALIDATOR
    );
    let tail = body["contents"][4]["parts"].as_array().unwrap();
    assert_eq!(tail[0]["functionResponse"]["id"], "call_unsigned_dup");
}

#[test]
fn replays_unsigned_foreign_tool_calls_with_skip_thought_signature_validator() {
    let call = nuo_tool::ToolCall {
        id: "call_foreign".to_string(),
        name: "write_todos".to_string(),
        arguments: r#"{"items":[{"content":"design","status":"in_progress"}]}"#.to_string(),
    };
    // Unsigned assistant message (e.g. from Claude/GPT or DeepSeek, no provider_meta)
    let body = body(
        vec![
            Message::new(Role::User, "create tasks"),
            Message {
                tool_calls: Some(vec![call.clone()]),
                provider_meta: None,
                ..Message::new(Role::Assistant, "Planning tasks")
            },
            Message::tool_result(&call, "Todo list updated"),
            Message::new(Role::User, "proceed"),
        ],
        test_body_input(None, false, None),
    );

    // Foreign tool call replays natively with documented Google bypass sentinel
    let model_parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(model_parts.len(), 2);
    assert_eq!(model_parts[0]["text"], "Planning tasks");
    assert_eq!(model_parts[1]["functionCall"]["name"], "write_todos");
    assert_eq!(
        model_parts[1]["thoughtSignature"],
        SKIP_THOUGHT_SIGNATURE_VALIDATOR
    );

    // Tool result replays natively as functionResponse, followed by user prompt
    assert_eq!(body["contents"][2]["role"], "user");
    let user_parts = body["contents"][2]["parts"].as_array().unwrap();
    assert_eq!(user_parts[0]["functionResponse"]["name"], "write_todos");
    assert_eq!(
        user_parts[0]["functionResponse"]["response"]["content"],
        "Todo list updated"
    );
    assert_eq!(user_parts[1]["text"], "proceed");
}

#[test]
fn degrades_incomplete_truncated_unsigned_tool_call_without_syntax_leaks() {
    let broken_call = nuo_tool::ToolCall {
        id: "call_broken".to_string(),
        name: "read_text".to_string(),
        arguments: r#"{"limit":120,"offset":1,"path": "docs/adr/018"#.to_string(),
    };
    let body = body(
        vec![
            Message::new(Role::User, "read file"),
            Message {
                tool_calls: Some(vec![broken_call.clone()]),
                provider_meta: None,
                ..Message::new(Role::Assistant, "Inspecting path")
            },
            Message::tool_result(&broken_call, "Operation aborted"),
        ],
        test_body_input(None, false, None),
    );

    let model_parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(model_parts.len(), 2);
    assert_eq!(model_parts[0]["text"], "Inspecting path");
    // Incomplete / truncated arguments must not leak raw syntax debris
    assert_eq!(
        model_parts[1]["text"],
        r#"[Aborted tool "read_text" (incomplete arguments)]"#
    );
    assert!(model_parts[1].get("functionCall").is_none());
}

#[test]
fn replays_mixed_signed_and_unsigned_tool_calls_natively() {
    let signed_call = nuo_tool::ToolCall {
        id: "call_signed".to_string(),
        name: "find_files".to_string(),
        arguments: r#"{"path":"."}"#.to_string(),
    };
    let unsigned_call = nuo_tool::ToolCall {
        id: "call_unsigned".to_string(),
        name: "default_api:execute_command".to_string(),
        arguments: r#"{"command":"cargo test"}"#.to_string(),
    };
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        THOUGHT_SIGNATURES_META_KEY.to_string(),
        json!({ "call_signed": "sig-signed-call" }),
    );
    stamp_test_origin(&mut provider_meta);

    let body = body(
        vec![
            Message::new(Role::User, "run task"),
            Message {
                tool_calls: Some(vec![signed_call.clone(), unsigned_call.clone()]),
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "Running tools")
            },
            Message::tool_result(&signed_call, "Cargo.toml"),
            Message::tool_result(&unsigned_call, "tests passed"),
        ],
        test_body_input(None, false, None),
    );

    let model_parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(model_parts.len(), 3);
    assert_eq!(model_parts[0]["text"], "Running tools");
    // Signed call remains native functionCall with signature
    assert_eq!(model_parts[1]["functionCall"]["name"], "find_files");
    assert_eq!(model_parts[1]["thoughtSignature"], "sig-signed-call");
    // Unsigned call replays natively with skip sentinel
    assert_eq!(
        model_parts[2]["functionCall"]["name"],
        "default_api:execute_command"
    );
    assert_eq!(
        model_parts[2]["thoughtSignature"],
        SKIP_THOUGHT_SIGNATURE_VALIDATOR
    );

    let user_parts = body["contents"][2]["parts"].as_array().unwrap();
    assert_eq!(user_parts.len(), 2);
    // Both results are native functionResponses
    assert_eq!(user_parts[0]["functionResponse"]["name"], "find_files");
    assert_eq!(
        user_parts[1]["functionResponse"]["name"],
        "default_api:execute_command"
    );
}

#[test]
fn sanitizes_legacy_mimicked_tool_call_text_from_assistant_prose() {
    let body = body(
        vec![
            Message::new(Role::User, "do something"),
            Message::new(
                Role::Assistant,
                "[Called tool read_text with arguments: {\"limit\": 90, \"offset\": 100}]",
            ),
            Message::new(Role::User, "pls go on"),
        ],
        test_body_input(None, false, None),
    );

    let model_turn = &body["contents"][1];
    let model_parts = model_turn["parts"].as_array().unwrap();
    assert_eq!(model_parts[0]["text"], "");
}

#[test]
fn sanitizes_legacy_mimicked_tool_call_with_inner_brackets() {
    let body = body(
        vec![
            Message::new(Role::User, "save draft"),
            Message::new(
                Role::Assistant,
                "[Called tool write_file with arguments: {\"content\": \"# Doc\\n[ADR-0080]: link\\n[item1, item2]\"}]",
            ),
            Message::new(Role::User, "continue"),
        ],
        test_body_input(None, false, None),
    );

    let model_turn = &body["contents"][1];
    let model_parts = model_turn["parts"].as_array().unwrap();
    // Inner brackets in the arguments object must never cause premature truncation
    assert_eq!(model_parts[0]["text"], "");
}

#[test]
fn sanitizes_executed_tool_facts_with_markdown_links_and_brackets() {
    let prose = r#"I have recorded the architecture decision:

[Executed tool "write_file" with arguments: {"content":"--- id: ADR-0080 title: \"Time-Varying Rate Cards\"\nscope: provider/nuo-provider\n---\n# 0080\nSee [ADR-0065] for details."}]"#;
    let body = body(
        vec![
            Message::new(Role::User, "write adr"),
            Message::new(Role::Assistant, prose),
            Message::new(Role::User, "next step"),
        ],
        test_body_input(None, false, None),
    );

    let model_turn = &body["contents"][1];
    let model_parts = model_turn["parts"].as_array().unwrap();
    // The historical executed fact is cleanly stripped from prose so the model is not prompted to mimic it
    assert_eq!(
        model_parts[0]["text"],
        "I have recorded the architecture decision:"
    );
}
