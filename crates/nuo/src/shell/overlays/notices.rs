//! The notice viewer: the client's retained disclosures, addressable.
//!
//! # Why this surface exists
//!
//! A bubble is the client's interruption channel and it has exactly one slot.
//! A fault that arrives 400ms after another *erases* it, and either way it is
//! gone in about three seconds. Everything the client wanted to tell the user
//! therefore needs a home with a viewport, a cursor and a scroll offset — which
//! is the receipt-log tier ([`nuo_view::model::log`], ADR-0116 `L2`).
//!
//! This module is that tier's viewer. It renders the log verbatim: newest at the
//! bottom, one row per notice, severity glyph in the gutter, wall-clock stamp,
//! topic tag, then the message. `Enter` expands a row to its wrapped detail, so
//! a long backend error is readable without leaving the surface.
//!
//! # It is not a dialog
//!
//! The viewer is reachable from every scene and is not registered as a
//! `DialogKind`, because its content is client-global: a clipboard failure does
//! not belong to the conversation on screen, and session-scoped lifetime rules
//! would discard it exactly when the user switched tabs to go looking for it.

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::app::notices::NoticeViewer;
use crate::shell::keymap::Key;
use crate::shell::render::Theme;
use nuo_ui_primitives::components::list::{
    SelectableListPage, draw_selectable_list_page, row_style,
};
use nuo_ui_primitives::components::modal::ModalHeader;
use nuo_ui_primitives::primitives::{ContentModalSpec, FooterHint, keyvocab};
use nuo_view::model::log::{LineStatus, SurfaceLine};

/// Column geometry of one notice row: gutter + `HH:MM` + gap.
const TIME_W: usize = 5;
const GUTTER_W: usize = 2;
const DETAIL_INDENT: &str = "      ";

