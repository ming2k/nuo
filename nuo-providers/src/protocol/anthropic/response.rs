//! Anthropic Messages — response parsing.
//!
//! Pure functions turning Anthropic's `/messages` response shape into the
//! harness's domain types: content-block assembly (text + thinking + tool_use),
//! the `usage` object (with cache-token folding), and the per-event stream
//! parsing.

use nuo_contracts::{Message, ProviderStreamEvent, Role, TokenUsage, ToolCall};
use serde_json::{Map, Value};

/// The assembled pieces of an Anthropic assistant response: the text content,
/// the reasoning content, and any tool calls. The provider also extracts a
/// thinking signature (captured separately for replay) and stashes the usage.
pub struct AssembledMessage {
    pub content: String,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    /// The `signature` of the most recent `thinking` block, to stash in
    /// `provider_meta` for replay on the next turn.
    pub thinking_signature: Option<String>,
}

/// Parse an Anthropic `usage` object into a [`TokenUsage`]. Returns `None`
/// when the object is absent or has no numeric fields.
///
/// `prompt_tokens` ← `input_tokens` + `cache_creation_input_tokens` +
/// `cache_read_input_tokens` (Anthropic's `input_tokens` is ONLY the uncached
/// dynamic suffix; the cache counts must be folded in or every cached turn is
/// undercounted). `completion_tokens` ← `output_tokens`. The cache counters are
/// kept verbatim for the ledger's separate cache accounting.
pub fn usage(usage: &Value) -> Option<TokenUsage> {
    let input = usage["input_tokens"].as_i64();
    let output = usage["output_tokens"].as_i64();
    let cache_creation = usage["cache_creation_input_tokens"].as_i64().unwrap_or(0);
    let cache_read = usage["cache_read_input_tokens"].as_i64().unwrap_or(0);
    let (uncached_input, c) = match (input, output) {
        (Some(p), Some(c)) => (p, c),
        (Some(p), None) => (p, 0),
        (None, Some(c)) => (0, c),
        (None, None) => return None,
    };
    let prompt = uncached_input + cache_creation + cache_read;
    Some(TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: c,
        total_tokens: prompt + c,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
        cache_miss_input_tokens: 0,
        // Anthropic reports no reasoning-token counter in `usage`; the
        // reasoning volume rides inside `output_tokens`.
        reasoning_tokens: 0,
    })
}

/// Running token counts for one SSE stream.
///
/// Anthropic splits a turn's usage across two event types: `message_start`
/// carries the input side (`input_tokens` plus the `cache_creation_input_tokens`
/// / `cache_read_input_tokens` prompt-cache counters) while `message_delta`
/// carries only the cumulative `output_tokens`. Neither event is complete on
/// its own, and the harness books the LAST `Usage` event it observes — so
/// forwarding each event's partial usage verbatim would book a prompt of 0 and
/// drop every cache counter on each cached streaming turn. This accumulator
/// merges the two (same rule as praxion's stream decoder); [`stream_events`]
/// emits the folded [`TokenUsage`].
#[derive(Debug, Default)]
pub struct StreamUsage {
    uncached_input: Option<i64>,
    cache_creation: Option<i64>,
    cache_read: Option<i64>,
    output: Option<i64>,
}

impl StreamUsage {
    /// Merge one event's `usage` object. The input side is a complete snapshot
    /// when present (`message_start`, or relays that repeat it in
    /// `message_delta`): all three counters are replaced together, never added.
    /// `output_tokens` is cumulative and is replaced whenever reported.
    pub fn merge(&mut self, usage: &Value) {
        let input = usage["input_tokens"].as_i64();
        let creation = usage["cache_creation_input_tokens"].as_i64();
        let read = usage["cache_read_input_tokens"].as_i64();
        if input.is_some() || creation.is_some() || read.is_some() {
            self.uncached_input = input;
            self.cache_creation = creation;
            self.cache_read = read;
        }
        if let Some(output) = usage["output_tokens"].as_i64() {
            self.output = Some(output);
        }
    }

