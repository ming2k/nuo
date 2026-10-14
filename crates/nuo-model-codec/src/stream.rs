//! Server-Sent Events (SSE) stream decoding and multi-vendor chunk parsing.

use crate::endpoint::{Endpoint, ProviderVendor};
use crate::error::Result;
use crate::types::{WireChunk, WireToolCallChunk, WireUsage};

/// Decodes raw SSE text chunks into structured [`WireChunk`] tokens.
///
/// Follows WHATWG EventSource specification:
/// - Strips comment lines starting with `:` (heartbeats/ping).
/// - Concatentates multi-line `data:` entries with `\n` until dispatch on empty line boundary.
/// - Handles `[DONE]` stream termination flags.
#[derive(Default)]
pub struct SseDecoder {
    buffer: String,
    current_data_lines: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            current_data_lines: Vec::new(),
        }
    }

    /// Feeds incoming binary/text bytes, returning parsed chunks as events are dispatched.
    pub fn feed(&mut self, endpoint: &Endpoint, data: &[u8]) -> Result<Vec<WireChunk>> {
        let text = String::from_utf8_lossy(data);
        self.buffer.push_str(&text);

        let mut chunks = Vec::new();

        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer[..pos].trim_end_matches('\r').to_string();
            self.buffer.drain(..=pos);

            let trimmed = line.trim();

            // Comment line (heartbeat/keepalive), ignore
            if trimmed.starts_with(':') {
                continue;
            }

            // Empty line marks event dispatch boundary
            if trimmed.is_empty() {
                if !self.current_data_lines.is_empty() {
                    let payload = self.current_data_lines.join("\n");
                    self.current_data_lines.clear();

                    if let Some(chunk) = Self::dispatch_payload(endpoint, payload.trim())? {
                        chunks.push(chunk);
                    }
                }
                continue;
            }

            // Extract data field
            if let Some(rest) = line.strip_prefix("data:") {
                let value = rest.strip_prefix(' ').unwrap_or(rest);
                self.current_data_lines.push(value.to_string());
            }
        }

        Ok(chunks)
    }

    /// Flushes any pending trailing content if the stream terminates without trailing empty line.
    pub fn finish(&mut self, endpoint: &Endpoint) -> Result<Vec<WireChunk>> {
        let remaining = std::mem::take(&mut self.buffer);
        let trimmed = remaining.trim_end_matches('\r').trim();

        if !trimmed.is_empty()
            && !trimmed.starts_with(':')
            && let Some(rest) = trimmed.strip_prefix("data:")
        {
            let value = rest.strip_prefix(' ').unwrap_or(rest);
            self.current_data_lines.push(value.to_string());
        }

        let mut chunks = Vec::new();
        if !self.current_data_lines.is_empty() {
            let payload = self.current_data_lines.join("\n");
            self.current_data_lines.clear();

            if let Some(chunk) = Self::dispatch_payload(endpoint, payload.trim())? {
                chunks.push(chunk);
            }
        }

        Ok(chunks)
    }

    fn dispatch_payload(endpoint: &Endpoint, payload: &str) -> Result<Option<WireChunk>> {
        if payload == "[DONE]" {
            return Ok(Some(WireChunk::done()));
        }

        if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(payload) {
            parse_vendor_chunk(endpoint, &json_val)
        } else {
            Ok(None)
        }
    }
}

/// Parses vendor-specific JSON payload from a single SSE event.
pub fn parse_vendor_chunk(
    endpoint: &Endpoint,
    json_val: &serde_json::Value,
) -> Result<Option<WireChunk>> {
    match endpoint.vendor {
        ProviderVendor::OpenAi | ProviderVendor::DeepSeek | ProviderVendor::Ollama => {
            parse_openai_chunk(json_val)
        }
        ProviderVendor::Anthropic => parse_anthropic_chunk(json_val),
        ProviderVendor::Google => parse_google_chunk(json_val),
    }
}

fn parse_openai_chunk(json_val: &serde_json::Value) -> Result<Option<WireChunk>> {
    let mut chunk = WireChunk::default();

    if let Some(usage_obj) = json_val.get("usage").filter(|u| !u.is_null()) {
        chunk.usage = Some(WireUsage {
            prompt_tokens: usage_obj["prompt_tokens"].as_u64().unwrap_or(0) as usize,
            completion_tokens: usage_obj["completion_tokens"].as_u64().unwrap_or(0) as usize,
            total_tokens: usage_obj["total_tokens"].as_u64().unwrap_or(0) as usize,
            cached_tokens: usage_obj["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0) as usize,
        });
    }

    let Some(choices) = json_val.get("choices").and_then(|c| c.as_array()) else {
        return if chunk.usage.is_some() {
            Ok(Some(chunk))
        } else {
            Ok(None)
        };
    };

    if let Some(choice) = choices.first() {
        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content").and_then(|c| c.as_str())
                && !content.is_empty()
            {
                chunk.delta_content = Some(content.to_string());
            }

            if let Some(thinking) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("thinking"))
                .and_then(|t| t.as_str())
                && !thinking.is_empty()
            {
                chunk.delta_thinking = Some(thinking.to_string());
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(|tc| tc.as_array()) {
                for (idx, tc) in tool_calls.iter().enumerate() {
                    let index = tc
                        .get("index")
                        .and_then(|i| i.as_u64())
                        .map(|i| i as usize)
                        .unwrap_or(idx);
                    let id = tc.get("id").and_then(|id| id.as_str()).map(str::to_string);
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(str::to_string);
                    let arguments_delta = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .map(str::to_string);

                    chunk.delta_tool_calls.push(WireToolCallChunk {
                        index,
                        id,
                        name,
                        arguments_delta,
                    });
                }
            }
        }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(|f| f.as_str())
            && !finish_reason.is_empty()
        {
            chunk.is_done = true;
        }
    }

    if chunk.delta_content.is_none()
        && chunk.delta_thinking.is_none()
        && chunk.delta_tool_calls.is_empty()
        && chunk.usage.is_none()
        && !chunk.is_done
    {
        Ok(None)
    } else {
        Ok(Some(chunk))
    }
}

