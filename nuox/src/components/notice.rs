//! Transcript notice component (System / Infrastructure notices as top-level transcript entries).

use nuotc::{
    Color, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::design::{TRANSCRIPT_BODY_LEADING_INDENT, TURN_HEADER_BODY_GAP_ROWS};
use crate::model::document::{MessageKind, NoticeParts, NoticeSeverity, TranscriptMessage};
use crate::model::layout::{BlockRegion, LayoutMap, NOTICE_BLOCK_IDX};
use crate::text_layout::wrap_text;

use super::super::Theme;

pub(crate) struct NoticeProps<'a> {
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

    // 2. Check for JSON payload
    if let Some(pos) = clean.find('{') {
        let prefix = clean[..pos].trim_end_matches([':', ' ', '\n', '\t', '\r']);
        let json_candidate = clean[pos..].trim();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_candidate) {
            let pretty_json =
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| json_candidate.to_string());
            let header = if prefix.is_empty() {
                "Provider HTTP Error".to_string()
            } else {
                prefix.to_string()
            };
            return NoticeContent {
                header,
                detail: Some(pretty_json),
            };
        }
    }

    // 3. Check for multi-line non-JSON detail
    if let Some((first_line, rest)) = clean.split_once('\n') {
        let rest_trimmed = rest.trim();
        if !rest_trimmed.is_empty() {
            return NoticeContent {
                header: first_line.trim().to_string(),
                detail: Some(rest_trimmed.to_string()),
            };
        }
    }

    // 4. Single-line plain message
    NoticeContent {
        header: clean.to_string(),
        detail: None,
    }
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
pub(crate) fn draw_notice_view(
    frame: &mut Frame,
    area: Rect,
    notice: NoticeProps<'_>,
    mi: usize,
    layout_map: &mut LayoutMap,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
    theme: &Theme,
    _hovered: bool,
    _focused: bool,
) {
    let Some(severity) = notice.severity() else {
        return;
    };
    let (lead_symbol, tag_color) = match severity {
        NoticeSeverity::Error => ("! ", theme.err()),
        NoticeSeverity::Warning => ("▲ ", theme.warn()),
        NoticeSeverity::Info => ("ℹ ", theme.info()),
    };
    let parsed = parse_notice_content(notice.raw_text());
    let mut dynamic_title = None;
    let mut dynamic_detail = None;
    let (topic, title, detail) =
        notice_content(&notice, &parsed, &mut dynamic_title, &mut dynamic_detail);
    let full_width = area.width as usize;
    let time_label = notice.message.sent_at_ms.map(crate::time::sent_time_label);

    // 1. Notification Entry Header
    let header_line = notice_header_line(
        lead_symbol,
        topic,
        tag_color,
        time_label.as_deref(),
        theme.muted(),
        full_width,
    );

    *content_lines += 1;
    if *skip_rows > 0 {
        *skip_rows -= 1;
    } else if *current_y < area.y + area.height {
        let line_rect = Rect::new(area.x, *current_y, area.width, 1);
        frame.render_widget(Paragraph::new(header_line), line_rect);

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
            frame.render_widget(Paragraph::new(Line::from(spans)), line_rect);

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
                    frame.render_widget(Paragraph::new(Line::from(spans)), line_rect);

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
        assert_eq!(parsed.header, "OpenAI HTTP 429 Too Many Requests");
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
            "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests"
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
    fn draw_notice_view_renders_as_entry_with_header_gap_and_body() {
        let theme = Theme::default();
        let mut grid = nuotc::Grid::new(60, 10);
        let mut frame = Frame::new(&mut grid);
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
        let buf = frame.buffer_mut();
        let mut row0 = String::new();
        for x in 0..20 {
            row0.push_str(buf[(x, 0)].symbol());
        }
        assert!(row0.starts_with("! notification"));

        // Verify row 2 contains "Connection refused", indented
        // TRANSCRIPT_BODY_LEADING_INDENT (2) past the entry head.
        let mut row2 = String::new();
        for x in 0..25 {
            row2.push_str(buf[(x, 2)].symbol());
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
        let mut grid = nuotc::Grid::new(60, 10);
        let mut frame = Frame::new(&mut grid);
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

        let buf = frame.buffer_mut();
        let row = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..40 {
                s.push_str(buf[(x, y)].symbol());
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
        // kind (ADR-0155).
        let core = nuo_contracts::AgentNotice::trust_changed("Workspace configurations changed")
            .with_body("Changed on disk: rules (AGENTS.md / rules) — quarantined pending review.");
        let msg = TranscriptMessage::notice_from_core(&core);

        let theme = Theme::default();
        let mut grid = nuotc::Grid::new(60, 10);
        let mut frame = Frame::new(&mut grid);
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

        let buf = frame.buffer_mut();
        let row = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..58 {
                s.push_str(buf[(x, y)].symbol());
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
        let mut grid_large = nuotc::Grid::new(80, 50);
        let mut frame_large = Frame::new(&mut grid_large);
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
        let mut grid_small = nuotc::Grid::new(80, 3);
        let mut frame_small = Frame::new(&mut grid_small);
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
        let mut grid_scrolled = nuotc::Grid::new(80, 3);
        let mut frame_scrolled = Frame::new(&mut grid_scrolled);
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

        let mut grid = nuotc::Grid::new(80, 20);
        let mut frame = Frame::new(&mut grid);
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
