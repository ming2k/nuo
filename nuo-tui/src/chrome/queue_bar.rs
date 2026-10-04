//! The persistent single-row queue/outbox bar pinned below transcript gap.

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::components::keycap::keycap_span;
use crate::design::{BAR_LEGEND_GAP_MIN, JOIN_ENUMERATE_COLS};
use crate::render::Theme;

/// One queued outbox item projected for the [`QueueBarProps`] / queue modal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueItemProps {
    pub queued_at_ms: u64,
    pub text: String,
}

/// Inputs for [`draw_queue_bar`]: the persistent one-row outbox summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueBarProps<'a> {
    pub items: &'a [QueueItemProps],
    pub paused: bool,
    pub blocked: bool,
    /// The chord that actually opens the queue panel, resolved from the
    /// command registry (ADR-0238). `None` — the command was unbound or the
    /// user remapped it away — renders no keycap: the bar advertises the
    /// binding that fires, and nothing when none does.
    pub expand_key: Option<crate::keymap::Key>,
}

/// How much of the queue bar's keycap legend survives under width pressure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LegendDensity {
    /// Key + label: `Ctrl-q expand`.
    Full,
    /// Bare keycap: `Ctrl-q`.
    Compact,
    /// Nothing — the legend is dropped entirely.
    Tiny,
}

/// Draw the persistent single-row queue/outbox summary bar.
pub fn draw_queue_bar(
    frame: &mut Frame,
    rect: Rect,
    props: QueueBarProps<'_>,
    theme: &Theme,
) -> Rect {
    let QueueBarProps {
        items,
        paused,
        blocked,
        expand_key,
    } = props;

    let full_w = rect.width as usize;
    let next = items.first();
    let count = items.len();
    let dim = Style::default().fg(theme.muted());
    let fg = Style::default().fg(theme.fg());

    let count_color = if blocked {
        theme.err()
    } else if paused {
        theme.warn()
    } else {
        theme.fg()
    };
    let count_style = Style::default()
        .fg(count_color)
        .add_modifier(Modifier::BOLD);
    let tag_style = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);

    let mut left: Vec<Span<'static>> = vec![
        Span::styled("FOLLOW-UPS", tag_style),
        Span::styled(" ", dim),
    ];
    let count_label = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    left.push(Span::styled(count_label, count_style));

    if blocked {
        left.push(Span::styled("  ", dim));
        left.push(Span::styled("blocked", count_style));
    }

    let mk_right = |density: LegendDensity| -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if matches!(density, LegendDensity::Tiny) {
            return spans;
        }
        // No resolved chord → no keycap. The label alone would advertise an
        // affordance with no way to reach it by keyboard (ADR-0238).
        let Some(key) = expand_key else {
            return spans;
        };
        spans.push(keycap_span(theme, key.display()));
        if matches!(density, LegendDensity::Full) {
            spans.push(Span::styled(" expand", theme.keycap_label_style()));
        }
        spans
    };

    let preview_text = next.map(|item| crate::overlays::common::one_line(item.text.trim()));

    let left_w: usize = left.iter().map(|s| s.content.width()).sum();
    let right_w =
        |right: &[Span<'static>]| -> usize { right.iter().map(|s| s.content.width()).sum() };
    const PREVIEW_MIN_COLS: usize = 8;

    let mut legend_density = LegendDensity::Full;
    let mut right = mk_right(legend_density);
    loop {
        let rw = right_w(&right);
        let reserved = left_w
            + if rw > 0 { BAR_LEGEND_GAP_MIN + rw } else { 0 }
            + if preview_text.is_some() {
                JOIN_ENUMERATE_COLS + PREVIEW_MIN_COLS
            } else {
                0
            };
        if reserved <= full_w {
            break;
        }
        match legend_density {
            LegendDensity::Full => {
                legend_density = LegendDensity::Compact;
                right = mk_right(legend_density);
            }
            LegendDensity::Compact => {
                legend_density = LegendDensity::Tiny;
                right = mk_right(legend_density);
            }
            LegendDensity::Tiny => {
                right.clear();
                break;
            }
        }
    }

    let rw = right_w(&right);
    let preview_budget = full_w
        .saturating_sub(left_w)
        .saturating_sub(if rw > 0 { BAR_LEGEND_GAP_MIN + rw } else { 0 })
        .saturating_sub(if preview_text.is_some() {
            JOIN_ENUMERATE_COLS
        } else {
            0
        });
    let preview = preview_text
        .filter(|_| preview_budget >= PREVIEW_MIN_COLS)
        .map(|text| {
            if text.width() > preview_budget {
                crate::overlays::common::truncate_ellipsis(&text, preview_budget)
            } else {
                text
            }
        });
    let preview_w = preview.as_ref().map_or(0, |p| p.width());

    let mut row: Vec<Span<'static>> = Vec::with_capacity(left.len() + right.len() + 4);
    row.extend(left);
    if let Some(preview) = preview {
        row.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), dim));
        row.push(Span::styled(preview, fg));
    }
    let used = left_w
        + if preview_w > 0 {
            JOIN_ENUMERATE_COLS + preview_w
        } else {
            0
        };
    let gap = full_w
        .saturating_sub(used + rw)
        .max(if rw > 0 { BAR_LEGEND_GAP_MIN } else { 0 });
    row.push(Span::styled(" ".repeat(gap), dim));
    row.extend(right);
    let used = used + gap + rw;
    row.push(Span::styled(" ".repeat(full_w.saturating_sub(used)), dim));

    frame.render_widget(Paragraph::new(Line::from(row)), rect);

    rect
}
