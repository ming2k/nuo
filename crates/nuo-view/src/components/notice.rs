//! Transcript notice component (System / Infrastructure notices as top-level transcript entries).
use nuotc::{
    Color, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::disclosure::{Disclosure, Interaction, summary_text_color};
use crate::model::document::{MessageKind, NoticeParts, NoticeSeverity, TranscriptMessage};
use crate::model::layout::{BlockRegion, LayoutMap, NOTICE_BLOCK_IDX};
use crate::text_layout::wrap_text;
use nuo_ui_primitives::design::{TRANSCRIPT_BODY_LEADING_INDENT, TURN_HEADER_BODY_GAP_ROWS};

use nuo_ui_primitives::theme::Theme;

pub struct NoticeProps<'a> {
    pub message: &'a TranscriptMessage,
}

impl<'a> NoticeProps<'a> {
    fn severity(&self) -> Option<NoticeSeverity> {
        match &self.message.kind {
            MessageKind::Notice { severity, .. } => Some(*severity),
            MessageKind::ProviderRetry { .. } => Some(NoticeSeverity::Warning),
            _ => None,
        }
    }

    fn raw_text(&self) -> &'a str {
        match &self.message.kind {
            MessageKind::ProviderRetry { failure, .. } => failure.as_str(),
            _ => &self.message.raw,
        }
    }

    /// The architecture-agreed two-part split (topic + title/detail), when
    /// this notice carries one. Core notices keep their structure across the
    /// boundary; local/legacy notices return `None` and the fallback parse
    /// below reconstructs a header/detail split from the raw text.
    fn parts(&self) -> Option<&'a NoticeParts> {
        self.message.notice_parts()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeContent {
    /// E.g. "OpenAI HTTP 429 Too Many Requests", "Anthropic HTTP 529 Overloaded", or "System Notice".
    pub header: String,
    /// Optional pretty-printed JSON body or multiline diagnostic detail.
    pub detail: Option<String>,
}

/// Parse raw notice text: extracts the canonical header (`[Message/Provider] StatusCode StatusReason`)
/// and pretty-prints any JSON response body into `detail`. Strips out mechanical "Gave up after..." boilerplate.
pub fn parse_notice_content(raw: &str) -> NoticeContent {
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
            let header = match (prefix.is_empty(), summary) {
                (true, Some(s)) => s,
                (true, None) => "Provider HTTP Error".to_string(),
                (false, Some(s)) if !prefix.contains(&s) => format!("{prefix}: {s}"),
                (false, _) => prefix.to_string(),
            };
            return NoticeContent {
                header,
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
            return NoticeContent {
                header: first_line_trimmed.to_string(),
                detail: Some(detail),
            };
        }
    }

    // 5. Single-line plain message
    NoticeContent {
        header: clean_str.to_string(),
        detail: None,
    }
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

fn parse_or_repair_json(json_candidate: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_candidate) {
        return Some(value);
    }
    try_repair_truncated_json(json_candidate)
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

/// Resolve a notice's content for rendering: the structured title/detail
/// pair when the message carries one (core notices), otherwise the
/// heuristic parse of the raw text (local notices, restored sessions).
/// A structured notice with an empty title degrades to the parse too.
fn notice_content<'v>(
    notice: &'v NoticeProps<'_>,
    parsed: &'v NoticeContent,
    dynamic_title: &'v mut Option<String>,
    dynamic_detail: &'v mut Option<String>,
) -> (&'v str, Option<&'v str>, Option<&'v str>) {
    if let MessageKind::ProviderRetry {
        attempt,
        max_attempts,
        retry_at,
        ..
    } = &notice.message.kind
    {
        let now = std::time::Instant::now();
        let title = if now < *retry_at {
            let secs = (*retry_at - now).as_secs_f32().ceil() as u64;
            format!(
                "Retrying provider request ({}/{}) in {}s...",
                attempt, max_attempts, secs
            )
        } else {
            format!(
                "Retrying provider request ({}/{})...",
                attempt, max_attempts
            )
        };
        *dynamic_title = Some(title);

        if let Some(detail) = &parsed.detail {
            if !parsed.header.is_empty() && parsed.header != "Provider HTTP Error" {
                *dynamic_detail = Some(format!("{}\n{}", parsed.header, detail));
            } else {
                *dynamic_detail = Some(detail.clone());
            }
        } else if !parsed.header.is_empty() {
            *dynamic_detail = Some(parsed.header.clone());
        }

        return ("retry", dynamic_title.as_deref(), dynamic_detail.as_deref());
    }

    match notice.parts() {
        Some(parts) if !parts.title.trim().is_empty() => (
            parts.topic.as_deref().unwrap_or("notification"),
            Some(parts.title.as_str()),
            parts.detail.as_deref(),
        ),
        parts => (
            parts
                .and_then(|p| p.topic.as_deref())
                .unwrap_or("notification"),
            Some(parsed.header.as_str()),
            parsed.detail.as_deref(),
        ),
    }
}

