//! OpenAI **Responses** API — response & streaming-event parsing.
//!
//! Pure functions turning a Responses payload into the harness's domain types:
//! usage extraction, the non-streaming assistant [`Message`], and the
//! per-`data:`-payload stream events. No `reqwest`, no `async`, no I/O.
//!
//! Streaming shape: each SSE `data:` payload is a JSON object with a `type`
//! field identifying the event. The harness-relevant events are:
//! - `response.output_text.delta` → `TextDelta`
//! - `response.reasoning_summary_text.delta` → `ReasoningDelta`
//! - `response.function_call_arguments.delta` → accumulating tool-call args
//! - `response.function_call_arguments.done` → the complete tool call
//! - `response.output_item.added` (function_call) → captures id/name/call_id
//! - `response.output_item.done` → preserves the complete opaque output item
//! - `response.completed` → terminal usage and exact replay artifacts

use nuo_model_codec::{Message, ProviderStreamEvent, Role, TokenUsage, ToolCall};
use serde_json::Value;

/// Parse a Responses `usage` object (`input_tokens` / `output_tokens` /
/// `total_tokens`) into a [`TokenUsage`]. Returns `None` when absent or without
/// numeric fields. The Responses API reports reasoning tokens under
/// `output_tokens_details.reasoning_tokens`; they are carried in
/// [`TokenUsage::reasoning_tokens`] (already included in the completion count,
/// never an addition to it). Its auto-cache discount surfaces as
/// `input_tokens_details.cached_tokens` and is surfaced in
/// [`TokenUsage::cache_read_input_tokens`].
pub fn usage(usage: &Value) -> Option<TokenUsage> {
    let input = usage["input_tokens"].as_i64();
    let output = usage["output_tokens"].as_i64();
    let total = usage["total_tokens"].as_i64();
    let reasoning = usage["output_tokens_details"]["reasoning_tokens"].as_i64();
    let prompt = input;
    let completion = output.or_else(|| total.zip(prompt).map(|(t, p)| (t - p).max(0)));
    // Route cache-read accounting through the shared helper so the cache
    // policy is enforced in one place (ADR-0161). The Responses API hides the
    // discount in `input_tokens_details.cached_tokens`, which the helper reads.
    let cache = nuo_model_codec::read_prompt_cache_usage(usage);
    match (prompt, completion, total) {
        (Some(p), Some(c), _) => Some(TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: total.unwrap_or(p + c),
            cache_creation_input_tokens: cache.write_tokens,
            cache_read_input_tokens: cache.read_tokens,
            cache_miss_input_tokens: cache.miss_tokens.unwrap_or(0),
            reasoning_tokens: reasoning.unwrap_or(0),
        }),
        _ => total.map(|t| TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: t,
            cache_creation_input_tokens: cache.write_tokens,
            cache_read_input_tokens: cache.read_tokens,
            cache_miss_input_tokens: cache.miss_tokens.unwrap_or(0),
            reasoning_tokens: reasoning.unwrap_or(0),
        }),
    }
}

/// Assemble a non-streaming Responses `output` array into one assistant
/// [`Message`]. Tolerant: gathers `output_text` parts from message items,
/// reasoning summary text from reasoning items, and every `function_call` item
/// into `tool_calls`. Items it does not recognize are ignored.
pub fn message(output: &Value) -> Message {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    if let Some(items) = output.as_array() {
        for item in items {
            match item["type"].as_str().unwrap_or("") {
                "message" => {
                    if let Some(parts) = item["content"].as_array() {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                content.push_str(text);
                            }
                        }
                    }
                }
                "reasoning" => {
                    // ChatGPT carries the summarized CoT in `summary` parts;
                    // third-party Responses providers (DeepSeek V4) carry the
                    // raw CoT as `reasoning_text` parts in `content`. Read
                    // both — any part with a `text` string contributes.
                    let parts = item["summary"]
                        .as_array()
                        .into_iter()
                        .chain(item["content"].as_array());
                    for part in parts.flatten() {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            let cleaned = strip_reasoning_placeholder(text);
                            if cleaned.trim().is_empty() {
                                continue;
                            }
                            if !reasoning.is_empty() {
                                reasoning.push('\n');
                            }
                            reasoning.push_str(&cleaned);
                        }
                    }
                }
                "function_call" => {
                    calls.push(ToolCall {
                        id: item["call_id"].as_str().unwrap_or("").to_string(),
                        name: item["name"].as_str().unwrap_or("").to_string(),
                        arguments: item["arguments"].as_str().unwrap_or("").to_string(),
                    });
                }
                _ => {}
            }
        }
    }
    Message {
        role: Role::Assistant,
        content,
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: (!calls.is_empty()).then_some(calls),
        ..Message::new(Role::Assistant, "")
    }
}

