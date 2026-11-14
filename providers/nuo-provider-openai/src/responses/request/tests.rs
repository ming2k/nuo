use super::*;

fn body_of(
    messages: Vec<nuo_tool::Message>,
    input: BodyInput<'_>,
) -> Result<Value, RequestBuildError> {
    body(&messages, &[], input)
}

fn body_with_caps_of(
    messages: Vec<nuo_tool::Message>,
    input: BodyInput<'_>,
    capabilities: &nuo_protocol::ModelCapabilities,
) -> Result<Value, RequestBuildError> {
    body_with_capabilities(&messages, &[], input, capabilities)
}
use nuo_tool::ToolCall;

static DEFAULT_DELIVERY: nuo_protocol::RequestDelivery = nuo_protocol::RequestDelivery::FullReplay;
static DEFAULT_CACHE_PLAN: nuo_protocol::ResolvedCachePolicy =
    nuo_protocol::ResolvedCachePolicy::Unsupported;

fn test_body_input<'a>(
    model: &'a str,
    stream: bool,
    tool_specs: Option<&'a [nuo_protocol::capability::ToolSpec]>,
    reasoning_effort: Option<Effort>,
    delivery: &'a nuo_protocol::RequestDelivery,
    store: bool,
    cache_plan: &'a nuo_protocol::ResolvedCachePolicy,
) -> BodyInput<'a> {
    BodyInput {
        model,
        stream,
        instructions: None,
        tool_specs,
        reasoning_effort,
        delivery,
        store,
        dialect: nuo_protocol::OpenAiResponsesDialect::Standard,
        cache_plan,
    }
}

fn assistant_with_call(call: ToolCall, content: &str) -> Message {
    Message {
        tool_calls: Some(vec![call]),
        ..Message::new(Role::Assistant, content)
    }
}

/// A portable-gateway route: same capabilities as the Go relay advertises
/// for its DeepSeek rows, but the dialect is the transcoding one.
fn portable_body_input<'a>(
    delivery: &'a nuo_protocol::RequestDelivery,
    cache_plan: &'a nuo_protocol::ResolvedCachePolicy,
) -> BodyInput<'a> {
    BodyInput {
        model: "deepseek-v4.1-flash",
        stream: true,
        instructions: None,
        tool_specs: None,
        reasoning_effort: Some(Effort::High),
        delivery,
        store: false,
        dialect: nuo_protocol::OpenAiResponsesDialect::Portable,
        cache_plan,
    }
}

/// The portable gateway's request shape, asserted field by field against
/// what the live endpoint accepts (ADR-0142). Its two failure modes are
/// silent until a turn 400s, so they are pinned here: a `store` field and
/// a nested tool definition are both rejected upstream.
#[test]
fn portable_dialect_emits_flat_tools_and_never_store() {
    let tool = nuo_protocol::capability::ToolSpec {
        name: "test_tool".into(),
        description: "test".into(),
        parameters: json!({"type": "object"}),
    };
    let specs = [tool];
    let mut input = portable_body_input(&DEFAULT_DELIVERY, &DEFAULT_CACHE_PLAN);
    input.tool_specs = Some(&specs);
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "hi")],
        input,
        &deepseek_capabilities(),
    )
    .unwrap();

    assert_eq!(
        body["tools"][0]["name"], "test_tool",
        "a translating gateway validates the flat OpenAI tool shape; the nested \
         `function` object answers 400: requires a native Responses target"
    );
    assert!(
        body["tools"][0].get("function").is_none(),
        "the nested form is DeepSeek's native schema, not this gateway's"
    );
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert!(
        body.get("store").is_none(),
        "`store: true` answers 400 (requires a native Responses target); the field \
         is absent entirely because the surface declares Stateless (ADR-0114)"
    );
    assert_eq!(
        body["include"],
        json!(["reasoning.encrypted_content"]),
        "a stateless Responses client replays opaque reasoning state"
    );
    assert_eq!(body["reasoning"]["summary"], "detailed");
    assert_eq!(body["reasoning"]["effort"], "high");
}