    /// Fold the accumulated counts into a [`TokenUsage`] using [`usage`]'s
    /// rule (`prompt_tokens` = uncached input + both cache counters). Returns
    /// `None` when nothing numeric has been seen yet.
    pub fn fold(&self) -> Option<TokenUsage> {
        if self.uncached_input.is_none()
            && self.cache_creation.is_none()
            && self.cache_read.is_none()
            && self.output.is_none()
        {
            return None;
        }
        let input = self.uncached_input.unwrap_or(0);
        let creation = self.cache_creation.unwrap_or(0);
        let read = self.cache_read.unwrap_or(0);
        let prompt = input + creation + read;
        let completion = self.output.unwrap_or(0);
        Some(TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cache_creation_input_tokens: creation,
            cache_read_input_tokens: read,
            cache_miss_input_tokens: 0,
            // Anthropic streams no reasoning-token counter; `0` = unreported.
            reasoning_tokens: 0,
        })
    }
}

/// Assemble the `content` block array of a non-streaming response into one
/// assistant message's pieces.
pub fn assemble_message(response: &Value) -> Result<AssembledMessage, String> {
    let mut content = String::new();
    let mut reasoning_content: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut thinking_signature: Option<String> = None;

    if let Some(err) = response.get("error") {
        return Err(format!("Anthropic Error: {}", err));
    }

    if let Some(blocks) = response["content"].as_array() {
        for block in blocks {
            match block["type"].as_str().unwrap_or("") {
                "text" => {
                    if let Some(text) = block["text"].as_str() {
                        content.push_str(text);
                    }
                }
                "thinking" => {
                    if let Some(text) = block["thinking"].as_str() {
                        reasoning_content = Some(text.to_string());
                    }
                    // The thinking block carries a `signature` the server needs
                    // to reconstruct it on the next replay.
                    if let Some(sig) = block["signature"].as_str()
                        && !sig.is_empty()
                    {
                        thinking_signature = Some(sig.to_string());
                    }
                }
                "tool_use" => {
                    tool_calls.push(ToolCall {
                        id: block["id"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4())),
                        name: block["name"].as_str().unwrap_or("").to_string(),
                        arguments: block
                            .get("input")
                            .map(|v| v.to_string())
                            .unwrap_or_default(),
                    });
                }
                _ => {}
            }
        }
    }

    Ok(AssembledMessage {
        content,
        reasoning_content,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        thinking_signature,
    })
}

/// Turn an [`AssembledMessage`] into a domain [`Message`], stamping the
/// thinking signature into `provider_meta` when present.
pub fn into_message(
    AssembledMessage {
        content,
        reasoning_content,
        tool_calls,
        thinking_signature,
    }: AssembledMessage,
) -> Message {
    let provider_meta = thinking_signature.map(|sig| {
        let mut map = Map::new();
        map.insert("thinking_signature".to_string(), Value::String(sig));
        map
    });
    Message {
        role: Role::Assistant,
        content,
        content_blob: None,
        display_content: None,
        reasoning_content,
        provider_meta,
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
        timestamp: Some(nuo_contracts::todos::unix_now()),
        sent_at_ms: None,
        cache_frozen: false,
    }
}

