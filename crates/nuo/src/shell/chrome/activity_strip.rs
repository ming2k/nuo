//! Consolidated single-row activity strip (ADR-0062 [INV-FOOTER-02]).
//!
//! Strictly bounded to at most 1 row above Composer. Aggregates live round execution
//! status, staged follow-ups queue indicator, and active background worker count,
//! eliminating layout jitter and avoiding inter-bar occlusion.

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

use super::common::{classify_liveness, dot_color, format_elapsed, truncate_for_bar};
use crate::shell::keymap::Key;
use crate::shell::render::Theme;
use nuo_ui_primitives::components::keycap::{keycap_span, keycap_warn_span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivityStripProps<'a> {
    /// Active agent status verb (e.g. "responding", "thinking", "editing"). Empty when idle.
    pub status: &'a str,
    /// The live provider-retry setback, if this session is backing off.
    ///
    /// The **typed state**, not a rendered sentence: the strip picks between the
    /// full and compact forms under width pressure, and both must come from the
    /// one owner (`RetryProgress`). Passing a pre-rendered string forced this
    /// renderer either to paint it at full width (squeezing the status out) or
    /// to slice the sentence back apart — the anti-pattern that let the retry
    /// vocabulary drift in the first place.
    pub backoff: Option<&'a nuo_view::retry::RetryProgress>,
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
    /// The chord that opens the queue, resolved from the live keymap by the
    /// caller. `None` means the command has no chord and the strip advertises
    /// nothing — a keycap is a promise, and this strip has no business minting
    /// one.
    pub queue_key: Option<Key>,
}

pub fn draw_activity_strip(
    frame: &mut Frame,
    rect: Rect,
    round_started_at: Option<Instant>,
    props: ActivityStripProps<'_>,
    interrupt_key: Option<Key>,
    spinner_phase: usize,
    theme: &Theme,
) -> Option<Rect> {
    let ActivityStripProps {
        status,
        backoff,
        awaiting_permission,
        queued_count,
        queued_preview,
        tasks_count,
        tasks_preview,
        queue_key,
    } = props;

    let status_active = !status.is_empty() && status != "idle";
    let is_active = status_active || awaiting_permission || queued_count > 0 || tasks_count > 0;
    if !is_active || rect.height == 0 {
        return None;
    }

    let row_width = rect.width as usize;
    let available_width = row_width;
    let dim = Style::default().fg(theme.muted());

    // 1. Right-side keyboard affordance.
    //
    // Every chord is resolved by the caller from the live keymap; nothing here
    // is a literal. The strip used to advertise a `C-x q` / `C-x b` namespace
    // that ADR-0131 retired outright, so both keycaps named key sequences that
    // dispatch nothing at all — a standing lie on the busiest row in the app.
    let mut right_spans: Vec<Span<'static>> = Vec::new();
    if status_active || awaiting_permission {
        if let Some(interrupt) = interrupt_key {
            // The armed interrupt is the chained double-tap
            // (`handle_esc_interrupt`), so it repeats *the resolved* chord.
            let key_w = UnicodeWidthStr::width(interrupt.display());
            let full_interrupt_w = key_w * 2 + 1 + 10;
            let compact_interrupt_w = key_w * 2 + 1;
            if available_width >= full_interrupt_w + 20 {
                right_spans.push(keycap_warn_span(theme, interrupt.display()));
                right_spans.push(Span::styled(" ", dim));
                right_spans.push(keycap_warn_span(theme, interrupt.display()));
                right_spans.push(Span::styled(" interrupt", theme.keycap_label_style()));
            } else if available_width >= compact_interrupt_w + 12 {
                right_spans.push(keycap_warn_span(theme, interrupt.display()));
                right_spans.push(Span::styled(" ", dim));
                right_spans.push(keycap_warn_span(theme, interrupt.display()));
            }
        }
    } else if queued_count > 0 {
        // The queue's one real chord, not a retired namespace prefix.
        if let Some(key) = queue_key {
            right_spans.push(keycap_span(theme, key.display()));
            right_spans.push(Span::styled(" queue", theme.keycap_label_style()));
        }
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

        // The clause and the status label compete for one row, and the clause is
        // the more urgent fact (it is why nothing is happening), so it is
        // budgeted first on a fixed ladder rather than being painted at full
        // width and squeezing the status out:
        //
        //   full (`retry 1 in 4s`) → compact (`retry 1`) → none
        //
        // The ladder's steps are deliberately close in width: both forms are
        // short, so the compact step buys the status verb only a few columns and
        // the choice never lurches between two shapes a reader has to re-learn.
        //
        // The compact form is a *rendering* of the same ordinal, produced by the
        // clause's owner (`RetryProgress::compact_label`) — never by this
        // renderer slicing the sentence, which is how the two vocabularies
        // drifted apart before.
        const MIN_STATUS_WIDTH: usize = 4;
        const CLAUSE_GAP: usize = 2;
        let prefix_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
        let status_natural_w = status.width();
        let fixed = prefix_w + right_w + elapsed_w + CLAUSE_GAP;

        // Full sentence, then the ordinal alone, then nothing. Both forms come
        // from the same owner (`RetryProgress`); this renderer only chooses
        // which one fits.
        let clause_choice = match backoff {
            None => None,
            Some(progress) => {
                let now = std::time::Instant::now();
                let full = progress.label(now);
                let compact = progress.compact_label();
                let full_w = CLAUSE_GAP + full.width();
                let compact_w = CLAUSE_GAP + compact.width();
                if fixed + status_natural_w + full_w <= available_width {
                    Some(full)
                } else if fixed + status_natural_w + compact_w <= available_width
                    || fixed + MIN_STATUS_WIDTH + compact_w <= available_width
                {
                    // Under real pressure the clause outranks the status verb:
                    // the countdown is *why* nothing is happening, and a
                    // truncated `waiting for mo…` beside it still reads.
                    Some(compact)
                } else {
                    None
                }
            }
        };
        let clause_w = clause_choice
            .as_ref()
            .map(|c| CLAUSE_GAP + c.width())
            .unwrap_or(0);

        let budget = available_width.saturating_sub(fixed + clause_w).max(1);
        let status_disp = truncate_for_bar(status, budget);
        left_spans.push(Span::styled(status_disp, lead_style));

        if let Some(clause) = clause_choice {
            // Painted verbatim from its owner; never re-wrapped or re-derived.
            left_spans.push(Span::styled(
                format!("  {clause}"),
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

    frame.render_widget(&mut Paragraph::new(Line::from(spans)), rect);
    Some(rect)
}
