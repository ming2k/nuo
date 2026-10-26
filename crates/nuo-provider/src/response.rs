//! Provider HTTP response validation, error classification, and JSON decoding.

use crate::transport::{HttpResponse, retry_after_ms};
use crate::{ProviderError, ProviderErrorKind};

/// Enforce HTTP success on provider response, classifying error kinds.
pub async fn ensure_success(
    response: HttpResponse,
    provider: &str,
    model: Option<&str>,
) -> Result<HttpResponse, ProviderError> {
    let status = response.status;
    if status.is_success() {
        return Ok(response);
    }
    let retry_after = retry_after_ms(&response.headers);
    let content_type = response
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.into_text().await.unwrap_or_default();
    let rollout_denied = (status.as_u16() == 404 && body.contains("model_not_found"))
        || (status.as_u16() == 403 && body.contains("permission_denied"));
    let message = match http_error_body_detail(content_type.as_deref(), &body) {
        Some(detail) => {
            if rollout_denied && let Some(model) = model {
                format!(
                    "Model `{model}` is registered in the upstream catalog but returned HTTP {status}: {detail}. Your API key or organization may not yet have rollout entitlement from {provider}."
                )
            } else {
                format!("{provider} HTTP {status}: {detail}")
            }
        }
        None => format!("{provider} HTTP {status}"),
    };
    let kind = classify_http_error(status, &body);
    let error = ProviderError::new(provider, kind, message).with_status(status.as_u16());
    if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
        Err(error.retryable(retry_after))
    } else {
        Err(error)
    }
}

pub fn classify_http_error(status: http::StatusCode, body: &str) -> ProviderErrorKind {
    match status.as_u16() {
        401 | 403 => ProviderErrorKind::Authentication,
        408 => ProviderErrorKind::Timeout,
        429 => ProviderErrorKind::RateLimited,
        400 | 413 | 422 if is_context_overflow_payload(body) => ProviderErrorKind::ContextOverflow,
        400 | 404 | 405 | 409 | 410 | 413 | 415 | 422 => ProviderErrorKind::InvalidRequest,
        _ if status.is_server_error() => ProviderErrorKind::Upstream,
        _ => ProviderErrorKind::Protocol,
    }
}

/// Conservatively recognize the untyped context-overflow errors returned by
/// OpenAI-compatible, Anthropic-compatible, and Google-compatible endpoints.
pub fn is_context_overflow_payload(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let has_known_code = lower
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|token| {
            matches!(
                token,
                "context_length_exceeded"
                    | "context_window_exceeded"
                    | "prompt_too_long"
                    | "input_too_long"
            )
        });
    if has_known_code {
        return true;
    }

    let explicitly_too_long = [
        "prompt is too long",
        "prompt too long",
        "input is too long",
        "too many input tokens",
        "too many tokens",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));
    let context_subject = [
        "context length",
        "context window",
        "context limit",
        "context size",
        "input token count",
        "token limit",
    ]
    .iter()
    .any(|subject| lower.contains(subject));
    let overflow_predicate = [
        "exceed",
        "maximum",
        "too long",
        "too large",
        "over limit",
        "greater than",
    ]
    .iter()
    .any(|predicate| lower.contains(predicate));

    explicitly_too_long || (context_subject && overflow_predicate)
}

fn http_error_body_detail(content_type: Option<&str>, body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let looks_html = content_type.is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
    }) || {
        let lower = trimmed
            .chars()
            .take(32)
            .collect::<String>()
            .to_ascii_lowercase();
        lower.starts_with("<!doctype html") || lower.starts_with("<html")
    };
    if looks_html {
        return None;
    }

    if let Some(mut val) = try_repair_truncated_json(trimmed) {
        normalize_json_value(&mut val);
        if let Some(summary) = extract_error_summary(&val) {
            if has_extra_diagnostic_metadata(&val) {
                if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                    return Some(format!("{summary}\n{pretty}"));
                }
            }
            return Some(summary);
        }
        if let Ok(compact) = serde_json::to_string(&val) {
            if compact.chars().count() <= DECODE_ERROR_BODY_PREVIEW {
                return Some(compact);
            }
        }
    }

    Some(body_preview(trimmed))
}

pub fn extract_error_summary(val: &serde_json::Value) -> Option<String> {
    let msg = extract_error_message(val)?;
    let err_type = extract_error_type_or_code(val);
    match err_type {
        Some(t) if !msg.to_ascii_lowercase().contains(&t.to_ascii_lowercase()) => {
            Some(format!("{msg} (type: {t})"))
        }
        _ => Some(msg),
    }
}

