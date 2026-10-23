//! Google Gemini Interactions API wire format request construction and response parsing.
//!
//! Stateful incremental interaction protocol (`POST /v1beta/interactions`), providing
//! server-side state tracking (`previous_interaction_id`), tools orchestration, and
//! streaming delta decoding.

use crate::endpoint::Endpoint;
use crate::error::Result;
use crate::types::{
    ContentBlock, WireChunk, WireRequest, WireResponse, WireRole, WireToolCall, WireToolCallChunk,
    WireUsage,
};
use serde_json::json;

/// Builds the HTTP request for Google Interactions API.
pub fn build_request(
    endpoint: &Endpoint,
    token: &str,
    request: &WireRequest,
    is_stream: bool,
) -> (String, http::HeaderMap, serde_json::Value) {
    let url = if is_stream {
        format!("{}/interactions?alt=sse", endpoint.base_url)
    } else {
        format!("{}/interactions", endpoint.base_url)
    };

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::header::HeaderValue::from_static("application/json"),
    );

    if !token.is_empty() {
        if token.starts_with("ya29.") || token.starts_with("Bearer ") {
            let bearer = if token.starts_with("Bearer ") {
                token.to_string()
            } else {
                format!("Bearer {token}")
            };
            if let Ok(val) = bearer.parse() {
                headers.insert(http::header::AUTHORIZATION, val);
            }
        } else if let Ok(val) = http::HeaderValue::from_str(token) {
            headers.insert(http::header::HeaderName::from_static("x-goog-api-key"), val);
        }
    }

    for (k, v) in &endpoint.client_headers {
        if let (Ok(name), Ok(val)) = (k.parse::<http::header::HeaderName>(), v.parse()) {
            headers.insert(name, val);
        }
    }

    for (k, v) in &endpoint.custom_headers {
        if let (Ok(name), Ok(val)) = (k.parse::<http::header::HeaderName>(), v.parse()) {
            headers.insert(name, val);
        }
    }

    let mut system_prompts = Vec::new();
    let mut parts = Vec::new();

    for msg in &request.messages {
        if msg.role == WireRole::System {
            for block in &msg.blocks {
                if let ContentBlock::Text { text } = block {
                    system_prompts.push(text.clone());
                }
            }
            continue;
        }

        for block in &msg.blocks {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(json!({ "text": text }));
                }
                ContentBlock::Thinking { .. } => {
                    // Thoughts are output artifacts, withheld from request parts
                }
                ContentBlock::Image { media_type, data } => {
                    parts.push(json!({
                        "inline_data": {
                            "mime_type": media_type,
                            "data": data,
                        }
                    }));
                }
                ContentBlock::ToolCall(tc) => {
                    parts.push(json!({
                        "function_call": {
                            "name": tc.name,
                            "args": tc.arguments,
                        }
                    }));
                }
                ContentBlock::ToolResult(tr) => {
                    parts.push(json!({
                        "function_response": {
                            "name": tr.call_id,
                            "response": { "output": tr.output },
                        }
                    }));
                }
            }
        }
    }

    let input_value = if parts.len() == 1 && parts[0].get("text").is_some() {
        parts[0]["text"].clone()
    } else if !parts.is_empty() {
        json!(parts)
    } else {
        json!("")
    };

    let mut body = json!({
        "model": endpoint.model,
        "input": input_value,
        "stream": is_stream,
        "store": true,
    });

    if !system_prompts.is_empty() {
        body["system_instruction"] = json!({
            "parts": [{ "text": system_prompts.join("\n\n") }]
        });
    }

    if !request.tools.is_empty() {
        let decls: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
        body["tools"] = json!([{ "function_declarations": decls }]);
    }

    let mut gen_config = serde_json::Map::new();
    if let Some(temp) = request.temperature {
        gen_config.insert("temperature".into(), json!(temp));
    }
    if let Some(max) = request.max_tokens {
        gen_config.insert("max_output_tokens".into(), json!(max));
    }
    if let Some(budget) = request.thinking_budget {
        let mut thinking_config = serde_json::Map::new();
        thinking_config.insert("include_thoughts".into(), json!(true));
        thinking_config.insert("thinking_budget".into(), json!(budget));
        gen_config.insert(
            "thinking_config".into(),
            serde_json::Value::Object(thinking_config),
        );
    }
    if !gen_config.is_empty() {
        body["generation_config"] = serde_json::Value::Object(gen_config);
    }

    (url, headers, body)
}

