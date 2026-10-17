//! Consolidated single-row activity strip (ADR-0062 [INV-FOOTER-02]).
//!
//! Strictly bounded to at most 1 row above Composer. Aggregates live round execution
//! status, staged follow-ups queue indicator, and active background worker count,
//! eliminating layout jitter and avoiding inter-bar occlusion.

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

use super::common::{classify_liveness, dot_color, format_elapsed, truncate_for_bar};
use crate::components::keycap::{keycap_span, keycap_warn_span};
use crate::keymap::Key;
use crate::render::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivityStripProps<'a> {
    /// Active agent status verb (e.g. "responding", "thinking", "editing"). Empty when idle.
    pub status: &'a str,
    /// Optional backoff/retry clause.
    pub backoff_clause: Option<&'a str>,
    /// Gate state (e.g. permission or ask_user pending).
    pub awaiting_permission: bool,
    /// Number of queued follow-up messages staged in outbox.
    pub queued_count: usize,
    /// Text of the first staged follow-up message (for idle preview).
    pub queued_preview: Option<&'a str>,
    /// Number of active/undismissed background tasks.
    pub tasks_count: usize,
    /// Label of the first active task (for idle preview).
    pub tasks_preview: Option<&'a str>,
}

pub fn draw_activity_strip(
    frame: &mut Frame,
    rect: Rect,
    round_started_at: Option<Instant>,
    props: ActivityStripProps<'_>,
    spinner_phase: usize,
    theme: &Theme,
) -> Option<Rect> {
    let ActivityStripProps {
        status,
        backoff_clause,
        awaiting_permission,
        queued_count,
        queued_preview,
        tasks_count,
        tasks_preview,
    } = props;

    let status_active = !status.is_empty() && status != "idle";
    let is_active = status_active || awaiting_permission || queued_count > 0 || tasks_count > 0;
    if !is_active || rect.height == 0 {
        return None;
    }

    let row_width = rect.width as usize;
    let available_width = row_width;
    let dim = Style::default().fg(theme.muted());

    // 1. Right-side keyboard affordance
    let mut right_spans: Vec<Span<'static>> = Vec::new();
    if status_active || awaiting_permission {
        let full_interrupt_w = UnicodeWidthStr::width("Esc Esc interrupt");
        let compact_interrupt_w = UnicodeWidthStr::width("Esc Esc");
        if available_width >= full_interrupt_w + 20 {
            right_spans.push(keycap_warn_span(theme, Key::ESC.display()));
            right_spans.push(Span::styled(" ", dim));
            right_spans.push(keycap_warn_span(theme, Key::ESC.display()));
            right_spans.push(Span::styled(" interrupt", theme.keycap_label_style()));
        } else if available_width >= compact_interrupt_w + 12 {
            right_spans.push(keycap_warn_span(theme, Key::ESC.display()));
            right_spans.push(Span::styled(" ", dim));
            right_spans.push(keycap_warn_span(theme, Key::ESC.display()));
        }
    } else if queued_count > 0 {
        right_spans.push(keycap_span(theme, "C-x"));
        right_spans.push(Span::styled(" q", theme.keycap_label_style()));
    } else if tasks_count > 0 {
        right_spans.push(keycap_span(theme, "C-x"));
        right_spans.push(Span::styled(" b", theme.keycap_label_style()));
    }
    let right_w: usize = right_spans.iter().map(|s| s.content.width()).sum();

    // 2. Left-side cluster
    let mut left_spans: Vec<Span<'static>> = Vec::new();

    // Badges for queued follow-ups and background tasks
    if queued_count > 0 {
        let badge_text = format!("[{queued_count} queued] ");
        left_spans.push(Span::styled(
            badge_text,
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if tasks_count > 0 {
        let badge_text = format!(
            "[{tasks_count} task{}] ",
            if tasks_count > 1 { "s" } else { "" }
        );
        left_spans.push(Span::styled(
            badge_text,
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Main status representation
    if status_active || awaiting_permission {
        let liveness = classify_liveness(awaiting_permission);
        let glyph_color = dot_color(liveness, spinner_phase, theme);
        left_spans.push(Span::styled(
            format!("{} ", theme.glyphs.dot),
            Style::default().fg(glyph_color),
        ));

        let lead_fg = if awaiting_permission {
            theme.warning
        } else {
            theme.fg()
        };
        let lead_style = Style::default().fg(lead_fg).add_modifier(Modifier::BOLD);

        let elapsed =
            round_started_at.map(|started| format!(" [{}]", format_elapsed(started.elapsed())));
        let elapsed_text = elapsed.unwrap_or_default();
        let elapsed_w = elapsed_text.width();

        let prefix_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
        let budget = available_width.saturating_sub(prefix_w + right_w + elapsed_w + 2);
        let status_disp = truncate_for_bar(status, budget);
        left_spans.push(Span::styled(status_disp, lead_style));

        if let Some(clause) = backoff_clause.filter(|c| !c.is_empty()) {
            let clause_text = format!("  ({})", clause);
            left_spans.push(Span::styled(
                clause_text,
                Style::default().fg(theme.warning),
            ));
        }

        if !elapsed_text.is_empty() {
            left_spans.push(Span::styled(elapsed_text, dim));
        }
    } else if queued_count > 0 {
        let preview = queued_preview.unwrap_or("");
        let prefix_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
        let budget = available_width.saturating_sub(prefix_w + right_w + 4);
        let text = format!("Follow-up: \"{}\"", truncate_for_bar(preview, budget));
        left_spans.push(Span::styled(text, dim));
    } else if tasks_count > 0 {
        let preview = tasks_preview.unwrap_or("");
        let prefix_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
        let budget = available_width.saturating_sub(prefix_w + right_w + 4);
        let text = format!("Running: {}", truncate_for_bar(preview, budget));
        left_spans.push(Span::styled(text, dim));
    }

    let left_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
    let gap = available_width.saturating_sub(left_w + right_w);

    let mut spans = left_spans;
    spans.push(Span::styled(" ".repeat(gap), Style::default()));
    spans.extend(right_spans);

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    Some(rect)
}
