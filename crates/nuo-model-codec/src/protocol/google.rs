//! Google Gemini generateContent and streamGenerateContent wire format request construction and response parsing.

use crate::endpoint::Endpoint;
use crate::error::{Result, WireError};
use crate::types::{ContentBlock, WireRequest, WireResponse, WireRole, WireToolCall, WireUsage};
use serde_json::json;

pub fn build_request(
    endpoint: &Endpoint,
    token: &str,
    request: &WireRequest,
    is_stream: bool,
) -> (String, http::HeaderMap, serde_json::Value) {
    let method_suffix = if is_stream {
        ":streamGenerateContent?alt=sse"
    } else {
        ":generateContent"
    };

    let url = format!(
        "{}/models/{}{}",
        endpoint.base_url, endpoint.model, method_suffix
    );

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

    let mut system_prompts = Vec::new();
    let mut contents = Vec::new();

    for msg in &request.messages {
        if msg.role == WireRole::System {
            for block in &msg.blocks {
                if let ContentBlock::Text { text } = block {
                    system_prompts.push(text.clone());
                }
            }
            continue;
        }

        let role_str = match msg.role {
            WireRole::Assistant => "model",
            _ => "user",
        };

        let mut parts = Vec::new();

        for block in &msg.blocks {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(json!({ "text": text }));
                }
                ContentBlock::Thinking { .. } => {
                    // Google Gemini REST API does not accept "thought" fields in request parts.
                    // Thinking text is emitted by the model during output but withheld from request contents.
                }
                ContentBlock::Image { media_type, data } => {
                    parts.push(json!({
                        "inlineData": {
                            "mimeType": media_type,
                            "data": data,
                        }
                    }));
                }
                ContentBlock::ToolCall(tc) => {
                    parts.push(json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": tc.arguments,
                        }
                    }));
                }
                ContentBlock::ToolResult(tr) => {
                    parts.push(json!({
                        "functionResponse": {
                            "name": tr.call_id,
                            "response": { "output": tr.output },
                        }
                    }));
                }
            }
        }

        if !parts.is_empty() {
            contents.push(json!({
                "role": role_str,
                "parts": parts,
            }));
        }
    }

    let mut body = json!({
        "contents": contents,
    });

    if !system_prompts.is_empty() {
        body["systemInstruction"] = json!({
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
        body["tools"] = json!([{ "functionDeclarations": decls }]);
    }

    let mut gen_config = serde_json::Map::new();
    if let Some(temp) = request.temperature {
        gen_config.insert("temperature".into(), json!(temp));
    }
    if let Some(max) = request.max_tokens {
        gen_config.insert("maxOutputTokens".into(), json!(max));
    }
    if let Some(budget) = request.thinking_budget {
        let mut thinking_config = serde_json::Map::new();
        thinking_config.insert("includeThoughts".into(), json!(true));
        thinking_config.insert("thinkingBudget".into(), json!(budget));
        gen_config.insert(
            "thinkingConfig".into(),
            serde_json::Value::Object(thinking_config),
        );
    }
    if !gen_config.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(gen_config);
    }

    (url, headers, body)
}

pub fn parse_response(json_val: &serde_json::Value) -> Result<WireResponse> {
    let candidate = json_val
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| {
            WireError::Protocol("Google Gemini response contained no candidates".into())
        })?;

    let parts = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .ok_or_else(|| WireError::Protocol("Google candidate contained no parts".into()))?;

    let mut text_acc = String::new();
    let mut thinking_acc = String::new();
    let mut tool_calls = Vec::new();

    for part in parts {
        let is_thought = part
            .get("thought")
            .and_then(|t| t.as_bool())
            .unwrap_or(false);
        if is_thought {
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

    let usage = if let Some(usage_meta) = json_val.get("usageMetadata") {
        let prompt = usage_meta["promptTokenCount"].as_u64().unwrap_or(0) as usize;
        let candidates = usage_meta["candidatesTokenCount"].as_u64().unwrap_or(0) as usize;
        let total = usage_meta["totalTokenCount"]
            .as_u64()
            .unwrap_or((prompt + candidates) as u64) as usize;
        let cached = usage_meta["cachedContentTokenCount"].as_u64().unwrap_or(0) as usize;

        WireUsage {
            prompt_tokens: prompt,
            completion_tokens: candidates,
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
    })
}