/// Parses the non-streaming JSON response from Google Interactions API.
pub fn parse_response(json_val: &serde_json::Value) -> Result<WireResponse> {
    let interaction_id = json_val
        .get("id")
        .and_then(|id| id.as_str())
        .map(str::to_string);

    let mut text_acc = String::new();
    let mut thinking_acc = String::new();
    let mut tool_calls = Vec::new();

    // Check outputs array or candidate content
    if let Some(outputs) = json_val.get("outputs").and_then(|o| o.as_array()) {
        for output in outputs {
            let out_type = output.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match out_type {
                "text" => {
                    if let Some(txt) = output.get("text").and_then(|t| t.as_str()) {
                        text_acc.push_str(txt);
                    }
                }
                "thought" => {
                    if let Some(th) = output.get("thought").and_then(|t| t.as_str()) {
                        thinking_acc.push_str(th);
                    }
                }
                "tool_call" | "function_call" => {
                    let call_id = output
                        .get("id")
                        .or_else(|| output.get("call_id"))
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = output
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = output
                        .get("arguments")
                        .or_else(|| output.get("args"))
                        .cloned()
                        .unwrap_or(json!({}));
                    tool_calls.push(WireToolCall {
                        id: if call_id.is_empty() {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            call_id
                        },
                        name,
                        arguments: args,
                    });
                }
                _ => {
                    // Fallback to text if present
                    if let Some(txt) = output.get("text").and_then(|t| t.as_str()) {
                        text_acc.push_str(txt);
                    }
                }
            }
        }
    } else if let Some(candidates) = json_val.get("candidates").and_then(|c| c.as_array())
        && let Some(candidate) = candidates.first()
        && let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
    {
        for part in parts {
            if part
                .get("thought")
                .and_then(|t| t.as_bool())
                .unwrap_or(false)
            {
                if let Some(txt) = part.get("text").and_then(|t| t.as_str()) {
                    thinking_acc.push_str(txt);
                }
            } else if let Some(txt) = part.get("text").and_then(|t| t.as_str()) {
                text_acc.push_str(txt);
            }
            if let Some(func_call) = part.get("functionCall") {
                let name = func_call
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = func_call.get("args").cloned().unwrap_or(json!({}));
                tool_calls.push(WireToolCall {
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    arguments: args,
                });
            }
        }
    }

    let usage = if let Some(usage_meta) = json_val
        .get("usage")
        .or_else(|| json_val.get("usageMetadata"))
    {
        let prompt = usage_meta
            .get("prompt_tokens")
            .or_else(|| usage_meta.get("promptTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let completion = usage_meta
            .get("completion_tokens")
            .or_else(|| usage_meta.get("candidatesTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let total = usage_meta
            .get("total_tokens")
            .or_else(|| usage_meta.get("totalTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or((prompt + completion) as u64) as usize;
        let cached = usage_meta
            .get("cached_tokens")
            .or_else(|| usage_meta.get("cachedContentTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        WireUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cached_tokens: cached,
        }
    } else {
        WireUsage::default()
    };

    Ok(WireResponse {
        content: if text_acc.is_empty() {
            None
        } else {
            Some(text_acc)
        },
        thinking: if thinking_acc.is_empty() {
            None
        } else {
            Some(thinking_acc)
        },
        tool_calls,
        usage,
        continuation_id: interaction_id,
    })
}

/// Parses an SSE streaming delta frame from Google Interactions API.
pub fn parse_chunk(json_val: &serde_json::Value) -> Result<Option<WireChunk>> {
    let mut chunk = WireChunk::default();

    // Check interaction id
    if let Some(id) = json_val
        .get("id")
        .or_else(|| json_val.get("interaction").and_then(|i| i.get("id")))
        .and_then(|id| id.as_str())
    {
        chunk.continuation_id = Some(id.to_string());
    }

    // Check deltas
    if let Some(delta) = json_val.get("delta") {
        if let Some(txt) = delta.get("text").and_then(|t| t.as_str()) {
            chunk.delta_content = Some(txt.to_string());
        }
        if let Some(th) = delta.get("thought").and_then(|t| t.as_str()) {
            chunk.delta_thinking = Some(th.to_string());
        }
        if let Some(tool_call) = delta
            .get("tool_call")
            .or_else(|| delta.get("function_call"))
        {
            let name = tool_call
                .get("name")
                .and_then(|n| n.as_str())
                .map(str::to_string);
            let args = tool_call
                .get("arguments")
                .or_else(|| tool_call.get("args"))
                .map(|a| a.to_string());
            let id = tool_call
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string);
            chunk.delta_tool_calls.push(WireToolCallChunk {
                index: 0,
                id,
                name,
                arguments_delta: args,
            });
        }
    }

    // Check outputs
    if let Some(outputs) = json_val.get("outputs").and_then(|o| o.as_array()) {
        for (idx, output) in outputs.iter().enumerate() {
            let out_type = output.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if out_type == "text" || out_type.is_empty() {
                if let Some(txt) = output.get("text").and_then(|t| t.as_str()) {
                    chunk.delta_content = Some(txt.to_string());
                }
            } else if out_type == "thought" {
                if let Some(th) = output.get("thought").and_then(|t| t.as_str()) {
                    chunk.delta_thinking = Some(th.to_string());
                }
            } else if out_type == "tool_call" || out_type == "function_call" {
                let name = output
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(str::to_string);
                let args = output
                    .get("arguments")
                    .or_else(|| output.get("args"))
                    .map(|a| a.to_string());
                let id = output
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string);
                chunk.delta_tool_calls.push(WireToolCallChunk {
                    index: idx,
                    id,
                    name,
                    arguments_delta: args,
                });
            }
        }
    }

    // Check status
    if let Some(status) = json_val.get("status").and_then(|s| s.as_str())
        && (status == "completed" || status == "stopped" || status == "failed")
    {
        chunk.is_done = true;
    }

    // Check usage
    if let Some(usage_meta) = json_val
        .get("usage")
        .or_else(|| json_val.get("usageMetadata"))
    {
        let prompt = usage_meta
            .get("prompt_tokens")
            .or_else(|| usage_meta.get("promptTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let completion = usage_meta
            .get("completion_tokens")
            .or_else(|| usage_meta.get("candidatesTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let total = usage_meta
            .get("total_tokens")
            .or_else(|| usage_meta.get("totalTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or((prompt + completion) as u64) as usize;
        let cached = usage_meta
            .get("cached_tokens")
            .or_else(|| usage_meta.get("cachedContentTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        chunk.usage = Some(WireUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cached_tokens: cached,
        });
    }

    if chunk.delta_content.is_none()
        && chunk.delta_thinking.is_none()
        && chunk.delta_tool_calls.is_empty()
        && chunk.usage.is_none()
        && chunk.continuation_id.is_none()
        && !chunk.is_done
    {
        Ok(None)
    } else {
        Ok(Some(chunk))
    }
}
