//! Queue modal — the full outbox overview.
//!
//! The activity strip above the composer shows a one-line summary (count, next
//! item, paused state, key legend); this modal is its expand surface, opened by
//! clicking that row or pressing the queue chord (`Ctrl+Q`). It lists every
//! queued dispatch for the viewed session in dispatch order (front pops first),
//! each with its queued time and (truncated) text.
//!
//! Opening the modal **pauses** the session's queue server-side (the driver
//! ships nothing) so the list can be managed safely: `↑`/`↓` move the
//! highlight, `Enter` re-edits the *selected* item (pulls it back to the
//! composer), `D` deletes it, `J`/`K` reorder it, and `Ctrl+P` toggles the
//! pause. Closing the modal resumes normal shipping. `Esc` / outside-click
//! close.
//!
//! Every listed item is an outbox row. `Waiting` rows are the client's view of
//! the driver's `FollowUpQueue`; a `Steering` row is a busy-Enter steer waiting
//! in the running round's admission window (it is cancelled, never reordered).
//! An *admitted* steer is transcript-owned and never appears here (ADR-0137).

use nuotc::{
    Frame, {Line, Span},
};

use super::common::{placeholder, truncate_ellipsis};
use crate::shell::render::{QueueItemProps, Theme};
use nuo_ui_primitives::components::list::{
    SelectableListPage, draw_selectable_list_page, row_style,
};
use nuo_ui_primitives::components::modal::ModalHeader;
use nuo_ui_primitives::primitives::{ContentModalSpec, FooterHint, keyvocab};
use unicode_width::UnicodeWidthStr;

/// Inputs for [`draw_queue_modal`]. The items slice is the viewed session's
/// outbox in dispatch order.
pub struct QueueModalProps<'a> {
    pub items: &'a [QueueItemProps],
    /// `true` while the session's queue is paused server-side (the modal pauses
    /// it on open for safe editing; `Ctrl+P` / `/queue block` toggle it). The
    /// paused state is stated by the activity strip, so the title stays plain.
    pub blocked: bool,
}

/// Draw the queue overview modal. Each row carries the queued-at time and the
/// item text truncated to the row budget. The header is a plain `Queue` title
/// — count and paused state already live in the activity strip.
#[allow(clippy::too_many_arguments)]
pub fn draw_queue_modal(
    frame: &mut Frame,
    props: QueueModalProps<'_>,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
) -> nuotc::Rect {
    let body_width =
        nuo_ui_primitives::components::modal::modal_body_width(frame, ContentModalSpec::QUEUE);

    let QueueModalProps { items, blocked } = props;

    // Keep the modal title a plain `Follow-ups`: the count and the paused state
    // are already visible in the activity strip, so echoing them in the header
    // only adds noise. The footer still surfaces the pause toggle, where the
    // state matters to the action.
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
            // No stamp, no time column content: the gutter still reserves its
            // width so the label and text stay aligned across rows.
            let time =
                nuo_ui_primitives::time::optional_time_label(item.queued_at_ms).unwrap_or_default();
            let one_line = super::common::one_line(item.text.trim());
            let text = if one_line.width() > text_budget {
                truncate_ellipsis(&one_line, text_budget)
            } else {
                one_line
            };
            // A pending steer is a course correction of the running round,
            // distinct from a next-round follow-up (ADR-0137).
            let label = if item.is_steer { "steer" } else { "follow-up" };
            let left_w = GUTTER_W + TIME_W + 2 + label.len() + 1 + text.width();
            let pad = body_width.saturating_sub(left_w);
            body.push(Line::from(vec![
                Span::styled(" ".repeat(GUTTER_W), nuotc::Style::default().bg(style.bg)),
                Span::styled(
                    format!("{time}  "),
                    nuotc::Style::default().bg(style.bg).fg(style.dim),
                ),
                Span::styled(
                    label,
                    nuotc::Style::default().bg(style.bg).fg(if item.is_steer {
                        theme.warn()
                    } else {
                        style.dim
                    }),
                ),
                Span::styled(" ", nuotc::Style::default().bg(style.bg)),
                Span::styled(text, nuotc::Style::default().bg(style.bg).fg(style.fg)),
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
                FooterHint::primary(crate::shell::keymap::Key::ENTER.display(), "edit"),
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
                FooterHint::secondary("J/K", "reorder"),
                FooterHint::secondary(
                    crate::shell::keymap::Key::CTRL_P.display(),
                    if blocked { "resume" } else { "block" },
                ),
            ],
            empty_footer_hints: &[FooterHint::always(
                crate::shell::keymap::Key::ESC.display(),
                "close",
            )],
            extra_footer_hints: &[FooterHint::with_band("D", "delete", 70)],
        },
        theme,
    )
}
