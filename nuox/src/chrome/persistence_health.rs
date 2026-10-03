//! The durability-health banner (ADR-0196 D4): a retained one-row alert the
//! TUI shows while the daemon's persistence writer is degraded, cleared by
//! the next `Healthy` transition. This is a *banner*, not a transcript
//! notice — the condition outlives any message and must not scroll away.

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};

use crate::render::Theme;

/// Draw the single-row durability-health banner. `state` is the degraded
/// state (`Recovering` / `Down`); a healthy writer renders no banner at all,
/// so the caller must not place the row.
pub fn draw_persistence_health_bar(
    frame: &mut Frame,
    rect: Rect,
    state: &nuo_wire::monitor::PersistenceHealth,
    theme: &Theme,
) {
    let (label, color) = match state {
        nuo_wire::monitor::PersistenceHealth::Recovering { .. } => {
            ("RECOVERING", theme.warn())
        }
        nuo_wire::monitor::PersistenceHealth::Down { .. } => ("STORAGE DOWN", theme.err()),
        // Healthy never places a row; drawing defensively keeps the
        // contract visible if a caller mis-places it.
        nuo_wire::monitor::PersistenceHealth::Healthy => ("STORAGE", theme.muted()),
    };

    let detail = state.detail().unwrap_or_default();
    let budget = rect.width as usize;
    let mut row: Vec<Span<'static>> = Vec::with_capacity(4);
    row.push(Span::styled(
        label.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    let detail_style = Style::default().fg(color);
    let reserved = label.chars().count() + 2;
    if budget > reserved {
        let detail_budget = budget - reserved;
        let detail = crate::overlays::common::one_line(detail.trim());
        let detail = if detail.chars().count() > detail_budget {
            crate::overlays::common::truncate_ellipsis(&detail, detail_budget)
        } else {
            detail
        };
        row.push(Span::styled("  ", Style::default().fg(theme.muted())));
        row.push(Span::styled(detail, detail_style));
    }

    frame.render_widget(Paragraph::new(Line::from(row)), rect);
}
