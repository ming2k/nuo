use super::*;

fn body_of(messages: Vec<nuo_tool::Message>, input: BodyInput<'_>) -> Value {
    body(&messages, &[], input)
}

fn body_with_caps_of(
    messages: Vec<nuo_tool::Message>,
    input: BodyInput<'_>,
    capabilities: &nuo_protocol::ModelCapabilities,
) -> Value {
    body_with_capabilities(&messages, &[], input, capabilities)
}
use nuo_tool::ToolCall;

static DEFAULT_CACHE_PLAN: nuo_protocol::ResolvedCachePolicy =
    nuo_protocol::ResolvedCachePolicy::Unsupported;

fn test_body_input<'a>(
    model: &'a str,
    stream: bool,
    tool_specs: Option<&'a [nuo_protocol::capability::ToolSpec]>,
    reasoning_effort: Option<Effort>,
    cache_plan: &'a nuo_protocol::ResolvedCachePolicy,
) -> BodyInput<'a> {
    BodyInput {
        model,
        stream,
        instructions: None,
        tool_specs,
        reasoning_effort,
        dialect: OpenAiChatDialect::Standard,
        cache_plan,
    }
}

/// Route capabilities with only the vision declaration varied, for the
/// image-projection tests below.
fn caps_with_vision(vision: Option<bool>) -> nuo_protocol::ModelCapabilities {
    nuo_protocol::ModelCapabilities {
        family: "test".into(),
        context_window: 200_000,
        max_output_tokens: None,
        thinking: nuo_protocol::ReasoningSupport::None,
        tool_call: true,
        vision,
        effort_levels: Vec::new(),
        effort_entities: Vec::new(),
        default_effort: None,
    }
}

fn image_message(text: &str) -> Message {
    Message::new(Role::User, text).with_images(vec![nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "aGk=".to_string(),
    }])
}

#[test]
fn declared_text_only_route_strips_image_parts() {
    let body = body_with_caps_of(
        vec![image_message("look")],
        test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
        &caps_with_vision(Some(false)),
    );

    // The prose survives as a plain string; the `image_url` part is gone, so
    // the API cannot reject the request for it.
    assert_eq!(body["messages"][0]["content"], "look");
}

#[test]
fn undeclared_route_keeps_image_parts() {
    // An endpoint that advertises no vision field must not have
    // its images silently stripped — the request goes out as-is and a
    // provider that cannot take it says so.
    let body = body_with_caps_of(
        vec![image_message("look")],
        test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
        &caps_with_vision(None),
    );

    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
    assert_eq!(
        body["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,aGk="
    );
}

#[test]
fn request_filters_empty_assistant_history() {
    let body = body_of(
        vec![
            Message::new(Role::User, "hello"),
            Message::new(Role::Assistant, ""),
            Message::new(Role::User, "again"),
        ],
        test_body_input("test-model", true, None, None, &DEFAULT_CACHE_PLAN),
    );

    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(body["messages"][1]["content"], "again");
}

#[test]
fn request_includes_reasoning_effort_when_configured() {
    let body = body_of(
        vec![Message::new(Role::User, "think")],
        test_body_input(
            "gpt-5.5",
            false,
            None,
            Some(Effort::Xhigh),
            &DEFAULT_CACHE_PLAN,
        ),
    );

    assert_eq!(body["reasoning_effort"], "xhigh");
}

/// The DeepSeek capability row this provider's chat surface can be pointed
/// at: reasoning via `reasoning_content`, the full native ladder.
fn deepseek_capabilities() -> nuo_protocol::ModelCapabilities {
    nuo_protocol::ModelCapabilities {
        family: "deepseek".into(),
        context_window: 1_000_000,
        max_output_tokens: None,
        thinking: nuo_protocol::ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: Some(true),
        effort_levels: vec![
            Effort::None.into(),
            Effort::Minimal.into(),
            Effort::Low.into(),
            Effort::Medium.into(),
            Effort::High.into(),
            Effort::Xhigh.into(),
            Effort::Max.into(),
            Effort::Ultra.into(),
        ],
        effort_entities: Vec::new(),
        default_effort: None,
    }
}

fn assistant_that_thought() -> (Message, Message) {
    let mut assistant = Message::new(Role::Assistant, "");
    assistant.reasoning_content = Some("checking".into());
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call_1".into(),
        name: "read".into(),
        arguments: "{}".into(),
    }]);
    let tool_result = Message {
        role: Role::Tool,
        content: "ok".into(),
        tool_call_id: Some("call_1".into()),
        ..Message::new(Role::Tool, "")
    };
    (assistant, tool_result)
}

