//! Transient activity bar rendering mid-round status, elapsed timer, and breathing dot.

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

use super::common::{classify_liveness, dot_color, format_elapsed, truncate_for_bar};
use crate::components::keycap::keycap_warn_span;
use crate::keymap::Key;
use crate::render::Theme;

pub struct ActivityBarProps<'a> {
    /// Root agent label (the typed phase's text).
    pub status: &'a str,
    /// Transport-setback clause rendered beside the label, warning-tinted. `None`
    /// when transport is healthy.
    pub backoff_clause: Option<&'a str>,
    /// Warning-tinted gate state (permission / ask_user pending).
    pub awaiting_permission: bool,
}

pub fn draw_activity_bar(
    frame: &mut Frame,
    rect: Rect,
    round_started_at: Option<Instant>,
    props: ActivityBarProps<'_>,
    spinner_phase: usize,
    theme: &Theme,
) -> Option<Rect> {
    let ActivityBarProps {
        status,
        backoff_clause,
        awaiting_permission,
    } = props;
    let status_active = !status.is_empty() && status != "idle";
    let dim = Style::default().fg(theme.muted());

    if !status_active {
        return None;
    }

    let row_width = rect.width as usize;
    let available_width = row_width;
    let elapsed =
        round_started_at.map(|started| format!(" [{}]", format_elapsed(started.elapsed())));
    let full_interrupt_width = UnicodeWidthStr::width("Esc Esc interrupt");
    let key_interrupt_width = UnicodeWidthStr::width("Esc Esc");
    let dot = theme.glyphs.dot;
    let prefix_width = UnicodeWidthStr::width(format!(" {dot} ").as_str());
    const MIN_STATUS_WIDTH: usize = 4;
    const MIN_TINY_STATUS_WIDTH: usize = 1;
    const SEGMENT_GAP: usize = 2;
    let show_interrupt_words =
        available_width >= prefix_width + SEGMENT_GAP + full_interrupt_width + MIN_STATUS_WIDTH;
    let show_interrupt_keys = show_interrupt_words
        || available_width
            >= prefix_width + SEGMENT_GAP + key_interrupt_width + MIN_TINY_STATUS_WIDTH;
    let interrupt_width = if show_interrupt_words {
        full_interrupt_width
    } else if show_interrupt_keys {
        key_interrupt_width
    } else {
        0
    };
    let interrupt_gap = if show_interrupt_keys { SEGMENT_GAP } else { 0 };

    let natural_full_clause = backoff_clause.map(|c| format!("  {c}")).unwrap_or_default();
    let full_clause_w = UnicodeWidthStr::width(natural_full_clause.as_str());
    let compact_clause = backoff_clause.map(|clause| {
        let attempt = clause
            .split("retry ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or(clause);
        format!("  ({attempt})")
    });
    let compact_clause_w = compact_clause
        .as_deref()
        .map(UnicodeWidthStr::width)
        .unwrap_or(0);
    let status_natural_w = UnicodeWidthStr::width(status);
    let elapsed_text = elapsed.clone().unwrap_or_default();
    let elapsed_w = UnicodeWidthStr::width(elapsed_text.as_str());
    let fixed_tail = interrupt_gap + interrupt_width;

    let fits = |status_w: usize, clause_w: usize, elapsed_on: bool| -> bool {
        let el = if elapsed_on { elapsed_w } else { 0 };
        prefix_width + status_w + clause_w + el + fixed_tail <= available_width
    };
    enum Clause {
        Full,
        Compact,
        None,
    }
    let (chosen_clause, show_elapsed) = if fits(status_natural_w, full_clause_w, true) {
        (Clause::Full, true)
    } else if fits(status_natural_w, compact_clause_w, true) {
        (Clause::Compact, true)
    } else if fits(status_natural_w, 0, true) {
        (Clause::None, true)
    } else {
        (Clause::None, false)
    };

    let remaining_for_status = available_width
        .saturating_sub(prefix_width + fixed_tail)
        .saturating_sub(match chosen_clause {
            Clause::Full => full_clause_w,
            Clause::Compact => compact_clause_w,
            Clause::None => 0,
        })
        .saturating_sub(if show_elapsed { elapsed_w } else { 0 });

    let status_display = if status_natural_w <= remaining_for_status {
        status.to_string()
    } else {
        truncate_for_bar(status, remaining_for_status)
    };

    let lead_fg = if awaiting_permission {
        theme.warning
    } else {
        theme.fg()
    };
    let lead_style = Style::default().fg(lead_fg).add_modifier(Modifier::BOLD);
    let liveness = classify_liveness(awaiting_permission);
    let glyph_color = dot_color(liveness, spinner_phase, theme);
    let glyph_style = Style::default().fg(glyph_color);

    let mut left_spans: Vec<Span<'static>> = Vec::with_capacity(6);
    left_spans.push(Span::styled(format!(" {} ", theme.glyphs.dot), glyph_style));
    left_spans.push(Span::styled(status_display, lead_style));

    match chosen_clause {
        Clause::Full if !natural_full_clause.is_empty() => {
            left_spans.push(Span::styled(
                natural_full_clause,
                Style::default().fg(theme.warning),
            ));
        }
        Clause::Compact => {
            if let Some(clause) = compact_clause {
                left_spans.push(Span::styled(clause, Style::default().fg(theme.warning)));
            }
        }
        _ => {}
    }

    if show_elapsed && let Some(el) = elapsed {
        left_spans.push(Span::styled(el, dim));
    }

    let mut right_spans: Vec<Span<'static>> = Vec::new();
    if show_interrupt_keys {
        right_spans.push(keycap_warn_span(theme, Key::ESC.display()));
        right_spans.push(Span::styled(" ", dim));
        right_spans.push(keycap_warn_span(theme, Key::ESC.display()));
        if show_interrupt_words {
            right_spans.push(Span::styled(" interrupt", theme.keycap_label_style()));
        }
    }

    let left_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
    let right_w: usize = right_spans.iter().map(|s| s.content.width()).sum();
    let min_gap = if right_w > 0 { SEGMENT_GAP } else { 0 };
    let middle_pad = available_width
        .saturating_sub(left_w + right_w)
        .max(min_gap);

    let mut full_line_spans = left_spans;
    if middle_pad > 0 {
        full_line_spans.push(Span::styled(" ".repeat(middle_pad), dim));
    }
    full_line_spans.extend(right_spans);

    frame.render_widget(Paragraph::new(Line::from(full_line_spans)), rect);
    Some(rect)
}
