//! Tier 1: the client-global tab bar and its overflow window.

use super::*;

use super::thread_bar::{id_tail, truncate_to_width};
use crate::shell::{BAR_LEGEND_GAP_MIN, TABBAR_H_INSET};

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

/// Draw Tier 1: Client Global TabBar (ADR-0039, ADR-0040, ADR-0060 [INV-TAB-09], ADR-0062).
///
/// Projects open tabs with clean semantic titles without ordinal prefixes (`1:`),
/// sliding viewport auto-centering the active tab with bilateral overflow guidance,
/// with the client command namespace (`C-x menu`) anchored to the right edge.
/// Returns bounding rects for mouse click hit-testing.
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

    // Status Cluster (ADR-0131 [INV-AFFORDANCE-01]):
    // 1. Unread fault notice badge (`[!N]`), if unread_faults > 0
    // 2. Sovereign Help affordance (`F1 help`)
    let notice_badge_text =
        (tab_bar.unread_faults > 0).then(|| format!("!{}", tab_bar.unread_faults));
    let notice_badge_w = notice_badge_text.as_ref().map_or(0, |t| t.width() + 2 + 1); // "[!N]" + " "
    let help_affordance = nuo_ui_primitives::components::keycap::KeyAffordance::tabbar_help("help");
    let help_w = help_affordance.width();
    let right_width = notice_badge_w + help_w;

    // Tab workspace rendering (ADR-0039 [INV-TAB-01], ADR-0040 [INV-UI-01], ADR-0060 [INV-TAB-09])
    if let Some(tabs) = tab_bar.tabs.filter(|t| !t.is_empty()) {
        // The tab rail may fill the row, but the status cluster keeps the
        // standing legend gap: a truncated `[▾]` must never butt against a
        // keycap.
        let available_for_tabs = text_width
            .saturating_sub(right_width)
            .saturating_sub(BAR_LEGEND_GAP_MIN);
        let title_budget = adaptive_tab_title_width(tabs.len());

        let tab_texts: Vec<String> = tabs
            .iter()
            .map(|tab| {
                let label = match &tab.kind {
                    crate::shell::surfaces::TabKind::Thread(id) => {
                        if let Some(title) = tab.display_title() {
                            let truncated = truncate_to_width(title, title_budget);
                            truncated.to_string()
                        } else {
                            format!("thread#{}", id_tail(id))
                        }
                    }
                    crate::shell::surfaces::TabKind::Dashboard => "dashboard".to_string(),
                    crate::shell::surfaces::TabKind::Settings => "settings".to_string(),
                };
                if theme.elevation.is_chromatic() {
                    format!(" {label} ")
                } else {
                    format!("[{label}]")
                }
            })
            .collect();

        let tab_widths: Vec<usize> = tab_texts.iter().map(|t| t.width()).collect();
        let (start, end) = compute_tab_window(&tab_widths, tab_bar.active_tab, available_for_tabs);

        let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
        let mut tab_rects = Vec::with_capacity(end - start + 1);
        let mut prev_rect = None;
        let mut next_rect = None;
        let mut overview_rect = None;

        let mut current_x = rect.x.saturating_add(pad as u16);
        let mut used_tab_w = 0usize;

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
            used_tab_w += ind_w;
            current_x = current_x.saturating_add(ind_w as u16);
        }

        for (i, (text, &tab_w)) in tab_texts
            .iter()
            .zip(tab_widths.iter())
            .enumerate()
            .skip(start)
            .take(end - start + 1)
        {
            let active = i == tab_bar.active_tab;

            let tab_style = if !theme.elevation.is_chromatic() {
                if active {
                    Style::default().add_modifier(Modifier::REVERSE | Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_muted)
                }
            } else if active {
                Style::default()
                    .bg(theme.selected_bg)
                    .fg(theme.heading())
                    .add_modifier(Modifier::BOLD)
            } else {
                let inactive_bg = if i % 2 == 0 {
                    theme.panel()
                } else {
                    theme.body()
                };
                Style::default().bg(inactive_bg).fg(theme.text_muted)
            };

            let hit_start = current_x;
            let hit_end = current_x.saturating_add(tab_w as u16);
            if hit_start < rect.right() {
                let w = hit_end.min(rect.right()).saturating_sub(hit_start);
                if w > 0 {
                    tab_rects.push((i, Rect::new(hit_start, rect.y, w, 1)));
                }
            }

            spans.push(Span::styled(text.clone(), tab_style));
            used_tab_w += tab_w;
            current_x = current_x.saturating_add(tab_w as u16);
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
            used_tab_w += ind_w;
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
            used_tab_w += ind_w;
        }

        let gap = text_width.saturating_sub(used_tab_w + right_width);
        spans.push(Span::styled(" ".repeat(gap), fill));

        // Right-aligned status cluster: [!N] + F1 help
        let mut notices_rect = None;
        let right_x = rect.x + pad as u16 + (used_tab_w + gap) as u16;
        let mut cur_x = right_x;

        if let Some(ref text) = notice_badge_text {
            let label = format!("[{text}]");
            let lw = label.width() as u16;
            notices_rect = Some(Rect::new(cur_x, rect.y, lw, 1));
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(theme.err())
                    .add_modifier(Modifier::BOLD)
                    .bg(bg),
            ));
            spans.push(Span::styled(" ", fill));
            cur_x += lw + 1;
        }

        let help_rect = Some(Rect::new(cur_x, rect.y, help_w as u16, 1));
        spans.extend(help_affordance.render_tabbar_spans(theme, bg));
        spans.push(Span::styled(" ".repeat(pad), fill));

        frame.render_widget(&mut Paragraph::new(Line::from(spans)).style(fill), rect);
        return TabBarNavRects {
            tab_rects,
            prev_rect,
            next_rect,
            overview_rect,
            notices_rect,
            help_rect,
        };
    }

    // Fallback: Brand mark + Status Cluster
    let brand = "nuo";
    let left_w = brand.width();
    let gap = text_width.saturating_sub(left_w + right_width);

    let mut spans = vec![
        Span::styled(" ".repeat(pad), fill),
        Span::styled(brand, fill.fg(theme.brand()).add_modifier(Modifier::BOLD)),
        Span::styled(" ".repeat(gap), fill),
    ];

    let mut notices_rect = None;
    let right_x = rect.x + pad as u16 + (left_w + gap) as u16;
    let mut cur_x = right_x;

    if let Some(ref text) = notice_badge_text {
        let label = format!("[{text}]");
        let lw = label.width() as u16;
        notices_rect = Some(Rect::new(cur_x, rect.y, lw, 1));
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        ));
        spans.push(Span::styled(" ", fill));
        cur_x += lw + 1;
    }

    let help_rect = Some(Rect::new(cur_x, rect.y, help_w as u16, 1));
    spans.extend(help_affordance.render_tabbar_spans(theme, bg));
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(&mut Paragraph::new(Line::from(spans)).style(fill), rect);
    TabBarNavRects {
        tab_rects: Vec::new(),
        prev_rect: None,
        next_rect: None,
        overview_rect: None,
        notices_rect,
        help_rect,
    }
}
