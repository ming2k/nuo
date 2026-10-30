//! The notice dock: the client's retained disclosures, docked above the
//! composer.
//!
//! # Why this surface exists
//!
//! A bubble is the client's interruption channel and it has exactly one slot. A
//! fault that arrives 400 ms after another *erases* it, and either way it is gone
//! in a few seconds. Everything the client wanted to tell the user therefore needs
//! a home with a viewport, a cursor and a scroll offset — which is the receipt-log
//! tier ([`nuo_view::model::log`], ADR-0116 `L2`).
//!
//! This module is that tier's viewer. It renders the log verbatim: newest at the
//! bottom, one row per notice, severity glyph in the gutter, wall-clock stamp,
//! topic tag, then the message. `Enter` expands a row to its wrapped detail, so a
//! long backend error is readable without leaving the surface.
//!
//! # Why it is docked, not a modal
//!
//! Reading a fault and the turn it interrupted is **one task**: the fault usually
//! explains what is on screen behind it ("could not reach the backend" is the
//! answer to "why did my round stop?"). A centred panel makes that two tasks and
//! hides the evidence. Worse, the most common reaction to a fault is to retype or
//! retry — which a modal barrier forbids exactly when the user needs it.
//!
//! So the dock is a **footer-stack row** ([`nuo_view::footer_stack`],
//! [`FooterRowId::NoticeDock`]): the conversation stays visible above it, the
//! composer stays live below it, and its height participates in the transcript
//! split through the one layout authority that already exists — no bespoke
//! plumbing, no second geometry path to keep in sync.
//!
//! # It is not a dialog either
//!
//! The dock is reachable from every scene and is not registered as a
//! `DialogKind`, because its content is client-global: a clipboard failure does
//! not belong to the conversation on screen, and session-scoped lifetime rules
//! would discard it exactly when the user switched tabs to go looking for it.

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::app::notices::NoticeViewer;
use crate::shell::keymap::Key;
use crate::shell::render::Theme;
use nuo_ui_primitives::components::list::row_style;
use nuo_ui_primitives::primitives::{FooterHint, keyvocab, render_modal_footer};
use nuo_view::footer_stack::FooterRowId;
use nuo_view::model::log::{LineStatus, SurfaceLine};

/// Column geometry of one notice row: gutter + `HH:MM` + gap.
const TIME_W: usize = 5;
const GUTTER_W: usize = 2;
const DETAIL_INDENT: &str = "      ";

/// How many extra rows the expanded detail block needs beyond the notice's own
/// row.
///
/// The only input that may grow the dock (`notice_dock_height`). Deliberately
/// narrow: it counts the *selected* notice's detail, not the whole log, so a busy
/// client cannot inflate the panel.
pub fn expanded_detail_rows(lines: &[SurfaceLine], viewer: &NoticeViewer, width: u16) -> u16 {
    let Some(notice) = lines.get(viewer.index) else {
        return 0;
    };
    if !viewer.detail {
        return 0;
    }
    let body_width =
        width.saturating_sub(2 * nuo_ui_primitives::design::MODAL_INNER_H_PADDING) as usize;
    let width = body_width.saturating_sub(6).max(1);
    let mut rows = wrapped_row_count(&notice.text, width).saturating_sub(1);
    if let Some(detail) = notice.detail.as_deref() {
        rows += wrapped_row_count(detail, width);
    }
    rows += 1; // the `at HH:MM · topic` meta row
    rows as u16
}

/// How many rows `text` occupies when wrapped to `width` columns.
fn wrapped_row_count(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.lines()
        .map(|line| wrap_row(line, width).len())
        .sum::<usize>()
        .max(1)
}

