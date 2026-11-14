use super::*;

#[test]
fn incomplete_response_still_delivers_output_usage_and_terminality() {
    // DeepSeek terminates a truncated turn with `response.incomplete`
    // (`incomplete_details.reason: max_output_tokens`). The tokens were
    // produced and billed, so the turn is a completion — not a dropped one.
    let mut stream = ResponsesStream::new();
    let events = stream
        .parse(
            r#"{"type":"response.incomplete","response":{"id":"resp_1","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"partial"}]}],"usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7},"incomplete_details":{"reason":"max_output_tokens"}}}"#,
        )
        .unwrap();

    let meta = match events.as_slice() {
        [ProviderStreamEvent::Completed(meta)] => meta,
        other => panic!("expected one terminal completion, got {other:?}"),
    };
    assert_eq!(
        meta.usage.as_ref().map(|usage| usage.completion_tokens),
        Some(2)
    );
    let output = meta
        .artifacts
        .as_ref()
        .and_then(|artifacts| artifacts.get(nuo_provider::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY))
        .and_then(Value::as_array)
        .expect("the truncated turn stays replayable");
    assert_eq!(output[0]["content"][0]["text"], "partial");
    assert!(
        stream
            .parse(r#"{"type":"response.output_text.delta","delta":"late"}"#)
            .is_err(),
        "a terminal event closes the stream"
    );
}

#[test]
fn failed_response_surfaces_the_upstream_error() {
    let mut stream = ResponsesStream::new();
    let error = stream
        .parse(
            r#"{"type":"response.failed","response":{"error":{"message":"Insufficient balance","type":"insufficient_balance","code":"insufficient_balance"}}}"#,
        )
        .unwrap_err();
    assert!(error.contains("Insufficient balance"), "{error}");
    assert!(error.contains("insufficient_balance"), "{error}");
}

#[test]
fn parses_output_text_delta() {
    let mut s = ResponsesStream::new();
    let ev = s
        .parse(r#"{"type":"response.output_text.delta","delta":"hel"}"#)
        .unwrap();
    assert_eq!(ev.len(), 1);
    assert!(matches!(
        &ev[0],
        ProviderStreamEvent::TextDelta(t) if t == "hel"
    ));
}

#[test]
fn parses_reasoning_summary_delta() {
    let mut s = ResponsesStream::new();
    let ev = s
        .parse(r#"{"type":"response.reasoning_summary_text.delta","delta":"thinking"}"#)
        .unwrap();
    assert_eq!(ev.len(), 1);
    assert!(matches!(
        &ev[0],
        ProviderStreamEvent::ReasoningDelta(t) if t == "thinking"
    ));
}

#[test]
fn parses_raw_reasoning_text_delta() {
    // Third-party Responses providers (DeepSeek V4) stream the raw CoT as
    // `reasoning_text` deltas rather than ChatGPT's summary stream.
    let mut s = ResponsesStream::new();
    let ev = s
        .parse(r#"{"type":"response.reasoning_text.delta","delta":"pondering"}"#)
        .unwrap();
    assert_eq!(ev.len(), 1);
    assert!(matches!(
        &ev[0],
        ProviderStreamEvent::ReasoningDelta(t) if t == "pondering"
    ));
}

#[test]
fn non_streaming_message_reads_reasoning_content_parts() {
    // DeepSeek's reasoning item carries plain-text `content` parts rather
    // than ChatGPT's `summary` parts.
    let output = serde_json::json!([
        {"type":"reasoning","content":[{"type":"reasoning_text","text":"step by step"}]},
        {"type":"message","content":[{"type":"output_text","text":"done"}]},
    ]);
    let msg = message(&output);
    assert_eq!(msg.content, "done");
    assert_eq!(msg.reasoning_content.as_deref(), Some("step by step"));
}

#[test]
fn assembles_function_call_across_events_with_stable_index() {
    let mut s = ResponsesStream::new();
    // output_item.added announces the call.
    let added = s.parse(
        r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"bash"}}"#,
    ).unwrap();
    assert_eq!(added.len(), 1);
    assert!(matches!(
        &added[0],
        ProviderStreamEvent::ToolCallDelta { index: 0, id, name, arguments }
        if id.as_deref() == Some("call_1") && name.as_deref() == Some("bash") && arguments.is_empty()
    ));
    // Argument deltas accumulate under the same index.
    let d1 = s.parse(
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"comm"}"#,
    ).unwrap();
    let d2 = s.parse(
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"and\":\"ls\"}"}"#,
    ).unwrap();
    assert_eq!(d1.len() + d2.len(), 2);
    // A second call gets index 1.
    let added2 = s.parse(
        r#"{"type":"response.output_item.added","item":{"id":"fc_2","type":"function_call","call_id":"call_2","name":"grep"}}"#,
    ).unwrap();
    assert!(matches!(
        &added2[0],
        ProviderStreamEvent::ToolCallDelta { index: 1, .. }
    ));
}

