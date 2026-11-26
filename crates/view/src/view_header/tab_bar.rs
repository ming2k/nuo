//! Tier 1: the client-global tab bar and its overflow window.
//!
//! Strictly inverted (DIP): the strip is a pure container architecture that
//! renders domain-agnostic [`TabBarItem`] slots projected each frame by the
//! active scene inside each tab container. It never inspects `ClientTab`,
//! `TabKind`, `SceneKind`, or thread identifiers.
//!
//! The strip carries the client's workspace row (ADR-0039, ADR-0060, ADR-0062,
//! ADR-0235, ADR-0245):
//!
//! - **Aligned 2-column margins (`TABBAR_H_INSET = 2`)** — `tabbar_bg` bleeds
//!   edge-to-edge while interactive content aligns vertically with Tier 2
//!   (`SceneBar`) and the transcript stream.
//! - **Zero-column status indication** — `Running` sweeps a calm left-to-right
//!   text shimmer across stationary title glyphs, while `Attention`,
//!   `Succeeded`, and `Failed` recolor the title text in their semantic tones.
//!   Pill geometry and title positions never shift across status transitions
//!   (`[INV-TAB-16]`).
//! - **Fade-out overflow** — overflowing titles do not terminate in `…`; their
//!   outermost columns dissolve smoothly toward the pill's background color so
//!   every column goes to real title characters.
//! - **In-place close** — pills do not reserve blank columns at rest for a
//!   close button. On hover, `✕` replaces the rightmost title columns in place
//!   (without changing pill width or reflowing the strip) and only then mounts
//!   its hit target.
//! - **Add** — a trailing `+` that is *reserved* out of the rail's budget, so a
//!   wall of long titles can never starve the strip's own creation affordance.
//! - **Hover reveal** — a title that overflowed its pill at rest slides through
//!   a bounded window while the pointer rests on it, fading hidden edges into
//!   the pill background. One pass, then rest: a parked pointer is not a
//!   permanent animation (`[INV-TAB-15]`).

use super::*;

use graticule_canvas::Color;
use ui_primitives::components::overflow::{marquee_reveal_len, marquee_reveal_offset};
use ui_primitives::design::{
    BAR_LEGEND_GAP_MIN, TAB_ADD_WIDTH, TAB_CLOSE_WIDTH, TAB_HELP_GAP_MIN, TAB_PILL_MIN_WIDTH,
    TAB_SHIMMER_BAND_COLS, TAB_SHIMMER_CYCLE_MS, TAB_SHIMMER_MIN_SPAN_COLS, TAB_TITLE_FADE_WIDTH,
    TAB_TITLE_MARQUEE_CADENCE, TAB_TITLE_REVEAL_MAX_TRAVEL, TABBAR_H_INSET, tab_close_glyph,
};

/// Width-adaptive form of the TabBar's right-edge help portal (ADR-0245
/// `[INV-TAB-19]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabHelpForm {
    /// `{key} help`: painted only when all tabs fit without overflow and idle
    /// slack covers `BAR_LEGEND_GAP_MIN` plus the expanded label.
    Expanded,
    /// `{key}`: compact fallback under width or tab-count pressure.
    Compact,
    /// Omitted: `OpenHelp` is unbound or even the compact portal would starve
    /// the active tab window, `+`, or `[!N]`.
    Hidden,
}

fn help_portal_width(form: TabHelpForm, key_text: &str) -> usize {
    match form {
        TabHelpForm::Expanded => key_text.width() + " help".len(),
        TabHelpForm::Compact => key_text.width(),
        TabHelpForm::Hidden => 0,
    }
}

/// Style pair `(key_style, label_style)` for the right-edge help portal.
///
/// Mirrors [`tab_add_style`] so hover lifts quietly onto `theme.raised()`
/// without changing the portal's geometry.
fn tab_help_styles(theme: &Theme, bg: Color, hovered: bool) -> (Style, Style) {
    if !theme.elevation.is_chromatic() {
        return if hovered {
            let bold = Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD);
            (bold, bold)
        } else {
            (
                Style::default()
                    .fg(theme.muted())
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(theme.muted()),
            )
        };
    }
    if hovered {
        let raised = theme.raised();
        (
            Style::default()
                .bg(raised)
                .fg(theme.fg())
                .add_modifier(Modifier::BOLD),
            Style::default().bg(raised).fg(theme.fg()),
        )
    } else {
        (
            Style::default()
                .bg(bg)
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
            Style::default().bg(bg).fg(theme.keycap_label()),
        )
    }
}
use unicode_segmentation::UnicodeSegmentation;

