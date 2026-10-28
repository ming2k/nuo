//! Notice text normalization: the **one** place that turns a notice's raw
//! transport text into the text a transcript entry displays.
//!
//! # Why this lives in the model
//!
//! A notice's displayed text is *data*, not a paint-time derivation. The entry
//! paints it, the selection hit-test addresses bytes in it, and copy returns
//! it — all three must agree by construction, so the derivation happens once,
//! at message construction, and the result is stored
//! (`TranscriptMessage::raw` + per-line `blocks`). Re-deriving at paint time
//! (the retired `parse_notice_content` path) made "what you see" and "what you
//! copy" two independent projections of `raw` that could disagree, and it left
//! the painted rows with no addressable document text at all.
//!
//! Normalization is deliberately narrow and mechanical:
//! - a JSON error body is pretty-printed for reading,
//! - the one-line summary becomes the leading line,
//! - harness boilerplate ("Gave up after N attempt(s)…") is dropped from the
//!   *display* text, since the entry head already carries the attempt count.
//!
//! Nothing here is presentational: no styles, no widths, no theme. The renderer
//! paints [`NoticeText`] and never parses it.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeText {
    /// E.g. "OpenAI HTTP 429 Too Many Requests", "Anthropic HTTP 529 Overloaded", or "System Notice".
    pub title: String,
    /// Optional pretty-printed JSON body or multiline diagnostic detail.
    pub detail: Option<String>,
}

/// Parse raw notice text: extracts the canonical header (`[Message/Provider] StatusCode StatusReason`)
/// and pretty-prints any JSON response body into `detail`. Strips out mechanical "Gave up after..." boilerplate.
pub fn normalize(raw: &str) -> NoticeText {
    // 1. Strip mechanical boilerplate text if present
    let clean = if let Some(pos) = raw.find("Gave up after") {
        &raw[..pos]
    } else {
        raw
    }
    .trim();

    // 2. Unescape escaped literal backslashes (\n, \r, \t) for uniform processing
    let unescaped;
    let clean_str = if clean.contains("\\n") || clean.contains("\\r") || clean.contains("\\t") {
        unescaped = clean
            .replace("\\n", "\n")
            .replace("\\r", "\r")
            .replace("\\t", "\t");
        &unescaped
    } else {
        clean
    };

    // 3. Check for JSON payload (single-line or multi-line prefix: JSON format)
    if let Some(pos) = clean_str.find('{') {
        let prefix = clean_str[..pos].trim_end_matches([':', ' ', '\n', '\t', '\r']);
        let json_candidate = clean_str[pos..].trim();
        if let Some(mut value) = parse_or_repair_json(json_candidate) {
            normalize_json_value(&mut value);
            let pretty_json =
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| json_candidate.to_string());
            let summary = extract_error_summary(&value);
            let title = match (prefix.is_empty(), summary) {
                (true, Some(s)) => s,
                (true, None) => "Provider HTTP Error".to_string(),
                (false, Some(s)) if !prefix.contains(&s) => format!("{prefix}: {s}"),
                (false, _) => prefix.to_string(),
            };
            return NoticeText {
                title,
                detail: Some(pretty_json),
            };
        }
    }

    // 4. Multi-line error format: first line is human summary / header, rest is diagnostic detail
    if let Some((first_line, rest)) = clean_str.split_once('\n') {
        let first_line_trimmed = first_line.trim();
        let rest_trimmed = rest.trim();
        if !rest_trimmed.is_empty() {
            let detail = if let Some(mut val) = parse_or_repair_json(rest_trimmed) {
                normalize_json_value(&mut val);
                serde_json::to_string_pretty(&val).unwrap_or_else(|_| rest_trimmed.to_string())
            } else {
                rest_trimmed.to_string()
            };
            return NoticeText {
                title: first_line_trimmed.to_string(),
                detail: Some(detail),
            };
        }
    }

    // 5. Single-line plain message
    NoticeText {
        title: clean_str.to_string(),
        detail: None,
    }
}

pub(crate) fn extract_error_summary(val: &serde_json::Value) -> Option<String> {
    let msg = extract_error_message(val)?;
    let err_type = extract_error_type_or_code(val);
    match err_type {
        Some(t) if !msg.to_ascii_lowercase().contains(&t.to_ascii_lowercase()) => {
            Some(format!("{msg} (type: {t})"))
        }
        _ => Some(msg),
    }
}

