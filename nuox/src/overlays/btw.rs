//! `/btw` asides modal (ADR-0103 §5) — the live background-asides list.
//!
//! Opened by `F5` or `/btw list`. Every live aside conversation appears as
//! one row, newest first: its title (first prompt), a running/idle state
//! badge, and its last-activity time. `Enter` jumps back into the selected
//! aside (the harness answers with `SideViewOpened` carrying the full
//! transcript back-fill); `D` closes and discards it outright — cancels its
//! round, removes it from the list, and deletes its session files; `F5`
//! re-queries the list in place.
//!
//! The list is a display mirror of the harness's aside registry, pushed on
//! every mutation and on `QueryBtwList` — the modal never guesses.

use nuotc::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{placeholder, truncate_ellipsis};
use crate::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::components::modal::ModalHeader;
use crate::primitives::{ContentModalSpec, FooterHint, keyvocab};
use crate::render::Theme;

/// Inputs for [`draw_btw_modal`]. `asides` is the mirrored
/// [`nuo_contracts::BtwAsideSummary`] list, newest first; `running` is a
/// parallel per-row liveness vector (derived from the TUI's per-session
/// running set, fresher than the list snapshot); `active_id` is the
/// currently-viewed aside (rendered with an `open` marker), if any.
pub struct BtwModalProps<'a> {
    pub asides: &'a [nuo_contracts::BtwAsideSummary],
    pub running: &'a [bool],
    pub active_id: Option<&'a str>,
}

/// Draw the asides list modal. Rows show `run`/`open` badges, the title, and
/// a relative last-activity label; selection uses the shared list row
/// highlight.
#[allow(clippy::too_many_arguments)] // showcase parity with other modal renderers
pub fn draw_btw_modal(
    frame: &mut Frame,
    props: BtwModalProps<'_>,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
    _selection: &crate::model::selection::SelectionState,
    _layout_map: &mut crate::model::layout::LayoutMap,
) -> nuotc::Rect {
    let body_width = crate::components::modal::modal_body_width(frame, ContentModalSpec::BTW);

    let BtwModalProps {
        asides,
        running,
        active_id,
    } = props;

    let title = if asides.is_empty() {
        "Asides".to_string()
    } else {
        format!("Asides ({})", asides.len())
    };

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if asides.is_empty() {
        body.push(placeholder(
            "No /btw asides. Use /btw to open one — it keeps running when you leave.",
            true,
            theme.muted(),
        ));
    } else {
        // Column geometry: gutter + badge (4 = "run " / "open" / "    ")
        // + relative time (6, e.g. "2m ago") + 2-col gap + title.
        const GUTTER_W: usize = 2;
        const BADGE_W: usize = 4;
        const TIME_W: usize = 6;
        let text_budget = body_width
            .saturating_sub(GUTTER_W + BADGE_W + TIME_W + 2)
            .max(1);

        for (i, aside) in asides.iter().enumerate() {
            let is_sel = i == modal_index;
            let style = row_style(is_sel, theme);
            if is_sel {
                selected_line = Some(body.len());
            }
            // Badge: the in-flight round is the state that matters most, then
            // the fact the user is standing in it, else quiet blank space so
            // idle rows stay visually calm.
            let is_open = active_id == Some(aside.id.as_str());
            let is_running = running.get(i).copied().unwrap_or(aside.running);
            let (badge, badge_color) = if is_running {
                ("run ", theme.brand())
            } else if is_open {
                ("open", theme.primary)
            } else {
                ("    ", style.dim)
            };
            let rel = relative_time_short(aside.updated_at);
            let one_line = super::common::one_line(aside.title.trim());
            let text = if one_line.width() > text_budget {
                truncate_ellipsis(&one_line, text_budget)
            } else {
                one_line
            };
            let left_w = GUTTER_W + BADGE_W + TIME_W + 2 + text.width();
            let pad = body_width.saturating_sub(left_w);
            body.push(Line::from(vec![
                Span::styled(" ".repeat(GUTTER_W), Style::default().bg(style.bg)),
                Span::styled(
                    format!("{badge:<BADGE_W$}"),
                    Style::default().bg(style.bg).fg(badge_color),
                ),
                Span::styled(
                    format!("{rel}  "),
                    Style::default().bg(style.bg).fg(style.dim),
                ),
                Span::styled(text, Style::default().bg(style.bg).fg(style.fg)),
                Span::styled(" ".repeat(pad), Style::default().bg(style.bg)),
            ]));
        }
    }

    let has_items = !asides.is_empty();
    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: ContentModalSpec::BTW,
            header: ModalHeader::title(&title),
            lines: body,
            scroll,
            selected_line,
            follow_selection,
            has_items,
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::key_primary(crate::keymap::Key::ENTER, "open"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
                FooterHint::key_secondary(crate::keymap::Key::F5, "refresh"),
            ],
            empty_footer_hints: &[FooterHint::key_always(crate::keymap::Key::ESC, "close")],
            // Destructive close sits at band 70 (outlives secondaries, never
            // the always-keep close) — the same convention as Connections /
            // Sessions / Queue deletes.
            extra_footer_hints: &[FooterHint::with_band("D", "close aside", 70)],
        },
        theme,
    )
}

/// Compact relative-time label for the asides rows: `now`, `Nm`, `Nh`, `Nd`,
/// falling back to the raw epoch when the clock reads before the timestamp.
fn relative_time_short(epoch_seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(epoch_seconds);
    if delta < 60 {
        "now".to_string()
    } else if delta < 3600 {
        format!("{}m", delta / 60)
    } else if delta < 86_400 {
        format!("{}h", delta / 3600)
    } else {
        format!("{}d", delta / 86_400)
    }
}
