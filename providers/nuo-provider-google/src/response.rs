//! Google Google — response parsing.
//!
//! Pure functions that turn Google's JSON response shape into the harness's
//! domain types. Google reports usage under `usageMetadata`
//! (`promptTokenCount` / `candidatesTokenCount` / `totalTokenCount`), and
//! content lives in `candidates[0].content.parts[]` as either `text` or
//! `functionCall`.

use nuo_model_codec::{Message, ProviderStreamEvent, Role, TokenUsage, ToolCall};
use serde_json::{Map, Value};

pub const THOUGHT_SIGNATURES_META_KEY: &str = "gemini_thought_signatures";
pub const TEXT_THOUGHT_SIGNATURE_META_KEY: &str = "gemini_text_thought_signature";

#[derive(Debug, Clone)]
pub struct StreamPayload {
    pub events: Vec<ProviderStreamEvent>,
    pub thought_signatures: Vec<(String, String)>,
    pub text_thought_signature: Option<String>,
}

/// Parse Google's `usageMetadata` into a [`TokenUsage`]. Returns `None` when
/// the object is absent or has no numeric fields. Google's implicit context
/// caching discount surfaces as `cachedContentTokenCount`; it is surfaced in
/// [`TokenUsage::cache_read_input_tokens`] so the token-source report shows the
/// hit rate. Google exposes no separate cache-write counter. Its hidden
/// reasoning budget surfaces as `thoughtsTokenCount` and is carried in
/// [`TokenUsage::reasoning_tokens`] (already included in
/// `candidatesTokenCount`, never an addition to it).
pub fn usage(usage: &Value) -> Option<TokenUsage> {
    let prompt = usage["promptTokenCount"].as_i64();
    let completion = usage["candidatesTokenCount"].as_i64();
    let total = usage["totalTokenCount"].as_i64();
    let reasoning = usage["thoughtsTokenCount"].as_i64();
    // Route cache-read accounting through the shared helper so the cache
    // policy is enforced in one place (ADR-0161). Google hides the discount in
    // `cachedContentTokenCount`, which the helper reads.
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
            prompt_tokens: prompt.unwrap_or(0),
            completion_tokens: completion.unwrap_or(0),
            total_tokens: t,
            cache_creation_input_tokens: cache.write_tokens,
            cache_read_input_tokens: cache.read_tokens,
            cache_miss_input_tokens: cache.miss_tokens.unwrap_or(0),
            reasoning_tokens: reasoning.unwrap_or(0),
        }),
    }
}

/// Assemble the candidate content of a non-streaming `generateContent`
/// response into one assistant [`Message`]. Text parts become content and
/// `functionCall` parts become native tool calls. Errors when the response has
/// no candidates or no `parts` array.
pub fn message(response: &Value) -> Result<Message, String> {
    let root = response.get("response").unwrap_or(response);
    let candidates = root
        .get("candidates")
        .and_then(|c| c.as_array())
        .ok_or_else(|| format!("Invalid Google response: {}", response))?;

    if candidates.is_empty() {
        return Err("Google returned no candidates".to_string());
    }

    let parts = candidates[0]["content"]
        .get("parts")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "Missing parts in Google response".to_string())?;

    let mut content_text = String::new();
    let mut reasoning_text = String::new();
    let mut tool_calls = Vec::new();
    let mut thought_signatures = Map::new();
    let mut text_thought_signature = None;
    for part in parts {
        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
            if part_is_thought(part) {
                reasoning_text.push_str(text);
            } else {
                content_text.push_str(text);
            }
            if let Some(signature) = thought_signature(part) {
                text_thought_signature = Some(signature);
            }
        }
        if let Some(call) = part.get("functionCall").and_then(function_call) {
            if let Some(signature) = thought_signature(part) {
                thought_signatures.insert(call.id.clone(), Value::String(signature.clone()));
                thought_signatures.insert(call.name.clone(), Value::String(signature));
            }
            tool_calls.push(call);
        }
    }

    if let Some(signature) = &text_thought_signature {
        for call in &tool_calls {
            thought_signatures
                .entry(call.id.clone())
                .or_insert_with(|| Value::String(signature.clone()));
            thought_signatures
                .entry(call.name.clone())
                .or_insert_with(|| Value::String(signature.clone()));
        }
    }

    let mut message = Message::new(Role::Assistant, content_text);
    message.tool_calls = (!tool_calls.is_empty()).then_some(tool_calls);
    if !reasoning_text.is_empty() {
        message.reasoning_content = Some(reasoning_text);
    }
    if !thought_signatures.is_empty() {
        let provider_meta = message.provider_meta.get_or_insert_with(Map::new);
        provider_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            Value::Object(thought_signatures),
        );
    }
    if let Some(signature) = text_thought_signature {
        let mut provider_meta = Map::new();
        provider_meta.insert(
            TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
            Value::String(signature),
        );
        if let Some(existing) = message.provider_meta.as_mut() {
            existing.extend(provider_meta);
        } else {
            message.provider_meta = Some(provider_meta);
        }
    }
    Ok(message)
}