/// Stateful accumulator that turns a sequence of Responses stream events into
/// [`ProviderStreamEvent`]s. The Responses API streams a function call across
/// several events (`output_item.added` carrying the id/name, then argument
/// deltas, then `.done`), and a single response may emit several calls, so this
/// tracks per-`item_id` state and assigns a stable tool-call `index`.
///
/// The harness *appends* every `ToolCallDelta` it receives (`id`/`name`/`
/// `arguments` via `push_str`), so this accumulator MUST emit each fragment
/// exactly once: the `output_item.added` event supplies id+name, the
/// `function_call_arguments.delta` events supply the argument text, and
/// `function_call_arguments.done` is treated as a completion signal — it only
/// re-emits a field as a fallback when that field's incremental events never
/// arrived (a degenerate backend), never as a duplicate.
#[derive(Debug, Default)]
pub struct ResponsesStream {
    /// `item_id` → tool-call index, assigned in first-appearance order.
    call_index: std::collections::HashMap<String, usize>,
    /// `item_id`s whose `output_item.added` already emitted id + name.
    seen_item: std::collections::HashSet<String>,
    /// `item_id`s that already received argument deltas.
    seen_args: std::collections::HashSet<String>,
    /// Complete provider output items, keyed by their response output index.
    /// Stateless continuation replays these values byte-for-byte at the JSON
    /// value level, including encrypted reasoning state and unknown item types.
    completed_output: std::collections::BTreeMap<usize, Value>,
    /// A response has exactly one terminal event and no data may follow it.
    terminal_seen: bool,
}

