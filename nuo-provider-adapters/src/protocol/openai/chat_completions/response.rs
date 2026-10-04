//! OpenAI-compatible chat completions — response parsing.
//!
//! Pure functions turning OpenAI's JSON response shape into the harness's
//! domain types: the assistant [`Message`] (with reasoning content and tool
//! calls), the top-level `usage` object, and the per-chunk stream events.

use nuo_model_codec::{Message, ProviderStreamEvent, Role, TokenUsage, ToolCall};
use serde_json::Value;

/// Provider-opaque message sidecar used to replay OpenRouter's signed or
/// encrypted reasoning blocks after a tool call.
pub const OPENROUTER_REASONING_DETAILS_META_KEY: &str = "openrouter_reasoning_details";

fn reasoning_text(value: &Value) -> Option<String> {
    value["reasoning"]
        .as_str()
        .or_else(|| value["reasoning_content"].as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| reasoning_details_text(value.get("reasoning_details")?))
}

fn reasoning_details_text(details: &Value) -> Option<String> {
    let mut text = String::new();
    for detail in details.as_array()? {
        if let Some(fragment) = detail
            .get("text")
            .or_else(|| detail.get("summary"))
            .and_then(Value::as_str)
        {
            text.push_str(fragment);
        }
    }
    (!text.is_empty()).then_some(text)
}

fn reasoning_details_meta(value: &Value) -> Option<serde_json::Map<String, Value>> {
    let details = value.get("reasoning_details")?.as_array()?;
    if details.is_empty() {
        return None;
    }
    let mut meta = serde_json::Map::new();
    meta.insert(
        OPENROUTER_REASONING_DETAILS_META_KEY.to_string(),
        Value::Array(details.clone()),
    );
    Some(meta)
}

/// Reassembles `delta.reasoning_details` fragments by their stable index for
/// replay in the next OpenRouter request. Text/data/summary fields are stream
/// deltas and concatenate; identity/signature fields keep the latest value.
#[derive(Debug, Default)]
pub struct ReasoningDetailsAccumulator {
    details: std::collections::BTreeMap<u64, Value>,
}

impl ReasoningDetailsAccumulator {
    pub fn observe(&mut self, event: &Value) {
        let Some(incoming) = event["choices"][0]["delta"]["reasoning_details"].as_array() else {
            return;
        };
        for (position, detail) in incoming.iter().enumerate() {
            let index = detail
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(position as u64);
            let Some(incoming_obj) = detail.as_object() else {
                continue;
            };
            let current = self
                .details
                .entry(index)
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            let Some(current_obj) = current.as_object_mut() else {
                *current = detail.clone();
                continue;
            };
            for (key, value) in incoming_obj {
                if matches!(key.as_str(), "text" | "data" | "summary")
                    && let (Some(existing), Some(fragment)) = (
                        current_obj
                            .get_mut(key)
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                        value.as_str(),
                    )
                {
                    current_obj.insert(key.clone(), Value::String(existing + fragment));
                } else if !value.is_null() {
                    current_obj.insert(key.clone(), value.clone());
                }
            }
        }
    }

    pub fn artifacts(&self) -> Option<serde_json::Map<String, Value>> {
        if self.details.is_empty() {
            return None;
        }
        let mut artifacts = serde_json::Map::new();
        artifacts.insert(
            OPENROUTER_REASONING_DETAILS_META_KEY.to_string(),
            Value::Array(self.details.values().cloned().collect()),
        );
        Some(artifacts)
    }
}