pub fn extract_error_message(val: &serde_json::Value) -> Option<String> {
    match val {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if (trimmed.starts_with('{') || trimmed.starts_with('['))
                && let Some(nested) = try_repair_truncated_json(trimmed)
                && let Some(inner) = extract_error_message(&nested)
            {
                return Some(inner);
            }
            if !trimmed.is_empty() {
                return Some(unescape_string_literals(trimmed));
            }
            None
        }
        serde_json::Value::Object(map) => {
            for key in [
                "error", "message", "detail", "param", "inner", "reason", "msg", "cause", "text",
                "title", "err",
            ] {
                if let Some(v) = map.get(key) {
                    if let Some(m) = extract_error_message(v) {
                        return Some(m);
                    }
                }
            }
            if map.len() == 1 {
                if let Some(v) = map.values().next() {
                    if let Some(m) = extract_error_message(v) {
                        return Some(m);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

pub fn extract_error_type_or_code(val: &serde_json::Value) -> Option<String> {
    if let Some(obj) = val.as_object() {
        if let Some(code) = obj.get("code").and_then(|v| v.as_str()) {
            return Some(code.to_string());
        }
        if let Some(err_val) = obj.get("error") {
            if let Some(code) = extract_error_type_or_code(err_val) {
                return Some(code);
            }
        }
        if let Some(err_type) = obj.get("type").and_then(|v| v.as_str()) {
            return Some(err_type.to_string());
        }
    }
    None
}

pub fn unescape_string_literals(s: &str) -> String {
    s.replace("\\n", "\n")
        .replace("\\r", "\r")
        .replace("\\t", "\t")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
}

fn has_extra_diagnostic_metadata(val: &serde_json::Value) -> bool {
    match val {
        serde_json::Value::Object(map) => {
            for key in [
                "providerMetadata",
                "gateway",
                "modelAttempts",
                "details",
                "metadata",
                "trace",
            ] {
                if map.contains_key(key) {
                    return true;
                }
            }
            map.values().any(has_extra_diagnostic_metadata)
        }
        serde_json::Value::Array(arr) => arr.iter().any(has_extra_diagnostic_metadata),
        _ => false,
    }
}

pub fn normalize_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (_key, val) in map.iter_mut() {
                if let serde_json::Value::String(s) = val {
                    if let Some(mut parsed) = try_extract_nested_json(s) {
                        normalize_json_value(&mut parsed);
                        *val = parsed;
                    }
                } else {
                    normalize_json_value(val);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for val in arr.iter_mut() {
                if let serde_json::Value::String(s) = val {
                    if let Some(mut parsed) = try_extract_nested_json(s) {
                        normalize_json_value(&mut parsed);
                        *val = parsed;
                    }
                } else {
                    normalize_json_value(val);
                }
            }
        }
        _ => {}
    }
}

fn try_extract_nested_json(s: &str) -> Option<serde_json::Value> {
    let trimmed = s.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(parsed);
    }
    try_repair_truncated_json(trimmed)
}

fn try_repair_truncated_json(input: &str) -> Option<serde_json::Value> {
    let trimmed = input.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(val);
    }

    let mut candidate = trimmed;
    for marker in ["…(truncated)", "(truncated)", "…<", "..."] {
        if let Some(idx) = candidate.rfind(marker) {
            candidate = candidate[..idx].trim_end();
        }
    }

    let mut slices = Vec::new();
    slices.push(candidate);
    let mut curr = candidate;
    for _ in 0..25 {
        if let Some(idx) = curr.rfind(',') {
            curr = curr[..idx].trim_end();
            slices.push(curr);
        } else {
            break;
        }
    }

    for slice in slices {
        if let Some(repaired) = close_open_json_brackets(slice) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&repaired) {
                return Some(val);
            }
        }
    }

    None
}

fn close_open_json_brackets(slice: &str) -> Option<String> {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in slice.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' => {
                    if stack.last() == Some(&'}') {
                        stack.pop();
                    } else {
                        return None;
                    }
                }
                ']' => {
                    if stack.last() == Some(&']') {
                        stack.pop();
                    } else {
                        return None;
                    }
                }
                _ => {}
            }
        }
    }

    let mut result = slice.to_string();
    if in_string {
        result.push('"');
    }
    while let Some(closing) = stack.pop() {
        result.push(closing);
    }
    Some(result)
}

const DECODE_ERROR_BODY_PREVIEW: usize = 2048;