/// Replaying an assistant turn on the portable surface must satisfy both
/// upstream replay contracts in one item: a `summary` array for the
/// upstreams that demand it, and the raw `reasoning_text` content parts for
/// the ones that demand those. Each alone 400s against the other.
#[test]
fn portable_dialect_replays_reasoning_in_both_forms() {
    let mut assistant = Message::new(Role::Assistant, "");
    assistant.reasoning_content = Some("plan the call".to_string());
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call_1".to_string(),
        name: "test_tool".to_string(),
        arguments: "{}".to_string(),
    }]);

    let body = body_with_caps_of(
        vec![Message::new(Role::User, "hi"), assistant],
        portable_body_input(&DEFAULT_DELIVERY, &DEFAULT_CACHE_PLAN),
        &deepseek_capabilities(),
    )
    .unwrap();

    let items = body["input"].as_array().unwrap();
    let reasoning = items
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("a reasoning item must precede the function call");
    assert_eq!(
        reasoning["summary"][0]["type"], "summary_text",
        "one upstream 400s with `input[N].summary: expected an array`"
    );
    assert_eq!(reasoning["summary"][0]["text"], "plan the call");
    assert_eq!(
        reasoning["content"][0]["type"], "reasoning_text",
        "the other upstream 400s: `The reasoning_text in the thinking mode must be \
         passed back to the API`"
    );
    assert_eq!(reasoning["content"][0]["text"], "plan the call");
}

/// A pruned older turn on the portable surface keeps the `summary` array
/// but drops the `content` parts: an empty `reasoning_text` is itself
/// rejected as "must be passed back", while an empty `summary` is accepted.
#[test]
fn portable_dialect_prunes_older_reasoning_to_an_empty_summary() {
    let mut older = Message::new(Role::Assistant, "");
    older.reasoning_content = Some("old chain".to_string());
    older.tool_calls = Some(vec![ToolCall {
        id: "call_1".to_string(),
        name: "test_tool".to_string(),
        arguments: "{}".to_string(),
    }]);
    let mut latest = Message::new(Role::Assistant, "");
    latest.reasoning_content = Some("new chain".to_string());
    latest.tool_calls = Some(vec![ToolCall {
        id: "call_2".to_string(),
        name: "test_tool".to_string(),
        arguments: "{}".to_string(),
    }]);

    let body = body_with_caps_of(
        vec![Message::new(Role::User, "hi"), older, latest],
        portable_body_input(&DEFAULT_DELIVERY, &DEFAULT_CACHE_PLAN),
        &deepseek_capabilities(),
    )
    .unwrap();

    let items = body["input"].as_array().unwrap();
    let reasoning: Vec<&Value> = items
        .iter()
        .filter(|item| item["type"] == "reasoning")
        .collect();
    assert_eq!(reasoning.len(), 2, "both assistant turns carry an item");
    let older_item = reasoning[0];
    assert_eq!(older_item["summary"], json!([]));
    assert!(
        older_item.get("content").is_none(),
        "an empty `reasoning_text` is rejected upstream, so the part is omitted"
    );
    let latest_item = reasoning[1];
    assert_eq!(latest_item["summary"][0]["text"], "new chain");
    assert_eq!(latest_item["content"][0]["text"], "new chain");
}

/// A plain conversational assistant turn carries no reasoning item. The
/// dual form is a *repair* for the shapes the wire demands, not a blanket
/// tax on every turn: emitting an empty item per reply would bloat each
/// request with items that no upstream asked for.
#[test]
fn portable_dialect_omits_reasoning_on_plain_text_turns() {
    let body = body_with_caps_of(
        vec![
            Message::new(Role::User, "hi"),
            Message::new(Role::Assistant, "Hello!"),
            Message::new(Role::Assistant, "How can I help?"),
            Message::new(Role::User, "say ok"),
        ],
        portable_body_input(&DEFAULT_DELIVERY, &DEFAULT_CACHE_PLAN),
        &deepseek_capabilities(),
    )
    .unwrap();

    let items = body["input"].as_array().unwrap();
    assert!(
        !items.iter().any(|item| item["type"] == "reasoning"),
        "no tool call and no recorded chain means no reasoning item"
    );
}