/// Columns a pill spends on its own edges: the padding pair on chromatic
/// surfaces, the bracket pair on framed ones. The title budget and the hit
/// rect both derive from it, so it is stated once.
const TAB_PILL_CHROME_COLS: usize = 2;

/// One grapheme unit for tab-title slicing, with control graphemes flattened to
/// a single space so a title can never break the row it lands in.
struct TitleUnit<'a> {
    text: &'a str,
    width: usize,
}

fn title_units(text: &str) -> Vec<TitleUnit<'_>> {
    text.graphemes(true)
        .map(|g| {
            if g.chars().all(char::is_control) {
                TitleUnit {
                    text: " ",
                    width: 1,
                }
            } else {
                TitleUnit {
                    text: g,
                    width: g.width(),
                }
            }
        })
        .collect()
}

/// Fade blend factor in `[0.0, 0.92]` toward the pill background for a column
/// at `position` steps inside a `fade_w`-column fade edge (`1` = innermost
/// faded column, `fade_w` = outermost edge column).
///
/// Matches opencode's `fadeTitleColor` ramp (`0.20 → 0.44 → 0.68 → 0.92` for
/// the default 4-column edge) so overflowing titles dissolve smoothly into the
/// pill background instead of cutting into `…`.
fn title_fade_opacity(position: usize, fade_w: usize) -> f32 {
    if position == 0 || fade_w == 0 {
        return 0.0;
    }
    if fade_w == 1 {
        return 0.68;
    }
    0.20 + 0.72 * ((position - 1) as f32 / (fade_w - 1) as f32)
}

fn faded_title_style(style: Style, opacity: f32) -> Style {
    if opacity <= 0.0 {
        return style;
    }
    if let (fg @ Color::Rgb(..), bg @ Color::Rgb(..)) = (style.fg, style.bg) {
        style.fg(ui_primitives::theme::mix(fg, bg, opacity))
    } else {
        style.add_modifier(Modifier::DIM)
    }
}

/// Raised-cosine shimmer wave intensity in `[0.0, 1.0]` for a grapheme spanning
/// `[col_start, col_start + col_w)` within a visible title of `shown_w` columns.
///
/// The wave enters smoothly before column 0 and exits smoothly past the end of
/// the virtual span (`max(shown_w, TAB_SHIMMER_MIN_SPAN_COLS)`), giving a calm
/// left-to-right light pass with a brief quiet rest between sweeps.
fn shimmer_intensity(col_start: usize, col_w: usize, shown_w: usize, animation_ms: u64) -> f32 {
    if shown_w == 0 || col_w == 0 || TAB_SHIMMER_CYCLE_MS == 0 {
        return 0.0;
    }
    let span = (shown_w as f32).max(TAB_SHIMMER_MIN_SPAN_COLS);
    let band = TAB_SHIMMER_BAND_COLS;
    let total_travel = span + 2.0 * band;
    let phase = (animation_ms % TAB_SHIMMER_CYCLE_MS) as f32 / TAB_SHIMMER_CYCLE_MS as f32;
    let center = -band + phase * total_travel;
    let col_mid = col_start as f32 + (col_w as f32) * 0.5;
    let dist = (col_mid - center).abs();
    if dist < band {
        0.5 * (1.0 + (std::f32::consts::PI * dist / band).cos())
    } else {
        0.0
    }
}

/// Minimum WCAG 2.1 contrast ratio guaranteed for semantic status colors on any
/// pill background (dark, light `paper`, or custom user palettes).
const TAB_STATUS_MIN_CONTRAST: f32 = 3.5;

