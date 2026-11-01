//! The command output pager modal overlay.
//!
//! Provides a dedicated, full-terminal inspection view for tool execution logs.
//! Replaces inline transcript explosions with a scrollable pager:
//! - Full line-numbered output stream.
//! - Continuous keyboard navigation (j/k, PgUp/PgDn, Home/End, g/G).
//! - Direct full-log copy with `y` / `c`.
//! - Instant exit with `q` / `Esc` returning cleanly to the exact transcript position.

use nuotc::{Frame, Line, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::truncate_to_width;
use crate::shell::render::Theme;
use crate::shell::surfaces::CommandOutputDialog;
use nuo_ui_primitives::elevation::modal_frame;
use nuo_ui_primitives::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, draw_scrollbar, modal_header,
    render_modal_footer,
};
use nuo_view::text_layout::padded_tail;

pub fn draw_command_output_modal(
    frame: &mut Frame,
    _area: Rect,
    dialog: &mut CommandOutputDialog,
    theme: &Theme,
) -> Rect {
    let geometry = ContentModalSpec::SESSION_TRACE;
    let bounds = content_modal_area(frame, geometry, 24);
    let f = modal_frame(frame, bounds, theme, true, true);
    let bg = theme.code_surface();

    let total_lines = dialog.lines.len();
    let title = if dialog.command.is_empty() {
        format!("Command Output ({} lines)", total_lines)
    } else {
        let cmd = truncate_to_width(&dialog.command, 48);
        if let Some(exit) = dialog.exit {
            if exit != 0 {
                format!("$ {}  (exit {}) — {} lines", cmd, exit, total_lines)
            } else {
                format!("$ {} — {} lines", cmd, total_lines)
            }
        } else {
            format!("$ {} — {} lines", cmd, total_lines)
        }
    };
    modal_header(frame, f.header, &title, theme);

    let body_h = f.body.height as usize;
    let body_w = f.body.width as usize;
    let max_scroll = total_lines.saturating_sub(body_h);
    dialog.scroll = dialog.scroll.min(max_scroll);

    let num_width = format!("{}", total_lines.max(1)).len().max(3);

    for row_idx in 0..body_h {
        let y = f.body.y + row_idx as u16;
        let line_idx = dialog.scroll + row_idx;
        if line_idx < total_lines {
            let line_text = &dialog.lines[line_idx];
            let num_str = format!("{:>width$} │ ", line_idx + 1, width = num_width);
            let prefix_w = num_str.width();
            let avail_w = body_w.saturating_sub(prefix_w).max(1);
            let truncated = truncate_to_width(line_text, avail_w);
            let used = prefix_w + truncated.width();

            let spans = vec![
                Span::styled(num_str, Style::default().bg(bg).fg(theme.dim())),
                Span::styled(
                    truncated.to_string(),
                    Style::default().bg(bg).fg(theme.fg()),
                ),
                Span::styled(padded_tail(body_w, used), Style::default().bg(bg)),
            ];
            frame.render_widget(
                &mut Paragraph::new(Line::from(spans)),
                Rect::new(f.body.x, y, f.body.width, 1),
            );
        } else {
            let empty_row = Line::from(vec![Span::styled(
                padded_tail(body_w, 0),
                Style::default().bg(bg),
            )]);
            frame.render_widget(
                &mut Paragraph::new(empty_row),
                Rect::new(f.body.x, y, f.body.width, 1),
            );
        }
    }

    if max_scroll > 0 {
        draw_scrollbar(frame, f.body, dialog.scroll, max_scroll, theme);
    }

    let hints = [
        FooterHint::always("q/Esc", "close"),
        FooterHint::always("y", "copy all"),
        FooterHint::navigation("↑/↓ / j/k", "scroll"),
    ];
    if let Some(footer) = f.footer {
        render_modal_footer(frame, footer, &hints, theme);
    }

    bounds
}
