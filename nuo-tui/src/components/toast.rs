//! Transient overlay toast bubbles.
//!
//! # Architecture & Scope Contract
//! Toast bubbles are ephemeral top-right overlays rendered at layer 3 above other chrome.
//!
//! ### Strictly Reserved For:
//! 1. Immediate action acknowledgments ("copied to clipboard", "input cleared").
//! 2. Two-step confirmation gates ("Esc again interrupts", "press Ctrl+C again to exit").
//! 3. Slash command acknowledgments (`CommandAck`, e.g. `/delegate on`) that must not pollute the transcript.
//!
//! ### Prohibited For Toasts:
//! Diagnostic errors, missing workspace assets, and initialization failures MUST route to
//! `NoticeSurface::Inline` (in the transcript timeline) rather than `Toast`.
//!
//! ### Modern Functional Minimalist Layout (Borderless Elevated Pill):
//! - **Clear Underlay**: Wipes underlying terminal cells (`frame.render_widget(Clear, area)`)
//!   to guarantee zero text bleed from background transcript content.
//! - **Elevated Floating Surface**: Renders on `theme.toast_bg()` which is distinctly lighter
//!   than the scene head background (`theme.raised()`), giving clear borderless pill elevation.
//! - **Borderless Clean Canvas**: Eliminates box lines and side decorations for visual consistency
//!   with the chromatic design language.
//! - **Structured Leading Gutter**:
//!   `[2 spaces][icon][2 spaces]` for row 1; secondary rows indent by `icon_w + 4` spaces to align
//!   directly under the text column without jagged margins.
//! - **Shrink-to-Fit Bounds**: Width tightly hugs wrapped lines, clamped between
//!   [`MIN_TOAST_WIDTH`] and [`MAX_TOAST_WIDTH`].

use nuotc::{
    Block as RtBlock, Clear, Color, Frame, Modifier, Paragraph, Rect, Span, {Line, Style},
};
use unicode_width::UnicodeWidthStr;

use crate::text_layout::wrap_text;

use super::super::Theme;

pub(crate) const MIN_TOAST_WIDTH: u16 = 18;
pub(crate) const MAX_TOAST_WIDTH: u16 = 60;
pub(crate) const MAX_TOAST_ROWS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastKind {
    CopyOk,
    CopyFailed,
    Armed,
    Info,
    Warning,
    Error,
}

impl ToastKind {
    pub(crate) fn color(&self, theme: &Theme) -> Color {
        match *self {
            ToastKind::CopyOk => theme.ok(),
            ToastKind::CopyFailed | ToastKind::Error => theme.err(),
            ToastKind::Armed | ToastKind::Warning => theme.warn(),
            ToastKind::Info => theme.info(),
        }
    }

    pub(crate) fn glyph(&self, theme: &Theme) -> &'static str {
        let is_ascii = theme.glyphs.border_v == "|";
        match *self {
            ToastKind::CopyOk => theme.glyphs.check,
            ToastKind::CopyFailed | ToastKind::Error => theme.glyphs.cross,
            ToastKind::Armed | ToastKind::Warning => {
                if is_ascii {
                    "[!]"
                } else {
                    "▲"
                }
            }
            ToastKind::Info => {
                if is_ascii {
                    "[i]"
                } else {
                    "ℹ"
                }
            }
        }
    }
}

pub(crate) struct ToastBubble<'a> {
    pub message: &'a str,
    pub kind: ToastKind,
}

impl<'a> ToastBubble<'a> {
    pub(crate) fn render(self, frame: &mut Frame, theme: &Theme) {
        let size = frame.area();
        self.render_at_width(frame, theme, size.width);
    }

    pub(crate) fn render_at_width(self, frame: &mut Frame, theme: &Theme, width: u16) {
        draw_toast_bubble(frame, theme, self.message, self.kind, width);
    }
}