pub async fn decode_response_json(
    response: HttpResponse,
    provider: &str,
) -> Result<serde_json::Value, ProviderError> {
    let bytes = response.into_bytes().await?;
    let text = String::from_utf8_lossy(&bytes);
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => Ok(value),
        Err(error) => {
            let preview = body_preview(&text);
            tracing::warn!(
                target: "nuo_inference_wire::provider",
                provider = provider,
                error = %error,
                body_len = text.len(),
                body_preview = %preview,
                "{} response was not valid JSON",
                provider,
            );
            Err(ProviderError::new(
                provider,
                ProviderErrorKind::Decode,
                format!(
                    "{provider} error decoding response body: {error} (raw body preview: {preview})"
                ),
            ))
        }
    }
}

pub fn body_preview(text: &str) -> String {
    let total_tokens = nuo_tool::tokenizer::count_tokens(text);
    let mut preview: String = text.chars().take(DECODE_ERROR_BODY_PREVIEW).collect();
    let truncated_tokens = total_tokens.saturating_sub(nuo_tool::tokenizer::count_tokens(&preview));
    if truncated_tokens > 0 {
        preview.push_str(&format!("…<{truncated_tokens} more tokens>"));
    }
    preview = preview
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_preview_short_body_passes_through() {
        assert_eq!(body_preview("<html>502</html>"), "<html>502</html>");
    }

    #[test]
    fn body_preview_truncates_long_body_and_reports_remaining_tokens() {
        let long = "a".repeat(DECODE_ERROR_BODY_PREVIEW * 2 + 50);
        let preview = body_preview(&long);
        let total = nuo_tool::tokenizer::count_tokens(&long);
        let kept = nuo_tool::tokenizer::count_tokens(&long[..DECODE_ERROR_BODY_PREVIEW]);
        let omitted = total - kept;
        assert!(
            preview.ends_with(&format!("…<{omitted} more tokens>")),
            "got: {preview}"
        );
    }

    #[test]
    fn http_error_body_detail_preserves_valid_json_without_corrupting_newlines() {
        let json_body = "{\n  \"error\": {\n    \"message\": \"bad request\"\n  }\n}";
        let detail = http_error_body_detail(Some("application/json"), json_body).unwrap();
        assert_eq!(detail, "bad request");
    }

    #[test]
    fn http_error_body_detail_unpacks_nested_json_string() {
        let nested = r#"{"error":{"message":"{\"inner\":\"error details\"}"}}"#;
        let detail = http_error_body_detail(None, nested).unwrap();
        assert_eq!(detail, "error details");
    }

    #[test]
    fn http_error_body_detail_unpacks_nested_gateway_json_string() {
        let raw_json = r#"{"error":{"message":"{\"error\":{\"message\":\"The messages parameter is illegal. Please check the documentation.\",\"type\":\"AI_APICallError\",\"param\":{\"error\":\"The messages parameter is illegal. Please check the documentation.\",\"statusCode\":400,\"name\":\"AI_APICallError\",\"message\":\"The messages parameter is illegal. Please check the documentation.\",\"isRetryable\":false,\"type\":\"AI_APICallError\"}},\"providerMetadata\":{\"gateway\":{\"routing\":{\"originalModelId\":\"zai/glm-5.3-flashx\",\"resolvedProvider\":\"zai\",\"fallbacksAvailable\":[],\"canonicalSlug\":\"zai/glm-5.3-flashx\",\"modelAttemptCount\":1,\"modelAttempts\":[{\"canonicalSlug\":\"zai/glm-5.3-flashx\",\"success\":false,\"providerAttemptCount\":1,\"providerAttempts\":[{\"provider\":\"zai\",\"credentialType\":\"system\",\"success\":false,\"error\":\"The messages parameter is illegal. Please check the documentation.\",\"startTime\":1792727628864,\"endTime\":1792727630532,\"providerRequestId\":\"20261023115350796e7fbd69c94610\",\"statusCode\":400}]}],\"totalProviderAttemptCount\":1,\"affinity\":{\"outcome\":\"no_pin\"},\"cli…(truncated)","type":"invalid_request_error"}}"#;
        let detail = http_error_body_detail(Some("application/json"), raw_json).unwrap();
        assert!(detail.starts_with("The messages parameter is illegal. Please check the documentation. (type: invalid_request_error)"));
        // The nested JSON string must be unpacked into a structured object,
        // so `\"` escapes must not appear and the inner message must be formatted cleanly.
        assert!(!detail.contains(r#"{\"error\""#));
        assert!(detail.contains("\"canonicalSlug\": \"zai/glm-5.3-flashx\""));
    }
}