/// Draw the notice viewer over `lines` (oldest first).
///
/// `viewer` is read and written back so the caller's scroll offset stays
/// consistent with the clamped body height.
pub fn draw_notice_viewer(
    frame: &mut Frame,
    lines: &[SurfaceLine],
    viewer: &mut NoticeViewer,
    theme: &Theme,
) -> Rect {
    let geometry = ContentModalSpec::NOTICES;
    let body_width = nuo_ui_primitives::components::modal::modal_body_width(frame, geometry);

    let mut body: Vec<Line<'static>> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if lines.is_empty() {
        body.push(Line::from(Span::styled(
            "Nothing yet. Warnings, errors and command replies land here after \
             their bubble fades.",
            Style::default().fg(theme.muted()),
        )));
    } else {
        let text_budget = body_width.saturating_sub(GUTTER_W + TIME_W + 2).max(1);

        for (i, notice) in lines.iter().enumerate() {
            let is_sel = viewer.index == i;
            let style = row_style(is_sel, theme);
            if is_sel {
                selected_line = Some(body.len());
            }

            let (glyph, tone) = match notice.status {
                LineStatus::Failed => (theme.glyphs.cross, theme.err()),
                LineStatus::Warn => ("▲", theme.warn()),
                LineStatus::Ok => (theme.glyphs.check, theme.ok()),
                LineStatus::Info => ("ℹ", theme.info()),
            };

            // One physical row per notice: a notice list that wraps would make
            // the cursor's position ambiguous, and the detail view exists
            // precisely to hold the wrapped form.
            let time = nuo_ui_primitives::time::sent_time_label(notice.at_ms);
            let topic = notice.label.as_deref().unwrap_or("");
            let head = if topic.is_empty() {
                one_line(&notice.text)
            } else {
                format!("{topic}: {}", one_line(&notice.text))
            };
            let text = truncate_ellipsis(&head, text_budget);
            let pad =
                body_width.saturating_sub(GUTTER_W + TIME_W + 2 + glyph.width() + 1 + text.width());

            body.push(Line::from(vec![
                Span::styled(" ".repeat(GUTTER_W), Style::default().bg(style.bg)),
                Span::styled(
                    format!("{time}  "),
                    Style::default().bg(style.bg).fg(style.dim),
                ),
                Span::styled(format!("{glyph} "), Style::default().bg(style.bg).fg(tone)),
                Span::styled(text, Style::default().bg(style.bg).fg(style.fg)),
                Span::styled(" ".repeat(pad), Style::default().bg(style.bg)),
            ]));

            // Detail: the full, wrapped message plus its topic and exact time —
            // the part a truncated row cannot carry. When the line also carries
            // a structured detail (a retry receipt's HTTP body, a diagnostic's
            // context), that is rendered too: it is the reason this view exists,
            // and a field nothing paints is a field nothing can read.
            if is_sel && viewer.detail {
                let detail_style = Style::default().fg(theme.fg());
                let label_style = Style::default().fg(theme.muted());
                for line in notice.text.lines() {
                    for wrapped in wrap_row(line, body_width.saturating_sub(6).max(1)) {
                        body.push(Line::from(Span::styled(
                            format!("{DETAIL_INDENT}{wrapped}"),
                            detail_style,
                        )));
                    }
                }
                if let Some(detail) = notice.detail.as_deref() {
                    for line in detail.lines() {
                        for wrapped in wrap_row(line, body_width.saturating_sub(6).max(1)) {
                            body.push(Line::from(Span::styled(
                                format!("{DETAIL_INDENT}{wrapped}"),
                                label_style,
                            )));
                        }
                    }
                }
                let mut meta = format!("{DETAIL_INDENT}at {time}");
                if !topic.is_empty() {
                    meta.push_str(&format!(" · topic {topic}"));
                }
                body.push(Line::from(Span::styled(meta, label_style)));
            }
        }
    }

    let has_items = !lines.is_empty();

    // The viewport is computed *before* painting, because the cursor's
    // visibility depends on it and the generic list page cannot help here: its
    // `follow` mechanism scrolls to a *row index*, and this body's rows are not
    // one-per-notice (an expanded notice contributes several). Driving the
    // offset from the measured viewport is the only version that keeps the
    // cursor on screen after `Enter` grows the selected row.
    let chrome = nuo_ui_primitives::primitives::modal_chrome_rows(geometry.modal_spec());
    let area = nuo_ui_primitives::primitives::content_modal_area(
        frame,
        geometry,
        body.len() as u16 + chrome,
    );
    let body_height = area.height.saturating_sub(chrome).max(1) as usize;
    viewer.body_height = body_height as u16;

    // Keep the cursor inside the window: scroll the minimum needed, so a
    // single `↓` at the bottom edge moves the view by one row rather than
    // recentering (recentering makes a list feel like it is jumping).
    if let Some(cursor_row) = selected_line {
        if cursor_row < viewer.scroll {
            viewer.scroll = cursor_row;
        } else if cursor_row >= viewer.scroll + body_height {
            viewer.scroll = cursor_row + 1 - body_height;
        }
    }
    let total = body.len();
    viewer.scroll = viewer.scroll.min(total.saturating_sub(body_height));

    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry,
            header: ModalHeader::title("Notices"),
            lines: body,
            scroll: &mut viewer.scroll,
            selected_line,
            // The offset is already correct; `follow` would re-derive it from a
            // row index that does not correspond to the notice cursor.
            follow_selection: false,
            has_items,
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(Key::ENTER.display(), "detail"),
                FooterHint::always(Key::ESC.display(), "close"),
                FooterHint::secondary("y", "copy all"),
                FooterHint::secondary("x", "clear"),
            ],
            empty_footer_hints: &[FooterHint::always(Key::ESC.display(), "close")],
            extra_footer_hints: &[],
        },
        theme,
    );
    area
}

/// Collapse every line break and run of whitespace into single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` display columns, appending the theme's ellipsis.
fn truncate_ellipsis(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    let ellipsis = "…";
    let budget = max.saturating_sub(ellipsis.width());
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.to_string().width();
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push_str(ellipsis);
    out
}

