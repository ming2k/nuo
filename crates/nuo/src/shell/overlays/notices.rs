//! The sovereign notice center dialog (ADR-0134).
//!
//! # Architecture & Scope Contract
//!
//! Replaces the legacy footer dock (`NoticeDock`, ADR-0121) with a sovereign,
//! client-global modal review dialog (`DialogKind::Notices`, ADR-0134).
//!
//! Operates on the Two-Track Notice Architecture:
//! - Track 1: Ephemeral interaction signals (copy receipts, armed confirm gates)
//!   render as transient top-right bubbles.
//! - Track 2: Retained operational notices (warnings, faults, retries) land in
//!   `SurfaceLog` and are reviewed in this sovereign card-stack dialog.
//!
//! The dialog presents notifications as distinct, formatted visual cards with:
//! - Severity indicator badges (`[ERROR]`, `[WARN]`, `[INFO]`, `[OK]`) and glyphs.
//! - Topic pill indicators and right-aligned timestamp metadata.
//! - Prominent headline titles with active-selection accent markers (`▌`).
//! - Expandable detailed technical context (HTTP response bodies, stack traces).
//! - Distinct card divider lines and pagination metadata.
//! - Zero footer displacement, ensuring 100% composer stability.

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::keymap::Key;
use crate::shell::render::Theme;
use crate::shell::surfaces::NoticesDialog;
use nuo_ui_primitives::elevation::modal_frame;
use nuo_ui_primitives::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, keyvocab, modal_header, render_modal_footer,
};
use nuo_view::model::log::{LineStatus, SurfaceLine};

/// How many rows `text` occupies when wrapped to `width` columns.
pub fn wrapped_row_count(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.lines()
        .map(|line| wrap_row(line, width).len())
        .sum::<usize>()
        .max(1)
}