#[test]
fn deepseek_replays_reasoning_content_and_passes_none_through() {
    let (assistant, tool_result) = assistant_that_thought();
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "inspect"), assistant, tool_result],
        BodyInput {
            model: "deepseek-flash",
            stream: true,
            instructions: None,
            tool_specs: None,
            // `none` is the thinking off switch on this surface; the clamp
            // must not snap it up to `low`.
            reasoning_effort: Some(Effort::None),
            dialect: OpenAiChatDialect::DeepSeek,
            cache_plan: &DEFAULT_CACHE_PLAN,
        },
        &deepseek_capabilities(),
    );

    assert_eq!(body["reasoning_effort"], "none");
    assert_eq!(
        body["messages"][1]["reasoning_content"], "checking",
        "DeepSeek concatenates a replayed CoT into the context (ADR-0083)"
    );
    assert!(
        body["messages"][1].get("reasoning").is_none(),
        "the documented field is `reasoning_content`, not OpenRouter's `reasoning`"
    );
}

#[test]
fn deepseek_replays_empty_reasoning_content_when_tool_calls_present_without_reasoning() {
    let (mut assistant, tool_result) = assistant_that_thought();
    assistant.reasoning_content = None;
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "inspect"), assistant, tool_result],
        BodyInput {
            model: "deepseek-flash",
            stream: true,
            instructions: None,
            tool_specs: None,
            reasoning_effort: None,
            dialect: OpenAiChatDialect::DeepSeek,
            cache_plan: &DEFAULT_CACHE_PLAN,
        },
        &deepseek_capabilities(),
    );

    assert_eq!(
        body["messages"][1]["reasoning_content"], "",
        "DeepSeek requires reasoning_content field on all tool calls even if reasoning was pruned"
    );
}

#[test]
fn deepseek_prunes_historical_reasoning_content_on_older_turns() {
    let (mut assistant1, tool1) = assistant_that_thought();
    assistant1.reasoning_content = Some("ancient thoughts".into());
    let (mut assistant2, tool2) = assistant_that_thought();
    assistant2.reasoning_content = Some("latest thoughts".into());

    let body = body_with_caps_of(
        vec![
            Message::new(Role::User, "first"),
            assistant1,
            tool1,
            Message::new(Role::User, "second"),
            assistant2,
            tool2,
        ],
        BodyInput {
            model: "deepseek-flash",
            stream: true,
            instructions: None,
            tool_specs: None,
            reasoning_effort: None,
            dialect: OpenAiChatDialect::DeepSeek,
            cache_plan: &DEFAULT_CACHE_PLAN,
        },
        &deepseek_capabilities(),
    );

    // Older assistant turn (index 1): reasoning_content is pruned to ""
    assert_eq!(body["messages"][1]["reasoning_content"], "");
    // Latest assistant turn (index 4): reasoning_content is preserved in full
    assert_eq!(body["messages"][4]["reasoning_content"], "latest thoughts");
}

#[test]
fn standard_chat_surface_never_replays_reasoning_content() {
    let (assistant, tool_result) = assistant_that_thought();
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "inspect"), assistant, tool_result],
        BodyInput {
            model: "deepseek-flash",
            stream: true,
            instructions: None,
            tool_specs: None,
            reasoning_effort: Some(Effort::High),
            dialect: OpenAiChatDialect::Standard,
            cache_plan: &DEFAULT_CACHE_PLAN,
        },
        &deepseek_capabilities(),
    );

    assert!(
        body["messages"][1].get("reasoning_content").is_none(),
        "an OpenAI-compatible surface that defines no replayable CoT gets none"
    );
}