/// Build the one-row header of a notification entry: topic left, time right.
/// The lead glyph (`! `, `▲ `, `ℹ `) and the topic label (`▲ trust`,
/// `▲ provider`, …; generic `notification` when no topic is known) are
/// rendered in the severity indicator tone (BOLD), followed by an optional
/// right-aligned time.
///
/// A notice is an **always-open** entry (see [`draw_notice_view`]), so it
/// carries no disclosure marker: the severity glyph is the whole marker
/// vocabulary, exactly as keeps command entries marker-free.
fn notice_header_line(
    lead_symbol: &str,
    topic: &str,
    severity_tone: Color,
    time_label: Option<&str>,
    muted: Color,
    full_width: usize,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(4);
    let mut used = 0usize;

    // Indicator tag: `! trust`, `▲ provider`, `ℹ command` in severity_tone + BOLD.
    let tag = format!("{lead_symbol}{topic}");
    used += tag.width();
    spans.push(Span::styled(
        tag,
        Style::default()
            .fg(severity_tone)
            .add_modifier(Modifier::BOLD),
    ));

    // Trailing timestamp: right-aligned in muted color.
    if let Some(time) = time_label {
        let time_width = time.width();
        if used + 2 + time_width <= full_width {
            spans.push(Span::styled(
                " ".repeat(full_width - used - time_width),
                Style::default(),
            ));
            spans.push(Span::styled(time.to_string(), Style::default().fg(muted)));
        }
    }

    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
pub fn draw_notice_view(
    frame: &mut Frame,
    area: Rect,
    notice: NoticeProps<'_>,
    mi: usize,
    layout_map: &mut LayoutMap,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
    theme: &Theme,
    hovered: bool,
    focused: bool,
) {
    let Some(severity) = notice.severity() else {
        return;
    };
    let parsed = parse_notice_content(notice.raw_text());
    let mut dynamic_title = None;
    let mut dynamic_detail = None;
    let (topic, title, detail) =
        notice_content(&notice, &parsed, &mut dynamic_title, &mut dynamic_detail);
    let full_width = area.width as usize;
    let time_label = notice
        .message
        .sent_at_ms
        .map(nuo_ui_primitives::time::sent_time_label);

    // A notice is an **always-open** entry (ADR-0020 "fully disclosed
    // entries"): its severity, title, and detail *are* the message, so nothing
    // gates them and it carries no disclosure marker — the severity glyph is
    // its whole marker vocabulary, matching marker-free rule for
    // command entries.
    //
    // The `expanded` field remains part of the record contract (`y`-copy,
    // `pin_notice_expanded`, restore). A future collapsible notice — one whose
    // detail is a long JSON body worth folding — must honor it in the body
    // *and* the header marker together, exactly as `compacted.rs` does.
    let (lead_symbol, tag_color) = match severity {
        NoticeSeverity::Error => ("! ", theme.err()),
        NoticeSeverity::Warning => ("▲ ", theme.warn()),
        NoticeSeverity::Info => ("ℹ ", theme.info()),
    };
    // The interaction channel still tints the topic label, so a hovered or
    // keyboard-focused notice is visibly the component attention is on
    // (affordance hue) even though its body never folds.
    let topic_color = summary_text_color(
        Some(tag_color),
        Disclosure::Expanded,
        Interaction::from_hover_focused(hovered, focused),
        theme,
    );
    let header_line = notice_header_line(
        lead_symbol,
        topic,
        topic_color,
        time_label.as_deref(),
        theme.muted(),
        full_width,
    );

    *content_lines += 1;
    if *skip_rows > 0 {
        *skip_rows -= 1;
    } else if *current_y < area.y + area.height {
        let line_rect = Rect::new(area.x, *current_y, area.width, 1);
        frame.render_widget(&mut Paragraph::new(header_line), line_rect);

        layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx: NOTICE_BLOCK_IDX,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect: line_rect,
            hidden_ranges: Vec::new(),
        });
        *current_y += 1;
    }

    // 2. 1-row blank gap between entry header and body
    for _ in 0..TURN_HEADER_BODY_GAP_ROWS {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows -= 1;
        } else if *current_y < area.y + area.height {
            *current_y += 1;
        }
    }

    // 3. Notice body: header text. Like every other entry body, the prose is
    //    indented TRANSCRIPT_BODY_LEADING_INDENT past the entry head so the
    //    header row reads as the entry's *head* and the body as its content.
    //    `title` is always `Some` from `notice_content`; unwrap mirrors the
    //    invariant of the old `parsed.header` path.
    let body_indent = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
    let body_wrap_width = full_width.saturating_sub(body_indent.width()).max(1);
    let body_lines = wrap_text(title.unwrap_or(parsed.header.as_str()), body_wrap_width);

    for wl in body_lines {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows -= 1;
            continue;
        }
        if *current_y < area.y + area.height {
            let line_rect = Rect::new(area.x, *current_y, area.width, 1);
            let spans = vec![
                Span::styled(body_indent.clone(), Style::default()),
                Span::styled(
                    wl.text.clone(),
                    Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
                ),
            ];
            frame.render_widget(&mut Paragraph::new(Line::from(spans)), line_rect);

            layout_map.push(BlockRegion {
                message_idx: mi,
                block_idx: NOTICE_BLOCK_IDX,
                start_byte: 0,
                end_byte: wl.text.len(),
                text: wl.text,
                prefix_cols: body_indent.width() as u16,
                rect: line_rect,
                hidden_ranges: Vec::new(),
            });
            *current_y += 1;
        }
    }

    // 4. Detail body: formatted detail (unfolded direct rendering). Same
    //    leading indent as the header body; wrap width matches what is
    //    actually painted (indent cols only — the stream gutter is already
    //    applied once at the entry point).
    if let Some(detail) = detail {
        let detail_indent = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
        let detail_wrap_width = full_width.saturating_sub(detail_indent.width()).max(1);

        for line_str in detail.lines() {
            // Paragraph separator: an empty source line renders as one blank
            // row (wrap_text would otherwise return zero rows and swallow it).
            if line_str.trim().is_empty() {
                *content_lines += 1;
                if *skip_rows > 0 {
                    *skip_rows -= 1;
                } else if *current_y < area.y + area.height {
                    *current_y += 1;
                }
                continue;
            }
            let wrapped_detail = wrap_text(line_str, detail_wrap_width);
            for dwl in wrapped_detail {
                *content_lines += 1;
                if *skip_rows > 0 {
                    *skip_rows -= 1;
                    continue;
                }
                if *current_y < area.y + area.height {
                    let line_rect = Rect::new(area.x, *current_y, area.width, 1);
                    let spans = vec![
                        Span::styled(detail_indent.clone(), Style::default()),
                        Span::styled(dwl.text.clone(), Style::default().fg(theme.muted())),
                    ];
                    frame.render_widget(&mut Paragraph::new(Line::from(spans)), line_rect);

                    layout_map.push(BlockRegion {
                        message_idx: mi,
                        block_idx: NOTICE_BLOCK_IDX,
                        start_byte: 0,
                        end_byte: dwl.text.len(),
                        text: dwl.text,
                        prefix_cols: detail_indent.width() as u16,
                        rect: line_rect,
                        hidden_ranges: Vec::new(),
                    });
                    *current_y += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_error_into_header_and_pretty_detail() {
        let raw = r#"OpenAI HTTP 429 Too Many Requests: {"error":{"code":"1305","message":"该模型当前访问量过大，请您稍后再试"}}
Gave up after 6 attempt(s); the upstream service appears overloaded. Resend the message to try again, or raise `provider_retry_max_attempts` for more attempts."#;
        let parsed = parse_notice_content(raw);
        assert_eq!(
            parsed.header,
            "OpenAI HTTP 429 Too Many Requests: 该模型当前访问量过大，请您稍后再试 (type: 1305)"
        );
        assert!(parsed.detail.is_some());
        let detail = parsed.detail.unwrap();
        assert!(detail.contains("\"code\": \"1305\""));
        assert!(detail.contains("该模型当前访问量过大，请您稍后再试"));
        assert!(!detail.contains("Gave up after"));
    }

    #[test]
    fn parses_retry_exhausted_json_error() {
        let raw = r#"Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: {"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED"}}"#;
        let parsed = parse_notice_content(raw);
        assert_eq!(
            parsed.header,
            "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: Resource has been exhausted"
        );
        assert!(parsed.detail.is_some());
        let detail = parsed.detail.unwrap();
        assert!(detail.contains("\"code\": 429"));
        assert!(detail.contains("\"status\": \"RESOURCE_EXHAUSTED\""));
    }

    #[test]
    fn parses_plain_http_error_without_json() {
        let raw = "OpenAI HTTP 503 Service Unavailable";
        let parsed = parse_notice_content(raw);
        assert_eq!(parsed.header, "OpenAI HTTP 503 Service Unavailable");
        assert_eq!(parsed.detail, None);
    }

    #[test]
    fn parses_plain_message_without_detail() {
        let raw = "Failed to connect to host";
        let parsed = parse_notice_content(raw);
        assert_eq!(parsed.header, "Failed to connect to host");
        assert_eq!(parsed.detail, None);
    }

    #[test]
    fn parses_multiline_text_into_first_line_and_rest() {
        let raw = "Claude HTTP 500 Server Error\nDetails:\nhost unreachable";
        let parsed = parse_notice_content(raw);
        assert_eq!(parsed.header, "Claude HTTP 500 Server Error");
        assert_eq!(
            parsed.detail,
            Some("Details:\nhost unreachable".to_string())
        );
    }

    #[test]
    fn parses_plain_text_with_escaped_newlines() {
        let raw = "Provider HTTP 400 Bad Request: Invalid message format\\nLine 2 details\\nLine 3 guidance";
        let parsed = parse_notice_content(raw);
        assert_eq!(
            parsed.header,
            "Provider HTTP 400 Bad Request: Invalid message format"
        );
        assert_eq!(
            parsed.detail,
            Some("Line 2 details\nLine 3 guidance".to_string())
        );
    }

    #[test]
    fn parses_nested_gateway_json_error_digesting_escapes_and_newlines() {
        let raw = r#"OpenAI Responses HTTP 400 Bad Request: {"error":{"message":"{\"error\":{\"message\":\"The messages parameter is illegal. Please check the documentation.\",\"type\":\"AI_APICallError\",\"param\":{\"error\":\"The messages parameter is illegal. Please check the documentation.\",\"statusCode\":400,\"name\":\"AI_APICallError\",\"message\":\"The messages parameter is illegal. Please check the documentation.\",\"isRetryable\":false,\"type\":\"AI_APICallError\"}},\"providerMetadata\":{\"gateway\":{\"routing\":{\"originalModelId\":\"zai/glm-5.3-flashx\",\"resolvedProvider\":\"zai\",\"fallbacksAvailable\":[],\"canonicalSlug\":\"zai/glm-5.3-flashx\",\"modelAttemptCount\":1,\"modelAttempts\":[{\"canonicalSlug\":\"zai/glm-5.3-flashx\",\"success\":false,\"providerAttemptCount\":1,\"providerAttempts\":[{\"provider\":\"zai\",\"credentialType\":\"system\",\"success\":false,\"error\":\"The messages parameter is illegal. Please check the documentation.\",\"startTime\":1792727628864,\"endTime\":1792727630532,\"providerRequestId\":\"20261023115350796e7fbd69c94610\",\"statusCode\":400}]}],\"totalProviderAttemptCount\":1,\"affinity\":{\"outcome\":\"no_pin\"},\"cli…(truncated)","type":"invalid_request_error"}}"#;
        let parsed = parse_notice_content(raw);
        assert_eq!(
            parsed.header,
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
    fn draw_notice_view_renders_as_entry_with_header_gap_and_body() {
        let theme = Theme::default();
        let mut grid = nuotc::CellGrid::new(60, 10);
        let mut damage = nuotc::Damage::new(10);
        let mut frame = Frame::new(&mut grid, &mut damage);
        let area = Rect::new(0, 0, 60, 10);
        let msg = TranscriptMessage::notice(NoticeSeverity::Error, "Connection refused");
        let notice = NoticeProps { message: &msg };
        let mut layout_map = LayoutMap::default();
        let mut skip_rows = 0;
        let mut current_y = 0;
        let mut content_lines = 0;

        draw_notice_view(
            &mut frame,
            area,
            notice,
            0,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &theme,
            false,
            false,
        );

        // Header (row 0) + 1-row gap (row 1) + Body (row 2) = 3 content lines
        assert_eq!(content_lines, 3);
        assert_eq!(current_y, 3);

        // Verify row 0 contains "! notification"
        let mut row0 = String::new();
        for x in 0..20 {
            row0.push_str(grid.get(x, 0).unwrap().symbol.as_str());
        }
        assert!(row0.starts_with("! notification"));

        // Verify row 2 contains "Connection refused", indented
        // TRANSCRIPT_BODY_LEADING_INDENT (2) past the entry head.
        let mut row2 = String::new();
        for x in 0..25 {
            row2.push_str(grid.get(x, 2).unwrap().symbol.as_str());
        }
        assert!(
            row2.starts_with("  Connection refused"),
            "body must be indented 2 cols past the entry head, got: {row2:?}"
        );

        // The body region must record the decorative indent as its prefix so
        // copy/hit-testing resolve to content, not the indent whitespace.
        let body = layout_map.region_at(2, 2).expect("body region");
        assert_eq!(body.text, "Connection refused");
        assert_eq!(body.prefix_cols, TRANSCRIPT_BODY_LEADING_INDENT);
    }

    #[test]
    fn draw_notice_view_renders_paragraph_separator_between_detail_parts() {
        let theme = Theme::default();
        let mut grid = nuotc::CellGrid::new(60, 10);
        let mut damage = nuotc::Damage::new(10);
        let mut frame = Frame::new(&mut grid, &mut damage);
        let area = Rect::new(0, 0, 60, 10);
        // Mirrors AgentNotice::render_text(): title, then body whose own
        // paragraphs are separated by a blank line.
        let raw = "Trust changed\nQuarantined pending review.\n\n/trust re-trusts all.";
        let msg = TranscriptMessage::notice(NoticeSeverity::Warning, raw);
        let notice = NoticeProps { message: &msg };
        let mut layout_map = LayoutMap::default();
        let mut skip_rows = 0;
        let mut current_y = 0;
        let mut content_lines = 0;

        draw_notice_view(
            &mut frame,
            area,
            notice,
            0,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &theme,
            false,
            false,
        );

        // head + gap + detail p1 + detail p2 + separator + detail p3 = 6 rows.
        assert_eq!(content_lines, 6);
        assert_eq!(current_y, 6);

        let row = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..40 {
                s.push_str(grid.get(x, y).unwrap().symbol.as_str());
            }
            s
        };
        assert!(row(2).starts_with("  Trust changed"));
        assert!(row(3).starts_with("  Quarantined pending review."));
        assert!(
            row(4).trim().is_empty(),
            "paragraph separator must render as a blank row, got: {:?}",
            row(4)
        );
        assert!(row(5).starts_with("  /trust re-trusts all."));
    }

    #[test]
    fn core_notice_renders_topic_head_instead_of_generic_notification() {
        // End-to-end for the architecture-agreed split: the entry head shows
        // the predictable topic (`▲ trust`), the bold body lead is the title,
        // and the muted detail is the structured body — none of it recovered
        // by the heuristic text parse. Mirrors the runtime's
        // `workspace_trust_notice`, which emits a first-class `TrustChanged`
        // kind.
        let core = nuo_wire::AgentNotice::trust_changed("Workspace configurations changed")
            .with_body("Changed on disk: rules (AGENTS.md / rules) — quarantined pending review.");
        let msg = TranscriptMessage::notice_from_core(&core);

        let theme = Theme::default();
        let mut grid = nuotc::CellGrid::new(60, 10);
        let mut damage = nuotc::Damage::new(10);
        let mut frame = Frame::new(&mut grid, &mut damage);
        let area = Rect::new(0, 0, 60, 10);
        let notice = NoticeProps { message: &msg };
        let mut layout_map = LayoutMap::default();
        let mut skip_rows = 0;
        let mut current_y = 0;
        let mut content_lines = 0;

        draw_notice_view(
            &mut frame,
            area,
            notice,
            0,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &theme,
            false,
            false,
        );

        let row = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..58 {
                s.push_str(grid.get(x, y).unwrap().symbol.as_str());
            }
            s
        };
        // Head names the subsystem, not the constant "notification".
        assert!(row(0).starts_with("▲ trust"));
        assert!(!row(0).contains("notification"));
        // Title (bold lead) then structured detail (muted).
        assert!(row(2).starts_with("  Workspace configurations changed"));
        assert!(row(3).starts_with("  Changed on disk: rules"));
    }

    #[test]
    fn draw_notice_view_measures_full_logical_content_lines_when_viewport_clips() {
        let raw = "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: {\n  \"error\": {\n    \"code\": 429,\n    \"message\": \"Individual quota reached.\",\n    \"status\": \"RESOURCE_EXHAUSTED\"\n  }\n}";
        let msg = TranscriptMessage::notice(NoticeSeverity::Error, raw);
        let theme = Theme::default();

        // 1. Measure with large viewport (no clipping)
        let mut grid_large = nuotc::CellGrid::new(80, 50);
        let mut damage_large = nuotc::Damage::new(50);
        let mut frame_large = Frame::new(&mut grid_large, &mut damage_large);
        let area_large = Rect::new(0, 0, 80, 50);
        let mut layout_map_large = LayoutMap::default();
        let mut skip_rows_large = 0;
        let mut current_y_large = 0;
        let mut content_lines_large = 0;

        draw_notice_view(
            &mut frame_large,
            area_large,
            NoticeProps { message: &msg },
            0,
            &mut layout_map_large,
            &mut skip_rows_large,
            &mut current_y_large,
            &mut content_lines_large,
            &theme,
            false,
            false,
        );

        // 2. Measure with tiny viewport (clipped at 3 rows)
        let mut grid_small = nuotc::CellGrid::new(80, 3);
        let mut damage_small = nuotc::Damage::new(3);
        let mut frame_small = Frame::new(&mut grid_small, &mut damage_small);
        let area_small = Rect::new(0, 0, 80, 3);
        let mut layout_map_small = LayoutMap::default();
        let mut skip_rows_small = 0;
        let mut current_y_small = 0;
        let mut content_lines_small = 0;

        draw_notice_view(
            &mut frame_small,
            area_small,
            NoticeProps { message: &msg },
            0,
            &mut layout_map_small,
            &mut skip_rows_small,
            &mut current_y_small,
            &mut content_lines_small,
            &theme,
            false,
            false,
        );

        // 3. Measure with scrolled viewport (skip_rows > 0)
        let mut grid_scrolled = nuotc::CellGrid::new(80, 3);
        let mut damage_scrolled = nuotc::Damage::new(3);
        let mut frame_scrolled = Frame::new(&mut grid_scrolled, &mut damage_scrolled);
        let area_scrolled = Rect::new(0, 0, 80, 3);
        let mut layout_map_scrolled = LayoutMap::default();
        let mut skip_rows_scrolled = 4;
        let mut current_y_scrolled = 0;
        let mut content_lines_scrolled = 0;

        draw_notice_view(
            &mut frame_scrolled,
            area_scrolled,
            NoticeProps { message: &msg },
            0,
            &mut layout_map_scrolled,
            &mut skip_rows_scrolled,
            &mut current_y_scrolled,
            &mut content_lines_scrolled,
            &theme,
            false,
            false,
        );

        // Logical content_lines MUST be identical regardless of viewport height or scroll offset!
        assert!(content_lines_large > 3);
        assert_eq!(content_lines_large, content_lines_small);
        assert_eq!(content_lines_large, content_lines_scrolled);
    }

    #[test]
    fn provider_retry_renders_as_notice_entry_with_countdown_and_failure() {
        let retry_at = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut msg =
            TranscriptMessage::provider_retry(2, 5, retry_at, "Anthropic HTTP 529: Overloaded");
        assert!(msg.is_provider_retry());
        assert!(msg.is_notice());

        let mut grid = nuotc::CellGrid::new(80, 20);
        let mut damage = nuotc::Damage::new(20);
        let mut frame = Frame::new(&mut grid, &mut damage);
        let area = Rect::new(0, 0, 80, 20);
        let mut layout_map = LayoutMap::default();
        let mut skip_rows = 0;
        let mut current_y = 0;
        let mut content_lines = 0;
        let theme = Theme::default();

        draw_notice_view(
            &mut frame,
            area,
            NoticeProps { message: &msg },
            0,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &theme,
            false,
            false,
        );

        assert!(content_lines >= 3);
        assert!(current_y >= 3);

        // Verify in-place update
        let new_retry_at = std::time::Instant::now() + std::time::Duration::from_secs(5);
        msg.update_provider_retry(3, 5, new_retry_at, "OpenAI HTTP 429: Rate limit exceeded");
        if let MessageKind::ProviderRetry {
            attempt, failure, ..
        } = &msg.kind
        {
            assert_eq!(*attempt, 3);
            assert_eq!(failure, "OpenAI HTTP 429: Rate limit exceeded");
        } else {
            panic!("Expected MessageKind::ProviderRetry");
        }
    }
}
