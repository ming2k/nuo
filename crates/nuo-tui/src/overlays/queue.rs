//! Queue modal — the full outbox overview.
//!
//! The persistent queue bar below the transcript gap shows a one-line summary
//! (identity, next-item preview, key legend); this modal is its expand
//! surface, opened by clicking that bar or pressing `Ctrl+Q`. It lists every
//! queued dispatch for the viewed session in dispatch order (front pops
//! first), each with its queued time and (truncated) text.
//!
//! Opening the modal **auto-blocks** the outbox (no message auto-drains) so
//! the list can be managed safely: `↑`/`↓` move the highlight, `Enter` re-edits
//! the *selected* item (pulls it back to the composer), `D` deletes it, `J`/`K`
//! reorder it, and `Ctrl+P` toggles the persistent block. Closing the modal
//! (Esc / outside-click) resumes normal auto-drain. `Esc` / outside-click
//! close.
//!
//! Every listed item is a **next-round** entry: either a follow-up staged
//! while a round runs, or a busy-Enter steer whose round ended before
//! admission. A live steer is transcript-owned — it
//! renders in the scrollback as a `⏸ Queued` entry and never appears here;
//! if its round closes first it is handed back into this list and becomes
//! manageable like any other item.

use nuotc::{
    Frame, {Line, Span},
};

use super::common::{placeholder, truncate_ellipsis};
use crate::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::components::modal::ModalHeader;
use crate::primitives::{ContentModalSpec, FooterHint, keyvocab};
use crate::render::{QueueItemProps, Theme};
use unicode_width::UnicodeWidthStr;

/// Inputs for [`draw_queue_modal`]. The items slice is the viewed session's
/// outbox in dispatch order.
pub struct QueueModalProps<'a> {
    pub items: &'a [QueueItemProps],
    /// `true` while the outbox is hard-blocked (the modal auto-blocks on open
    /// for safe editing; `F3` toggles a persistent block). The block state is
    /// explicit in the persistent queue bar, so the modal title stays plain.
    pub blocked: bool,
}

/// Draw the queue overview modal. Each row carries the queued-at time and the
/// item text truncated to the row budget. The header is a plain `Queue` title
/// — count and held-back state already live in the queue bar.
#[allow(clippy::too_many_arguments)]
pub fn draw_queue_modal(
    frame: &mut Frame,
    props: QueueModalProps<'_>,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
) -> nuotc::Rect {
    let body_width = crate::components::modal::modal_body_width(frame, ContentModalSpec::QUEUE);

    let QueueModalProps { items, blocked } = props;

    // Keep the modal title a plain `Follow-ups`: the count and the paused/blocked
    // state are already visible in the persistent queue bar, so echoing them
    // in the header only adds noise. The footer still surfaces `F3 block /
    // resume` where the block state matters.
    let title = "Follow-ups";

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if items.is_empty() {
        body.push(placeholder(
            "No follow-ups queued. Press Enter while the agent runs to stage one.",
            true,
            theme.muted(),
        ));
    } else {
        // Column geometry: gutter + time(HH:MM = 5) + 2-col gap + text.
        // Selection is shown via the existing background row highlight,
        // matching the other list modals.
        const GUTTER_W: usize = 2;
        const TIME_W: usize = 5; // HH:MM
        let text_budget = body_width.saturating_sub(GUTTER_W + TIME_W + 2).max(1);

        for (i, item) in items.iter().enumerate() {
            let is_sel = i == modal_index;
            let style = row_style(is_sel, theme);
            if is_sel {
                selected_line = Some(body.len());
            }
            let time = crate::time::sent_time_label(item.queued_at_ms);
            let one_line = super::common::one_line(item.text.trim());
            let text = if one_line.width() > text_budget {
                truncate_ellipsis(&one_line, text_budget)
            } else {
                one_line
            };
            let left_w = GUTTER_W + TIME_W + 2 + text.width();
            let pad = body_width.saturating_sub(left_w);
            body.push(Line::from(vec![
                Span::styled(
                    " ".repeat(GUTTER_W),
                    nuotc::Style::default().bg(style.bg),
                ),
                Span::styled(
                    format!("{time}  "),
                    nuotc::Style::default().bg(style.bg).fg(style.dim),
                ),
                Span::styled(
                    text,
                    nuotc::Style::default().bg(style.bg).fg(style.fg),
                ),
                Span::styled(" ".repeat(pad), nuotc::Style::default().bg(style.bg)),
            ]));
        }
    }

    let has_items = !items.is_empty();
    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: ContentModalSpec::QUEUE,
            header: ModalHeader::title(title),
            lines: body,
            scroll,
            selected_line,
            follow_selection,
            has_items,
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::key_primary(crate::keymap::Key::ENTER, "edit"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
                FooterHint::secondary("J/K", "reorder"),
                FooterHint::key_secondary(
                    crate::keymap::Key::CTRL_P,
                    if blocked { "resume" } else { "block" },
                ),
            ],
            empty_footer_hints: &[FooterHint::key_always(crate::keymap::Key::ESC, "close")],
            extra_footer_hints: &[FooterHint::with_band("D", "delete", 70)],
        },
        theme,
    )
}
