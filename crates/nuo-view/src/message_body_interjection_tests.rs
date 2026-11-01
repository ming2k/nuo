//! ADR-0137 rendering contract: the turn-seam interjection is a *one-body-row*
//! block, and the weight decision is driven by admission provenance
//! (`SteerOriginTag`), never by the `origin` vocabulary alone.
//!
//! These live in `nuo-view` so the geometry claim is testable without the
//! shell's event loop: the block's row count is a property of the message body
//! renderer, not of any application state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_ui_primitives::Theme;
use nuotc::{Rect, TestTerminal};

use crate::model::document::{SteerOriginTag, TranscriptMessage, UserMessageOrigin};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::text_layout::BlockWrapCache;

/// Render one message once and return the painted rows plus the renderer's own
/// content-height counter.
///
/// `TestTerminal::draw` may run the closure more than once (its harness can
/// repaint), so the counter is accumulated per invocation and the *first*
/// invocation's value is the measurement — re-running would double-count.
fn render_rows(message: &TranscriptMessage, width: u16, height: u16) -> (Vec<String>, usize) {
    let theme = Theme::default();
    let mut terminal = TestTerminal::new(width, height);
    let mut heights: Vec<usize> = Vec::new();

    terminal.draw(|frame| {
        let area = frame.area();
        let mut layout_map = LayoutMap::new();
        let selection = SelectionState::None;
        let mut wrap = BlockWrapCache::default();
        let mut current_y = 0u16;
        let mut skip_rows = 0usize;
        let mut content_lines = 0usize;
        crate::message_body::draw_message_body(
            frame,
            Rect::new(area.x, area.y, area.width, area.height),
            message,
            0,
            &selection,
            None,
            &theme,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            false,
            &mut wrap,
        );
        heights.push(content_lines);
    });

    let buffer = terminal.buffer();
    let rows = (0..buffer.area().height)
        .map(|row| {
            (0..buffer.area().width)
                .map(|col| buffer.get(col, row).unwrap().symbol.as_str().to_string())
                .collect::<String>()
        })
        .collect();
    // The first invocation is the measurement (see doc comment).
    (rows, heights.first().copied().unwrap_or(0))
}

fn steer(text: &str, tags: Vec<SteerOriginTag>) -> TranscriptMessage {
    TranscriptMessage::new(nuo_protocol::Role::User, text)
        .with_origin(UserMessageOrigin::Steer)
        .with_sent_at_ms(1_700_000_000_000)
        .with_origin_tags(tags)
}

/// `[INV-STEER-03]` — a one-line in-round interjection costs exactly one row.
///
/// The claim is about the renderer's accounting: one body row, no header row,
/// no header/body gap row, no solid transition padding rows. A round-panel user
/// message costs five extra rows for the same text; the interjection must not.
#[test]
fn in_round_interjection_costs_exactly_one_row() {
    let (rows, content_lines) = render_rows(
        &steer("use rg, not find", vec![SteerOriginTag::InRound]),
        72,
        8,
    );

    assert_eq!(
        content_lines,
        1,
        "a one-line interjection must account for exactly one body row, got {content_lines}:\n{}",
        rows.join("\n")
    );

    let rendered: Vec<&String> = rows.iter().filter(|r| !r.trim().is_empty()).collect();
    assert_eq!(
        rendered.len(),
        1,
        "exactly one row carries content:\n{}",
        rows.join("\n")
    );
    let row = rendered[0];
    assert!(
        row.contains("steer") && row.contains("use rg, not find"),
        "the identity row carries the lead and the text: {row:?}"
    );
}

/// The contrast case: the same text WITHOUT the provenance tag renders at full
/// round-panel weight (a header row, a gap row, and two transition rows).
///
/// This assertion is what makes the previous test meaningful — if the renderer
/// ignored the tag, both forms would cost the same, and the interjection would
/// silently lose its identity row instead of its padding.
#[test]
fn same_text_without_the_tag_renders_at_round_panel_weight() {
    let (rows, content_lines) = render_rows(&steer("use rg, not find", Vec::new()), 72, 14);

    assert!(
        content_lines >= 5,
        "a round-panel user message accounts for its header, gap and two \
         transition rows (>= 5), got {content_lines}:\n{}",
        rows.join("\n")
    );
}

/// A multi-line interjection costs `1 + wrapped` rows and never adds padding:
/// continuation lines are band rows under the text column.
#[test]
fn multi_line_interjection_has_no_padding_rows() {
    let long = "first line of guidance\nsecond line of guidance\nthird line";
    let (rows, content_lines) = render_rows(&steer(long, vec![SteerOriginTag::InRound]), 72, 12);

    assert_eq!(
        content_lines,
        3,
        "a three-line interjection costs exactly three rows (no header, no \
         padding), got {content_lines}:\n{}",
        rows.join("\n")
    );
}

/// The provenance tag is render metadata: it is independent of the origin
/// vocabulary (a `Steer` origin with no tag is a different visual weight from
/// one with the tag), and tags match exactly rather than by presence.
#[test]
fn origin_tag_is_independent_of_origin_vocabulary() {
    let tagged = steer("x", vec![SteerOriginTag::InRound]);
    let untagged = steer("x", Vec::new());

    assert_eq!(tagged.origin, untagged.origin, "same origin vocabulary");
    assert!(tagged.has_origin_tag(SteerOriginTag::InRound));
    assert!(!untagged.has_origin_tag(SteerOriginTag::InRound));
    assert!(
        !tagged.has_origin_tag(SteerOriginTag::RoundOpening),
        "tags are matched exactly, not by presence of any tag"
    );
}
