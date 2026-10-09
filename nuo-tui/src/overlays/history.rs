//! History search panel (Ctrl+R).
//!
//! A floating dropdown panel anchored above the composer. Unlike a centered
//! modal, the composer itself stays live: it becomes the filter input for the
//! panel, so typing immediately narrows the list below. The panel lists the
//! **entire cross-session history**, newest-first — Ctrl+R deliberately
//! ignores which session or workspace an entry came from, since its whole
//! purpose is global recall. The inline ↑/↓ recall, by contrast, is scoped
//! to the current session (see `App::current_session_history`).
//!
//! Each row is a single line (multi-line prompts collapse to the first line
//! with a `↵` marker); the row numbers + prompt text are enough to navigate,
//! so there is no origin status strip — the `~/project · #session… · time`
//! line was redundant noise and was removed. `Enter` inserts the focused entry
//! into the composer.

use nuo_wire::HistoryEntry;
use nuotc::{
    Clear as RtClear, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};

use super::common::truncate_ellipsis;
use crate::fuzzy::FuzzyMatch;
use crate::primitives::{ElevationContainer, SCROLL_EDGE_MARGIN, contrast_fg, render_body};
use crate::render::Theme;

/// Maximum number of rows the dropdown reserves vertically. Capped so a long
/// history stays scannable — a Ctrl+R picker is a glance surface, not a full
/// browser — and the body scrolls within this budget. Ten entries is enough to
/// recall a recent prompt at a glance; anything older is a search away (the
/// composer below is the live filter field).
const HISTORY_PANEL_MAX_ROWS: u16 = 10;

/// Draw the history search dropdown, anchored above `input_rect`.
///
/// `ranked` is the pre-computed `(original_history_index, FuzzyMatch)` list
/// produced by `App::history_rows` — passing it in avoids a second fuzzy pass
/// per frame. `modal_index` selects into `ranked`. `scroll` is read AND
/// written back so the caller's offset stays consistent with the clamped body
/// height; /// `follow_selection` gates whether the body auto-scrolls to keep
/// `modal_index` in view (true after navigation, false once the user scrolls
/// manually). `keymap_open` replaces the body with the
/// in-panel keybindings list (the `?` expand).
///
/// The panel floats with `Recess::None` (no dimming), and the composer below
/// stays fully live as the filter field — the caller still renders it.
///
/// `activity_height` is the row count the transient activity bar occupies in
/// the row(s) immediately above the composer this frame (0 when the bar is
/// hidden). The dropdown treats the activity bar's bottom edge as an upper
/// bound: it never grows into the activity bar's rows, so the activity bar is
/// always visible and always reads as above the history dropdown. This keeps
/// the dropdown an extension of the composer rather than something that can
/// occlude the live status surface above it.
///
/// The returned rect is the panel's footprint (for click-outside-dismiss hit
/// testing); it is `None` when there is no room above the activity bar
/// (height 0), in which case nothing is drawn.
/// Properties for rendering the input history panel.
pub struct HistoryPanelProps<'a> {
    pub history: &'a [HistoryEntry],
    pub ranked: &'a [(usize, FuzzyMatch)],
    pub modal_index: usize,
    pub scroll: &'a mut usize,
    pub follow_selection: bool,
    pub input_rect: Rect,
    pub activity_height: u16,
    pub query: &'a str,
    pub cursor_position: usize,
    pub show_caret: bool,
}

