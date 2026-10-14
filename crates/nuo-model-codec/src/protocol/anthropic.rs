//! Anthropic Messages API wire format request construction and response parsing.

use crate::endpoint::Endpoint;
use crate::error::{Result, WireError};
use crate::types::{
    ContentBlock, WireRequest, WireResponse, WireRole, WireToolCall, WireUsage,
};
use serde_json::json;

pub fn build_request(
    endpoint: &Endpoint,
    token: &str,
    request: &WireRequest,
    is_stream: bool,
) -> (String, http::HeaderMap, serde_json::Value) {
    let url = format!("{}/messages", endpoint.base_url);
    let mut headers = http::HeaderMap::new();

    if !token.is_empty()
        && let Ok(val) = token.parse()
    {
        headers.insert(http::header::HeaderName::from_static("x-api-key"), val);
    }
    headers.insert(
        http::header::HeaderName::from_static("anthropic-version"),
        http::header::HeaderValue::from_static("2023-06-01"),
    );
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

    let mut system_blocks = Vec::new();
    let mut messages = Vec::new();

    for msg in &request.messages {
        if msg.role == WireRole::System {
            for block in &msg.blocks {
                if let ContentBlock::Text { text } = block {
                    let mut b = json!({
                        "type": "text",
                        "text": text,
                    });
                    if msg.cache_control.is_some() {
                        b["cache_control"] = json!({ "type": "ephemeral" });
                    }
                    system_blocks.push(b);
                }
            }
            continue;
        }

        let role_str = match msg.role {
            WireRole::Assistant => "assistant",
            _ => "user",
        };

        let mut content_blocks = Vec::new();

        for block in &msg.blocks {
            match block {
                ContentBlock::Text { text } => {
                    let mut b = json!({
                        "type": "text",
                        "text": text,
                    });
                    if msg.cache_control.is_some() {
                        b["cache_control"] = json!({ "type": "ephemeral" });
                    }
                    content_blocks.push(b);
                }
                ContentBlock::Thinking { text, .. } => {
                    content_blocks.push(json!({
                        "type": "thinking",
                        "thinking": text,
                    }));
                }
                ContentBlock::Image { media_type, data } => {
                    content_blocks.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data,
                        }
                    }));
                }
                ContentBlock::ToolCall(tc) => {
                    content_blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
                ContentBlock::ToolResult(tr) => {
                    content_blocks.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tr.call_id,
                        "content": tr.output,
                        "is_error": tr.is_error,
                    }));
                }
            }
        }

        if !content_blocks.is_empty() {
            messages.push(json!({
                "role": role_str,
                "content": content_blocks,
            }));
        }
    }

    let mut body = json!({
        "model": endpoint.model,
        "messages": messages,
        "max_tokens": request.max_tokens.unwrap_or(4096),
    });

    if is_stream {
        body["stream"] = json!(true);
    }

    if !system_blocks.is_empty() {
        body["system"] = json!(system_blocks);
    }

    if let Some(budget) = request.thinking_budget {
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": budget,
        });
    }

    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }

    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }

    (url, headers, body)
}

pub fn parse_response(json_val: &serde_json::Value) -> Result<WireResponse> {
    let content_blocks = json_val
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| WireError::Protocol("Anthropic response contained no content blocks".into()))?;

    let mut text_acc = String::new();
    let mut thinking_acc = String::new();
    let mut tool_calls = Vec::new();

    for block in content_blocks {
        let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match btype {
            "text" => {
                if let Some(txt) = block.get("text").and_then(|t| t.as_str()) {
                    text_acc.push_str(txt);
                }
            }
            "thinking" => {
                if let Some(th) = block.get("thinking").and_then(|t| t.as_str()) {
                    thinking_acc.push_str(th);
                }
            }
            "tool_use" => {
                let id = block
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(json!({}));
                tool_calls.push(WireToolCall {
                    id,
                    name,
                    arguments: input,
                });
            }
            _ => {}
        }
    }

    let usage = if let Some(usage_obj) = json_val.get("usage") {
        let input_tokens = usage_obj["input_tokens"].as_u64().unwrap_or(0) as usize;
        let output_tokens = usage_obj["output_tokens"].as_u64().unwrap_or(0) as usize;
        let cached_tokens = usage_obj["cache_read_input_tokens"].as_u64().unwrap_or(0) as usize;
        WireUsage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
            total_tokens: input_tokens + output_tokens,
            cached_tokens,
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
    })
}