#[test]
fn openrouter_uses_unified_reasoning_and_replays_details() {
    let mut assistant = Message::new(Role::Assistant, "");
    assistant.reasoning_content = Some("checking".into());
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call_1".into(),
        name: "read".into(),
        arguments: "{}".into(),
    }]);
    assistant.provider_meta = Some({
        let mut meta = serde_json::Map::new();
        meta.insert(
            super::super::response::OPENROUTER_REASONING_DETAILS_META_KEY.into(),
            serde_json::json!([{
                "type": "reasoning.text",
                "text": "checking",
                "signature": "sig",
                "index": 0
            }]),
        );
        meta
    });
    let tool_result = Message {
        role: Role::Tool,
        content: "ok".into(),
        tool_call_id: Some("call_1".into()),
        ..Message::new(Role::Tool, "")
    };
    let capabilities = nuo_protocol::ModelCapabilities {
        family: "nex".into(),
        context_window: 262_144,
        max_output_tokens: Some(235_929),
        thinking: nuo_protocol::ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: Some(true),
        effort_levels: vec![
            Effort::None.into(),
            Effort::Medium.into(),
            Effort::High.into(),
        ],
        effort_entities: Vec::new(),
        default_effort: None,
    };
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "inspect"), assistant, tool_result],
        BodyInput {
            model: "nex-agi/nex-n2.5-pro:free",
            stream: true,
            instructions: None,
            tool_specs: None,
            reasoning_effort: Some(Effort::High),
            dialect: OpenAiChatDialect::OpenRouter,
            cache_plan: &DEFAULT_CACHE_PLAN,
        },
        &capabilities,
    );

    assert_eq!(body["reasoning"]["effort"], "high");
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(
        body["messages"][1]["reasoning_details"][0]["signature"],
        "sig"
    );
}

#[test]
fn openrouter_headers_include_app_attribution() {
    let headers = headers("sk-or-test", OpenAiChatDialect::OpenRouter);
    assert!(
        headers
            .iter()
            .any(|(name, value)| { *name == "Authorization" && value == "Bearer sk-or-test" })
    );
    assert!(
        headers
            .iter()
            .any(|(name, value)| { *name == "X-OpenRouter-Title" && value == "Nuo" })
    );
}

#[test]
fn request_injects_prompt_cache_key_when_present() {
    let cache_plan = nuo_protocol::ResolvedCachePolicy::Enabled {
        mode: nuo_protocol::PromptCacheMode::Implicit,
        retention: None,
        routing_key: Some("session-42".into()),
        max_breakpoints: None,
    };
    let body = body_of(
        vec![Message::new(Role::User, "hi")],
        test_body_input("kimi-k2.7-code", false, None, None, &cache_plan),
    );
    assert_eq!(body["prompt_cache_key"], "session-42");
}

#[test]
fn request_omits_prompt_cache_key_when_absent() {
    let body = body_of(
        vec![Message::new(Role::User, "hi")],
        test_body_input("gpt-5.5", false, None, None, &DEFAULT_CACHE_PLAN),
    );
    assert!(body.get("prompt_cache_key").is_none());
}