/// Draw the input history dropdown panel.
pub fn draw_history_panel(
    frame: &mut Frame,
    props: HistoryPanelProps<'_>,
    theme: &Theme,
) -> Option<Rect> {
    let HistoryPanelProps {
        history,
        ranked,
        modal_index,
        scroll,
        follow_selection,
        input_rect,
        activity_height,
        query,
        cursor_position,
        show_caret,
    } = props;
    // Compute the panel footprint: it grows upward from the top edge of the
    // composer. The activity bar sits flush above the composer, so reserve
    // its rows: the dropdown's ceiling is the activity bar's top edge, never
    // the composer's top edge — it must never paint over the live status bar
    // above it. Height tracks the actual content (one row per entry) floored
    // at a single body row so an empty/short history reads as a sliver rather
    // than a fixed-size box, capped at the max so a huge history scrolls
    // instead of eating the whole screen.
    let activity_h = activity_height.min(input_rect.y);
    let area_top = input_rect.y.saturating_sub(activity_h);
    let room_above = area_top;
    let row_count = ranked.len().max(1) as u16;
    let desired_rows = row_count.min(HISTORY_PANEL_MAX_ROWS);
    // +1 header (title), +2 composer chrome (the full panel-bg top/bottom padding
    // rows the panel shares with the composer below it).
    const CHROME_ROWS: u16 = 3;
    let desired_h = desired_rows.saturating_add(CHROME_ROWS);
    let panel_h = desired_h.min(room_above);
    if panel_h == 0 {
        return None;
    }
    // The panel grows upward from the activity bar's top edge (its reserved
    // ceiling), never from the composer's top edge: this is what keeps it out
    // of the activity bar's rows. Its footprint is [area_top - panel_h, area_top).
    let panel_y = area_top.saturating_sub(panel_h);
    let area = Rect::new(input_rect.x, panel_y, input_rect.width, panel_h);

    // The panel shares the composer's surface language so the dropdown reads as
    // an extension of the input box rather than a separate floating window: a
    // solid `panel()` fill with full panel-bg padding rows on the top and
    // bottom edges, so it breathes exactly like the composer does. No left
    // accent bar — the composer has none, and a full-height brand column would
    // read as selection/severity, which a history list is not. The edges are
    // painted by the same panel fill (no half-block `▄`/`▀` glyphs), so the
    // transition is identical across terminals.
    frame.render_widget(RtClear, area);
    let inner = ElevationContainer::overlay().render(frame, area, theme);
    let inner_w = inner.width;

    // Header row: title + live query echo + counts. Sits just inside the top
    // transition row, full width (no left-accent column to inset around).
    let header_rect = Rect::new(inner.x, inner.y + 1, inner_w, 1);
    // Body sits below header and fills the remaining height above bottom transition.
    let body_rect = if inner.height >= CHROME_ROWS {
        Rect::new(
            inner.x,
            header_rect.y + 1,
            inner_w,
            inner.height.saturating_sub(CHROME_ROWS),
        )
    } else {
        // Degenerate tiny terminal: give the body whatever is left after the
        // top transition + header so the list is still visible.
        Rect::new(
            inner.x,
            header_rect.y + 1,
            inner_w,
            inner.height.saturating_sub(2),
        )
    };

    draw_header(
        frame,
        header_rect,
        history.len(),
        ranked.len(),
        query,
        cursor_position,
        show_caret,
        theme,
    );

    {
        let body = list_body(
            history,
            ranked,
            modal_index,
            theme,
            body_rect.width as usize,
        );
        let follow = follow_selection.then_some(modal_index);
        render_body(
            frame,
            body_rect,
            body,
            scroll,
            crate::primitives::BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
            theme,
        );
    }

    Some(area)
}