#[test]
fn emits_usage_on_completion() {
    let mut s = ResponsesStream::new();
    let ev = s.parse(
        r#"{"type":"response.completed","response":{"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}],"usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19}}}"#,
    ).unwrap();
    assert_eq!(ev.len(), 1);
    match &ev[0] {
        ProviderStreamEvent::Completed(meta) => {
            let u = meta.usage.expect("usage attached to completed event");
            assert_eq!(u.prompt_tokens, 12);
            assert_eq!(u.completion_tokens, 7);
            assert_eq!(u.total_tokens, 19);
        }
        _ => panic!("expected completed with usage"),
    }
}

#[test]
fn done_event_does_not_double_arguments_or_id() {
    // Regression: a well-behaved stream emits output_item.added (id+name),
    // then argument deltas, then function_call_arguments.done. The harness
    // appends every delta, so the .done event MUST NOT re-emit the full
    // arguments/id/name — otherwise they double (e.g.
    // `{"path":"."}{"path":"."}` and `call_1call_1`).
    let mut s = ResponsesStream::new();
    s.parse(
        r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"find_files"}}"#,
    ).unwrap();
    let delta = serde_json::json!({
        "type": "response.function_call_arguments.delta",
        "item_id": "fc_1",
        "delta": "{\"path\":\".\",\"max_results\":100}"
    })
    .to_string();
    let done = serde_json::json!({
        "type": "response.function_call_arguments.done",
        "item_id": "fc_1",
        "call_id": "call_1",
        "name": "find_files",
        "arguments": "{\"path\":\".\",\"max_results\":100}"
    })
    .to_string();
    s.parse(&delta).unwrap();
    // The .done event must emit nothing — args and id/name already flowed.
    let done_events = s.parse(&done).unwrap();
    assert!(
        done_events.is_empty(),
        "done must not re-emit after deltas; got {done_events:?}"
    );
}

#[test]
fn done_emits_full_call_as_fallback_when_no_deltas() {
    // A degenerate backend that sends only .done (no added, no deltas):
    //the .done must surface the complete call exactly once.
    let mut s = ResponsesStream::new();
    let done = serde_json::json!({
        "type": "response.function_call_arguments.done",
        "item_id": "fc_1",
        "call_id": "call_1",
        "name": "find_files",
        "arguments": "{\"path\":\".\"}"
    })
    .to_string();
    let ev = s.parse(&done).unwrap();
    assert_eq!(ev.len(), 1);
    match &ev[0] {
        ProviderStreamEvent::ToolCallDelta {
            index: 0,
            id,
            name,
            arguments,
        } => {
            assert_eq!(id.as_deref(), Some("call_1"));
            assert_eq!(name.as_deref(), Some("find_files"));
            assert_eq!(arguments, "{\"path\":\".\"}");
        }
        _ => panic!("expected one tool-call delta"),
    }
}

#[test]
fn non_streaming_message_gathers_text_reasoning_and_calls() {
    let output = serde_json::json!([
        {"type":"reasoning","summary":[{"type":"summary_text","text":"deliberating"}]},
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello "},{"type":"output_text","text":"world"}]},
        {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{\"command\":\"ls\"}"}
    ]);
    let m = message(&output);
    assert_eq!(m.content, "hello world");
    assert_eq!(m.reasoning_content.as_deref(), Some("deliberating"));
    let calls = m.tool_calls.as_ref().unwrap();
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].name, "bash");
}