/// Parse one `streamGenerateContent` SSE payload and concatenate the text from
/// `candidates[0].content.parts[].text`. Returns an empty string when the
/// payload carries no text part (e.g. a finish-reason-only chunk).
pub fn stream_text(payload: &str) -> String {
    stream_events(payload)
        .into_iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::TextDelta(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// Parse one `streamGenerateContent` SSE payload into normalized provider
/// events. Google may stream text, usage metadata, and whole `functionCall`
/// parts. Function calls are emitted as one delta per part because the REST
/// stream does not use OpenAI-style argument fragments.
pub fn stream_events(payload: &str) -> Vec<ProviderStreamEvent> {
    stream_payload(payload).events
}

/// Parse one `streamGenerateContent` SSE payload into normalized provider
/// events plus any Google thought signatures attached to function-call parts.
pub fn stream_payload(payload: &str) -> StreamPayload {
    // Single-parse entry point (ADR-0184): callers holding an already-parsed
    // payload should prefer [`stream_payload_value`] to avoid re-parsing.
    match serde_json::from_str(payload) {
        Ok(value) => stream_payload_value(&value),
        Err(_) => StreamPayload {
            events: Vec::new(),
            thought_signatures: Vec::new(),
            text_thought_signature: None,
        },
    }
}

/// Assemble the stream payload from an already-parsed JSON value — the
/// single-parse path used by the SSE executor (ADR-0184).
pub fn stream_payload_value(value: &Value) -> StreamPayload {
    let root = value.get("response").unwrap_or(value);
    let mut events = Vec::new();
    let mut thought_signatures = Vec::new();
    let mut text_thought_signature = None;
    if let Some(usage) = usage(&root["usageMetadata"]) {
        events.push(ProviderStreamEvent::Usage(usage));
    }
    if let Some(parts) = root
        .get("candidates")
        .and_then(|candidates| candidates.as_array())
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate["content"]["parts"].as_array())
    {
        let mut call_index = 0usize;
        for part in parts {
            if let Some(signature) = thought_signature(part)
                && part.get("functionCall").is_none()
            {
                text_thought_signature = Some(signature);
            }
            if let Some(text) = part.get("text").and_then(|text| text.as_str())
                && !text.is_empty()
            {
                // Route reasoning summaries to ReasoningDelta so the harness
                // surfaces them as a thinking trace, distinct from the answer.
                if part_is_thought(part) {
                    events.push(ProviderStreamEvent::ReasoningDelta(text.to_string()));
                } else {
                    events.push(ProviderStreamEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(call) = part.get("functionCall").and_then(function_call) {
                if let Some(signature) = thought_signature(part) {
                    thought_signatures.push((call.id.clone(), signature));
                }
                events.push(ProviderStreamEvent::ToolCallDelta {
                    index: call_index,
                    id: Some(call.id),
                    name: Some(call.name),
                    arguments: call.arguments,
                });
                call_index += 1;
            }
        }
    }
    if text_thought_signature.is_none()
        && let Some(candidate) = root
            .get("candidates")
            .and_then(|candidates| candidates.as_array())
            .and_then(|candidates| candidates.first())
        && let Some(signature) = thought_signature(candidate)
    {
        text_thought_signature = Some(signature);
    }
    StreamPayload {
        events,
        thought_signatures,
        text_thought_signature,
    }
}

fn function_call(value: &Value) -> Option<ToolCall> {
    let name = value.get("name")?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
    let arguments = value
        .get("args")
        .filter(|args| !args.is_null())
        .map(Value::to_string)
        .unwrap_or_else(|| "{}".to_string());
    Some(ToolCall {
        id,
        name,
        arguments,
    })
}

fn thought_signature(part: &Value) -> Option<String> {
    part.get("thoughtSignature")
        .or_else(|| part.get("thought_signature"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Whether this part is a Google reasoning *summary* — the model's thinking
/// text, surfaced only when the request set
/// `generationConfig.thinkingConfig.includeThoughts`. Such parts carry
/// `"thought": true` and MUST be routed to `ReasoningDelta` /
/// `reasoning_content`, never folded into the answer `content` (otherwise the
/// reasoning leaks into the visible reply).
fn part_is_thought(part: &Value) -> bool {
    part.get("thought")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether an upstream error rejects the `thinkingConfig` we stamped — i.e.
/// the request asked for `includeThoughts` / a `thinkingLevel` and the endpoint
/// answered that it does not accept that field **for this model**. This is the
/// "chain withheld" family of upstream behavior: a model served through a
/// legacy or filtered route may reason internally while refusing to disclose
/// the chain, and Google's error surfaces that as an `INVALID_ARGUMENT` naming
/// `thinkingConfig` (or one of its keys) / `include_thoughts` / `thought`.
///
/// Deliberately narrow: a bare 400 without a thinking-related token is some
/// other contract violation and must keep failing loudly. The match is
/// case-insensitive because relays re-case Google's field names freely, and it
/// accepts both the camelCase (`thinkingConfig`) and snake_case
/// (`thinking_config`) spellings for the same reason.
pub fn rejects_thinking_config(error: &str) -> bool {
    let haystack = error.to_ascii_lowercase();
    let names_thinking = [
        "thinkingconfig",
        "thinking_config",
        "include_thoughts",
        "includethoughts",
        "thinkinglevel",
        "thinking_level",
        "thinkingbudget",
        "thinking_budget",
    ]
    .iter()
    .any(|needle| haystack.contains(needle));
    if !names_thinking {
        return false;
    }
    [
        "invalid_argument",
        "invalid argument",
        "http 400",
        "unknown name",
        "unsupported",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

/// Augment a transport-layer error with model-specific guidance. For native
/// Google, a `404 NOT_FOUND` almost always means the upstream does not serve
/// this model id — not a transient fault. `429 RESOURCE_EXHAUSTED` explains
/// quota / rate limits. Other errors pass through unchanged.
///
/// The input may be a `[NUO_RETRYABLE]`-enveloped error (429/5xx from
/// [`ensure_success`](crate::transport::ensure_success)); appending to the
/// envelope verbatim would corrupt its JSON and strip the error of its
/// retryable classification downstream, so the guidance is folded **into**
/// the envelope's message and any `RetryInfo` delay Google embedded in the
/// body is promoted to `retry_after_ms` when the envelope has none.
pub fn clarify_error(
    err: nuo_model_codec::ProviderError,
    model: &str,
    base_url: &str,
) -> nuo_model_codec::ProviderError {
    let server_delay = google_retry_after_ms(err.message());
    let mut err = err.map_message(|msg| clarify_message(msg, model, base_url));
    if server_delay.is_some() {
        err = err.with_retry_after_if_absent(server_delay);
    }
    err
}

/// The plain-message half of [`clarify_error`]: append guidance only, never
/// touching any envelope structure.
fn clarify_message(err: String, model: &str, base_url: &str) -> String {
    if err.contains("HTTP 404") || err.contains("\"status\": \"NOT_FOUND\"") {
        format!(
            "{err}\n\n\
             Google returned 404 for model `{model}` at {base_url}. \
             The requested endpoint or model was not found. Check the provider's \
             endpoint and protocol dialect, then its model availability; HTTP 404 \
             alone does not establish that the model is unsupported."
        )
    } else if err.contains("HTTP 429") || err.contains("RESOURCE_EXHAUSTED") {
        let quota_reset = google_quota_reset_hint(&err);
        format!(
            "{err}\n\n\
             Google rate limit / quota exhausted (RESOURCE_EXHAUSTED).{quota_reset}"
        )
    } else {
        err
    }
}

/// Extract Google's own reset hint when the error body carries one. Google
/// attaches `details[]: [{...QuotaFailure}, {@type: RetryInfo, retryDelay:
/// "45s"}]` to a 429; a `RetryInfo` delay is authoritative, so it is quoted
/// verbatim. Legacy prose (the old fixed 45–60 minute guess) is not invented
/// when Google said nothing — an invented number reads as a promise.
fn google_quota_reset_hint(err: &str) -> String {
    if let Some(milliseconds) = google_retry_after_ms(err) {
        let seconds = milliseconds / 1000;
        let human = if seconds >= 3600 {
            format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
        } else if seconds >= 60 {
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        } else {
            format!("{seconds}s")
        };
        return format!("\nGoogle reports the quota resets in ~{human} (`RetryInfo.retryDelay`).");
    }
    "\nYour Google One / Antigravity request quota is exhausted; it resets at the \
     start of the next window (5-hour or daily/weekly, whichever limit tripped)."
        .to_string()
}

/// Parse `RetryInfo`'s `retryDelay` (or a plain `retryDelay`) out of a Google
/// error body. Accepts `"45s"`, `"1.5s"`, `"2m30s"`-style durations plus a
/// bare second count; returns milliseconds.
fn google_retry_after_ms(err: &str) -> Option<u64> {
    let value = serde_json::from_str::<serde_json::Value>(
        err.split_once(": ").map(|(_, rest)| rest).unwrap_or(err),
    )
    .ok()
    .or_else(|| {
        // The body may be prefixed with provider framing; find the first
        // `{` from the first `HTTP 429`/`RESOURCE_EXHAUSTED` mention onward.
        err.find('{').and_then(|start| {
            let end = err.rfind('}')?;
            err.get(start..=end)
                .map(str::to_string)
                .and_then(|slice| serde_json::from_str::<serde_json::Value>(&slice).ok())
        })
    })?;
    let retry_delay = find_retry_delay(&value)?;
    parse_google_duration(&retry_delay)
}

/// Depth-first search for the first `retryDelay` string anywhere in the
/// payload (Google nests it under `details[]` of the error).
fn find_retry_delay(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(delay)) = map.get("retryDelay") {
                return Some(delay.clone());
            }
            for child in map.values() {
                if let Some(found) = find_retry_delay(child) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => {
            for child in items {
                if let Some(found) = find_retry_delay(child) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Parse a Google protobuf duration string (`"45s"`, `"1.500s"`, `"90m"`) or
/// a bare number of seconds into milliseconds.
fn parse_google_duration(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if let Ok(seconds) = trimmed.parse::<f64>() {
        return Some((seconds.max(0.0) * 1000.0) as u64);
    }
    let mut total_seconds = 0.0f64;
    let mut matched = false;
    let mut rest = trimmed;
    while let Some(pos) = rest.find(|c: char| c.is_ascii_digit() || c == '.') {
        let digits: String = rest[pos..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let unit_start = pos + digits.len();
        let unit: String = rest[unit_start..]
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        if digits.is_empty() || unit.is_empty() {
            break;
        }
        let value: f64 = digits.parse().ok()?;
        let multiplier = match unit.as_str() {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            "ms" => 0.001,
            _ => return None,
        };
        total_seconds += value * multiplier;
        matched = true;
        rest = &rest[unit_start + unit.len()..];
    }
    matched.then(|| (total_seconds.max(0.0) * 1000.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_text_concatenates_parts_and_preserves_multibyte() {
        let payload = r#"{"candidates":[{"content":{"parts":[{"text":"😀😁"},{"text":"😃😄"}]}}]}"#;
        assert_eq!(stream_text(payload), "😀😁😃😄");
    }

    #[test]
    fn stream_text_returns_empty_for_non_text_payload() {
        assert_eq!(
            stream_text(r#"{"candidates":[{"finishReason":"STOP"}]}"#),
            ""
        );
        assert_eq!(stream_text("not json"), "");
    }

    #[test]
    fn message_extracts_function_calls() {
        let value: Value = serde_json::from_str(
            r#"{
              "candidates": [{
                "content": {
                  "parts": [
                    {
                      "thoughtSignature": "sig-1",
                      "functionCall": {"id": "abc", "name": "list_dir", "args": {"path": "."}}
                    },
                    {"thoughtSignature": "text-sig", "text": "done"}
                  ]
                }
              }]
            }"#,
        )
        .unwrap();

        let message = super::message(&value).unwrap();
        assert_eq!(message.content, "done");
        let calls = message.tool_calls.unwrap();
        assert_eq!(calls[0].id, "abc");
        assert_eq!(calls[0].name, "list_dir");
        assert_eq!(calls[0].arguments, r#"{"path":"."}"#);
        assert_eq!(
            message.provider_meta.unwrap()[THOUGHT_SIGNATURES_META_KEY]["abc"],
            "sig-1"
        );
        let value: Value = serde_json::from_str(
            r#"{"candidates":[{"content":{"parts":[{"thoughtSignature":"text-sig","text":"done"}]}}]}"#,
        )
        .unwrap();
        let message = super::message(&value).unwrap();
        assert_eq!(
            message.provider_meta.unwrap()[TEXT_THOUGHT_SIGNATURE_META_KEY],
            "text-sig"
        );
    }

    #[test]
    fn stream_events_extract_function_calls_and_usage() {
        let events = stream_events(
            r#"{
              "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 2,
                "totalTokenCount": 12
              },
              "candidates": [{
                "content": {
                  "parts": [
                    {"text": "thinking"},
                    {
                      "thoughtSignature": "sig-2",
                      "functionCall": {"id": "abc", "name": "grep", "args": {"query": "x"}}
                    }
                  ]
                }
              }]
            }"#,
        );

        assert!(matches!(events[0], ProviderStreamEvent::Usage(_)));
        assert_eq!(
            events[1],
            ProviderStreamEvent::TextDelta("thinking".to_string())
        );
        assert_eq!(
            events[2],
            ProviderStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("abc".to_string()),
                name: Some("grep".to_string()),
                arguments: r#"{"query":"x"}"#.to_string(),
            }
        );

        let payload = stream_payload(
            r#"{"candidates":[{"content":{"parts":[{"thought_signature":"sig-3","functionCall":{"id":"xyz","name":"list_dir","args":{}}}]}}]}"#,
        );
        assert_eq!(
            payload.thought_signatures,
            vec![("xyz".to_string(), "sig-3".to_string())]
        );
    }

    #[test]
    fn stream_routes_thought_parts_to_reasoning_delta() {
        // A response with a thought summary followed by the answer. Only the
        // `"thought": true` part must become ReasoningDelta; the plain text
        // part stays a TextDelta.
        let events = stream_events(
            r#"{
              "candidates": [{
                "content": {
                  "parts": [
                    {"text": "let me consider the angles...", "thought": true},
                    {"text": "The answer is 42."}
                  ]
                }
              }]
            }"#,
        );
        assert_eq!(
            events[0],
            ProviderStreamEvent::ReasoningDelta("let me consider the angles...".to_string())
        );
        assert_eq!(
            events[1],
            ProviderStreamEvent::TextDelta("The answer is 42.".to_string())
        );
    }

    #[test]
    fn message_routes_thought_parts_to_reasoning_content() {
        // Non-streaming: thought text goes to reasoning_content, never content.
        let value: Value = serde_json::from_str(
            r#"{
              "candidates": [{
                "content": {
                  "parts": [
                    {"text": "reasoning step by step", "thought": true},
                    {"text": "final answer"}
                  ]
                }
              }]
            }"#,
        )
        .unwrap();
        let message = super::message(&value).unwrap();
        assert_eq!(message.content, "final answer");
        assert_eq!(
            message.reasoning_content.as_deref(),
            Some("reasoning step by step")
        );
    }

    #[test]
    fn message_without_thought_flag_keeps_all_text_in_content() {
        // No `thought` flag → nothing is reasoning; all text is the answer.
        // Guards against accidentally treating ordinary text as thinking.
        let value: Value = serde_json::from_str(
            r#"{
              "candidates": [{
                "content": {
                  "parts": [
                    {"text": "part one "},
                    {"text": "part two"}
                  ]
                }
              }]
            }"#,
        )
        .unwrap();
        let message = super::message(&value).unwrap();
        assert_eq!(message.content, "part one part two");
        assert!(message.reasoning_content.is_none());
    }

    #[test]
    fn usage_surfaces_google_cached_content_tokens_as_read() {
        let u = usage(&serde_json::json!({
            "promptTokenCount": 900,
            "candidatesTokenCount": 30,
            "totalTokenCount": 930,
            "cachedContentTokenCount": 600
        }))
        .unwrap();
        assert_eq!(u.prompt_tokens, 900);
        assert_eq!(u.cache_read_input_tokens, 600);
        assert_eq!(u.cache_creation_input_tokens, 0);
    }

    #[test]
    fn parses_antigravity_wrapped_response_and_stream() {
        let wrapped_json = serde_json::json!({
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [
                            {"text": "thinking deep", "thought": true},
                            {"text": "Antigravity response"}
                        ]
                    }
                }],
                "usageMetadata": {
                    "promptTokenCount": 100,
                    "candidatesTokenCount": 20,
                    "totalTokenCount": 120
                }
            },
            "responseId": "resp-123"
        });

        let msg = super::message(&wrapped_json).unwrap();
        assert_eq!(msg.content, "Antigravity response");
        assert_eq!(msg.reasoning_content.as_deref(), Some("thinking deep"));

        let stream_str = wrapped_json.to_string();
        let payload = stream_payload(&stream_str);
        assert_eq!(payload.events.len(), 3); // Usage + ReasoningDelta + TextDelta
        assert_eq!(
            payload.events[0],
            ProviderStreamEvent::Usage(TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
                ..Default::default()
            })
        );
        assert_eq!(
            payload.events[1],
            ProviderStreamEvent::ReasoningDelta("thinking deep".to_string())
        );
        assert_eq!(
            payload.events[2],
            ProviderStreamEvent::TextDelta("Antigravity response".to_string())
        );
    }

    #[test]
    fn clarify_error_appends_guidance_to_retryable_error() {
        let raw = nuo_model_codec::ProviderError::new(
            "Google",
            nuo_model_codec::ProviderErrorKind::RateLimited,
            "Google HTTP 429 Too Many Requests: {\"error\":{\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\"}}",
        )
        .with_status(429)
        .retryable(None);
        let clarified = super::clarify_error(
            raw,
            "gemini-3.7-flash",
            "https://cloudcode-pa.googleapis.com",
        );

        assert_eq!(clarified.status(), Some(429));
        assert_eq!(
            clarified.retry_disposition(),
            nuo_model_codec::RetryDisposition::Retry {
                retry_after_ms: None
            }
        );
        assert!(
            clarified.message().contains("RESOURCE_EXHAUSTED"),
            "the provider body survives: {}",
            clarified.message()
        );
        assert!(
            clarified
                .message()
                .contains("Google rate limit / quota exhausted"),
            "the guidance is attached: {}",
            clarified.message()
        );
    }

    #[test]
    fn clarify_error_promotes_google_retryinfo_delay_into_retry_after_ms() {
        let body = r#"{"error":{"code":429,"message":"Resource has been exhausted (e.g. check quota).","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"3620s"}]}}"#;
        let raw = nuo_model_codec::ProviderError::new(
            "Google",
            nuo_model_codec::ProviderErrorKind::RateLimited,
            format!("Google HTTP 429 Too Many Requests: {body}"),
        )
        .with_status(429)
        .retryable(None);
        let clarified = super::clarify_error(raw, "gemini-3.7-flash", "https://x");
        assert_eq!(
            clarified.retry_disposition(),
            nuo_model_codec::RetryDisposition::Retry {
                retry_after_ms: Some(3_620_000)
            }
        );
        assert!(
            clarified.message().contains("~1h 00m"),
            "the reset hint is humanized from Google's own delay: {}",
            clarified.message()
        );
    }

    #[test]
    fn clarify_error_preserves_an_existing_retry_after() {
        let raw = nuo_model_codec::ProviderError::new(
            "Google",
            nuo_model_codec::ProviderErrorKind::RateLimited,
            "Google HTTP 429: {\"error\":{\"status\":\"RESOURCE_EXHAUSTED\",\"details\":[{\"retryDelay\":\"45s\"}]}}",
        )
        .with_status(429)
        .retryable(Some(120_000));
        let clarified = super::clarify_error(raw, "m", "https://x");
        assert_eq!(
            clarified.retry_disposition(),
            nuo_model_codec::RetryDisposition::Retry {
                retry_after_ms: Some(120_000)
            }
        );
    }

    #[test]
    fn clarify_error_passes_non_retryable_errors_through_with_guidance() {
        let raw = nuo_model_codec::ProviderError::new(
            "Google",
            nuo_model_codec::ProviderErrorKind::InvalidRequest,
            "Google HTTP 404 Not Found",
        )
        .with_status(404);
        let clarified = super::clarify_error(raw, "m", "https://x");
        assert!(clarified.message().starts_with("Google HTTP 404"));
        assert!(
            clarified
                .message()
                .contains("endpoint and protocol dialect")
        );
        assert!(!clarified.message().contains("Switch to a model"));
        assert_eq!(
            clarified.retry_disposition(),
            nuo_model_codec::RetryDisposition::Never
        );
    }

    #[test]
    fn parse_google_duration_accepts_protobuf_and_bare_forms() {
        use super::parse_google_duration;
        assert_eq!(parse_google_duration("45s"), Some(45_000));
        assert_eq!(parse_google_duration("1.500s"), Some(1_500));
        assert_eq!(parse_google_duration("2m30s"), Some(150_000));
        assert_eq!(parse_google_duration("1h"), Some(3_600_000));
        assert_eq!(parse_google_duration("0.5s"), Some(500));
        assert_eq!(parse_google_duration("30"), Some(30_000));
        assert_eq!(parse_google_duration("nonsense"), None);
    }

    #[test]
    fn rejects_thinking_config_matches_google_invalid_argument() {
        use super::rejects_thinking_config;
        // The canonical Google shape: INVALID_ARGUMENT naming the field.
        assert!(rejects_thinking_config(
            "Google HTTP 400: {\"error\":{\"code\":400,\"status\":\"INVALID_ARGUMENT\",\
             \"message\":\"Invalid JSON payload received. Unknown name \\\"thinkingConfig\\\" \
             at 'generation_config': Cannot find field.\"}}"
        ));
        // Relay re-casings and the snake_case spelling both match.
        assert!(rejects_thinking_config(
            "Google HTTP 400: unknown name include_thoughts"
        ));
        assert!(rejects_thinking_config(
            "Google HTTP 400: thinking_level is not supported on this model"
        ));
    }

    #[test]
    fn rejects_thinking_config_ignores_other_400s() {
        use super::rejects_thinking_config;
        // A 400 that does not name the thinking surface is some other
        // contract violation — it must keep failing loudly.
        assert!(!rejects_thinking_config(
            "Google HTTP 400: {\"error\":{\"status\":\"INVALID_ARGUMENT\",\
             \"message\":\"Request payload size exceeds the limit.\"}}"
        ));
        // A thinking-related message without an invalid-argument signal
        // (e.g. quoted inside unrelated prose) does not downgrade either.
        assert!(!rejects_thinking_config("thinkingConfig is great"));
        // Retryable exhaustion naming nothing about thinking stays itself.
        assert!(!rejects_thinking_config(
            "Google HTTP 429: RESOURCE_EXHAUSTED thinking about quota"
        ));
    }
}
