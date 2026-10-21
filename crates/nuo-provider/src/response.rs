//! Provider HTTP response validation, error classification, and JSON decoding.

use nuo_egress::{HttpResponse, retry_after_ms};
use nuo_inference_wire::{ProviderError, ProviderErrorKind};

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
    (!looks_html).then(|| body_preview(trimmed))
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
}