/// Greedy word wrap to `width` display columns, for the expanded detail body.
fn wrap_row(text: &str, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate_w = if current.is_empty() {
            word.width()
        } else {
            current.width() + 1 + word.width()
        };
        if candidate_w > width && !current.is_empty() {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// The severity a row paints, for tests and for the header badge.
pub fn is_fault(notice: &SurfaceLine) -> bool {
    notice.status.is_fault()
}

/// The bold header a copy of the whole log opens with.
pub fn copy_header(count: usize) -> Span<'static> {
    Span::styled(
        format!("{count} retained"),
        Style::default().add_modifier(Modifier::BOLD),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_view::model::log::SurfaceLine;

    fn all_text(terminal: &nuotc::TestTerminal) -> String {
        terminal
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect()
    }

    fn notice(status: LineStatus, topic: &str, text: &str, at_ms: u64) -> SurfaceLine {
        SurfaceLine::disclosure(status, text, at_ms).with_label(topic)
    }

    #[test]
    fn rows_carry_time_topic_and_message_newest_last() {
        let theme = Theme::default();
        let lines = vec![
            notice(
                LineStatus::Failed,
                "backend",
                "link down",
                1_700_000_000_000,
            ),
            notice(
                LineStatus::Warn,
                "images",
                "image withheld",
                1_700_000_060_000,
            ),
        ];
        let mut viewer = NoticeViewer::open_at_newest(0);
        let mut terminal = nuotc::TestTerminal::new(100, 30);
        terminal.draw(|f| {
            draw_notice_viewer(f, &lines, &mut viewer, &theme);
        });
        let text = all_text(&terminal);
        assert!(text.contains("Notices"), "header names the surface: {text}");
        let backend = text.find("backend: link down").expect("first notice");
        let images = text.find("images: image withheld").expect("second notice");
        assert!(backend < images, "oldest first, newest last");
        assert!(text.contains(':'), "rows carry a HH:MM stamp");
    }

    #[test]
    fn a_long_message_is_truncated_on_the_row_and_wrapped_in_the_detail() {
        let theme = Theme::default();
        let long = "Failed to open link https://example.test/a/very/long/path: \
                    permission denied by the operating system";
        let lines = vec![notice(LineStatus::Failed, "link", long, 1_700_000_000_000)];
        let mut viewer = NoticeViewer::open_at_newest(lines.len());
        viewer.detail = true;
        let mut terminal = nuotc::TestTerminal::new(100, 30);
        terminal.draw(|f| {
            draw_notice_viewer(f, &lines, &mut viewer, &theme);
        });
        let text = all_text(&terminal);
        assert!(
            text.contains("permission denied"),
            "detail is not truncated"
        );
        assert!(
            text.contains("Failed to open link"),
            "the message survives expansion"
        );
        assert!(
            text.contains("topic link"),
            "the expanded row states its topic"
        );
    }

    #[test]
    fn an_empty_log_explains_itself_instead_of_showing_a_blank_panel() {
        let theme = Theme::default();
        let mut viewer = NoticeViewer::open_at_newest(0);
        let mut terminal = nuotc::TestTerminal::new(100, 30);
        terminal.draw(|f| {
            draw_notice_viewer(f, &[], &mut viewer, &theme);
        });
        let text = all_text(&terminal);
        assert!(text.contains("Nothing yet"), "{text}");
    }

    #[test]
    fn severity_glyphs_distinguish_failure_warning_and_acknowledgement() {
        let theme = Theme::default();
        let lines = vec![
            notice(LineStatus::Failed, "a", "boom", 1),
            notice(LineStatus::Warn, "b", "degraded", 2),
            notice(LineStatus::Ok, "c", "done", 3),
        ];
        let mut viewer = NoticeViewer::open_at_newest(0);
        let mut terminal = nuotc::TestTerminal::new(100, 30);
        terminal.draw(|f| {
            draw_notice_viewer(f, &lines, &mut viewer, &theme);
        });
        let text = all_text(&terminal);
        assert!(text.contains(theme.glyphs.cross), "error glyph");
        assert!(text.contains('▲'), "warning glyph");
        assert!(text.contains(theme.glyphs.check), "ok glyph");
        assert!(is_fault(&lines[0]));
        assert!(is_fault(&lines[1]));
        assert!(!is_fault(&lines[2]));
    }

    #[test]
    fn wrapping_respects_the_column_budget() {
        let rows = wrap_row("aaa bbb ccc ddd", 7);
        assert_eq!(rows, vec!["aaa bbb", "ccc ddd"]);
        assert!(rows.iter().all(|r| r.width() <= 7));
    }

    #[test]
    fn truncation_is_width_aware_not_byte_aware() {
        // A CJK message must not be cut mid-glyph or over-budget: the row is a
        // fixed-column surface.
        let out = truncate_ellipsis("后端连接失败，请检查网络配置", 8);
        assert!(out.width() <= 8, "{out:?}");
        assert!(out.ends_with('…'));
    }
}