/// Ensure `preferred` semantic foreground remains legible against `pill_bg`
/// across all dark, light (`paper`), and custom palettes while preserving the
/// semantic hue as faithfully as possible.
fn tab_status_fg(pill_bg: Color, preferred: Color) -> Color {
    let (Color::Rgb(..), Color::Rgb(..)) = (pill_bg, preferred) else {
        return preferred;
    };
    if ui_primitives::theme::wcag_contrast_ratio(preferred, pill_bg) >= TAB_STATUS_MIN_CONTRAST {
        return preferred;
    }
    let polar = ui_primitives::primitives::contrast_fg(pill_bg);
    // Find the smallest mix toward `polar` that meets the contrast floor so
    // the semantic hue (green/red/amber/running) stays recognizable.
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for _ in 0..8 {
        let mid = 0.5 * (lo + hi);
        let candidate = ui_primitives::theme::mix(preferred, polar, mid);
        if ui_primitives::theme::wcag_contrast_ratio(candidate, pill_bg) >= TAB_STATUS_MIN_CONTRAST
        {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    ui_primitives::theme::mix(preferred, polar, hi)
}

/// Apply the per-grapheme shimmer style for a [`TabStatus::Running`] pill before
/// any overflow edge fade is blended.
fn shimmer_title_style(style: Style, theme: &Theme, active: bool, intensity: f32) -> Style {
    if !theme.elevation.is_chromatic() {
        return if intensity >= 0.5 {
            if active {
                style.add_modifier(Modifier::UNDERLINE)
            } else {
                style.add_modifier(Modifier::BOLD)
            }
        } else {
            style
        };
    }

    let pill_bg = match style.bg {
        Color::Reset => theme.tabbar_bg(),
        bg => bg,
    };
    let pill_fg = match style.fg {
        Color::Reset => theme.fg(),
        fg => fg,
    };
    let peak = tab_status_fg(pill_bg, theme.running());
    let fg = match (pill_fg, peak) {
        (Color::Rgb(..), Color::Rgb(..)) => {
            // Retain a subtle 24% running tint between wave passes so the tab
            // still reads as active during the brief pause before the next sweep.
            let base = tab_status_fg(pill_bg, ui_primitives::theme::mix(pill_fg, peak, 0.24));
            ui_primitives::theme::mix(base, peak, intensity)
        }
        _ => peak,
    };
    style.fg(fg)
}

/// Paint a tab title into `budget` columns starting `offset` columns in,
/// applying optional `Running` text shimmer first and fading any overflowing
/// edge toward the pill's background color instead of emitting `…`, then
/// padding any remaining columns with spaces in `pill_style`.
#[allow(clippy::too_many_arguments)]
fn push_faded_title_spans<'a>(
    spans: &mut Vec<Span<'a>>,
    units: &[TitleUnit<'_>],
    budget: usize,
    offset: usize,
    title_style: Style,
    pill_style: Style,
    shimmer: Option<(&Theme, bool, u64)>,
) {
    if budget == 0 {
        return;
    }

    let mut skipped_w = 0usize;
    let mut start_idx = 0usize;
    if offset > 0 {
        for (i, u) in units.iter().enumerate() {
            if skipped_w + u.width > offset {
                start_idx = i;
                break;
            }
            skipped_w += u.width;
            start_idx = i + 1;
        }
    }

    let mut shown: Vec<(&str, usize, usize)> = Vec::new();
    let mut shown_w = 0usize;
    let mut end_idx = start_idx;
    for (i, u) in units.iter().enumerate().skip(start_idx) {
        if shown_w + u.width > budget {
            break;
        }
        shown.push((u.text, shown_w, u.width));
        shown_w += u.width;
        end_idx = i + 1;
    }

    let has_hidden_head = start_idx > 0;
    let has_hidden_tail = end_idx < units.len();
    let fade_w = if !has_hidden_head && !has_hidden_tail {
        0
    } else if has_hidden_head && has_hidden_tail {
        TAB_TITLE_FADE_WIDTH.min(shown_w.saturating_sub(1) / 2)
    } else if shown_w > TAB_TITLE_FADE_WIDTH {
        TAB_TITLE_FADE_WIDTH
    } else {
        shown_w / 2
    };

    let mut pending = String::new();
    let mut pending_style: Option<Style> = None;
    for &(grapheme, col_start, col_w) in &shown {
        let col_end = col_start + col_w;
        let lead_pos = if has_hidden_head && col_start < fade_w {
            fade_w - col_start
        } else {
            0
        };
        let tail_pos = if has_hidden_tail {
            let dist = shown_w.saturating_sub(col_end);
            fade_w.saturating_sub(dist)
        } else {
            0
        };
        let opacity =
            title_fade_opacity(lead_pos, fade_w).max(title_fade_opacity(tail_pos, fade_w));
        let base_cell_style = match shimmer {
            Some((theme, active, animation_ms)) => {
                let intensity = shimmer_intensity(col_start, col_w, shown_w, animation_ms);
                shimmer_title_style(title_style, theme, active, intensity)
            }
            None => title_style,
        };
        let cell_style = faded_title_style(base_cell_style, opacity);
        if pending_style == Some(cell_style) {
            pending.push_str(grapheme);
        } else {
            if let Some(prev_style) = pending_style.replace(cell_style) {
                spans.push(Span::styled(std::mem::take(&mut pending), prev_style));
            }
            pending.push_str(grapheme);
        }
    }
    if let Some(last_style) = pending_style {
        spans.push(Span::styled(pending, last_style));
    }

    let pad_w = budget.saturating_sub(shown_w);
    if pad_w > 0 {
        spans.push(Span::styled(" ".repeat(pad_w), pill_style));
    }
}

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

/// Resolve a pill's structural style from selection, hover, and alternation.
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

/// Resolve the title text's semantic style from the pill's structural style and
/// its scene-projected [`TabStatus`].
///
/// Status is communicated strictly through text color (and text shimmer for
/// stationary [`TabStatus::Running`] titles), spending zero horizontal columns
/// so pill widths and title offsets never shift (`[INV-TAB-16]`). When
/// `steady_running` is true (e.g. while hover-reveal is sliding an overflowing
/// title), `Running` paints in a steady contrast-safe running tone so text
/// motion and shimmer never clash optically.
fn tab_title_style(theme: &Theme, pill: Style, status: TabStatus, steady_running: bool) -> Style {
    if !theme.elevation.is_chromatic() {
        return match status {
            TabStatus::Idle => pill,
            TabStatus::Running => {
                if steady_running {
                    pill.add_modifier(Modifier::UNDERLINE)
                } else {
                    pill
                }
            }
            TabStatus::Attention => pill
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINE),
            TabStatus::Succeeded => pill.fg(theme.ok()),
            TabStatus::Failed => pill.fg(theme.err()).add_modifier(Modifier::UNDERLINE),
        };
    }
    let pill_bg = match pill.bg {
        Color::Reset => theme.tabbar_bg(),
        bg => bg,
    };
    match status {
        TabStatus::Idle => pill,
        TabStatus::Running => {
            if steady_running {
                pill.fg(tab_status_fg(pill_bg, theme.running()))
            } else {
                pill
            }
        }
        TabStatus::Attention => pill
            .fg(tab_status_fg(pill_bg, theme.warn()))
            .add_modifier(Modifier::BOLD),
        TabStatus::Succeeded => pill.fg(tab_status_fg(pill_bg, theme.ok())),
        TabStatus::Failed => pill.fg(tab_status_fg(pill_bg, theme.err())),
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
/// ADR-0062, ADR-0235, ADR-0245).
///
/// Pure container renderer (DIP): accepts scene-projected [`TabBarItem`] slices
/// without inspecting concrete tab or scene kinds. Renders a sliding viewport
/// auto-centering the active tab with bilateral overflow guidance, 2-column
/// horizontal end margins aligned with Tier 2 (`TABBAR_H_INSET`), zero-column
/// status styling (text shimmer for `Running`, semantic text colors for
/// `Attention` / `Succeeded` / `Failed`), right-edge `[!N]` fault badge,
/// yield-first elastic help portal, and trailing `+` creation affordance.
pub fn draw_tab_bar(
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
    // forms a continuous frame line. Inside the row, TABBAR_H_INSET (2) aligns
    // the tab pills and right-pinned status cluster vertically with Tier 2
    // (SceneBar) and the transcript stream. Consecutive unselected tabs butt
    // seamlessly against each other without intervening separator gaps,
    // disambiguated by alternating background colors.
    let pad = TABBAR_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);
    let framed = !theme.elevation.is_chromatic();
    let ascii = theme.glyphs.border_v == "|";

    // Precompute tab pill widths once so both the elastic help portal decision
    // and the sliding-window pass measure the exact same pills.
    let active_items = (!tab_bar.items.is_empty()).then_some(tab_bar.items);
    let label_units: Vec<Vec<TitleUnit<'_>>> = active_items
        .map(|items| {
            items
                .iter()
                .map(|item| title_units(item.title.as_ref()))
                .collect()
        })
        .unwrap_or_default();
    let label_widths: Vec<usize> = label_units
        .iter()
        .map(|units| units.iter().map(|u| u.width).sum())
        .collect();
    // A pill's width is decided here, once, and **neither status nor hover
    // changes it**: chrome + the budgeted title, with no blank columns reserved
    // for status marks or close buttons at rest. On hover, the close mark
    // replaces the rightmost title columns in place so the strip never reflows
    // (`[INV-TAB-16]`). The floor guarantees the close slot always fits beside
    // a legible title.
    let tab_widths: Vec<usize> = if let Some(items) = active_items {
        let title_budget = adaptive_tab_title_width(items.len());
        label_widths
            .iter()
            .map(|&label_w| label_w.min(title_budget) + TAB_PILL_CHROME_COLS)
            .map(|width| width.max(TAB_PILL_MIN_WIDTH))
            .collect()
    } else {
        Vec::new()
    };

    // Status Cluster (ADR-0131, amended by ADR-0219 and ADR-0245 `[INV-TAB-19]`):
    // optional unread-fault badge (`[!N]`) plus a yield-first elastic help
    // portal resolved from the live keymap (`Option<Key>`).
    let notice_badge_text =
        (tab_bar.unread_faults > 0).then(|| format!("!{}", tab_bar.unread_faults));
    let notice_badge_w = notice_badge_text.as_ref().map_or(0, |t| t.width() + 2); // "[!N]"
    let help_key_text = tab_bar
        .help_key
        .as_ref()
        .map(ui_primitives::chord::Key::display);

    let cluster_metrics = |form: TabHelpForm, key_text: Option<&str>| -> (usize, usize) {
        let help_w = key_text.map_or(0, |k| help_portal_width(form, k));
        let sep = if notice_badge_w > 0 && help_w > 0 {
            JOIN_ENUMERATE_COLS
        } else {
            0
        };
        let right_w = notice_badge_w + sep + help_w;
        let gap = if notice_badge_w > 0 || matches!(form, TabHelpForm::Expanded) {
            BAR_LEGEND_GAP_MIN
        } else if help_w > 0 {
            TAB_HELP_GAP_MIN
        } else {
            0
        };
        (right_w, gap)
    };

    let help_form = match help_key_text {
        None => TabHelpForm::Hidden,
        Some(key_text) => {
            let (expanded_right_w, expanded_gap) =
                cluster_metrics(TabHelpForm::Expanded, Some(key_text));
            let (compact_right_w, compact_gap) =
                cluster_metrics(TabHelpForm::Compact, Some(key_text));

            let (natural_left_w, min_left_w) = if let Some(items) = active_items {
                let active = tab_bar.active_tab.min(items.len() - 1);
                let all_w: usize = tab_widths.iter().sum::<usize>();
                let min_window_w = window_total_width(&tab_widths, active, active, items.len());
                (all_w + TAB_ADD_WIDTH, min_window_w + TAB_ADD_WIDTH)
            } else {
                let brand_w = "nuo".width() + TAB_ADD_WIDTH;
                (brand_w, brand_w)
            };

            if natural_left_w + expanded_gap + expanded_right_w <= text_width {
                TabHelpForm::Expanded
            } else if min_left_w + compact_gap + compact_right_w <= text_width {
                TabHelpForm::Compact
            } else {
                TabHelpForm::Hidden
            }
        }
    };
    let (right_width, rail_gap) = cluster_metrics(help_form, help_key_text);

    // A hover that names a pill the strip no longer paints (a close racing the
    // pointer's own motion report) is stale: it must paint nothing, not light up
    // a neighbour that moved into that slot.
    let hover = tab_bar.hover.filter(|hover| match hover.tab() {
        Some(index) => index < tab_bar.items.len(),
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
    let mut shimmer_active = false;

    // Tab workspace rendering (ADR-0039 [INV-TAB-01], ADR-0040 [INV-UI-01],
    // ADR-0060 [INV-TAB-09]).
    if let Some(items) = active_items {
        // The add affordance is *reserved* here, not competed for above it:
        // creation belongs to the strip, so titles are what yield
        // (`[INV-TAB-17]`).
        let available_for_tabs = text_width
            .saturating_sub(right_width)
            .saturating_sub(rail_gap)
            .saturating_sub(TAB_ADD_WIDTH);
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

        for i in start..=end {
            let units = &label_units[i];
            let label_w = label_widths[i];
            let width = tab_widths[i];
            let active = i == tab_bar.active_tab;
            let hovered = hover.is_some_and(|hover| hover.tab() == Some(i));
            let close_hovered = close_hovered_tab == Some(i);
            let style = tab_pill_style(theme, active, hovered, i);
            let status = items[i].status;

            // Every interior column of a resting pill goes to the title:
            // live status and unseen outcomes cost zero columns. On hover, the
            // close affordance replaces `TAB_CLOSE_WIDTH` columns from the
            // trailing edge in place so pill geometry never reflows (`[INV-TAB-16]`).
            let unhovered_title_w = width - TAB_PILL_CHROME_COLS;
            let close = hovered && unhovered_title_w >= TAB_CLOSE_WIDTH;
            let title_w = if close {
                unhovered_title_w - TAB_CLOSE_WIDTH
            } else {
                unhovered_title_w
            };

            // Hover is the only state that moves a title, and only for titles
            // that actually overflowed at rest (`label_w > unhovered_title_w`): a
            // fitting title simply yields its rightmost columns to the close
            // mark (fading the clipped edge) without jittering. For overflowing
            // titles it slides one bounded pass toward the tail (`[INV-TAB-15]`)
            // and rests there rather than snapping back to the head.
            let hovering_overflow = hovered && label_w > unhovered_title_w;
            let title_style = tab_title_style(theme, style, status, hovering_overflow);
            // Pause shimmer while hover-reveal is sliding an overflowing title
            // so horizontal glyph stepping and light-wave sweep never clash.
            let shimmer = (status == TabStatus::Running && !hovering_overflow).then_some((
                theme,
                active,
                tab_bar.animation_ms,
            ));
            if shimmer.is_some() {
                shimmer_active = true;
            }
            let offset = if hovering_overflow {
                let max_offset = label_w
                    .saturating_sub(title_w)
                    .min(TAB_TITLE_REVEAL_MAX_TRAVEL);
                let pass = marquee_reveal_len(max_offset, TAB_TITLE_MARQUEE_CADENCE);
                if tab_bar.hover_elapsed_ms < pass {
                    reveal_active = true;
                }
                marquee_reveal_offset(
                    max_offset,
                    tab_bar.hover_elapsed_ms,
                    TAB_TITLE_MARQUEE_CADENCE,
                )
            } else {
                0
            };

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
            let (lead_chrome, trail_chrome) = if framed { ("[", "]") } else { (" ", " ") };
            spans.push(Span::styled(lead_chrome, style));
            push_faded_title_spans(
                &mut spans,
                units,
                title_w,
                offset,
                title_style,
                style,
                shimmer,
            );
            if close {
                spans.push(Span::styled(" ", style));
                spans.push(Span::styled(close_glyph, close_style));
            }
            spans.push(Span::styled(trail_chrome, style));

            used_w += width;
            current_x = current_x.saturating_add(width as u16);
        }

        // Right overflow indicator: N›
        if end + 1 < items.len() {
            let hidden_right = items.len() - 1 - end;
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
        if start > 0 || end + 1 < items.len() {
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
        let add_text = match (framed, add_width) {
            (true, w) if w >= 3 => "[+]",
            (false, w) if w >= 3 => " + ",
            (_, 2) => " +",
            _ => "+",
        };
        spans.push(Span::styled(
            add_text,
            tab_add_style(theme, hover == Some(TabBarHover::Add)),
        ));
        used_w += add_width;
    }

    let gap = text_width.saturating_sub(used_w + right_width);
    spans.push(Span::styled(" ".repeat(gap), fill));

    // Right-aligned status cluster: optional `[!N]` fault badge followed by the
    // yield-first elastic help portal (ADR-0245 `[INV-TAB-19]`, `[INV-TAB-20]`).
    let mut notices_rect = None;
    let mut help_rect = None;
    let mut cluster_x = rect.x + pad as u16 + (used_w + gap) as u16;

    if let Some(ref text) = notice_badge_text {
        let label = format!("[{text}]");
        let lw = label.width() as u16;
        notices_rect = Some(Rect::new(cluster_x, rect.y, lw, 1));
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        ));
        cluster_x = cluster_x.saturating_add(lw);
    }

    if help_form != TabHelpForm::Hidden
        && let Some(key_text) = help_key_text
    {
        if notice_badge_text.is_some() {
            spans.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), fill));
            cluster_x = cluster_x.saturating_add(JOIN_ENUMERATE_COLS as u16);
        }
        let help_hovered = hover == Some(TabBarHover::Help);
        let (key_style, label_style) = tab_help_styles(theme, bg, help_hovered);
        let hw = help_portal_width(help_form, key_text) as u16;
        help_rect = Some(Rect::new(cluster_x, rect.y, hw, 1));
        spans.push(Span::styled(key_text, key_style));
        if help_form == TabHelpForm::Expanded {
            spans.push(Span::styled(" help", label_style));
        }
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
        help_rect,
        reveal_active,
        shimmer_active,
    }
}