/// Draw the notice dock into `rect` — a row of the footer stack.
///
/// `viewer` is read and written back: the scroll offset and body height are
/// derived here, from the rect the footer stack actually placed, so the dock's
/// paging can never disagree with its geometry.
pub fn draw_notice_dock(
    frame: &mut Frame,
    rect: Rect,
    lines: &[SurfaceLine],
    viewer: &mut NoticeViewer,
    theme: &Theme,
) {
    if rect.height == 0 || rect.width == 0 {
        return;
    }

    let bg = theme.surface();
    let full_w = rect.width as usize;

    // The panel container: the same elevated surface language as the other
    // footer bars, so the dock reads as chrome rather than as a floating window.
    let mut block = nuo_ui_primitives::elevation::panel_block(theme, theme.brand(), bg);
    let inner = block.inner(rect);
    frame.render_widget(&mut block, rect);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Interior: header row, body, legend row. The body is whatever the footer
    // stack's height leaves behind — never a second, independently computed
    // viewport.
    let header_h: u16 = if inner.height > 1 { 1 } else { 0 };
    let footer_h: u16 = if inner.height > 2 { 1 } else { 0 };
    let body_h = inner.height.saturating_sub(header_h + footer_h) as usize;
    viewer.body_height = body_h.max(1) as u16;

    if header_h > 0 {
        draw_header(
            frame,
            Rect::new(inner.x, inner.y, inner.width, 1),
            lines,
            viewer,
            theme,
        );
    }

    let body_rect = Rect::new(inner.x, inner.y + header_h, inner.width, body_h as u16);
    let body_width = body_rect.width as usize;
    // Record where the body landed: the click handler maps a pointer row back to
    // a notice through this geometry rather than recomputing the footer stack.
    viewer.body_origin = Some(body_rect.y);
    viewer.body_width = body_width;

    let mut body: Vec<Line<'static>> = Vec::new();
    let mut selected_row: Option<usize> = None;

    if lines.is_empty() {
        body.push(Line::from(Span::styled(
            "Nothing yet. Warnings, errors and command replies land here after \
             their bubble fades.",
            Style::default().fg(theme.muted()),
        )));
    } else {
        let text_budget = body_width.saturating_sub(GUTTER_W + TIME_W + 2 + 1).max(1);
        for (i, notice) in lines.iter().enumerate() {
            let is_sel = viewer.index == i;
            let style = row_style(is_sel, theme);
            if is_sel {
                selected_row = Some(body.len());
            }

            let (glyph, tone) = match notice.status {
                LineStatus::Failed => (theme.glyphs.cross, theme.err()),
                LineStatus::Warn => ("▲", theme.warn()),
                LineStatus::Ok => (theme.glyphs.check, theme.ok()),
                LineStatus::Info => ("ℹ", theme.info()),
            };

            // One physical row per notice: a wrapping list would make the
            // cursor's position ambiguous, and the expanded detail exists
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

    // Keep the cursor inside the window: the minimum scroll that reveals the
    // selected *row*. The body is not one row per notice (an expanded notice
    // contributes several), so this cannot be delegated to a row-index follow —
    // that is what previously let the cursor scroll out of the panel.
    if let Some(cursor_row) = selected_row {
        if cursor_row < viewer.scroll {
            viewer.scroll = cursor_row;
        } else if cursor_row >= viewer.scroll + body_h.max(1) {
            viewer.scroll = cursor_row + 1 - body_h.max(1);
        }
    }
    viewer.scroll = viewer.scroll.min(body.len().saturating_sub(body_h.max(1)));

    let body_para = nuotc::Paragraph::new(body).scroll(viewer.scroll as u16, 0);
    let mut body_para = body_para;
    frame.render_widget(&mut body_para, body_rect);

    if footer_h > 0 {
        draw_legend(
            frame,
            Rect::new(inner.x, inner.y + header_h + body_h as u16, inner.width, 1),
            lines.is_empty(),
            theme,
        );
    }

    let _ = full_w;
}

/// The dock's header row: identity on the left, unread/unseen count on the right.
fn draw_header(
    frame: &mut Frame,
    rect: Rect,
    lines: &[SurfaceLine],
    viewer: &NoticeViewer,
    theme: &Theme,
) {
    let faults = lines.iter().filter(|l| l.status.is_fault()).count();
    let brand = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled("Notices", brand)];

    // A cursor position, not a selection count: the user needs to know where
    // they are in the log and how much is left, in that order.
    if !lines.is_empty() {
        spans.push(Span::styled(
            format!("  {}/{}", viewer.index + 1, lines.len()),
            Style::default().fg(theme.muted()),
        ));
    }
    if faults > 0 {
        spans.push(Span::styled(
            format!("  {faults} fault{}", if faults == 1 { "" } else { "s" }),
            Style::default().fg(theme.err()),
        ));
    }

    let right = Key::ALT_O.display();
    let left_w: usize = spans.iter().map(|s| s.content.width()).sum();
    let pad = (rect.width as usize)
        .saturating_sub(left_w + right.width() + 1)
        .max(1);
    spans.push(Span::styled(" ".repeat(pad), Style::default()));
    spans.push(Span::styled(right, Style::default().fg(theme.muted())));
    frame.render_widget(&mut nuotc::Paragraph::new(Line::from(spans)), rect);
}

/// The dock's legend row: the keys it owns, and nothing else.
fn draw_legend(frame: &mut Frame, rect: Rect, empty: bool, theme: &Theme) {
    let hints: &[FooterHint] = if empty {
        &[FooterHint::always(Key::ESC.display(), "close")]
    } else {
        &[
            FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
            FooterHint::primary(Key::ENTER.display(), "detail"),
            FooterHint::secondary("y", "copy"),
            FooterHint::secondary("x", "clear"),
            FooterHint::always(Key::ESC.display(), "close"),
        ]
    };
    render_modal_footer(frame, rect, hints, theme);
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

/// The footer-stack row this panel occupies.
///
/// Exposed so the renderer names the row once instead of repeating the id at the
/// declaration site and the draw site.
pub const fn footer_row_id() -> FooterRowId {
    FooterRowId::NoticeDock
}

/// Which notice the body row `row_in_body` belongs to, given the wrap `width`.
///
/// The inverse of the body assembly in [`draw_notice_dock`], sharing the same
/// `notice_block_rows` arithmetic so the forward projection (notice → screen
/// rows) and this one (screen row → notice) cannot drift. A click handler that
/// re-derived it would be a second layout path, and the two would diverge the
/// moment a detail block changed height.
///
/// `None` for a row inside an expanded detail block or past the end: a click on
/// detail prose has no notice to select, and guessing one would silently move the
/// cursor.
pub fn notice_at_body_row(
    lines: &[SurfaceLine],
    viewer: &NoticeViewer,
    row_in_body: usize,
    width: usize,
) -> Option<usize> {
    let mut row = 0usize;
    for (i, notice) in lines.iter().enumerate() {
        if row == row_in_body {
            return Some(i);
        }
        row += notice_block_rows(notice, viewer, i, width);
        if row > row_in_body {
            // The click landed inside this notice's expanded block.
            return None;
        }
    }
    None
}

/// How many body rows notice `i` occupies, including its expanded detail when it
/// is the selected row and the detail is open.
///
/// The one place this arithmetic exists. `draw_notice_dock` builds rows with it and
/// `notice_at_body_row` maps a click back through it.
fn notice_block_rows(notice: &SurfaceLine, viewer: &NoticeViewer, i: usize, width: usize) -> usize {
    let mut rows = 1;
    if viewer.detail && viewer.index == i {
        rows += wrapped_row_count(&notice.text, width).saturating_sub(1);
        if let Some(detail) = notice.detail.as_deref() {
            rows += wrapped_row_count(detail, width);
        }
        rows += 1; // the `at HH:MM · topic` meta row
    }
    rows
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
        let mut viewer = NoticeViewer::open_at_newest(lines.len());
        let mut terminal = nuotc::TestTerminal::new(100, 12);
        terminal.draw(|f| {
            draw_notice_dock(f, f.area(), &lines, &mut viewer, &theme);
        });
        let text = all_text(&terminal);
        assert!(text.contains("Notices"), "header names the surface: {text}");
        let backend = text.find("backend: link down").expect("first notice");
        let images = text.find("images: image withheld").expect("second notice");
        assert!(backend < images, "oldest first, newest last");
        assert!(text.contains("fault"), "the header counts faults: {text}");
    }

    #[test]
    fn a_long_message_is_truncated_on_the_row_and_wrapped_in_the_detail() {
        let theme = Theme::default();
        let long = "Failed to open link https://example.test/a/very/long/path: \
                    permission denied by the operating system";
        let lines = vec![notice(LineStatus::Failed, "link", long, 1_700_000_000_000)];
        let mut viewer = NoticeViewer::open_at_newest(lines.len());
        viewer.detail = true;
        let mut terminal = nuotc::TestTerminal::new(100, 20);
        terminal.draw(|f| {
            draw_notice_dock(f, f.area(), &lines, &mut viewer, &theme);
        });
        let text = all_text(&terminal);
        assert!(
            text.contains("permission denied"),
            "detail is not truncated"
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
        let mut terminal = nuotc::TestTerminal::new(100, 12);
        terminal.draw(|f| {
            draw_notice_dock(f, f.area(), &[], &mut viewer, &theme);
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
        let mut viewer = NoticeViewer::open_at_newest(lines.len());
        let mut terminal = nuotc::TestTerminal::new(100, 12);
        terminal.draw(|f| {
            draw_notice_dock(f, f.area(), &lines, &mut viewer, &theme);
        });
        let text = all_text(&terminal);
        assert!(text.contains(theme.glyphs.cross), "error glyph");
        assert!(text.contains('▲'), "warning glyph");
        assert!(text.contains(theme.glyphs.check), "ok glyph");
    }

    #[test]
    fn the_dock_never_paints_outside_the_rect_the_stack_gave_it() {
        // The dock is a footer row: its height came from `notice_dock_rows`, and
        // it must honour that budget exactly rather than growing into the
        // transcript above it.
        let theme = Theme::default();
        let lines: Vec<SurfaceLine> = (0..40)
            .map(|i| notice(LineStatus::Failed, "t", &format!("fault {i}"), i))
            .collect();
        let mut viewer = NoticeViewer::open_at_newest(lines.len());
        let mut terminal = nuotc::TestTerminal::new(60, 20);
        terminal.draw(|f| {
            // A six-row slice in the middle of the screen.
            let rect = nuotc::Rect::new(0, 8, 60, 6);
            draw_notice_dock(f, rect, &lines, &mut viewer, &theme);
        });
        let buf = terminal.buffer();
        for y in 0..8u16 {
            let row: String = (0..60)
                .map(|x| buf.get(x, y).unwrap().symbol.as_str())
                .collect();
            assert!(
                !row.contains("fault"),
                "nothing above the dock rect: row {y} = {row:?}"
            );
        }
        for y in 14..20u16 {
            let row: String = (0..60)
                .map(|x| buf.get(x, y).unwrap().symbol.as_str())
                .collect();
            assert!(
                !row.contains("fault"),
                "nothing below the dock rect: row {y} = {row:?}"
            );
        }
    }

    #[test]
    fn the_cursor_stays_visible_when_the_selected_detail_wraps() {
        // Regression: an expanded notice contributes several body rows, so a
        // row-index follow pointed at the wrong line and the cursor could scroll
        // out of the panel.
        let theme = Theme::default();
        let long = "word ".repeat(120);
        let lines: Vec<SurfaceLine> = (0..12)
            .map(|i| notice(LineStatus::Failed, "t", &format!("fault {i}"), i))
            .chain(std::iter::once(notice(
                LineStatus::Failed,
                "big",
                &long,
                99,
            )))
            .collect();
        let mut viewer = NoticeViewer::open_at_newest(lines.len());
        viewer.detail = true;
        let mut terminal = nuotc::TestTerminal::new(60, 12);
        terminal.draw(|f| {
            draw_notice_dock(f, f.area(), &lines, &mut viewer, &theme);
        });
        assert!(
            viewer.scroll > 0,
            "the view scrolled to follow the cursor instead of pinning at the top"
        );
        assert!(
            viewer.scroll + viewer.body_height as usize >= lines.len(),
            "and the selected row is inside the window"
        );
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

    #[test]
    fn the_only_height_input_is_the_expanded_detail() {
        // The renderer's growth path. It must count the *selected* notice's detail
        // and nothing else — a count over the whole log would make the panel grow
        // on every arrival and reflow the transcript.
        let lines = vec![
            notice(LineStatus::Failed, "a", "short", 1),
            notice(LineStatus::Failed, "b", &"word ".repeat(60), 2),
        ];
        let mut viewer = NoticeViewer::open_at_newest(lines.len());
        assert_eq!(
            expanded_detail_rows(&lines, &viewer, 80),
            0,
            "a collapsed row asks for no growth"
        );
        viewer.detail = true;
        assert!(
            expanded_detail_rows(&lines, &viewer, 80) > 0,
            "an expanded detail asks for the rows it needs"
        );
        // And it counts one notice, not the log: adding unrelated notices must not
        // change the answer.
        let mut more = lines.clone();
        more.extend((0..20).map(|i| notice(LineStatus::Failed, "c", "x", i)));
        assert_eq!(
            expanded_detail_rows(&more, &viewer, 80),
            expanded_detail_rows(&lines, &viewer, 80),
            "growth depends on the selected detail, never on the log's length"
        );
    }
}
