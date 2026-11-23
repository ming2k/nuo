//! Tier 1: the client-global tab bar and its overflow window.
//!
//! The strip is the client's own workspace row (ADR-0039, ADR-0060, ADR-0062):
//! one pill per open tab, a sliding window that keeps the active tab in view,
//! and the `[!N]` fault badge pinned to the right edge. ADR-0235 gives the row
//! the three lifecycle affordances a tab strip owes its user:
//!
//! - **Close** — a hovered pill reveals a `✕` inside columns it already reserved,
//!   and only then mounts the hit target. A tab you cannot see yourself closing
//!   is a tab you click by accident, so the mark and its hit rect appear
//!   together.
//! - **Add** — a trailing `+` that is *reserved* out of the rail's budget, so a
//!   wall of long titles can never starve the strip's own creation affordance.
//! - **Hover reveal** — a title that overflowed its pill slides through a bounded
//!   window while the pointer rests on it, so the full title is reachable
//!   without widening the terminal. One pass, then rest: a parked pointer is not
//!   a permanent animation (`[INV-TAB-15]`).

use super::*;

use super::thread_bar::id_tail;
use crate::shell::{BAR_LEGEND_GAP_MIN, TABBAR_H_INSET};
use ui_primitives::components::overflow::{
    Overflow, fit_with, marquee_reveal_len, marquee_reveal_offset, window_max_offset,
};
use ui_primitives::design::{
    TAB_ADD_WIDTH, TAB_CLOSE_RESERVE, TAB_PILL_MIN_WIDTH, TAB_TITLE_MARQUEE_CADENCE,
    TAB_TITLE_REVEAL_MAX_TRAVEL, tab_close_glyph,
};

/// Columns a pill spends on its own edges: the padding pair on chromatic
/// surfaces, the bracket pair on framed ones. The title budget, the close
/// reserve, and the hit rect all derive from it, so it is stated once.
const TAB_PILL_CHROME_COLS: usize = 2;

fn overflow_indicators_width(hidden_left: usize, hidden_right: usize) -> (usize, usize, usize) {
    let left_w = if hidden_left > 0 {
        2 + hidden_left.to_string().len()
    } else {
        0
    };
    let right_w = if hidden_right > 0 {
        2 + hidden_right.to_string().len()
    } else {
        0
    };
    let overview_w = if hidden_left > 0 || hidden_right > 0 {
        4
    } else {
        0
    };
    (left_w, right_w, overview_w)
}

fn window_total_width(tab_widths: &[usize], start: usize, end: usize, total_tabs: usize) -> usize {
    let tabs_w: usize = tab_widths[start..=end].iter().sum::<usize>();
    let (left_w, right_w, overview_w) = overflow_indicators_width(start, total_tabs - 1 - end);
    tabs_w + left_w + right_w + overview_w
}

/// Determine the sliding window `[start..=end]` of tabs visible on the TabBar
/// such that `active` tab is guaranteed to be within the visible range.
pub(crate) fn compute_tab_window(
    tab_widths: &[usize],
    active: usize,
    available_w: usize,
) -> (usize, usize) {
    let total = tab_widths.len();
    if total == 0 {
        return (0, 0);
    }
    let active = active.min(total - 1);

    // If all tabs fit without overflow indicators:
    let all_w: usize = tab_widths.iter().sum::<usize>();
    if all_w <= available_w {
        return (0, total - 1);
    }

    let mut start = active;
    let mut end = active;

    loop {
        let mut expanded = false;

        let dist_left = active.saturating_sub(start);
        let dist_right = end.saturating_sub(active);
        let try_left_first = dist_left < dist_right;

        let order = if try_left_first {
            [(true, start > 0), (false, end + 1 < total)]
        } else {
            [(false, end + 1 < total), (true, start > 0)]
        };

        for (is_left, can_expand) in order {
            if !can_expand {
                continue;
            }
            let next_start = if is_left { start - 1 } else { start };
            let next_end = if !is_left { end + 1 } else { end };

            if window_total_width(tab_widths, next_start, next_end, total) <= available_w {
                start = next_start;
                end = next_end;
                expanded = true;
                break;
            }
        }

        if !expanded {
            break;
        }
    }

    (start, end)
}