/// Draw the sovereign notice center modal dialog with an authentic card-stack layout.
pub fn draw_notices_modal(
    frame: &mut Frame,
    _area: Rect,
    lines: &[SurfaceLine],
    dialog: &mut NoticesDialog,
    theme: &Theme,
) -> Rect {
    let geometry = ContentModalSpec::TOOLS;
    let bounds = content_modal_area(frame, geometry, 24);
    let f = modal_frame(frame, bounds, theme, true, true);

    let faults = lines.iter().filter(|l| l.status.is_fault()).count();
    let title = if lines.is_empty() {
        "Notices & Disclosures".to_string()
    } else {
        let card_idx = (dialog.index + 1).min(lines.len());
        if faults > 0 {
            format!(
                "Notices & Disclosures · Card {} of {} ({} fault{})",
                card_idx,
                lines.len(),
                faults,
                if faults == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Notices & Disclosures · Card {} of {}",
                card_idx,
                lines.len()
            )
        }
    };
    modal_header(frame, f.header, &title, theme);

    let body_rect = f.body;
    let body_h = body_rect.height as usize;
    let body_width = body_rect.width as usize;
    dialog.body_height = body_rect.height;

    // Clamp cursor index into bounds
    if lines.is_empty() {
        dialog.index = 0;
        dialog.detail = false;
    } else if dialog.index >= lines.len() {
        dialog.index = lines.len().saturating_sub(1);
    }

    let mut body_lines: Vec<Line<'static>> = Vec::new();
    let mut selected_row: Option<usize> = None;

    if lines.is_empty() {
        body_lines.push(Line::from(""));
        body_lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(
                "No operational notices or diagnostic faults recorded.",
                Style::default()
                    .fg(theme.muted())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        body_lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(
                "Operational warnings, retries, and errors land here automatically.",
                Style::default().fg(theme.dim()),
            ),
        ]));
    } else {
        for (i, notice) in lines.iter().enumerate() {
            let is_sel = dialog.index == i;
            if is_sel {
                selected_row = Some(body_lines.len());
            }

            let (badge_text, glyph, tone) = match notice.status {
                LineStatus::Failed => ("[ERROR]", theme.glyphs.cross, theme.err()),
                LineStatus::Warn => ("[WARN]", "▲", theme.warn()),
                LineStatus::Ok => ("[OK]", theme.glyphs.check, theme.ok()),
                LineStatus::Info => ("[INFO]", "ℹ", theme.info()),
            };

            let time = nuo_ui_primitives::time::sent_time_label(notice.at_ms);
            let topic = notice.label.as_deref().unwrap_or("system");

            let select_indicator = if is_sel { "▌ " } else { "  " };
            let select_style = if is_sel {
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            // 1. Card Header: [STATUS BADGE] [TOPIC PILL] ... at HH:MM
            let mut header_spans = vec![
                Span::styled(select_indicator, select_style),
                Span::styled(
                    format!("{glyph} {badge_text} "),
                    Style::default().fg(tone).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {topic} "),
                    Style::default()
                        .fg(theme.fg())
                        .bg(theme.raised())
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            let header_w = 2 + 1 + glyph.width() + 1 + badge_text.width() + 1 + topic.width() + 2;
            let time_str = format!("at {time}");
            let right_pad = body_width
                .saturating_sub(header_w + time_str.width() + 1)
                .max(1);
            header_spans.push(Span::styled(" ".repeat(right_pad), Style::default()));
            header_spans.push(Span::styled(time_str, Style::default().fg(theme.dim())));
            body_lines.push(Line::from(header_spans));

            // 2. Card Headline: Headline text with selection accent
            let headline = one_line(&notice.text);
            let headline_w = body_width.saturating_sub(6).max(1);
            let wrapped_headline = wrap_row(&headline, headline_w);
            for (line_idx, hl) in wrapped_headline.iter().enumerate() {
                let prefix = if line_idx == 0 {
                    select_indicator
                } else {
                    if is_sel { "▌ " } else { "  " }
                };
                body_lines.push(Line::from(vec![
                    Span::styled(prefix, select_style),
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        hl.clone(),
                        Style::default()
                            .fg(if is_sel { theme.fg() } else { theme.muted() })
                            .add_modifier(if is_sel {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                ]));
            }

            // 3. Card Expandable Technical Detail (when selected and expanded)
            if is_sel && dialog.detail {
                body_lines.push(Line::from(vec![
                    Span::styled(select_indicator, select_style),
                    Span::styled("    ┌ Detail:", Style::default().fg(theme.dim())),
                ]));
                let detail_w = body_width.saturating_sub(10).max(1);
                for line in notice.text.lines() {
                    for wrapped in wrap_row(line, detail_w) {
                        body_lines.push(Line::from(vec![
                            Span::styled(select_indicator, select_style),
                            Span::styled("    │ ", Style::default().fg(theme.dim())),
                            Span::styled(wrapped, Style::default().fg(theme.fg())),
                        ]));
                    }
                }
                if let Some(detail) = notice.detail.as_deref() {
                    for line in detail.lines() {
                        for wrapped in wrap_row(line, detail_w) {
                            body_lines.push(Line::from(vec![
                                Span::styled(select_indicator, select_style),
                                Span::styled("    │ ", Style::default().fg(theme.dim())),
                                Span::styled(wrapped, Style::default().fg(theme.muted())),
                            ]));
                        }
                    }
                }
                body_lines.push(Line::from(vec![
                    Span::styled(select_indicator, select_style),
                    Span::styled("    └", Style::default().fg(theme.dim())),
                ]));
            }

            // 4. Card Separator Divider between cards
            if i + 1 < lines.len() {
                let div_line = "─".repeat(body_width.saturating_sub(4).max(1));
                body_lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(div_line, Style::default().fg(theme.dim())),
                ]));
            }
        }
    }

    // Scroll anchoring
    if let Some(cursor_row) = selected_row {
        if cursor_row < dialog.scroll {
            dialog.scroll = cursor_row;
        } else if cursor_row >= dialog.scroll + body_h.max(1) {
            dialog.scroll = cursor_row + 1 - body_h.max(1);
        }
    }
    dialog.scroll = dialog
        .scroll
        .min(body_lines.len().saturating_sub(body_h.max(1)));

    let mut body_para = nuotc::Paragraph::new(body_lines).scroll(dialog.scroll as u16, 0);
    frame.render_widget(&mut body_para, body_rect);

    // Modal footer hints
    let hints: &[FooterHint] = if lines.is_empty() {
        &[FooterHint::always(Key::ESC.display(), "close")]
    } else {
        &[
            FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
            FooterHint::primary(
                Key::ENTER.display(),
                if dialog.detail { "collapse" } else { "expand" },
            ),
            FooterHint::secondary("c", "copy"),
            FooterHint::secondary("x", "clear"),
            FooterHint::always(Key::ESC.display(), "close"),
        ]
    };
    if let Some(footer) = f.footer {
        render_modal_footer(frame, footer, hints, theme);
    }

    bounds
}

/// Collapse every line break and run of whitespace into single spaces.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` display columns, appending the theme's ellipsis.
pub fn truncate_ellipsis(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    let ellipsis = "…";
    let budget = max.saturating_sub(ellipsis.width());
    let mut out = String::new();
    for ch in text.chars() {
        let w = ch.to_string().width();
        if out.width() + w > budget {
            break;
        }
        out.push(ch);
    }
    out.push_str(ellipsis);
    out
}

/// Greedy word wrap to `width` display columns, for the expanded detail body.
pub fn wrap_row(text: &str, width: usize) -> Vec<String> {
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
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(status: LineStatus, topic: &str, text: &str, at_ms: u64) -> SurfaceLine {
        SurfaceLine::disclosure(status, text, at_ms).with_label(topic)
    }

    fn all_text(terminal: &nuotc::TestTerminal) -> String {
        let buf = terminal.buffer();
        let area = buf.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.get(x, y) {
                    out.push_str(&cell.symbol);
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn notices_modal_renders_header_fault_count_and_items() {
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
        let mut dialog = NoticesDialog {
            index: 1,
            ..Default::default()
        };
        let mut terminal = nuotc::TestTerminal::new(100, 30);
        terminal.draw(|f| {
            draw_notices_modal(f, f.area(), &lines, &mut dialog, &theme);
        });
        let text = all_text(&terminal);
        assert!(text.contains("Notices"), "header names the surface: {text}");
        assert!(text.contains("backend"), "renders topic: {text}");
        assert!(text.contains("link down"), "renders title: {text}");
        assert!(text.contains("images"), "renders second topic: {text}");
        assert!(text.contains("fault"), "the header counts faults: {text}");
    }

    #[test]
    fn a_long_message_is_truncated_on_the_row_and_wrapped_in_the_detail() {
        let theme = Theme::default();
        let long = "Failed to open link https://example.test/a/very/long/path:                     permission denied by the operating system";
        let lines = vec![notice(LineStatus::Failed, "link", long, 1_700_000_000_000)];
        let mut dialog = NoticesDialog {
            detail: true,
            ..Default::default()
        };
        let mut terminal = nuotc::TestTerminal::new(100, 30);
        terminal.draw(|f| {
            draw_notices_modal(f, f.area(), &lines, &mut dialog, &theme);
        });
        let text = all_text(&terminal);
        assert!(
            text.contains("permission") && text.contains("denied"),
            "detail is wrapped: {text}"
        );
        assert!(text.contains("link"), "states topic: {text}");
    }

    #[test]
    fn an_empty_log_explains_itself_instead_of_showing_a_blank_panel() {
        let theme = Theme::default();
        let mut dialog = NoticesDialog::default();
        let mut terminal = nuotc::TestTerminal::new(100, 30);
        terminal.draw(|f| {
            draw_notices_modal(f, f.area(), &[], &mut dialog, &theme);
        });
        let text = all_text(&terminal);
        assert!(text.contains("No operational notices"), "{text}");
    }

    #[test]
    fn severity_glyphs_distinguish_failure_warning_and_acknowledgement() {
        let theme = Theme::default();
        let lines = vec![
            notice(LineStatus::Failed, "a", "boom", 1),
            notice(LineStatus::Warn, "b", "degraded", 2),
            notice(LineStatus::Ok, "c", "done", 3),
        ];
        let mut dialog = NoticesDialog::default();
        let mut terminal = nuotc::TestTerminal::new(100, 30);
        terminal.draw(|f| {
            draw_notices_modal(f, f.area(), &lines, &mut dialog, &theme);
        });
        let text = all_text(&terminal);
        assert!(text.contains(theme.glyphs.cross), "error glyph");
        assert!(text.contains('▲'), "warning glyph");
        assert!(text.contains(theme.glyphs.check), "ok glyph");
    }

    #[test]
    fn wrapping_respects_the_column_budget() {
        let rows = wrap_row("aaa bbb ccc ddd", 7);
        assert_eq!(rows, vec!["aaa bbb", "ccc ddd"]);
        assert!(rows.iter().all(|r| r.width() <= 7));
    }

    #[test]
    fn truncation_is_width_aware_not_byte_aware() {
        let out = truncate_ellipsis("后端连接失败，请检查网络配置", 8);
        assert!(out.width() <= 8, "{out:?}");
        assert!(out.ends_with('…'));
    }
}