#[test]
fn unknown_event_types_are_ignored() {
    let mut s = ResponsesStream::new();
    let ev = s.parse(r#"{"type":"response.created"}"#).unwrap();
    assert!(ev.is_empty());
    assert!(s.parse("not json").is_err());
}

#[test]
fn reasoning_summary_strips_html_comment_placeholder() {
    let mut s = ResponsesStream::new();
    // A header delta passes through.
    let ev = s
        .parse(r#"{"type":"response.reasoning_summary_text.delta","delta":"**Planning**\n\n"}"#)
        .unwrap();
    assert!(matches!(
        &ev[0],
        ProviderStreamEvent::ReasoningDelta(t) if t.contains("Planning")
    ));
    // The empty-body placeholder delta is dropped entirely (no event).
    let ev = s
        .parse(r#"{"type":"response.reasoning_summary_text.delta","delta":"<!-- -->"}"#)
        .unwrap();
    assert!(
        ev.is_empty(),
        "placeholder delta must be dropped; got {ev:?}"
    );
    // A delta combining text + placeholder keeps the text, drops the marker.
    let ev = s
        .parse(r#"{"type":"response.reasoning_summary_text.delta","delta":"done\n<!-- -->"}"#)
        .unwrap();
    match &ev[0] {
        ProviderStreamEvent::ReasoningDelta(t) => {
            assert!(t.contains("done"));
            assert!(!t.contains("<!-- -->"));
        }
        _ => panic!("expected reasoning delta"),
    }
}

#[test]
fn usage_surfaces_responses_cached_tokens_as_read() {
    let u = usage(&serde_json::json!({
        "input_tokens": 800,
        "output_tokens": 40,
        "input_tokens_details": { "cached_tokens": 500 }
    }))
    .unwrap();
    assert_eq!(u.prompt_tokens, 800);
    assert_eq!(u.cache_read_input_tokens, 500);
    assert_eq!(u.cache_creation_input_tokens, 0);
}

#[test]
fn completed_reconstructs_empty_terminal_output_from_done_items() {
    let mut stream = ResponsesStream::new();
    let reasoning = serde_json::json!({
        "id": "rs_1",
        "type": "reasoning",
        "encrypted_content": "opaque-state",
        "summary": []
    });
    let call = serde_json::json!({
        "id": "fc_1",
        "type": "function_call",
        "call_id": "call_1",
        "name": "find_files",
        "arguments": "{\"path\":\".\"}",
        "status": "completed"
    });
    stream
        .parse_value(&serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": call
        }))
        .unwrap();
    stream
        .parse_value(&serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": reasoning
        }))
        .unwrap();

    let events = stream
        .parse_value(&serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1",
                "output": [],
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }
        }))
        .unwrap();
    let ProviderStreamEvent::Completed(meta) = &events[0] else {
        panic!("expected completion metadata");
    };
    let output = meta
        .artifacts
        .as_ref()
        .and_then(|artifacts| artifacts.get(nuo_provider::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY))
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(output[0]["encrypted_content"], "opaque-state");
    assert_eq!(output[1]["call_id"], "call_1");
}

#[test]
fn completed_without_any_output_is_a_protocol_error() {
    let error = ResponsesStream::new()
        .parse_value(&serde_json::json!({
            "type": "response.completed",
            "response": {"output": []}
        }))
        .unwrap_err();
    assert!(error.contains("without any replayable output items"));
}

#[test]
fn completed_rejects_gaps_in_done_item_indices() {
    let mut stream = ResponsesStream::new();
    stream
        .parse_value(&serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": {"type": "message", "content": []}
        }))
        .unwrap();
    let error = stream
        .parse_value(&serde_json::json!({
            "type": "response.completed",
            "response": {"output": []}
        }))
        .unwrap_err();
    assert!(error.contains("missing completed output index 0"));
}

#[test]
fn terminal_event_is_unique_and_final() {
    let mut stream = ResponsesStream::new();
    stream
        .parse_value(&serde_json::json!({
            "type": "response.completed",
            "response": {
                "output": [{"type": "message", "content": []}]
            }
        }))
        .unwrap();
    let error = stream
        .parse_value(&serde_json::json!({"type": "response.created"}))
        .unwrap_err();
    assert!(error.contains("after its terminal event"));
}