/// The semantic label a tab projects, before any width budgeting.
fn tab_label(tab: &crate::shell::surfaces::ClientTab) -> String {
    match &tab.kind {
        crate::shell::surfaces::TabKind::Thread(id) => tab
            .display_title()
            .map(str::to_string)
            .unwrap_or_else(|| format!("thread#{}", id_tail(id))),
        crate::shell::surfaces::TabKind::Dashboard => "dashboard".to_string(),
        crate::shell::surfaces::TabKind::Settings => "settings".to_string(),
    }
}

/// Resolve a pill's style from the three facts the strip actually has.
///
/// One function so the states cannot drift: the hovered pill is the *same* pill
/// as the inactive one, lifted one step in the palette's own ladder
/// (`raised()`), never a new hue — a lift must not read as selection.
fn tab_pill_style(theme: &Theme, active: bool, hovered: bool, index: usize) -> Style {
    if !theme.elevation.is_chromatic() {
        // Framed surfaces have no background latitude (ADR-0053): the active
        // pill is reverse video, and hover is carried by weight plus the close
        // mark that appears inside the frame.
        return if active {
            Style::default().add_modifier(Modifier::REVERSE | Modifier::BOLD)
        } else if hovered {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_muted)
        };
    }
    if active {
        Style::default()
            .bg(theme.selected_bg)
            .fg(theme.heading())
            .add_modifier(Modifier::BOLD)
    } else if hovered {
        Style::default().bg(theme.raised()).fg(theme.hover())
    } else {
        let inactive_bg = if index.is_multiple_of(2) {
            theme.panel()
        } else {
            theme.body()
        };
        Style::default().bg(inactive_bg).fg(theme.text_muted)
    }
}

/// The close mark's own style: the pill's background with a brighter glyph, and
/// a brighter still one when the pointer is actually on the mark.
fn tab_close_style(theme: &Theme, pill: Style, close_hovered: bool) -> Style {
    if !theme.elevation.is_chromatic() {
        return if close_hovered {
            pill.add_modifier(Modifier::BOLD)
        } else {
            pill
        };
    }
    pill.fg(if close_hovered {
        theme.fg()
    } else {
        theme.hover()
    })
}

/// The trailing add-tab affordance's style.
fn tab_add_style(theme: &Theme, hovered: bool) -> Style {
    let base = if theme.elevation.is_chromatic() && hovered {
        Style::default().bg(theme.raised())
    } else {
        Style::default()
    };
    if hovered {
        base.fg(theme.fg()).add_modifier(Modifier::BOLD)
    } else {
        base.fg(theme.muted())
    }
}

