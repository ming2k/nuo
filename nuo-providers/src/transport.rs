//! HTTP transport helpers shared by every protocol adapter: success
//! enforcement, retry/error classification, JSON decode diagnostics, and
//! credential masking in error messages. The pooled HTTP client itself lives
//! in [`crate::client`]; endpoint configuration in [`crate::endpoint`]; SSE
//! byte reassembly in [`crate::sse`].

use nuo_contracts::{ProviderError, ProviderErrorKind};
use std::time::SystemTime;

pub fn retry_after_ms(headers: &http::header::HeaderMap) -> Option<u64> {
    if let Some(milliseconds) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f64>().ok())
    {
        return Some(milliseconds.max(0.0) as u64);
    }
    let value = headers.get(http::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<f64>() {
        return Some((seconds.max(0.0) * 1000.0) as u64);
    }
    let parsed = httpdate::parse_http_date(value).ok()?;
    let now = SystemTime::now();
    Some(
        parsed
            .duration_since(now)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64,
    )
}

/// Enforce HTTP success on either transport.
pub async fn ensure_success(
    response: crate::egress::HttpResponse,
    provider: &str,
    model: Option<&str>,
) -> Result<crate::egress::HttpResponse, ProviderError> {
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

fn classify_http_error(status: http::StatusCode, body: &str) -> ProviderErrorKind {
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
/// Error codes are exact; prose requires both a context/token subject and an
/// overflow predicate so unrelated `max_tokens` validation errors stay 400s.
fn is_context_overflow_payload(body: &str) -> bool {
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

/// Keep structured provider diagnostics, but do not surface a reverse
/// proxy's HTML error document as transcript content. Besides being noise,
/// those pages commonly carry CRLF/control bytes and can be surprisingly
/// large. The HTTP status already contains the useful gateway failure.
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
    response: crate::egress::HttpResponse,
    provider: &str,
) -> Result<serde_json::Value, ProviderError> {
    let bytes = response.into_bytes().await?;
    let text = String::from_utf8_lossy(&bytes);
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => Ok(value),
        Err(error) => {
            let preview = body_preview(&text);
            tracing::warn!(
                target: "nuo_contracts::provider",
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

fn body_preview(text: &str) -> String {
    // Diagnostic text inside a decode-error message: report the omitted tail
    // in tokens (ADR-0120) — how much context the body would have cost.
    let total_tokens = nuo_contracts::tokenizer::count_tokens(text);
    let mut preview: String = text.chars().take(DECODE_ERROR_BODY_PREVIEW).collect();
    let truncated_tokens =
        total_tokens.saturating_sub(nuo_contracts::tokenizer::count_tokens(&preview));
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
    fn retry_after_supports_seconds_and_milliseconds() {
        let mut headers = http::header::HeaderMap::new();
        headers.insert("retry-after", "2.5".parse().unwrap());
        assert_eq!(retry_after_ms(&headers), Some(2_500));

        headers.insert("retry-after-ms", "750".parse().unwrap());
        assert_eq!(retry_after_ms(&headers), Some(750));
    }

    #[test]
    fn body_preview_short_body_passes_through() {
        assert_eq!(body_preview("<html>502</html>"), "<html>502</html>");
    }

    #[test]
    fn body_preview_truncates_long_body_and_reports_remaining_tokens() {
        let long = "a".repeat(DECODE_ERROR_BODY_PREVIEW * 2 + 50);
        let preview = body_preview(&long);
        // The omitted tail is reported in tokens (ADR-0120): the whole body
        // tokenizes to N, the kept preview to fewer, the difference is the
        // count in the suffix.
        let total = nuo_contracts::tokenizer::count_tokens(&long);
        let kept = nuo_contracts::tokenizer::count_tokens(&long[..DECODE_ERROR_BODY_PREVIEW]);
        let omitted = total - kept;
        assert!(
            preview.ends_with(&format!("…<{omitted} more tokens>")),
            "got: {preview}"
        );
    }

    #[test]
    fn body_preview_escapes_control_characters() {
        let preview = body_preview("line1\nline2\ttab\rend");
        assert!(
            !preview.contains('\n') && !preview.contains('\t') && !preview.contains('\r'),
            "control chars must be escaped: {preview:?}"
        );
        assert!(preview.contains("\\n") && preview.contains("\\t") && preview.contains("\\r"));
    }

    #[test]
    fn body_preview_truncates_on_char_boundary() {
        let chars = "日".repeat(DECODE_ERROR_BODY_PREVIEW + 10);
        let preview = body_preview(&chars);
        assert!(!preview.contains('\u{FFFD}'));
    }

    #[test]
    fn http_error_body_hides_html_gateway_pages() {
        let body = "<html>\r\n<head><title>504 Gateway Time-out</title></head>\r\n</html>";
        assert_eq!(
            http_error_body_detail(Some("text/html; charset=utf-8"), body),
            None
        );
        assert_eq!(http_error_body_detail(None, body), None);
    }

    #[test]
    fn http_error_body_keeps_bounded_structured_diagnostics() {
        let body = "{\"error\":{\"message\":\"rate limited\"}}\r\n";
        assert_eq!(
            http_error_body_detail(Some("application/json"), body),
            Some("{\"error\":{\"message\":\"rate limited\"}}".to_string())
        );
    }



    #[test]
    fn context_overflow_payload_detection_is_conservative() {
        assert!(is_context_overflow_payload(
            "{\"error\":{\"message\":\"This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens.\"}}"
        ));
        assert!(is_context_overflow_payload(
            "{\"error\":{\"message\":\"prompt is too long for model\"}}"
        ));
        assert!(is_context_overflow_payload("context_length_exceeded"));
        assert!(is_context_overflow_payload(
            "The input token count exceeds the maximum number of tokens allowed"
        ));
        assert!(!is_context_overflow_payload(
            "{\"error\":{\"message\":\"invalid api key provided\"}}"
        ));
        assert!(!is_context_overflow_payload(
            "{\"error\":{\"message\":\"max_tokens must be greater than zero\"}}"
        ));
    }

    #[test]
    fn http_error_classification_promotes_only_context_failures() {
        assert_eq!(
            classify_http_error(http::StatusCode::BAD_REQUEST, "context_length_exceeded"),
            ProviderErrorKind::ContextOverflow
        );
        assert_eq!(
            classify_http_error(
                http::StatusCode::BAD_REQUEST,
                "max_tokens must be greater than zero"
            ),
            ProviderErrorKind::InvalidRequest
        );
        assert_eq!(
            classify_http_error(http::StatusCode::TOO_MANY_REQUESTS, "too many tokens"),
            ProviderErrorKind::RateLimited
        );
    }

    #[tokio::test]
    async fn staged_rollout_error_translation_annotates_404_and_403() {
        use crate::egress::HttpResponse;
        use futures::StreamExt;

        let res_404 = HttpResponse {
            status: http::StatusCode::NOT_FOUND,
            headers: http::HeaderMap::new(),
            body: futures::stream::once(async {
                Ok(bytes::Bytes::from(
                    r#"{"error":{"message":"The model `gpt-6-astra` does not exist or you do not have access to it.","code":"model_not_found"}}"#,
                ))
            })
            .boxed(),
        };
        let err_404 = ensure_success(res_404, "OpenAI", Some("gpt-6-astra"))
            .await
            .unwrap_err();
        assert!(err_404.message().contains("rollout entitlement"));
        assert!(err_404.message().contains("gpt-6-astra"));

        let res_403 = HttpResponse {
            status: http::StatusCode::FORBIDDEN,
            headers: http::HeaderMap::new(),
            body: futures::stream::once(async {
                Ok(bytes::Bytes::from(
                    r#"{"error":{"message":"User is not permitted to use model","code":"permission_denied"}}"#,
                ))
            })
            .boxed(),
        };
        let err_403 = ensure_success(res_403, "OpenAI", Some("gpt-6-astra"))
            .await
            .unwrap_err();
        assert!(err_403.message().contains("rollout entitlement"));
        assert!(err_403.message().contains("gpt-6-astra"));
    }
}