#[test]
fn request_drops_orphan_tool_results() {
    let matched = ToolCall {
        id: "call_matched".to_string(),
        name: "read_text".to_string(),
        arguments: "{}".to_string(),
    };
    let assistant_with_call = Message {
        role: Role::Assistant,
        content: String::new(),
        content_blob: None,
        display_content: None,
        reasoning_content: None,
        provider_meta: None,
        tool_calls: Some(vec![matched.clone()]),
        tool_call_id: None,
        images: None,
        provider: None,
        model: None,
        effort: None,
        hidden: false,
        children: None,
        subagent_meta: None,
        origin: None,
        timestamp: None,
        sent_at_ms: None,
    };
    let good_result = Message {
        role: Role::Tool,
        content: "ok".to_string(),
        tool_call_id: Some("call_matched".to_string()),
        ..Message::new(Role::Tool, "")
    };
    let orphan_result = Message {
        tool_call_id: Some("call_orphan".to_string()),
        ..Message::new(Role::Tool, "orphan")
    };
    let empty_id_result = Message {
        tool_call_id: Some(String::new()),
        ..Message::new(Role::Tool, "empty id")
    };

    let body = body_of(
        vec![
            Message::new(Role::User, "hi"),
            assistant_with_call,
            good_result,
            orphan_result,
            empty_id_result,
        ],
        test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
    );

    let messages = body["messages"].as_array().unwrap();
    // user, assistant(tool_calls), and only the matched tool result survive.
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "call_matched");
}

#[test]
fn strips_unanswered_tool_calls() {
    let call_a = ToolCall {
        id: "a".into(),
        name: "bash".into(),
        arguments: "{}".into(),
    };
    let call_b = ToolCall {
        id: "b".into(),
        name: "read_text".into(),
        arguments: "{}".into(),
    };
    let call_c = ToolCall {
        id: "c".into(),
        name: "grep".into(),
        arguments: "{}".into(),
    };

    // Case 1: trailing unanswered assistant (the original bug)
    let body = body_of(
        vec![
            Message::new(Role::User, "go"),
            Message {
                tool_calls: Some(vec![call_a.clone()]),
                ..Message::new(Role::Assistant, "")
            },
        ],
        test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
    );
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(
        msgs.len(),
        1,
        "trailing unanswered assistant must be dropped"
    );

    // Case 2: assistant with content but no tool result
    let body = body_of(
        vec![
            Message::new(Role::User, "go"),
            Message {
                tool_calls: Some(vec![call_a.clone()]),
                ..Message::new(Role::Assistant, "let me think")
            },
        ],
        test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
    );
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert!(
        msgs[1]["content"]
            .as_str()
            .unwrap_or("")
            .contains("let me think")
    );
    assert!(
        msgs[1].get("tool_calls").is_none(),
        "unanswered calls must be stripped"
    );

    // Case 3: partially answered call set
    let body = body_of(
        vec![
            Message::new(Role::User, "go"),
            Message {
                tool_calls: Some(vec![call_a.clone(), call_b.clone()]),
                ..Message::new(Role::Assistant, "")
            },
            Message {
                tool_call_id: Some("a".into()),
                ..Message::new(Role::Tool, "result a")
            },
        ],
        test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
    );
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    let calls = msgs[1]["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "a");

    // Case 4: multiple consecutive unanswered assistants
    let body = body_of(
        vec![
            Message::new(Role::User, "go"),
            Message {
                tool_calls: Some(vec![call_a.clone()]),
                ..Message::new(Role::Assistant, "first")
            },
            Message {
                tool_calls: Some(vec![call_b.clone()]),
                ..Message::new(Role::Assistant, "second")
            },
        ],
        test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
    );
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3, "both assistants kept for their content");
    assert!(msgs[1].get("tool_calls").is_none());
    assert!(msgs[2].get("tool_calls").is_none());

    // Case 5: fully healthy conversation (no stripping)
    let body = body_of(
        vec![
            Message::new(Role::User, "go"),
            Message {
                tool_calls: Some(vec![call_a.clone(), call_c.clone()]),
                ..Message::new(Role::Assistant, "")
            },
            Message {
                tool_call_id: Some("a".into()),
                ..Message::new(Role::Tool, "ok")
            },
            Message {
                tool_call_id: Some("c".into()),
                ..Message::new(Role::Tool, "ok")
            },
            Message::new(Role::Assistant, "all done"),
        ],
        test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
    );
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 5, "healthy conversation untouched");
    assert_eq!(msgs[1]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(msgs[4]["content"], "all done");
}