/// A *latest* turn that carries a real chain replays it in both forms: the
/// upstream continuing the turn needs the raw text back.
#[test]
fn portable_dialect_replays_the_latest_chain_in_both_forms() {
    let mut assistant = Message::new(Role::Assistant, "done");
    assistant.reasoning_content = Some("reasoned it through".to_string());

    let body = body_with_caps_of(
        vec![Message::new(Role::User, "hi"), assistant],
        portable_body_input(&DEFAULT_DELIVERY, &DEFAULT_CACHE_PLAN),
        &deepseek_capabilities(),
    )
    .unwrap();

    let items = body["input"].as_array().unwrap();
    let reasoning = items
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("a latest turn with a chain must carry it");
    assert_eq!(reasoning["summary"][0]["text"], "reasoned it through");
    assert_eq!(reasoning["content"][0]["text"], "reasoned it through");
}

/// Capability view for a DeepSeek Responses route: reasoning via plaintext
/// `reasoning` output items, the full native effort ladder (ADR-0083).
fn deepseek_capabilities() -> nuo_protocol::ModelCapabilities {
    nuo_protocol::ModelCapabilities {
        family: "deepseek".into(),
        context_window: 1_000_000,
        max_output_tokens: None,
        thinking: nuo_protocol::ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: Some(true),
        effort_levels: Effort::ORDER.iter().copied().map(Into::into).collect(),
        effort_entities: Vec::new(),
        default_effort: None,
    }
}

fn deepseek_body_input<'a>(
    reasoning_effort: Option<Effort>,
    store: bool,
    cache_plan: &'a nuo_protocol::ResolvedCachePolicy,
) -> BodyInput<'a> {
    BodyInput {
        model: "deepseek-flash",
        stream: true,
        instructions: None,
        tool_specs: None,
        reasoning_effort,
        delivery: &DEFAULT_DELIVERY,
        store,
        dialect: nuo_protocol::OpenAiResponsesDialect::DeepSeek,
        cache_plan,
    }
}

#[test]
fn deepseek_dialect_emits_only_the_fields_its_schema_defines() {
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "hi")],
        deepseek_body_input(Some(Effort::None), false, &DEFAULT_CACHE_PLAN),
        &deepseek_capabilities(),
    )
    .unwrap();

    assert_eq!(
        body["reasoning"]["effort"], "none",
        "`none` is the Responses thinking off switch and must not clamp up to `low`"
    );
    assert!(
        body["reasoning"].get("summary").is_none(),
        "`reasoning.summary` is OpenAI's field; DeepSeek defines `effort` only"
    );
    assert!(
        body.get("include").is_none(),
        "`reasoning.encrypted_content` is an OpenAI-only grant on a stateless surface"
    );
    assert!(
        body.get("store").is_none(),
        "DeepSeek is purely stateless; `store` is not defined in its schema (ADR-0083)"
    );
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("parallel_tool_calls").is_none());
}

#[test]
fn deepseek_dialect_with_tools_omits_parallel_tool_calls_and_tool_choice() {
    let tool = nuo_protocol::capability::ToolSpec {
        name: "test_tool".into(),
        description: "test".into(),
        parameters: json!({"type": "object"}),
    };
    let mut input = deepseek_body_input(None, false, &DEFAULT_CACHE_PLAN);
    let specs = [tool];
    input.tool_specs = Some(&specs);
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "run tool")],
        input,
        &deepseek_capabilities(),
    )
    .unwrap();

    assert!(body.get("tools").is_some());
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(
        body["tools"][0]["function"]["name"], "test_tool",
        "DeepSeek Responses requires nested {{type: 'function', function: {{...}}}} format"
    );
    assert!(
        body["tools"][0].get("strict").is_none(),
        "DeepSeek does not define strict field on tool"
    );
    assert!(
        body.get("tool_choice").is_none(),
        "DeepSeek does not define tool_choice"
    );
    assert!(
        body.get("parallel_tool_calls").is_none(),
        "DeepSeek does not define parallel_tool_calls"
    );
    assert!(
        body.get("store").is_none(),
        "DeepSeek does not define store"
    );
}