/// Draw Tier 1: Client Global TabBar (ADR-0039, ADR-0040, ADR-0060 [INV-TAB-09],
/// ADR-0062, ADR-0219).
///
/// Projects open tabs with clean semantic titles without ordinal prefixes (`1:`),
/// sliding viewport auto-centering the active tab with bilateral overflow
/// guidance. The right edge carries the conditional `[!N]` fault badge — no
/// standing keycap (ADR-0219) — and the strip ends with its own `+` creation
/// affordance. Returns bounding rects for mouse click hit-testing, including the
/// hovered pill's close mark, and reports whether a hover title reveal is mid-pass
/// so the loop keeps its animation heartbeat alive.
pub(crate) fn draw_tab_bar(
    frame: &mut Frame,
    rect: Rect,
    tab_bar: &TabBarProps<'_>,
    theme: &Theme,
) -> TabBarNavRects {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH || rect.height == 0 {
        return TabBarNavRects::default();
    }

    let bg = theme.tabbar_bg();
    let fill = Style::default().bg(bg);
    // Tier 1 bleeds its tabbar_bg fill to both terminal edges so the top bar
    // forms a continuous frame line. Inside the row, TABBAR_H_INSET (1) leaves
    // 1 column of breathing room at each end so tab pills and the right-pinned
    // status cluster do not touch the terminal borders. Consecutive unselected
    // tabs butt seamlessly against each other without intervening separator gaps,
    // disambiguated by alternating background colors.
    let pad = TABBAR_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);
    let framed = !theme.elevation.is_chromatic();
    let ascii = theme.glyphs.border_v == "|";
    let ellipsis = theme.glyphs.ellipsis;
    let ellipsis_w = ellipsis.width();

    // Status Cluster (ADR-0131, amended by ADR-0219): the unread fault notice
    // badge (`[!N]`, present only when `unread_faults > 0`) is the row's sole
    // standing right-edge status. The constant `F1 help` keycap stays retired.
    let notice_badge_text =
        (tab_bar.unread_faults > 0).then(|| format!("!{}", tab_bar.unread_faults));
    let notice_badge_w = notice_badge_text.as_ref().map_or(0, |t| t.width() + 2); // "[!N]"
    let right_width = notice_badge_w;

    // A hover that names a pill the strip no longer paints (a close racing the
    // pointer's own motion report) is stale: it must paint nothing, not light up
    // a neighbour that moved into that slot.
    let hover = tab_bar.hover.filter(|hover| match hover.tab() {
        Some(index) => tab_bar.tabs.is_some_and(|tabs| index < tabs.len()),
        None => true,
    });
    let close_hovered_tab = hover
        .filter(|hover| hover.is_close())
        .and_then(TabBarHover::tab);

    let mut spans: Vec<Span<'_>> = vec![Span::styled(" ".repeat(pad), fill)];
    let mut current_x = rect.x.saturating_add(pad as u16);
    // Columns the strip itself has spent: brand or pills, overflow guidance, and
    // the add affordance. The right-pinned cluster is budgeted separately.
    let mut used_w = 0usize;
    let mut tab_rects: Vec<(usize, Rect)> = Vec::new();
    let mut close_rects: Vec<(usize, Rect)> = Vec::new();
    let mut prev_rect = None;
    let mut next_rect = None;
    let mut overview_rect = None;
    let mut reveal_active = false;

    // Tab workspace rendering (ADR-0039 [INV-TAB-01], ADR-0040 [INV-UI-01],
    // ADR-0060 [INV-TAB-09]).
    if let Some(tabs) = tab_bar.tabs.filter(|t| !t.is_empty()) {
        // The rail may fill the row, but a right-pinned status element keeps the
        // standing legend gap: a truncated overflow marker must never butt
        // against the fault badge. With no badge there is nothing to separate
        // from, so the gap is not reserved.
        let rail_gap = if right_width > 0 {
            BAR_LEGEND_GAP_MIN
        } else {
            0
        };
        // The add affordance is *reserved* here, not competed for above it:
        // creation belongs to the strip, so titles are what yield
        // (`[INV-TAB-17]`).
        let available_for_tabs = text_width
            .saturating_sub(right_width)
            .saturating_sub(rail_gap)
            .saturating_sub(TAB_ADD_WIDTH);
        let title_budget = adaptive_tab_title_width(tabs.len());

        let labels: Vec<String> = tabs.iter().map(tab_label).collect();
        // A pill's width is decided here, once, and **hover does not change it**:
        // chrome + the close reserve + the budgeted title. Reserving the close
        // columns at rest is what lets a hovered pill reveal its `✕` without
        // reflowing the strip or re-truncating a title (`[INV-TAB-16]`). The
        // floor then guarantees the reserve always fits, so no strip geometry
        // can produce a pill the pointer cannot close.
        let tab_widths: Vec<usize> = labels
            .iter()
            .map(|label| label.width().min(title_budget) + TAB_PILL_CHROME_COLS + TAB_CLOSE_RESERVE)
            .map(|width| width.max(TAB_PILL_MIN_WIDTH))
            .collect();
        let (start, end) = compute_tab_window(&tab_widths, tab_bar.active_tab, available_for_tabs);

        // Left overflow indicator: ‹N
        if start > 0 {
            let left_text = format!("‹{} ", start);
            let ind_w = left_text.width();
            let ind_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);

            let hit_start = current_x;
            let hit_end = current_x.saturating_add(ind_w as u16);
            if hit_start < rect.right() {
                let w = hit_end.min(rect.right()).saturating_sub(hit_start);
                if w > 0 {
                    prev_rect = Some(Rect::new(hit_start, rect.y, w, 1));
                }
            }

            spans.push(Span::styled(left_text, ind_style));
            used_w += ind_w;
            current_x = current_x.saturating_add(ind_w as u16);
        }

        for (i, label) in labels.iter().enumerate().skip(start).take(end - start + 1) {
            let width = tab_widths[i];
            let active = i == tab_bar.active_tab;
            let hovered = hover.is_some_and(|hover| hover.tab() == Some(i));
            let close_hovered = close_hovered_tab == Some(i);
            let style = tab_pill_style(theme, active, hovered, i);
            // The close mark occupies the reserve every pill carries, and is
            // painted only while the pointer is on the pill. The title column is
            // therefore the same width hovered or not.
            let close = hovered && width >= TAB_PILL_CHROME_COLS + TAB_CLOSE_RESERVE;
            let title_w = width - TAB_PILL_CHROME_COLS - TAB_CLOSE_RESERVE;

            // Hover is the only state that moves a title, and it moves for one
            // bounded pass toward the tail (`[INV-TAB-15]`): the offset is a
            // function of elapsed time, and when the pass ends the pill rests on
            // the tail it travelled to rather than snapping back to the head.
            let overflow = if hovered && label.width() > title_w {
                let max_offset = window_max_offset(label.width(), title_w, ellipsis_w)
                    .min(TAB_TITLE_REVEAL_MAX_TRAVEL);
                let pass = marquee_reveal_len(max_offset, TAB_TITLE_MARQUEE_CADENCE);
                // A pass that is still owed keeps the heartbeat alive; a pass
                // that is over rests at the tail it travelled to. Snapping back
                // to the truncated head would take away the text the user just
                // waited to read.
                if tab_bar.hover_elapsed_ms < pass {
                    reveal_active = true;
                }
                Overflow::Window {
                    offset: marquee_reveal_offset(
                        max_offset,
                        tab_bar.hover_elapsed_ms,
                        TAB_TITLE_MARQUEE_CADENCE,
                    ),
                }
            } else {
                Overflow::End
            };
            let fitted = fit_with(label, title_w, overflow, ellipsis);
            let title_pad = " ".repeat(title_w.saturating_sub(fitted.text.width()));

            let hit_start = current_x;
            let hit_end = current_x.saturating_add(width as u16);
            if hit_start < rect.right() {
                let w = hit_end.min(rect.right()).saturating_sub(hit_start);
                if w > 0 {
                    tab_rects.push((i, Rect::new(hit_start, rect.y, w, 1)));
                }
            }
            // The close mark sits one column in from the pill's trailing edge on
            // both surfaces: `" ✕ "` after the title on chromatic pills,
            // `" ✕]"` inside the frame on framed ones.
            if close {
                let close_x = current_x.saturating_add(width as u16 - 2);
                if close_x < rect.right() {
                    close_rects.push((i, Rect::new(close_x, rect.y, 1, 1)));
                }
            }

            let close_style = tab_close_style(theme, style, close_hovered);
            let close_glyph = tab_close_glyph(ascii);
            if framed {
                spans.push(Span::styled("[", style));
                spans.push(Span::styled(fitted.text, style));
                if !title_pad.is_empty() {
                    spans.push(Span::styled(title_pad, style));
                }
                if close {
                    spans.push(Span::styled(" ", style));
                    spans.push(Span::styled(close_glyph, close_style));
                } else {
                    spans.push(Span::styled("  ", style));
                }
                spans.push(Span::styled("]", style));
            } else {
                spans.push(Span::styled(" ", style));
                spans.push(Span::styled(fitted.text, style));
                if !title_pad.is_empty() {
                    spans.push(Span::styled(title_pad, style));
                }
                if close {
                    spans.push(Span::styled(" ", style));
                    spans.push(Span::styled(close_glyph, close_style));
                } else {
                    spans.push(Span::styled("  ", style));
                }
                spans.push(Span::styled(" ", style));
            }

            used_w += width;
            current_x = current_x.saturating_add(width as u16);
        }

        // Right overflow indicator: N›
        if end + 1 < tabs.len() {
            let hidden_right = tabs.len() - 1 - end;
            let right_text = format!(" {}›", hidden_right);
            let ind_w = right_text.width();
            let ind_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);

            let hit_start = current_x;
            let hit_end = current_x.saturating_add(ind_w as u16);
            if hit_start < rect.right() {
                let w = hit_end.min(rect.right()).saturating_sub(hit_start);
                if w > 0 {
                    next_rect = Some(Rect::new(hit_start, rect.y, w, 1));
                }
            }

            spans.push(Span::styled(right_text, ind_style));
            used_w += ind_w;
            current_x = current_x.saturating_add(ind_w as u16);
        }

        // Overflow overview dropdown button: [▾]
        if start > 0 || end + 1 < tabs.len() {
            let overview_text = " [▾]";
            let ind_w = overview_text.width();
            let overview_style = fill.fg(theme.heading()).add_modifier(Modifier::DIM);

            let hit_start = current_x;
            let hit_end = current_x.saturating_add(ind_w as u16);
            if hit_start < rect.right() {
                let w = hit_end.min(rect.right()).saturating_sub(hit_start);
                if w > 0 {
                    overview_rect = Some(Rect::new(hit_start, rect.y, w, 1));
                }
            }

            spans.push(Span::styled(overview_text, overview_style));
            used_w += ind_w;
            current_x = current_x.saturating_add(ind_w as u16);
        }
    } else {
        // Fallback: Brand mark. The add affordance still follows, so a client
        // with no tabs open can create one from the strip itself.
        spans.push(Span::styled(
            "nuo",
            fill.fg(theme.brand()).add_modifier(Modifier::BOLD),
        ));
        used_w += "nuo".width();
    }

    // Trailing add-tab affordance (ADR-0235 `[INV-TAB-17]`): the strip's own
    // creation verb, phrased as an affordance (`+`) rather than a chord legend,
    // so it does not reintroduce the standing keycap ADR-0219 retired.
    let add_width = TAB_ADD_WIDTH.min(text_width.saturating_sub(used_w + right_width));
    let add_rect =
        (add_width > 0).then(|| Rect::new(current_x, rect.y, add_width as u16, rect.height.min(1)));
    if add_width > 0 {
        let add_text = if framed { "[+]" } else { " + " };
        spans.push(Span::styled(
            fit_with(add_text, add_width, Overflow::End, ellipsis).text,
            tab_add_style(theme, hover == Some(TabBarHover::Add)),
        ));
        used_w += add_width;
    }

    let gap = text_width.saturating_sub(used_w + right_width);
    spans.push(Span::styled(" ".repeat(gap), fill));

    // Right-aligned status cluster: [!N] only.
    let mut notices_rect = None;
    let right_x = rect.x + pad as u16 + (used_w + gap) as u16;

    if let Some(ref text) = notice_badge_text {
        let label = format!("[{text}]");
        let lw = label.width() as u16;
        notices_rect = Some(Rect::new(right_x, rect.y, lw, 1));
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        ));
    }

    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(&mut Paragraph::new(Line::from(spans)).style(fill), rect);
    TabBarNavRects {
        tab_rects,
        close_rects,
        add_rect,
        prev_rect,
        next_rect,
        overview_rect,
        notices_rect,
        reveal_active,
    }
}