fn parse_anthropic_chunk(json_val: &serde_json::Value) -> Result<Option<WireChunk>> {
    let msg_type = json_val.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let mut chunk = WireChunk::default();

    match msg_type {
        "content_block_delta" => {
            if let Some(delta) = json_val.get("delta") {
                let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match delta_type {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                            chunk.delta_content = Some(text.to_string());
                        }
                    }
                    "thinking_delta" => {
                        if let Some(th) = delta.get("thinking").and_then(|t| t.as_str()) {
                            chunk.delta_thinking = Some(th.to_string());
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) = delta.get("partial_json").and_then(|p| p.as_str()) {
                            let index = json_val.get("index").and_then(|i| i.as_u64()).unwrap_or(0)
                                as usize;
                            chunk.delta_tool_calls.push(WireToolCallChunk {
                                index,
                                id: None,
                                name: None,
                                arguments_delta: Some(partial.to_string()),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        "content_block_start" => {
            if let Some(content_block) = json_val.get("content_block")
                && content_block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
            {
                let index = json_val.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let id = content_block
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string);
                let name = content_block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(str::to_string);
                chunk.delta_tool_calls.push(WireToolCallChunk {
                    index,
                    id,
                    name,
                    arguments_delta: None,
                });
            }
        }
        "message_delta" => {
            if let Some(usage_obj) = json_val.get("usage") {
                chunk.usage = Some(WireUsage {
                    prompt_tokens: 0,
                    completion_tokens: usage_obj["output_tokens"].as_u64().unwrap_or(0) as usize,
                    total_tokens: usage_obj["output_tokens"].as_u64().unwrap_or(0) as usize,
                    cached_tokens: 0,
                });
            }
        }
        "message_stop" => {
            chunk.is_done = true;
        }
        _ => {}
    }

    if chunk.delta_content.is_none()
        && chunk.delta_thinking.is_none()
        && chunk.delta_tool_calls.is_empty()
        && chunk.usage.is_none()
        && !chunk.is_done
    {
        Ok(None)
    } else {
        Ok(Some(chunk))
    }
}

fn parse_google_chunk(json_val: &serde_json::Value) -> Result<Option<WireChunk>> {
    let mut chunk = WireChunk::default();

    if let Some(candidates) = json_val.get("candidates").and_then(|c| c.as_array())
        && let Some(candidate) = candidates.first()
    {
        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
        {
            for (idx, part) in parts.iter().enumerate() {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    chunk.delta_content = Some(text.to_string());
                }
                if let Some(th) = part.get("thought").and_then(|t| t.as_str()) {
                    chunk.delta_thinking = Some(th.to_string());
                }
                if let Some(func_call) = part.get("functionCall") {
                    let name = func_call
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(str::to_string);
                    let args_str = func_call.get("args").map(|a| a.to_string());
                    chunk.delta_tool_calls.push(WireToolCallChunk {
                        index: idx,
                        id: Some(uuid::Uuid::new_v4().to_string()),
                        name,
                        arguments_delta: args_str,
                    });
                }
            }
        }
        if let Some(finish_reason) = candidate.get("finishReason").and_then(|f| f.as_str())
            && finish_reason == "STOP"
        {
            chunk.is_done = true;
        }
    }

    if let Some(usage_meta) = json_val.get("usageMetadata") {
        let prompt = usage_meta["promptTokenCount"].as_u64().unwrap_or(0) as usize;
        let candidates = usage_meta["candidatesTokenCount"].as_u64().unwrap_or(0) as usize;
        let total = usage_meta["totalTokenCount"]
            .as_u64()
            .unwrap_or((prompt + candidates) as u64) as usize;
        let cached = usage_meta["cachedContentTokenCount"]
            .as_u64()
            .unwrap_or(0) as usize;

        chunk.usage = Some(WireUsage {
            prompt_tokens: prompt,
            completion_tokens: candidates,
            total_tokens: total,
            cached_tokens: cached,
        });
    }

    if chunk.delta_content.is_none()
        && chunk.delta_thinking.is_none()
        && chunk.delta_tool_calls.is_empty()
        && chunk.usage.is_none()
        && !chunk.is_done
    {
        Ok(None)
    } else {
        Ok(Some(chunk))
    }
}