pub(crate) fn draw_toast_bubble(
    frame: &mut Frame,
    theme: &Theme,
    message: &str,
    kind: ToastKind,
    width: u16,
) {
    let clean = message.trim();
    if clean.is_empty() {
        return;
    }

    let color = kind.color(theme);
    let icon = kind.glyph(theme);
    let icon_w = icon.width();

    // Horizontal padding budget:
    // Leading pad (2 spaces) + icon (icon_w) + icon gap (2 spaces) + trailing pad (2 spaces)
    let chrome_w = icon_w + 6;

    // Usable width on the terminal: reserve at least 2 columns margin on left & right.
    let max_toast_w = (width.saturating_sub(4) as usize)
        .min(MAX_TOAST_WIDTH as usize)
        .max(MIN_TOAST_WIDTH as usize);

    let text_budget = max_toast_w.saturating_sub(chrome_w).max(1);

    let logical_lines: Vec<&str> = clean.lines().collect();
    let is_multiline = logical_lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .count()
        > 1;

    let mut rendered_lines: Vec<(String, Style)> = Vec::new();
    let mut truncated = false;

    for (idx, line_str) in logical_lines.iter().enumerate() {
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Title (first non-empty logical line) is bold foreground.
        // Detail / subsequent lines use muted text for functional visual hierarchy.
        let style = if idx == 0 || !is_multiline {
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        };

        let wrapped = wrap_text(trimmed, text_budget);
        for wl in wrapped {
            if rendered_lines.len() >= MAX_TOAST_ROWS {
                truncated = true;
                break;
            }
            rendered_lines.push((wl.text, style));
        }
        if truncated {
            break;
        }
    }

    if rendered_lines.is_empty() {
        return;
    }

    if truncated && let Some((last_text, _)) = rendered_lines.last_mut() {
        let ell = theme.glyphs.ellipsis;
        let ell_w = ell.width();
        while last_text.width() + ell_w > text_budget && !last_text.is_empty() {
            last_text.pop();
        }
        last_text.push_str(ell);
    }

    let max_content_w = rendered_lines
        .iter()
        .map(|(text, _)| text.width())
        .max()
        .unwrap_or(0);

    // Toast width: content width + leading gutter + trailing pad
    let toast_width =
        ((max_content_w + chrome_w) as u16).clamp(MIN_TOAST_WIDTH, max_toast_w as u16);
    let x = width.saturating_sub(toast_width).saturating_sub(2).max(1);

    // Borderless pill height matches the content line count directly (compact zero border chrome)
    let toast_height = (rendered_lines.len() as u16).min(frame.area().height);
    let area = Rect::new(x, 1, toast_width, toast_height);

    // 1. Clear underlying cells to prevent background transcript bleed-through
    frame.render_widget(Clear, area);

    // 2. Elevated container styling (lighter surface than scene head `theme.raised()`)
    let bg = theme.toast_bg();
    let is_structured = theme.elevation == nuotc::ElevationArchetype::Structured;
    let base_style = if is_structured {
        Style::default().add_modifier(Modifier::REVERSE)
    } else {
        Style::default().bg(bg)
    };

    let block = RtBlock::default().style(base_style);

    // 3. Render content lines with strict 4-column leading gutter
    let apply_base = |s: Style| -> Style {
        let mut res = s.bg(base_style.bg);
        if is_structured {
            res = res.add_modifier(Modifier::REVERSE);
        }
        res
    };

    let indent_spaces = " ".repeat(icon_w + 4);
    let lines: Vec<Line> = rendered_lines
        .into_iter()
        .enumerate()
        .map(|(idx, (text, style))| {
            if idx == 0 {
                Line::from(vec![
                    Span::styled("  ", apply_base(Style::default())),
                    Span::styled(icon, apply_base(Style::default().fg(color))),
                    Span::styled("  ", apply_base(Style::default())),
                    Span::styled(text, apply_base(style)),
                    Span::styled("  ", apply_base(Style::default())),
                ])
            } else {
                Line::from(vec![
                    Span::styled(indent_spaces.clone(), apply_base(Style::default())),
                    Span::styled(text, apply_base(style)),
                    Span::styled("  ", apply_base(Style::default())),
                ])
            }
        })
        .collect();

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuotc::TestTerminal;

    fn grid_row(terminal: &TestTerminal, y: u16) -> String {
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        (0..width).map(|x| buffer[(x, y)].symbol()).collect()
    }

    #[test]
    fn single_line_toast_renders_compactly_without_empty_rows() {
        let theme = Theme::default();
        let mut terminal = TestTerminal::new(80, 10);
        terminal.draw(|f| {
            draw_toast_bubble(f, &theme, "copied to clipboard", ToastKind::CopyOk, 80);
        });

        // y=0: outside toast
        let row_0 = grid_row(&terminal, 0);
        assert!(
            !row_0.contains("copied to clipboard"),
            "row 0 should be outside toast"
        );

        // y=1: single compact content row (borderless floating pill)
        let row_1 = grid_row(&terminal, 1);
        assert!(
            !row_1.contains('┃') && !row_1.contains('╭') && !row_1.contains('─'),
            "row 1 should have no border decoration: {row_1}"
        );
        assert!(
            row_1.contains("  ✓  copied to clipboard"),
            "row 1 has icon and text: {row_1}"
        );

        // y=2: outside toast (compact 1-row height)
        let row_2 = grid_row(&terminal, 2);
        assert!(
            !row_2.contains("copied to clipboard"),
            "row 2 should be outside toast: {row_2}"
        );
    }

    #[test]
    fn multiline_toast_renders_compactly_with_consistent_padding() {
        let theme = Theme::default();
        let mut terminal = TestTerminal::new(80, 10);
        let msg =
            "Some additional workspace roots could not be loaded\nSkipped roots: `../opencode`";
        terminal.draw(|f| {
            draw_toast_bubble(f, &theme, msg, ToastKind::Armed, 80);
        });

        // y=0: empty
        let row_0 = grid_row(&terminal, 0);
        assert!(!row_0.contains("roots"), "row 0 must not be toast");

        // y=1: Title line with warning icon and leading pad
        let row_1 = grid_row(&terminal, 1);
        assert!(
            !row_1.contains('┃') && !row_1.contains('╭'),
            "row 1 should be borderless: {row_1}"
        );
        assert!(
            row_1.contains("  ▲  Some additional workspace roots could not be loaded"),
            "row 1 must have icon and title: {row_1}"
        );

        // y=2: Detail line aligned past the icon gutter (5 spaces)
        let row_2 = grid_row(&terminal, 2);
        assert!(
            !row_2.contains('┃') && !row_2.contains('╰'),
            "row 2 should be borderless: {row_2}"
        );
        assert!(
            row_2.contains("     Skipped roots: `../opencode`"),
            "row 2 must align under title text (indented 5 spaces): {row_2}"
        );

        // y=3: outside toast
        let row_3 = grid_row(&terminal, 3);
        assert!(
            !row_3.contains("roots"),
            "row 3 should not contain toast: {row_3}"
        );
    }

    #[test]
    fn long_line_toast_wraps_within_max_bounds() {
        let theme = Theme::default();
        let mut terminal = TestTerminal::new(80, 10);
        let long_msg = "This is a very long notification message that definitely exceeds the text budget and must wrap cleanly across multiple lines without overflowing or getting cut off abruptly.";
        terminal.draw(|f| {
            draw_toast_bubble(f, &theme, long_msg, ToastKind::Info, 80);
        });

        let row_1 = grid_row(&terminal, 1);
        assert!(
            row_1.contains("  ℹ  This is a very long"),
            "row 1 starts with info glyph: {row_1}"
        );

        let row_2 = grid_row(&terminal, 2);
        assert!(
            row_2.starts_with(' ') || row_2.contains("     "),
            "row 2 indented: {row_2}"
        );
    }

    #[test]
    fn ascii_toast_renders_ascii_glyphs() {
        let theme = Theme {
            glyphs: nuotc::ASCII_GLYPHS,
            ..Default::default()
        };
        let mut terminal = TestTerminal::new(80, 10);
        terminal.draw(|f| {
            draw_toast_bubble(f, &theme, "copied to clipboard", ToastKind::CopyOk, 80);
        });

        let row_1 = grid_row(&terminal, 1);
        assert!(
            !row_1.contains('|') && !row_1.contains('+'),
            "row 1 should be borderless in ASCII too: {row_1}"
        );
        assert!(
            row_1.contains("[OK]"),
            "row 1 should contain ASCII check [OK]: {row_1}"
        );
        assert!(
            row_1.contains("copied to clipboard"),
            "row 1 has text: {row_1}"
        );
    }

    #[test]
    fn preview_toast_styles() {
        let theme = Theme::default();
        let cases = [
            (
                "1. Success / Copied",
                "copied to clipboard",
                ToastKind::CopyOk,
            ),
            (
                "2. Failure / Error",
                "clipboard is empty",
                ToastKind::CopyFailed,
            ),
            (
                "3. Armed Confirmation",
                "Esc again interrupts",
                ToastKind::Armed,
            ),
            (
                "4. Command Ack / Info",
                "/delegate on: sub-agent routing active",
                ToastKind::Info,
            ),
            (
                "5. Multiline Notice",
                "Workspace roots could not be loaded\nSkipped: `../opencode`",
                ToastKind::Warning,
            ),
        ];

        println!("\n=== Modern Functional Minimalist Borderless Toast Preview ===");
        for (label, msg, kind) in cases {
            let mut term = TestTerminal::new(70, 5);
            term.draw(|f| {
                draw_toast_bubble(f, &theme, msg, kind, 70);
            });
            println!("\n--- {label} ---");
            for y in 1..=3 {
                let row = grid_row(&term, y);
                let trimmed = row.trim();
                if !trimmed.is_empty() {
                    println!("{row}");
                }
            }
        }
        println!("=============================================================\n");
    }
}
