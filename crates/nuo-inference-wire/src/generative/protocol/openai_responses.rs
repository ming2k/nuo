//! OpenAI Responses API wire format request construction and response parsing.
//!
//! Stateful interaction protocol (`POST /v1/responses`), supporting continuation chains
//! (`previous_response_id`), server-side item storage, and event-based delta decoding.

use crate::endpoint::Endpoint;
use crate::error::Result;
use crate::types::{
    ContentBlock, WireChunk, WireRequest, WireResponse, WireRole, WireToolCall, WireToolCallChunk,
    WireUsage,
};
use serde_json::json;

/// Builds the HTTP request for OpenAI Responses API.
pub fn build_request(
    endpoint: &Endpoint,
    token: &str,
    request: &WireRequest,
    is_stream: bool,
) -> (String, http::HeaderMap, serde_json::Value) {
    let url = format!("{}/responses", endpoint.base_url);
    let mut headers = http::HeaderMap::new();

    if !token.is_empty() {
        headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {}", token)
                .parse()
                .unwrap_or(http::header::HeaderValue::from_static("")),
        );
    }
    headers.insert(
        http::header::CONTENT_TYPE,
        http::header::HeaderValue::from_static("application/json"),
    );

    for (k, v) in endpoint.client_profile.headers() {
        if let (Ok(name), Ok(val)) = (k.parse::<http::header::HeaderName>(), v.parse()) {
            headers.insert(name, val);
        }
    }

    for (k, v) in &endpoint.custom_headers {
        if let (Ok(name), Ok(val)) = (k.parse::<http::header::HeaderName>(), v.parse()) {
            headers.insert(name, val);
        }
    }

    let mut instructions = String::new();
    let mut input_items = Vec::new();

    for msg in &request.messages {
        if msg.role == WireRole::System {
            for block in &msg.blocks {
                if let ContentBlock::Text { text } = block {
                    if !instructions.is_empty() {
                        instructions.push_str("\n\n");
                    }
                    instructions.push_str(text);
                }
            }
            continue;
        }

        let role_str = match msg.role {
            WireRole::Assistant => "assistant",
            _ => "user",
        };

        for block in &msg.blocks {
            match block {
                ContentBlock::Text { text } => {
                    input_items.push(json!({
                        "role": role_str,
                        "content": text,
                    }));
                }
                ContentBlock::Thinking { .. } => {}
                ContentBlock::Image { media_type, data } => {
                    input_items.push(json!({
                        "role": role_str,
                        "content": [{
                            "type": "input_image",
                            "image_url": format!("data:{};base64,{}", media_type, data),
                        }]
                    }));
                }
                ContentBlock::ToolCall(tc) => {
                    input_items.push(json!({
                        "type": "function_call",
                        "call_id": tc.id,
                        "name": tc.name,
                        "arguments": tc.arguments.to_string(),
                    }));
                }
                ContentBlock::ToolResult(tr) => {
                    input_items.push(json!({
                        "type": "function_call_output",
                        "call_id": tr.call_id,
                        "output": tr.output,
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": endpoint.model,
        "input": input_items,
        "stream": is_stream,
    });

    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }

    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }

    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }
    if let Some(max) = request.max_tokens {
        body["max_output_tokens"] = json!(max);
    }

    (url, headers, body)
}

/// Parses the non-streaming JSON response from OpenAI Responses API.
pub fn parse_response(json_val: &serde_json::Value) -> Result<WireResponse> {
    let response_id = json_val
        .get("id")
        .and_then(|id| id.as_str())
        .map(str::to_string);

    let mut text_acc = String::new();
    let mut thinking_acc = String::new();
    let mut tool_calls = Vec::new();

    if let Some(output) = json_val.get("output").and_then(|o| o.as_array()) {
        for item in output {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                        for part in content {
                            let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if part_type == "text" || part_type.is_empty() {
                                if let Some(txt) = part.get("text").and_then(|t| t.as_str()) {
                                    text_acc.push_str(txt);
                                }
                            }
                        }
                    } else if let Some(txt) = item.get("content").and_then(|c| c.as_str()) {
                        text_acc.push_str(txt);
                    }
                }
                "reasoning" => {
                    if let Some(summary) = item.get("summary").and_then(|s| s.as_str()) {
                        thinking_acc.push_str(summary);
                    }
                }
                "function_call" => {
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let raw_args = item
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}");
                    let args = serde_json::from_str(raw_args).unwrap_or(json!({}));

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
                _ => {}
            }
        }
    }

    let usage = if let Some(usage_obj) = json_val.get("usage") {
        let prompt = usage_obj["input_tokens"].as_u64().unwrap_or(0) as usize;
        let completion = usage_obj["output_tokens"].as_u64().unwrap_or(0) as usize;
        let total = usage_obj["total_tokens"]
            .as_u64()
            .unwrap_or((prompt + completion) as u64) as usize;
        let cached = usage_obj["input_token_details"]["cached_tokens"]
            .as_u64()
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
        continuation_id: response_id,
    })
}

/// Parses an SSE streaming delta frame from OpenAI Responses API.
pub fn parse_chunk(json_val: &serde_json::Value) -> Result<Option<WireChunk>> {
    let mut chunk = WireChunk::default();

    if let Some(resp_id) = json_val
        .get("response")
        .and_then(|r| r.get("id"))
        .or_else(|| json_val.get("id"))
        .and_then(|id| id.as_str())
    {
        chunk.continuation_id = Some(resp_id.to_string());
    }

    let event_type = json_val.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match event_type {
        "response.output_item.added" => {
            if let Some(item) = json_val.get("item") {
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    let id = item
                        .get("call_id")
                        .and_then(|id| id.as_str())
                        .map(str::to_string);
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(str::to_string);
                    chunk.delta_tool_calls.push(WireToolCallChunk {
                        index: 0,
                        id,
                        name,
                        arguments_delta: None,
                    });
                }
            }
        }
        "response.output_item.delta" | "response.content_part.delta" => {
            if let Some(delta) = json_val.get("delta") {
                if let Some(txt) = delta.as_str() {
                    chunk.delta_content = Some(txt.to_string());
                } else if let Some(txt) = delta.get("text").and_then(|t| t.as_str()) {
                    chunk.delta_content = Some(txt.to_string());
                }
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(delta) = json_val.get("delta").and_then(|d| d.as_str()) {
                chunk.delta_tool_calls.push(WireToolCallChunk {
                    index: 0,
                    id: None,
                    name: None,
                    arguments_delta: Some(delta.to_string()),
                });
            }
        }
        "response.reasoning.delta" => {
            if let Some(delta) = json_val.get("delta").and_then(|d| d.as_str()) {
                chunk.delta_thinking = Some(delta.to_string());
            }
        }
        "response.completed" | "response.done" => {
            chunk.is_done = true;
            if let Some(usage_obj) = json_val.get("response").and_then(|r| r.get("usage")) {
                let prompt = usage_obj["input_tokens"].as_u64().unwrap_or(0) as usize;
                let completion = usage_obj["output_tokens"].as_u64().unwrap_or(0) as usize;
                let total = usage_obj["total_tokens"]
                    .as_u64()
                    .unwrap_or((prompt + completion) as u64) as usize;
                let cached = usage_obj["input_token_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0) as usize;

                chunk.usage = Some(WireUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: total,
                    cached_tokens: cached,
                });
            }
        }
        _ => {}
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
