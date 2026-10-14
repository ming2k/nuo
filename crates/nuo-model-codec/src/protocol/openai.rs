//! OpenAI wire format request construction and response parsing.

use crate::endpoint::Endpoint;
use crate::error::{Result, WireError};
use crate::types::{
    ContentBlock, WireMessage, WireRequest, WireResponse, WireRole, WireToolCall, WireUsage,
};
use serde_json::json;

pub fn build_request(
    endpoint: &Endpoint,
    token: &str,
    request: &WireRequest,
    is_stream: bool,
) -> (String, http::HeaderMap, serde_json::Value) {
    let url = format!("{}/chat/completions", endpoint.base_url);
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

    let wire_messages = format_messages(&request.messages);
    let mut body = json!({
        "model": endpoint.model,
        "messages": wire_messages,
    });

    if is_stream {
        body["stream"] = json!(true);
        body["stream_options"] = json!({ "include_usage": true });
    }

    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }

    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }
    if let Some(max) = request.max_tokens {
        body["max_tokens"] = json!(max);
    }

    (url, headers, body)
}

fn format_messages(messages: &[WireMessage]) -> Vec<serde_json::Value> {
    let mut list = Vec::new();

    for msg in messages {
        // Collect tool results first, as OpenAI represents tool outputs as separate messages
        let mut tool_results = Vec::new();
        let mut text_parts = Vec::new();
        let mut image_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut thinking_text = None;

        for block in &msg.blocks {
            match block {
                ContentBlock::Text { text } => text_parts.push(text.clone()),
                ContentBlock::Thinking { text, .. } => thinking_text = Some(text.clone()),
                ContentBlock::Image { media_type, data } => {
                    image_parts.push(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", media_type, data)
                        }
                    }));
                }
                ContentBlock::ToolCall(tc) => tool_calls.push(tc.clone()),
                ContentBlock::ToolResult(tr) => tool_results.push(tr.clone()),
            }
        }

        if !tool_results.is_empty() {
            for tr in tool_results {
                list.push(json!({
                    "role": "tool",
                    "tool_call_id": tr.call_id,
                    "content": tr.output,
                }));
            }
            continue;
        }

        let role_str = match msg.role {
            WireRole::System => "system",
            WireRole::User => "user",
            WireRole::Assistant => "assistant",
            WireRole::Tool => "tool",
        };

        let mut obj = serde_json::Map::new();
        obj.insert("role".into(), json!(role_str));

        if let Some(name) = &msg.name {
            obj.insert("name".into(), json!(name));
        }

        if image_parts.is_empty() {
            // Standard single text string content
            let combined_text = text_parts.join("\n");
            obj.insert("content".into(), json!(combined_text));
        } else {
            // Multimodal content array
            let mut content_arr = Vec::new();
            for t in text_parts {
                content_arr.push(json!({
                    "type": "text",
                    "text": t,
                }));
            }
            content_arr.extend(image_parts);
            obj.insert("content".into(), json!(content_arr));
        }

        if let Some(th) = thinking_text {
            obj.insert("reasoning_content".into(), json!(th));
        }

        if !tool_calls.is_empty() {
            let calls: Vec<serde_json::Value> = tool_calls
                .into_iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments.to_string(),
                        }
                    })
                })
                .collect();
            obj.insert("tool_calls".into(), json!(calls));
        }

        list.push(serde_json::Value::Object(obj));
    }

    list
}

pub fn parse_response(json_val: &serde_json::Value) -> Result<WireResponse> {
    let choice = json_val
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| WireError::Protocol("OpenAI response contained no choices".into()))?;

    let message_obj = choice
        .get("message")
        .ok_or_else(|| WireError::Protocol("OpenAI choice contained no message".into()))?;

    let content = message_obj
        .get("content")
        .and_then(|c| c.as_str())
        .map(str::to_string);

    let thinking = message_obj
        .get("reasoning_content")
        .or_else(|| message_obj.get("thinking"))
        .and_then(|t| t.as_str())
        .map(str::to_string);

    let mut tool_calls = Vec::new();
    if let Some(calls) = message_obj.get("tool_calls").and_then(|c| c.as_array()) {
        for call in calls {
            let id = call["id"].as_str().unwrap_or("").to_string();
            let name = call["function"]["name"].as_str().unwrap_or("").to_string();
            let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
            let arguments: serde_json::Value = serde_json::from_str(args_str).unwrap_or(json!({}));

            tool_calls.push(WireToolCall {
                id,
                name,
                arguments,
            });
        }
    }

    let usage = if let Some(usage_obj) = json_val.get("usage") {
        WireUsage {
            prompt_tokens: usage_obj["prompt_tokens"].as_u64().unwrap_or(0) as usize,
            completion_tokens: usage_obj["completion_tokens"].as_u64().unwrap_or(0) as usize,
            total_tokens: usage_obj["total_tokens"].as_u64().unwrap_or(0) as usize,
            cached_tokens: usage_obj["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0) as usize,
        }
    } else {
        WireUsage::default()
    };

    Ok(WireResponse {
        content,
        thinking,
        tool_calls,
        usage,
    })
}