#[test]
fn standard_dialect_emits_the_flattened_openai_tool_shape() {
    // OpenAI-compatible gateways (e.g. CommandCode's `/provider/v1`) validate
    // the flattened Responses tool shape and require a top-level `name`.
    // The DeepSeek dialect's nested `{function:{name}}` form leaves that
    // field undefined, which such a gateway rejects with
    // `400 expected string, received undefined` (ADR-0113).
    let specs = [nuo_protocol::capability::ToolSpec {
        name: "test_tool".into(),
        description: "test".into(),
        parameters: json!({"type": "object"}),
    }];
    let body = body_of(
        vec![Message::new(Role::User, "run tool")],
        test_body_input(
            "deepseek/deepseek-v4.1-flash",
            true,
            Some(&specs),
            None,
            &DEFAULT_DELIVERY,
            true,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();

    let tool = &body["tools"][0];
    assert_eq!(tool["type"], "function");
    assert_eq!(
        tool["name"], "test_tool",
        "the gateway's schema requires a top-level tool `name`"
    );
    assert!(
        tool.get("function").is_none(),
        "the flat OpenAI shape carries no nested `function` object"
    );
    assert_eq!(tool["strict"], false);
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(body["store"], true);
}

#[test]
fn deepseek_dialect_reconstructs_reasoning_item_for_assistant_tool_calls() {
    let mut assistant = Message::new(Role::Assistant, "");
    assistant.reasoning_content = Some("thinking about tool".into());
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call_1".into(),
        name: "test_tool".into(),
        arguments: "{}".into(),
    }]);
    let mut tool = Message::new(Role::Tool, "tool output");
    tool.tool_call_id = Some("call_1".into());

    let body = body_with_caps_of(
        vec![Message::new(Role::User, "run tool"), assistant, tool],
        deepseek_body_input(None, false, &DEFAULT_CACHE_PLAN),
        &deepseek_capabilities(),
    )
    .unwrap();

    let input = body["input"].as_array().unwrap();
    // Index 0: user message
    // Index 1: reasoning item
    // Index 2: function_call
    // Index 3: function_call_output
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["content"][0]["type"], "reasoning_text");
    assert_eq!(input[1]["content"][0]["text"], "thinking about tool");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[3]["type"], "function_call_output");
}

#[test]
fn deepseek_dialect_prunes_historical_reasoning_items_on_older_turns() {
    let mut assistant1 = Message::new(Role::Assistant, "");
    assistant1.reasoning_content = Some("ancient reasoning".into());
    assistant1.tool_calls = Some(vec![ToolCall {
        id: "call_1".into(),
        name: "tool_1".into(),
        arguments: "{}".into(),
    }]);
    let mut tool1 = Message::new(Role::Tool, "out1");
    tool1.tool_call_id = Some("call_1".into());

    let mut assistant2 = Message::new(Role::Assistant, "");
    assistant2.reasoning_content = Some("latest reasoning".into());
    assistant2.tool_calls = Some(vec![ToolCall {
        id: "call_2".into(),
        name: "tool_2".into(),
        arguments: "{}".into(),
    }]);
    let mut tool2 = Message::new(Role::Tool, "out2");
    tool2.tool_call_id = Some("call_2".into());

    let body = body_with_caps_of(
        vec![
            Message::new(Role::User, "run 1"),
            assistant1,
            tool1,
            Message::new(Role::User, "run 2"),
            assistant2,
            tool2,
        ],
        deepseek_body_input(None, false, &DEFAULT_CACHE_PLAN),
        &deepseek_capabilities(),
    )
    .unwrap();

    let input = body["input"].as_array().unwrap();
    // Index 0: user "run 1"
    // Index 1: reasoning for assistant1 (pruned to empty string)
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["content"][0]["text"], "");
    // Index 2: function_call "tool_1"
    // Index 3: function_call_output "out1"
    // Index 4: user "run 2"
    // Index 5: reasoning for assistant2 (active/latest, preserved in full)
    assert_eq!(input[5]["type"], "reasoning");
    assert_eq!(input[5]["content"][0]["text"], "latest reasoning");
}