/// Parse an OpenAI top-level `usage` object (`prompt_tokens` /
/// `completion_tokens` / `total_tokens`) into a [`TokenUsage`]. Returns `None`
/// when the object is absent or has no numeric fields.
///
/// OpenAI auto-caches without explicit breakpoints. Its discount surfaces as
/// `prompt_tokens_details.cached_tokens` (Moonshot exposes the same number as a
/// top-level `cached_tokens`). That count is a **cache read** — served from the
/// auto-cache at a discount — and is now surfaced in
/// [`TokenUsage::cache_read_input_tokens`] so the token-source report shows the
/// hit rate and the cost is attributed correctly. `cache_creation_input_tokens`
/// stays zero: OpenAI-style auto-caching has no separate write counter.
pub fn usage(usage: &Value) -> Option<TokenUsage> {
    let cache = nuo_model_codec::read_prompt_cache_usage(usage);
    let prompt = usage["prompt_tokens"].as_i64();
    let completion = usage["completion_tokens"].as_i64();
    let total = usage["total_tokens"].as_i64();
    let reasoning = usage["completion_tokens_details"]["reasoning_tokens"].as_i64();
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
        (Some(p), None, Some(t)) => Some(TokenUsage {
            prompt_tokens: p,
            completion_tokens: (t - p).max(0),
            total_tokens: t,
            cache_creation_input_tokens: cache.write_tokens,
            cache_read_input_tokens: cache.read_tokens,
            cache_miss_input_tokens: cache.miss_tokens.unwrap_or(0),
            reasoning_tokens: reasoning.unwrap_or(0),
        }),
        _ => {
            // Fall back to total_tokens only.
            total.map(|t| TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: t,
                cache_creation_input_tokens: cache.write_tokens,
                cache_read_input_tokens: cache.read_tokens,
                cache_miss_input_tokens: cache.miss_tokens.unwrap_or(0),
                reasoning_tokens: reasoning.unwrap_or(0),
            })
        }
    }
}