/// Parse one already-parsed SSE `data:` payload into provider stream events.
/// Anthropic wraps each event in `{type, ...}`; the `type` discriminator
/// selects the block/delta shape. The caller parses each stream payload
/// exactly once and passes the `Value` here (non-JSON payloads — relay
/// keep-alives and stray comment lines — are skipped by the caller before
/// this function ever runs).
///
/// `usage_state` accumulates token counts across the stream (see
/// [`StreamUsage`]). Events carrying a `usage` object — `message_start`
/// (`message.usage`) and `message_delta` (`usage`) — emit the merged
/// [`TokenUsage`] as a [`ProviderStreamEvent::Usage`], so the harness's
/// last-Usage-wins booking settles on the full input + cache + output counts
/// instead of the delta's output-only fragment.
///
/// Returns `Err` only for an in-stream `error` event; other non-content events
/// are no-ops that yield no events.
pub fn stream_events(
    event: &Value,
    usage_state: &mut StreamUsage,
) -> Result<Vec<ProviderStreamEvent>, String> {
    let event_type = event["type"].as_str().unwrap_or("");
    match event_type {
        "error" => {
            let message = event["error"]["message"]
                .as_str()
                .unwrap_or("Anthropic stream error")
                .to_string();
            Err(message)
        }
        "content_block_start" => {
            let index = event["index"].as_u64().unwrap_or(0) as usize;
            let block = &event["content_block"];
            let block_type = block["type"].as_str().unwrap_or("");
            if block_type == "tool_use" {
                Ok(vec![ProviderStreamEvent::ToolCallDelta {
                    index,
                    id: block["id"].as_str().map(str::to_string),
                    name: block["name"].as_str().map(str::to_string),
                    arguments: String::new(),
                }])
            } else {
                Ok(Vec::new())
            }
        }
        "content_block_delta" => {
            let index = event["index"].as_u64().unwrap_or(0) as usize;
            let delta = &event["delta"];
            match delta["type"].as_str().unwrap_or("") {
                "text_delta" => Ok(delta["text"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(|t| ProviderStreamEvent::TextDelta(t.to_string()))
                    .into_iter()
                    .collect()),
                "thinking_delta" => Ok(delta["thinking"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(|t| ProviderStreamEvent::ReasoningDelta(t.to_string()))
                    .into_iter()
                    .collect()),
                "input_json_delta" => {
                    let frag = delta["partial_json"].as_str().unwrap_or("");
                    Ok(vec![ProviderStreamEvent::ToolCallDelta {
                        index,
                        id: None,
                        name: None,
                        arguments: frag.to_string(),
                    }])
                }
                _ => Ok(Vec::new()),
            }
        }
        // Usage-bearing events: merge into the accumulator and emit the
        // combined counts. `message_start` reports the input side (plus cache
        // counters) up front; `message_delta` reports the final cumulative
        // output right before message_stop.
        "message_start" => Ok(merge_usage_event(usage_state, &event["message"]["usage"])),
        "message_delta" => Ok(merge_usage_event(usage_state, &event["usage"])),
        _ => Ok(Vec::new()),
    }
}

/// Merge one payload's `usage` member into `state` and emit the folded
/// [`TokenUsage`]. Payloads without a `usage` member yield nothing, so
/// usage-less events never re-emit stale counts.
fn merge_usage_event(state: &mut StreamUsage, usage: &Value) -> Vec<ProviderStreamEvent> {
    if usage.is_null() {
        return Vec::new();
    }
    state.merge(usage);
    state
        .fold()
        .into_iter()
        .map(ProviderStreamEvent::Usage)
        .collect()
}

/// Extract the plain text delta from one stream payload (the simple
/// `stream_chat` path, which ignores reasoning/tool calls).
pub fn stream_text(data: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return String::new();
    };
    let mut text = String::new();
    if v["type"].as_str() == Some("content_block_delta")
        && v["delta"]["type"].as_str() == Some("text_delta")
        && let Some(t) = v["delta"]["text"].as_str()
    {
        text.push_str(t);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for the caller-side single parse: production parses each
    /// stream payload once and hands the `Value` to [`stream_events`].
    fn parse(data: &str) -> Value {
        serde_json::from_str(data).expect("test event parses")
    }

    #[test]
    fn stream_parser_extracts_text_and_tool_deltas() {
        let mut state = StreamUsage::default();
        // A text delta event.
        let text_events = stream_events(
            &parse(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#),
            &mut state,
        )
        .expect("text delta parses");
        assert_eq!(
            text_events,
            vec![ProviderStreamEvent::TextDelta("Hello".to_string())]
        );

        // A tool_use block opening: id and name arrive up front.
        let open_events = stream_events(
            &parse(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash"}}"#),
            &mut state,
        )
        .expect("content_block_start parses");
        assert_eq!(
            open_events,
            vec![ProviderStreamEvent::ToolCallDelta {
                index: 1,
                id: Some("toolu_1".to_string()),
                name: Some("bash".to_string()),
                arguments: String::new(),
            }]
        );

        // Argument JSON fragments arrive as input_json_delta; the harness
        // concatenates them.
        let frag_events = stream_events(
            &parse(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}"#),
            &mut state,
        )
        .expect("input_json_delta parses");
        assert_eq!(
            frag_events,
            vec![ProviderStreamEvent::ToolCallDelta {
                index: 1,
                id: None,
                name: None,
                arguments: "{\"comm".to_string(),
            }]
        );
    }

    #[test]
    fn stream_parser_extracts_reasoning_deltas() {
        let events = stream_events(
            &parse(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}"#),
            &mut StreamUsage::default(),
        )
        .expect("thinking_delta parses");
        assert_eq!(
            events,
            vec![ProviderStreamEvent::ReasoningDelta("hm".to_string())]
        );
    }

    #[test]
    fn stream_parser_surfaces_error_events_as_err() {
        let result = stream_events(
            &parse(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            ),
            &mut StreamUsage::default(),
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("Overloaded"),
            "error message must be surfaced"
        );
    }

    #[test]
    fn stream_parser_ignores_non_content_events() {
        // Parsing lives one layer up (each stream payload is parsed exactly
        // once, and non-JSON payloads are skipped by the caller before this
        // function runs) — here only well-formed non-content events matter.
        let mut state = StreamUsage::default();
        for payload in [
            r#"{"type":"message_start","message":{"id":"msg_1"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            r#"{"type":"message_stop"}"#,
            r#"{"type":"content_block_stop","index":0}"#,
        ] {
            let events =
                stream_events(&parse(payload), &mut state).expect("non-content event is ok");
            assert!(
                events.is_empty(),
                "non-content event must yield nothing: {payload}"
            );
        }
    }

    #[test]
    fn stream_usage_merges_message_start_and_message_delta() {
        // Anthropic splits usage across events: message_start carries the
        // input side (incl. prompt-cache counters), message_delta only the
        // cumulative output. The harness books the LAST Usage event, so the
        // merged counts must be emitted or every cached streaming turn books
        // prompt = 0 and drops the cache counters.
        let mut state = StreamUsage::default();
        let start_events = stream_events(
            &parse(r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":25,"cache_creation_input_tokens":100,"cache_read_input_tokens":400,"output_tokens":1}}}"#),
            &mut state,
        )
        .expect("message_start parses");
        assert_eq!(
            start_events,
            vec![ProviderStreamEvent::Usage(TokenUsage {
                prompt_tokens: 525,
                completion_tokens: 1,
                total_tokens: 526,
                cache_creation_input_tokens: 100,
                cache_read_input_tokens: 400,
                cache_miss_input_tokens: 0,
                reasoning_tokens: 0,
            })]
        );

        // Content events between the two never re-emit usage.
        let text_events = stream_events(
            &parse(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#),
            &mut state,
        )
        .expect("text delta parses");
        assert_eq!(
            text_events,
            vec![ProviderStreamEvent::TextDelta("Hi".to_string())]
        );

        // The delta's output-only usage merges with the start's input side.
        let delta_events = stream_events(
            &parse(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#),
            &mut state,
        )
        .expect("message_delta parses");
        assert_eq!(
            delta_events,
            vec![ProviderStreamEvent::Usage(TokenUsage {
                prompt_tokens: 525,
                completion_tokens: 12,
                total_tokens: 537,
                cache_creation_input_tokens: 100,
                cache_read_input_tokens: 400,
                cache_miss_input_tokens: 0,
                reasoning_tokens: 0,
            })]
        );
    }

    #[test]
    fn stream_usage_replaces_input_side_when_delta_repeats_it() {
        // Some Anthropic-compatible relays repeat the full usage (input side
        // included) in message_delta. The input side is a snapshot, not a
        // delta: it must REPLACE, not add, or the counts double.
        let mut state = StreamUsage::default();
        stream_events(
            &parse(r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":25,"cache_read_input_tokens":400,"output_tokens":1}}}"#),
            &mut state,
        )
        .expect("message_start parses");
        let delta_events = stream_events(
            &parse(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":25,"cache_read_input_tokens":400,"output_tokens":12}}"#),
            &mut state,
        )
        .expect("message_delta parses");
        assert_eq!(
            delta_events,
            vec![ProviderStreamEvent::Usage(TokenUsage {
                prompt_tokens: 425,
                completion_tokens: 12,
                total_tokens: 437,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 400,
                cache_miss_input_tokens: 0,
                reasoning_tokens: 0,
            })]
        );
    }

    #[test]
    fn stream_usage_tolerates_missing_message_start() {
        // Degenerate stream whose message_start carried no usage: the delta's
        // output-only usage still surfaces (prompt 0), matching the pre-merge
        // behavior.
        let mut state = StreamUsage::default();
        let events = stream_events(
            &parse(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#),
            &mut state,
        )
        .expect("message_delta parses");
        assert_eq!(
            events,
            vec![ProviderStreamEvent::Usage(TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 7,
                total_tokens: 7,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cache_miss_input_tokens: 0,
                reasoning_tokens: 0,
            })]
        );
    }
}