pub(crate) fn extract_error_message(val: &serde_json::Value) -> Option<String> {
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
            if let Some(err_val) = map.get("error") {
                if let Some(m) = extract_error_message(err_val) {
                    return Some(m);
                }
            }
            if let Some(msg_val) = map.get("message") {
                if let Some(m) = extract_error_message(msg_val) {
                    return Some(m);
                }
            }
            if let Some(det_val) = map.get("detail") {
                if let Some(m) = extract_error_message(det_val) {
                    return Some(m);
                }
            }
            if let Some(param_val) = map.get("param") {
                if let Some(m) = extract_error_message(param_val) {
                    return Some(m);
                }
            }
            None
        }
        _ => None,
    }
}

pub(crate) fn extract_error_type_or_code(val: &serde_json::Value) -> Option<String> {
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

pub(crate) fn unescape_string_literals(s: &str) -> String {
    s.replace("\\n", "\n")
        .replace("\\r", "\r")
        .replace("\\t", "\t")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
}

fn parse_or_repair_json(json_candidate: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_candidate) {
        return Some(value);
    }
    try_repair_truncated_json(json_candidate)
}

pub(crate) fn normalize_json_value(value: &mut serde_json::Value) {
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

impl NoticeText {
    /// The entry's display text: the leading line, then the detail — verbatim,
    /// with its own newlines.
    ///
    /// This string is the message's `raw` and the concatenation of its
    /// per-line blocks, so painting, selection, and copy all read one text. The
    /// lead and detail are joined by a single newline (not a blank paragraph
    /// line): the entry head already separates the lead visually, and a detail
    /// that carries its own paragraph breaks keeps them.
    pub fn display_text(&self) -> String {
        match self.detail.as_deref().filter(|d| !d.trim().is_empty()) {
            Some(detail) => format!("{}\n{}", self.title, detail),
            None => self.title.clone(),
        }
    }

    /// Whether this notice has any text to display at all.
    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.detail.as_deref().is_none_or(|d| d.trim().is_empty())
    }
}