#[test]
fn deepseek_dialect_folds_ephemeral_instructions_into_instructions() {
    let bundle = nuo_protocol::InstructionText::new("system prompt", "ephemeral instruction");
    let mut input = deepseek_body_input(None, false, &DEFAULT_CACHE_PLAN);
    input.instructions = Some(&bundle);
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "hi")],
        input,
        &deepseek_capabilities(),
    )
    .unwrap();

    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains("system prompt"));
    assert!(instructions.contains("ephemeral instruction"));

    // Must NOT emit a developer role message in input items
    let input_items = body["input"].as_array().unwrap();
    assert!(
        !input_items
            .iter()
            .any(|item| item.get("role").and_then(Value::as_str) == Some("developer")),
        "DeepSeek rejects the developer role"
    );
}

#[test]
fn standard_dialect_keeps_openai_summary_and_encrypted_reasoning() {
    let body = body_of(
        vec![Message::new(Role::User, "hi")],
        test_body_input(
            "gpt-5.6-sol",
            true,
            None,
            Some(Effort::High),
            &DEFAULT_DELIVERY,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();

    assert_eq!(body["reasoning"]["summary"], "detailed");
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn reasoning_object_is_omitted_when_there_is_nothing_to_say() {
    // A route with no effort ladder and no override must not send `{}`:
    // the documented default (thinking on, server-chosen effort) applies
    // only when the field is absent.
    let capabilities = nuo_protocol::ModelCapabilities {
        effort_levels: Vec::new(),
        ..deepseek_capabilities()
    };
    let body = body_with_caps_of(
        vec![Message::new(Role::User, "hi")],
        deepseek_body_input(None, false, &DEFAULT_CACHE_PLAN),
        &capabilities,
    )
    .unwrap();
    assert!(body.get("reasoning").is_none());
}

#[test]
fn system_messages_fold_into_instructions() {
    let body = body_of(
        vec![
            Message::new(Role::System, "be concise"),
            Message::new(Role::User, "hi"),
        ],
        test_body_input(
            "gpt-5.6-sol",
            true,
            None,
            None,
            &DEFAULT_DELIVERY,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();
    assert_eq!(body["instructions"], "be concise");
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[0]["content"][0]["type"], "input_text");
}

#[test]
fn function_call_and_output_round_trip_as_items() {
    let call = ToolCall {
        id: "call_1".into(),
        name: "bash".into(),
        arguments: "{\"command\":\"ls\"}".into(),
    };
    let result = Message {
        tool_call_id: Some("call_1".into()),
        ..Message::new(Role::Tool, "file.txt")
    };
    let body = body_of(
        vec![
            Message::new(Role::User, "list files"),
            assistant_with_call(call, "running it"),
            result,
        ],
        test_body_input(
            "gpt-5.6-sol",
            true,
            None,
            None,
            &DEFAULT_DELIVERY,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();
    let input = body["input"].as_array().unwrap();
    // user, assistant(message), function_call, function_call_output.
    assert_eq!(input.len(), 4);
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["output"], "file.txt");
}

#[test]
fn duplicate_provider_call_ids_are_uniquified_with_their_outputs() {
    let first_call = ToolCall {
        id: "call_244115".into(),
        name: "first".into(),
        arguments: "{}".into(),
    };
    let second_call = ToolCall {
        id: "call_244115".into(),
        name: "second".into(),
        arguments: "{}".into(),
    };
    let body = body_of(
        vec![
            Message::new(Role::User, "first turn"),
            assistant_with_call(first_call.clone(), ""),
            Message::tool_result(&first_call, "first result"),
            Message::new(Role::User, "second turn"),
            assistant_with_call(second_call.clone(), ""),
            Message::tool_result(&second_call, "second result"),
        ],
        test_body_input(
            "deepseek-v4-flash",
            true,
            None,
            None,
            &nuo_protocol::RequestDelivery::OpaqueReplay,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();

    let input = body["input"].as_array().unwrap();
    let calls: Vec<&Value> = input
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect();
    let outputs: Vec<&Value> = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(outputs.len(), 2);
    assert_eq!(calls[0]["call_id"], "call_244115");
    assert_ne!(calls[0]["call_id"], calls[1]["call_id"]);
    assert_eq!(outputs[0]["call_id"], calls[0]["call_id"]);
    assert_eq!(outputs[1]["call_id"], calls[1]["call_id"]);
}

#[test]
fn duplicate_ids_inside_opaque_replay_preserve_provider_items() {
    fn opaque_assistant(call: ToolCall, item_id: &str) -> Message {
        let mut provider_meta = serde_json::Map::new();
        provider_meta.insert(
            nuo_protocol::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY.to_string(),
            serde_json::json!([{
                "id": item_id,
                "type": "function_call",
                "call_id": call.id,
                "name": call.name,
                "arguments": call.arguments,
                "status": "completed"
            }]),
        );
        Message {
            tool_calls: Some(vec![call]),
            provider_meta: Some(provider_meta),
            ..Message::new(Role::Assistant, "")
        }
    }

    let first = ToolCall {
        id: "duplicate".into(),
        name: "first".into(),
        arguments: "{}".into(),
    };
    let second = ToolCall {
        id: "duplicate".into(),
        name: "second".into(),
        arguments: "{}".into(),
    };
    let body = body_of(
        vec![
            opaque_assistant(first.clone(), "fc_1"),
            Message::tool_result(&first, "one"),
            opaque_assistant(second.clone(), "fc_2"),
            Message::tool_result(&second, "two"),
        ],
        test_body_input(
            "deepseek-v4-flash",
            true,
            None,
            None,
            &nuo_protocol::RequestDelivery::OpaqueReplay,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();

    let input = body["input"].as_array().unwrap();
    assert_eq!(input[0]["id"], "fc_1");
    assert_eq!(input[0]["status"], "completed");
    assert_eq!(input[0]["call_id"], "duplicate");
    assert_eq!(input[1]["call_id"], "duplicate");
    assert_eq!(input[2]["id"], "fc_2");
    assert_eq!(input[2]["status"], "completed");
    assert_eq!(input[2]["call_id"], "call_nuo_1");
    assert_eq!(input[3]["call_id"], "call_nuo_1");
}

#[test]
fn remote_continuation_keeps_outputs_for_calls_in_the_cursor_anchor() {
    let call = ToolCall {
        id: "remote_call".into(),
        name: "lookup".into(),
        arguments: "{}".into(),
    };
    let delivery = nuo_protocol::RequestDelivery::RemoteContinuation {
        continuation_id: "resp_1".into(),
        input_start: 2,
    };
    let body = body_of(
        vec![
            Message::new(Role::User, "look it up"),
            assistant_with_call(call.clone(), ""),
            Message::tool_result(&call, "done"),
        ],
        test_body_input(
            "gpt-5.6-sol",
            true,
            None,
            None,
            &delivery,
            true,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();

    assert_eq!(body["previous_response_id"], "resp_1");
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["type"], "function_call_output");
    assert_eq!(input[0]["call_id"], "remote_call");
    assert_eq!(input[0]["output"], "done");
}

#[test]
fn unanswered_function_calls_are_stripped() {
    let call = ToolCall {
        id: "call_x".into(),
        name: "bash".into(),
        arguments: "{}".into(),
    };
    let body = body_of(
        vec![
            Message::new(Role::User, "go"),
            assistant_with_call(call, "thinking"),
        ],
        test_body_input(
            "gpt-5.6-sol",
            false,
            None,
            None,
            &DEFAULT_DELIVERY,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();
    let input = body["input"].as_array().unwrap();
    // Only user + assistant message survive; the unanswered call is dropped.
    assert_eq!(input.len(), 2);
    assert!(input.iter().all(|i| i["type"] != "function_call"));
}

#[test]
fn reasoning_summary_always_requested_with_effort_when_supported() {
    let body = body_of(
        vec![Message::new(Role::User, "hi")],
        test_body_input(
            "gpt-5.6-sol",
            true,
            None,
            Some(Effort::Medium),
            &DEFAULT_DELIVERY,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap();
    // The most verbose summaries the backend offers — the raw chain of
    // thought is never exposed, so `detailed` is the ceiling.
    assert_eq!(body["reasoning"]["summary"], "detailed");
    assert_eq!(body["reasoning"]["effort"], "medium");
}

#[test]
fn stateless_requests_preserve_encrypted_reasoning_and_parallel_tools() {
    let request = body_with_caps_of(
        vec![Message::new(Role::User, "hi")],
        test_body_input(
            "gpt-5.6-sol",
            true,
            Some(&[nuo_protocol::capability::ToolSpec {
                name: "lookup".to_string(),
                description: "lookup".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            None,
            &nuo_protocol::RequestDelivery::OpaqueReplay,
            false,
            &nuo_protocol::ResolvedCachePolicy::Unsupported,
        ),
        &nuo_protocol::ModelCapabilities::for_channel("gpt-5.6-sol", None),
    )
    .unwrap();
    assert_eq!(
        request["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
    assert_eq!(request["parallel_tool_calls"], true);
}

#[test]
fn headers_include_account_id_when_present() {
    let h = headers("tok", Some("acct-1"), false, true);
    assert_eq!(h[0].0, "Authorization");
    assert_eq!(h[1], ("originator", "codex_cli_rs".to_string()));
    assert_eq!(h[2].0, "ChatGPT-Account-Id");
    assert_eq!(h[2].1, "acct-1");
    // No account id → no header.
    assert_eq!(headers("tok", None, false, true).len(), 2);
    // A third-party Responses endpoint gets neither Codex header.
    assert_eq!(headers("tok", Some("acct-1"), false, false).len(), 1);
}

#[test]
fn headers_inject_copilot_set_and_drop_account_id() {
    // Copilot mode: the account id is ignored and Copilot's required
    // headers replace the ChatGPT account-id header.
    let h = headers("tok", Some("acct-1"), true, false);
    assert_eq!(h[0].0, "Authorization");
    assert_eq!(h[0].1, "Bearer tok");
    let names: Vec<&str> = h.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names,
        [
            "Authorization",
            "Copilot-Integration-Id",
            "Editor-Version",
            "Editor-Plugin-Version",
            "x-initiator",
            "Openai-Intent",
            "X-GitHub-Api-Version"
        ]
    );
    assert_eq!(h[1].1, "vscode-chat");
    assert_eq!(h[2].1, "vscode/1.107.0");
    assert_eq!(h[3].1, "copilot-chat/0.35.0");
    assert_eq!(h[4].1, "user");
    assert_eq!(h[5].1, "conversation-edits");
    assert_eq!(h[6].1, "2026-06-01");
    // No ChatGPT-Account-Id leaks through in Copilot mode.
    assert!(h.iter().all(|(n, _)| *n != "ChatGPT-Account-Id"));
}

#[test]
fn has_input_image_detects_image_parts() {
    let with_image = json!({
        "input": [
            {"role": "user", "content": [
                {"type": "input_text", "text": "what is this?"},
                {"type": "input_image", "image_url": "data:..."}
            ]}
        ]
    });
    assert!(has_input_image(&with_image));

    let text_only = json!({
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
    });
    assert!(!has_input_image(&text_only));

    // Defensive: a missing or non-array input is no image.
    assert!(!has_input_image(&json!({"model": "x"})));
    assert!(!has_input_image(&json!({"input": "not-an-array"})));
}

#[test]
fn user_message_with_image_serializes_as_input_image_string_url() {
    let mut msg = Message::new(Role::User, "look at this");
    msg.images = Some(vec![nuo_tool::message::ImagePart {
        mime: "image/png".to_string(),
        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".to_string(),
    }]);

    let remote = nuo_protocol::RemoteModelMetadata {
        vision: Some(true),
        ..Default::default()
    };
    let caps = nuo_protocol::ModelCapabilities::for_channel("gpt-4o", Some(&remote));

    let payload = body_with_caps_of(
        vec![msg],
        test_body_input(
            "gpt-4o",
            false,
            None,
            None,
            &DEFAULT_DELIVERY,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
        &caps,
    )
    .unwrap();

    let input = payload["input"].as_array().expect("input array");
    assert_eq!(input.len(), 1);
    let content = input[0]["content"].as_array().expect("content array");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "input_text");
    assert_eq!(content[0]["text"], "look at this");

    assert_eq!(content[1]["type"], "input_image");
    assert_eq!(
        content[1]["image_url"],
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
    );
}

#[test]
fn declared_text_only_route_projects_remote_continuation_images_away() {
    // The projection runs before the delivery split, so an image
    // carried by the locally-new suffix of a remote continuation is dropped
    // for a route that declared no image input — the Responses API 400s on
    // `input_image` outright.
    let mut msg = Message::new(Role::User, "look at this");
    msg.images = Some(vec![nuo_tool::message::ImagePart {
        mime: "image/png".to_string(),
        data: "aGk=".to_string(),
    }]);

    let remote = nuo_protocol::RemoteModelMetadata {
        vision: Some(false),
        ..Default::default()
    };
    let caps = nuo_protocol::ModelCapabilities::for_channel("gpt-4o", Some(&remote));

    let payload = body_with_caps_of(
        vec![msg],
        test_body_input(
            "gpt-4o",
            false,
            None,
            None,
            &DEFAULT_DELIVERY,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
        &caps,
    )
    .unwrap();

    let content = payload["input"][0]["content"]
        .as_array()
        .expect("content array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "input_text");
}

#[test]
fn empty_opaque_replay_artifact_fails_closed() {
    let mut provider_meta = serde_json::Map::new();
    provider_meta.insert(
        nuo_protocol::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY.to_string(),
        serde_json::json!([]),
    );
    let assistant = Message {
        provider_meta: Some(provider_meta),
        ..Message::new(Role::Assistant, "previous answer")
    };
    let error = body_of(
        vec![assistant, Message::new(Role::User, "continue")],
        test_body_input(
            "gpt-5.6-sol",
            true,
            None,
            None,
            &nuo_protocol::RequestDelivery::OpaqueReplay,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap_err();
    assert_eq!(error, RequestBuildError::EmptyReplayArtifact);
    assert!(error.to_string().contains("start a new session"));
}

#[test]
fn orphan_tool_output_fails_instead_of_disappearing() {
    let error = body_of(
        vec![Message {
            tool_call_id: Some("missing_call".to_string()),
            ..Message::new(Role::Tool, "result")
        }],
        test_body_input(
            "gpt-5.6-sol",
            true,
            None,
            None,
            &DEFAULT_DELIVERY,
            false,
            &DEFAULT_CACHE_PLAN,
        ),
    )
    .unwrap_err();
    assert!(error.to_string().contains("unknown call_id `missing_call`"));
}