/// Extract the tool calls from a response `choices[0].message.tool_calls`
/// array (if present).
pub fn tool_calls(choice: &Value) -> Option<Vec<ToolCall>> {
    choice.get("tool_calls").and_then(|tc| {
        tc.as_array().map(|arr| {
            arr.iter()
                .map(|t| ToolCall {
                    id: t["id"]
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4())),
                    name: t["function"]["name"].as_str().unwrap_or("").to_string(),
                    arguments: t["function"]["arguments"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
    })
}

/// Assemble the `choices[0].message` of a non-streaming chat response into one
/// assistant [`Message`].
///
/// `content_filter` is applied to the text content: it receives the raw
/// `choice` text and the resolved tool calls, and returns the text safe to
/// show. This is the seam where the tool-call "echo" filter (GLM/Qwen models
/// that mirror a native tool call as text) is applied — see [`super::echo`].
pub fn message(choice: &Value, content_filter: impl FnOnce(&str, bool) -> String) -> Message {
    let reasoning_content = reasoning_text(choice);

    let tool_calls = tool_calls(choice);

    let raw_content = choice["content"].as_str().unwrap_or("");
    let had_native_tool_calls = tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
    let content = content_filter(raw_content, had_native_tool_calls);

    Message {
        role: Role::Assistant,
        content,
        content_blob: None,
        display_content: None,
        reasoning_content,
        provider_meta: reasoning_details_meta(choice),
        tool_calls,
        tool_call_id: None,
        images: None,
        provider: None,
        model: None,
        effort: None,
        hidden: false,
        children: None,
        subagent_meta: None,
        origin: None,
        timestamp: Some(nuo_tool::todos::unix_now()),
        sent_at_ms: None,
        cache_frozen: false,
    }
}

/// Parse one already-parsed streaming chat-completion event into provider
/// stream events. The caller deserializes each SSE `data:` payload exactly
/// once (surfacing a decode error for non-JSON payloads) and passes the
/// `Value` here. The terminal chunk (carrying `finish_reason`) may include a
/// top-level `usage` object when `stream_options: {include_usage: true}` was
/// set — forwarded as a [`ProviderStreamEvent::Usage`].
pub fn stream_events(event: &Value) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    if let Some(usage) = usage(&event["usage"]) {
        events.push(ProviderStreamEvent::Usage(usage));
    }
    let delta = &event["choices"][0]["delta"];
    if let Some(content) = delta["content"].as_str().filter(|value| !value.is_empty()) {
        events.push(ProviderStreamEvent::TextDelta(content.to_string()));
    }
    if let Some(reasoning) = reasoning_text(delta) {
        events.push(ProviderStreamEvent::ReasoningDelta(reasoning));
    }
    if let Some(tool_calls) = delta["tool_calls"].as_array() {
        for call in tool_calls {
            events.push(ProviderStreamEvent::ToolCallDelta {
                index: call["index"].as_u64().unwrap_or(0) as usize,
                id: call["id"].as_str().map(str::to_string),
                name: call["function"]["name"].as_str().map(str::to_string),
                arguments: call["function"]["arguments"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    events
}

/// Extract the plain text delta from one stream payload (the simple
/// `stream_chat` path, which ignores reasoning/tool calls).
pub fn stream_text(data: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return String::new();
    };
    let mut content = String::new();
    if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
        content.push_str(delta);
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for the caller-side single parse: production deserializes
    /// each stream payload once and hands the `Value` to [`stream_events`].
    fn parse(data: &str) -> Value {
        serde_json::from_str(data).expect("test event parses")
    }

    #[test]
    fn stream_parser_preserves_tool_call_fragments() {
        let events = stream_events(&parse(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_","arguments":"{\"pa"}}]}}]}"#,
        ));
        assert_eq!(
            events,
            vec![ProviderStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("read_".to_string()),
                arguments: "{\"pa".to_string(),
            }]
        );
    }

    #[test]
    fn stream_text_extracts_content_delta() {
        let text = stream_text(r#"{"choices":[{"delta":{"content":"Hi there"}}]}"#);
        assert_eq!(text, "Hi there");
    }

    #[test]
    fn message_assembles_text_and_tool_calls() {
        let choice = serde_json::json!({
            "content": "hello",
            "tool_calls": [{
                "id": "call_1",
                "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"}
            }]
        });
        let msg = message(&choice, |raw, _| raw.to_string());
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(msg.tool_calls.unwrap()[0].name, "bash");
    }

    #[test]
    fn message_reads_openrouter_reasoning_and_keeps_details_for_replay() {
        let choice = serde_json::json!({
            "content": "",
            "reasoning": "checking the repository",
            "reasoning_details": [{
                "type": "reasoning.text",
                "text": "checking the repository",
                "signature": "sig-1",
                "id": "reasoning-1",
                "format": "qwen3",
                "index": 0
            }],
            "tool_calls": [{
                "id": "call_1",
                "function": {"name": "read", "arguments": "{}"}
            }]
        });

        let msg = message(&choice, |raw, _| raw.to_string());
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("checking the repository")
        );
        assert_eq!(
            msg.provider_meta.as_ref().unwrap()[OPENROUTER_REASONING_DETAILS_META_KEY][0]["signature"],
            "sig-1"
        );
    }

    #[test]
    fn streaming_openrouter_reasoning_details_are_reassembled_by_position_or_index() {
        let mut accumulator = ReasoningDetailsAccumulator::default();
        accumulator.observe(&serde_json::json!({
            "choices": [{"delta": {"reasoning_details": [{
                "type": "reasoning.text", "text": "check ", "index": 0
            }]}}]
        }));
        accumulator.observe(&serde_json::json!({
            "choices": [{"delta": {"reasoning_details": [{
                "text": "files", "signature": "sig"
            }]}}]
        }));

        let artifacts = accumulator.artifacts().unwrap();
        let detail = &artifacts[OPENROUTER_REASONING_DETAILS_META_KEY][0];
        assert_eq!(detail["text"], "check files");
        assert_eq!(detail["signature"], "sig");
    }

    #[test]
    fn usage_surfaces_openai_cached_tokens_as_read() {
        let u = usage(&serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "total_tokens": 1050,
            "prompt_tokens_details": { "cached_tokens": 700 }
        }))
        .unwrap();
        assert_eq!(u.prompt_tokens, 1000);
        assert_eq!(u.cache_read_input_tokens, 700);
        // OpenAI auto-cache has no separate write counter.
        assert_eq!(u.cache_creation_input_tokens, 0);
    }

    #[test]
    fn usage_surfaces_moonshot_top_level_cached_tokens() {
        let u = usage(&serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "cached_tokens": 300
        }))
        .unwrap();
        assert_eq!(u.cache_read_input_tokens, 300);
    }

    #[test]
    fn usage_without_cache_field_has_zero_counters() {
        let u = usage(&serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "total_tokens": 1050
        }))
        .unwrap();
        assert_eq!(u.cache_read_input_tokens, 0);
        assert_eq!(u.cache_creation_input_tokens, 0);
    }
}