/// Run the extractor over `raw` and return the display text in one step.
pub fn display_text(raw: &str) -> String {
    normalize(raw).display_text()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_json_error_into_header_and_pretty_detail() {
        let raw = r#"OpenAI HTTP 429 Too Many Requests: {"error":{"code":"1305","message":"该模型当前访问量过大，请您稍后再试"}}
Gave up after 6 attempt(s); the upstream service appears overloaded. Resend the message to try again, or raise `provider_retry_max_attempts` for more attempts."#;
        let parsed = normalize(raw);
        assert_eq!(
            parsed.title,
            "OpenAI HTTP 429 Too Many Requests: 该模型当前访问量过大，请您稍后再试 (type: 1305)"
        );
        assert!(parsed.detail.is_some());
        let detail = parsed.detail.unwrap();
        assert!(detail.contains("\"code\": \"1305\""));
        assert!(detail.contains("该模型当前访问量过大，请您稍后再试"));
        assert!(!detail.contains("Gave up after"));
    }

    #[test]
    fn normalizes_retry_exhausted_json_error() {
        let raw = r#"Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: {"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED"}}"#;
        let parsed = normalize(raw);
        assert_eq!(
            parsed.title,
            "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: Resource has been exhausted"
        );
        assert!(parsed.detail.is_some());
        let detail = parsed.detail.unwrap();
        assert!(detail.contains("\"code\": 429"));
        assert!(detail.contains("\"status\": \"RESOURCE_EXHAUSTED\""));
    }

    #[test]
    fn normalizes_plain_http_error_without_json() {
        let raw = "OpenAI HTTP 503 Service Unavailable";
        let parsed = normalize(raw);
        assert_eq!(parsed.title, "OpenAI HTTP 503 Service Unavailable");
        assert_eq!(parsed.detail, None);
    }

    #[test]
    fn normalizes_plain_message_without_detail() {
        let raw = "Failed to connect to host";
        let parsed = normalize(raw);
        assert_eq!(parsed.title, "Failed to connect to host");
        assert_eq!(parsed.detail, None);
    }

    #[test]
    fn normalizes_multiline_text_into_first_line_and_rest() {
        let raw = "Claude HTTP 500 Server Error\nDetails:\nhost unreachable";
        let parsed = normalize(raw);
        assert_eq!(parsed.title, "Claude HTTP 500 Server Error");
        assert_eq!(
            parsed.detail,
            Some("Details:\nhost unreachable".to_string())
        );
    }

    #[test]
    fn normalizes_plain_text_with_escaped_newlines() {
        let raw = "Provider HTTP 400 Bad Request: Invalid message format\\nLine 2 details\\nLine 3 guidance";
        let parsed = normalize(raw);
        assert_eq!(
            parsed.title,
            "Provider HTTP 400 Bad Request: Invalid message format"
        );
        assert_eq!(
            parsed.detail,
            Some("Line 2 details\nLine 3 guidance".to_string())
        );
    }

    #[test]
    fn normalizes_nested_gateway_json_error_digesting_escapes_and_newlines() {
        let raw = r#"OpenAI Responses HTTP 400 Bad Request: {"error":{"message":"{\"error\":{\"message\":\"The messages parameter is illegal. Please check the documentation.\",\"type\":\"AI_APICallError\",\"param\":{\"error\":\"The messages parameter is illegal. Please check the documentation.\",\"statusCode\":400,\"name\":\"AI_APICallError\",\"message\":\"The messages parameter is illegal. Please check the documentation.\",\"isRetryable\":false,\"type\":\"AI_APICallError\"}},\"providerMetadata\":{\"gateway\":{\"routing\":{\"originalModelId\":\"zai/glm-5.3-flashx\",\"resolvedProvider\":\"zai\",\"fallbacksAvailable\":[],\"canonicalSlug\":\"zai/glm-5.3-flashx\",\"modelAttemptCount\":1,\"modelAttempts\":[{\"canonicalSlug\":\"zai/glm-5.3-flashx\",\"success\":false,\"providerAttemptCount\":1,\"providerAttempts\":[{\"provider\":\"zai\",\"credentialType\":\"system\",\"success\":false,\"error\":\"The messages parameter is illegal. Please check the documentation.\",\"startTime\":1792727628864,\"endTime\":1792727630532,\"providerRequestId\":\"20261023115350796e7fbd69c94610\",\"statusCode\":400}]}],\"totalProviderAttemptCount\":1,\"affinity\":{\"outcome\":\"no_pin\"},\"cli…(truncated)","type":"invalid_request_error"}}"#;
        let parsed = normalize(raw);
        assert_eq!(
            parsed.title,
            "OpenAI Responses HTTP 400 Bad Request: The messages parameter is illegal. Please check the documentation. (type: invalid_request_error)"
        );
        assert!(parsed.detail.is_some());
        let detail = parsed.detail.unwrap();
        // The nested JSON string must be unpacked into a structured object,
        // so `\"` escapes must not appear and the inner message must be formatted cleanly.
        assert!(!detail.contains(r#"{\"error\""#));
        assert!(
            detail.contains("The messages parameter is illegal. Please check the documentation.")
        );
        assert!(detail.contains("\"canonicalSlug\": \"zai/glm-5.3-flashx\""));
    }

    #[test]
    fn display_text_is_lead_then_detail_verbatim() {
        // The lead/detail seam is one newline: the detail keeps its own
        // paragraph breaks, and the entry head is what separates the lead
        // visually — an injected blank row would show up as a phantom gap.
        let text =
            normalize("Claude HTTP 500 Server Error\nDetails:\nhost unreachable").display_text();
        assert_eq!(
            text,
            "Claude HTTP 500 Server Error\nDetails:\nhost unreachable"
        );
    }

    #[test]
    fn display_text_of_a_lead_only_notice_is_the_lead() {
        assert_eq!(
            display_text("Cancelled via [Esc Esc]"),
            "Cancelled via [Esc Esc]"
        );
    }

    #[test]
    fn blank_detail_never_mints_an_empty_paragraph() {
        let text = NoticeText {
            title: "lead".into(),
            detail: Some("   \n".into()),
        }
        .display_text();
        assert_eq!(text, "lead");
    }
}