/// Header: `History` title, the query echo, and the count of visible/total.
fn draw_header(
    frame: &mut Frame,
    rect: Rect,
    total: usize,
    shown: usize,
    query: &str,
    cursor_position: usize,
    show_caret: bool,
    theme: &Theme,
) {
    let title = Span::styled(
        "History",
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    );
    let count = if total == 0 {
        Span::styled("  no history yet", Style::default().fg(theme.muted()))
    } else if shown == total {
        Span::styled(format!("  {shown}"), Style::default().fg(theme.muted()))
    } else {
        Span::styled(
            format!("  {shown}/{total}"),
            Style::default().fg(theme.muted()),
        )
    };
    let mut spans = vec![title];
    if show_caret || !query.is_empty() {
        spans.push(Span::styled("  › ", Style::default().fg(theme.muted())));
        if query.is_empty() {
            spans.push(Span::styled(
                "type to filter…",
                Style::default().fg(theme.muted()),
            ));
        } else {
            spans.push(Span::styled(
                query.to_string(),
                Style::default()
                    .fg(theme.fg())
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    spans.push(count);
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);

    if show_caret {
        let prefix_cols = "History  › ".chars().count() as u16;
        let query_cols = query.chars().take(cursor_position).count() as u16;
        let cursor_x = rect.x.saturating_add(prefix_cols).saturating_add(query_cols);
        if cursor_x < rect.x + rect.width {
            frame.set_cursor_position((cursor_x, rect.y));
        }
    }
}

/// Build the one-line-per-entry fuzzy list body. Multi-line entries are
/// collapsed to their first line with a trailing ` ↵` marker so a long prompt
/// never breaks the single-row grid.
fn list_body<'a>(
    history: &'a [HistoryEntry],
    ranked: &'a [(usize, FuzzyMatch)],
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> Vec<Line<'static>> {
    let mut body: Vec<Line> = Vec::new();
    if history.is_empty() {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            " (no history yet — send a message to populate this list)",
            Style::default().fg(theme.muted()),
        )));
        return body;
    }
    if ranked.is_empty() {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            " (no matches — try a shorter or different query)",
            Style::default().fg(theme.muted()),
        )));
        return body;
    }

    // Row-number prefix " 123 " = 6 columns; the " ↵" continuation marker is
    // reserved 2 columns only when actually appended.
    const ROW_NUM_COLS: usize = 6;
    for (row, (orig_idx, m)) in ranked.iter().enumerate() {
        let is_selected = row == modal_index;
        let is_structured = theme.elevation.is_structured();
        let bg = if is_selected {
            theme.brand()
        } else {
            theme.panel()
        };
        let fg = if is_selected {
            contrast_fg(theme.brand())
        } else {
            theme.fg()
        };
        let (num_style, base_style, matched_style) = if is_selected {
            if is_structured {
                (
                    Style::default().add_modifier(Modifier::REVERSE),
                    Style::default().add_modifier(Modifier::REVERSE),
                    Style::default().add_modifier(Modifier::REVERSE | Modifier::UNDERLINED),
                )
            } else {
                (
                    Style::default().bg(bg).fg(contrast_fg(theme.brand())),
                    Style::default().bg(bg).fg(fg),
                    Style::default()
                        .bg(bg)
                        .fg(contrast_fg(theme.brand()))
                        .add_modifier(Modifier::UNDERLINED),
                )
            }
        } else {
            (
                Style::default().fg(theme.muted()),
                Style::default().bg(bg).fg(fg),
                Style::default()
                    .bg(bg)
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            )
        };

        let raw = history
            .get(*orig_idx)
            .map(|e| e.text.as_str())
            .unwrap_or("");
        // Collapse to a single line: take the first physical line and mark
        // continuation so a multi-line prompt reads as one row. The highlight
        // positions (computed against `raw`) map onto the first line since any
        // character past the first `\n` is dropped before truncation.
        let (first_line, multiline) = match raw.find('\n') {
            Some(i) => (&raw[..i], true),
            None => (raw, false),
        };
        // Reserve room for the continuation glyph before truncating so it
        // never lands outside the panel edge.
        let reserve = if multiline { 2 } else { 0 };
        let entry_max = body_width.saturating_sub(ROW_NUM_COLS + reserve);
        let entry = truncate_ellipsis(first_line, entry_max);
        let matched: std::collections::HashSet<usize> = m
            .positions
            .iter()
            .copied()
            .filter(|&p| p <= first_line.len())
            .collect();

        let mut spans: Vec<Span> = Vec::with_capacity(entry.chars().count() + 2);
        spans.push(Span::styled(format!(" {:>3} ", row + 1), num_style));
        for (char_idx, c) in entry.chars().enumerate() {
            let style = if matched.contains(&char_idx) {
                matched_style
            } else {
                base_style
            };
            spans.push(Span::styled(c.to_string(), style));
        }
        if multiline {
            let multi_style = if is_selected && is_structured {
                Style::default().add_modifier(Modifier::REVERSE)
            } else {
                Style::default().bg(bg).fg(num_style.fg)
            };
            spans.push(Span::styled(" ↵", multi_style));
        }
        body.push(Line::from(spans));
    }
    body
}