impl ResponsesStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse one `data:` payload (a JSON object with a `type` field) into zero
    /// or more harness stream events. Unknown event types are ignored.
    pub fn parse(&mut self, data: &str) -> Result<Vec<ProviderStreamEvent>, String> {
        let value = serde_json::from_str::<Value>(data)
            .map_err(|error| format!("Invalid JSON in Responses event: {error}"))?;
        self.parse_value(&value)
    }

    /// Parse an already-decoded event. Transport adapters use this entry point
    /// so validation and event assembly share one JSON decode.
    pub fn parse_value(&mut self, value: &Value) -> Result<Vec<ProviderStreamEvent>, String> {
        if self.terminal_seen {
            return Err("Responses stream emitted data after its terminal event.".to_string());
        }
        let mut events = Vec::new();
        let event_type = value["type"].as_str().unwrap_or("");
        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = value["delta"].as_str() {
                    events.push(ProviderStreamEvent::TextDelta(delta.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = value["delta"].as_str() {
                    // `reasoning_summary_text` is the ChatGPT backend's
                    // summarized CoT; `reasoning_text` is the raw CoT stream
                    // used by third-party Responses providers (DeepSeek V4).
                    // The ChatGPT Responses backend emits an empty `<!-- -->`
                    // HTML comment as the body placeholder for header-only
                    // reasoning-summary parts (a part's full text is e.g.
                    // `**Planning…**\n\n<!-- -->`). codex drops these
                    // (`history_cell/messages.rs`); we strip them so the
                    // reasoning trace stays clean. A delta that is only the
                    // placeholder collapses to nothing and is skipped.
                    let cleaned = strip_reasoning_placeholder(delta);
                    if !cleaned.is_empty() {
                        events.push(ProviderStreamEvent::ReasoningDelta(cleaned));
                    }
                }
            }
            "response.output_item.added" => {
                // A function_call item announces its id/name/call_id before its
                // argument deltas begin. Register its index and surface the call
                // id + name immediately so the harness can render it.
                let item = &value["item"];
                if item["type"].as_str() == Some("function_call")
                    && let Some(item_id) = item["id"].as_str()
                {
                    let idx = self.assign_index(item_id);
                    self.seen_item.insert(item_id.to_string());
                    events.push(ProviderStreamEvent::ToolCallDelta {
                        index: idx,
                        id: item["call_id"].as_str().map(str::to_string),
                        name: item["name"].as_str().map(str::to_string),
                        arguments: String::new(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let (Some(item_id), Some(delta)) =
                    (value["item_id"].as_str(), value["delta"].as_str())
                {
                    let idx = self.assign_index(item_id);
                    self.seen_args.insert(item_id.to_string());
                    events.push(ProviderStreamEvent::ToolCallDelta {
                        index: idx,
                        id: None,
                        name: None,
                        arguments: delta.to_string(),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                // Completion signal only: the argument deltas above already
                // built the args. Emit a field solely as a fallback when its
                // incremental events never arrived, so a well-behaved stream is
                // never doubled.
                if let Some(item_id) = value["item_id"].as_str() {
                    let idx = self.assign_index(item_id);
                    let had_item = self.seen_item.contains(item_id);
                    let had_args = self.seen_args.contains(item_id);
                    let id = (!had_item)
                        .then(|| value["call_id"].as_str().map(str::to_string))
                        .flatten();
                    let name = (!had_item)
                        .then(|| value["name"].as_str().map(str::to_string))
                        .flatten();
                    let arguments = if !had_args {
                        value["arguments"].as_str().unwrap_or("").to_string()
                    } else {
                        Default::default()
                    };
                    if id.is_some() || name.is_some() || !arguments.is_empty() {
                        events.push(ProviderStreamEvent::ToolCallDelta {
                            index: idx,
                            id,
                            name,
                            arguments,
                        });
                    }
                }
            }
            "response.output_item.done" => {
                let output_index = value["output_index"].as_u64().ok_or_else(|| {
                    "Responses output_item.done event is missing output_index.".to_string()
                })? as usize;
                let item = value
                    .get("item")
                    .filter(|item| item.is_object())
                    .ok_or_else(|| {
                        "Responses output_item.done event is missing its complete item.".to_string()
                    })?;
                if item
                    .get("type")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(
                        "Responses output_item.done event contains an item without a type."
                            .to_string(),
                    );
                }
                if self
                    .completed_output
                    .insert(output_index, item.clone())
                    .is_some()
                {
                    return Err(format!(
                        "Responses stream completed output index {output_index} more than once."
                    ));
                }
            }
            "response.completed" => {
                let response = &value["response"];
                let output = match response.get("output").and_then(Value::as_array) {
                    Some(items) if !items.is_empty() => items.clone(),
                    _ if !self.completed_output.is_empty() => {
                        for (expected, actual) in self.completed_output.keys().enumerate() {
                            if expected != *actual {
                                return Err(format!(
                                    "Responses stream is missing completed output index {expected}."
                                ));
                            }
                        }
                        self.completed_output.values().cloned().collect()
                    }
                    _ => {
                        return Err(
                            "Responses stream completed without any replayable output items."
                                .to_string(),
                        );
                    }
                };
                for item in &output {
                    if !item.is_object()
                        || item
                            .get("type")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                    {
                        return Err(
                            "Responses completion contains a malformed output item.".to_string()
                        );
                    }
                }
                let mut artifacts = serde_json::Map::new();
                artifacts.insert(
                    nuo_model_codec::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY.to_string(),
                    Value::Array(output),
                );
                if let Some(id) = response["id"].as_str() {
                    artifacts.insert(
                        nuo_model_codec::OPENAI_RESPONSE_ID_ARTIFACT_KEY.to_string(),
                        serde_json::Value::String(id.to_string()),
                    );
                }
                events.push(ProviderStreamEvent::Completed(
                    nuo_model_codec::ProviderCompletionMeta {
                        usage: usage(&response["usage"]),
                        artifacts: Some(artifacts),
                        continuation: None,
                    },
                ));
                self.terminal_seen = true;
            }
            // `response.created`, `response.in_progress`, and part-level
            // `*.added`/`*.done` events carry no additional harness payload.
            // Terminal failures are rejected by the transport before parsing.
            _ => {}
        }
        Ok(events)
    }

    /// Assign (or look up) the tool-call index for an `item_id`, in
    /// first-appearance order.
    fn assign_index(&mut self, item_id: &str) -> usize {
        let next = self.call_index.len();
        *self.call_index.entry(item_id.to_string()).or_insert(next)
    }
}

/// Strip the ChatGPT Responses backend's empty-body placeholder (`<!-- -->`)
/// from a reasoning-summary fragment. The backend uses it to mark
/// header-only summary parts (e.g. `**Planning…**\n\n<!-- -->`); codex drops
/// it the same way.
fn strip_reasoning_placeholder(s: &str) -> String {
    s.replace("<!-- -->", "")
}

#[cfg(test)]
mod tests {
    use super::*;

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
            r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"list_dir"}}"#,
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
            "name": "list_dir",
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
            "name": "list_dir",
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
                assert_eq!(name.as_deref(), Some("list_dir"));
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
            "name": "list_dir",
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
            .and_then(|artifacts| {
                artifacts.get(nuo_model_codec::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY)
            })
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
}
