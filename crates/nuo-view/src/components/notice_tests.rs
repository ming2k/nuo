//! Notice entry rendering: chrome/body split, verbatim body, selection geometry.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::model::layout::SemanticCursor;
use crate::model::selection::{SelectionState, get_selected_text};
use nuo_view_test_support::*;

mod nuo_view_test_support {
    use super::*;
    use nuotc::{CellGrid, Damage, Frame};

    /// One notice, one frame: paint it and hand back everything a caller wants
    /// to assert on (rows, content accounting, the layout map).
    pub(super) struct Painted {
        pub grid: CellGrid,
        pub layout_map: LayoutMap,
        pub content_lines: usize,
        pub current_y: u16,
    }

    impl Painted {
        pub(super) fn row(&self, y: u16, width: u16) -> String {
            let mut s = String::new();
            for x in 0..width {
                s.push_str(
                    self.grid
                        .get(x, y)
                        .map(|c| c.symbol.as_str())
                        .unwrap_or(" "),
                );
            }
            s
        }

        pub(super) fn rows(&self) -> Vec<String> {
            (0..self.grid.height())
                .map(|y| self.row(y, self.grid.width()).trim_end().to_string())
                .collect()
        }
    }

    /// Paint `msg` into a `width × height` frame.
    pub(super) fn paint(
        msg: &TranscriptMessage,
        width: u16,
        height: u16,
        selection: SelectionState,
    ) -> Painted {
        let theme = Theme::default();
        let mut grid = CellGrid::new(width, height);
        let mut damage = Damage::new(height);
        let mut frame = Frame::new(&mut grid, &mut damage);
        let mut layout_map = LayoutMap::default();
        let mut wrap = BlockWrapCache::default();
        let mut skip_rows = 0usize;
        let mut current_y = 0u16;
        let mut content_lines = 0usize;

        draw_notice_view(
            &mut frame,
            Rect::new(0, 0, width, height),
            NoticeProps { message: msg },
            0,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &mut wrap,
            &selection,
            &theme,
            false,
            false,
        );

        Painted {
            grid,
            layout_map,
            content_lines,
            current_y,
        }
    }
}

#[test]
fn entry_head_is_chrome_and_body_is_indented_document_text() {
    let msg = TranscriptMessage::notice(NoticeSeverity::Error, "Connection refused");
    let painted = paint(&msg, 60, 10, SelectionState::None);

    // Head (row 0) + 1-row gap (row 1) + body (row 2).
    assert_eq!(painted.content_lines, 3);
    assert_eq!(painted.current_y, 3);
    assert!(painted.row(0, 20).starts_with("! notification"));
    assert!(
        painted.row(2, 25).starts_with("  Connection refused"),
        "body must be indented past the entry head: {:?}",
        painted.row(2, 25)
    );

    let head = painted.layout_map.region_at(0, 0).expect("head region");
    assert!(
        !head.is_text_region(),
        "the head is chrome: it must not be selectable text"
    );

    let body = painted.layout_map.region_at(2, 2).expect("body region");
    assert_eq!(body.text, "Connection refused");
    assert_eq!(body.prefix_cols, TRANSCRIPT_BODY_LEADING_INDENT);
    assert_eq!(body.block_idx, 0, "body rows address real blocks");
    assert!(body.is_text_region());
}

#[test]
fn body_rows_address_real_block_bytes() {
    let msg = TranscriptMessage::notice(
        NoticeSeverity::Error,
        "OpenAI HTTP 429 Too Many Requests\n{\"error\":{\"code\":\"1305\"}}",
    );
    let painted = paint(&msg, 60, 12, SelectionState::None);

    // Every body region must name a block that exists and a byte range inside
    // it — the contract that makes copy resolve to the painted text.
    for region in painted.layout_map.regions() {
        if !region.is_text_region() {
            continue;
        }
        let block = msg
            .blocks
            .get(region.block_idx)
            .unwrap_or_else(|| panic!("region names a missing block: {}", region.block_idx));
        assert!(
            region.end_byte <= block.raw_text().len(),
            "region {region:?} runs past its block"
        );
        assert_eq!(
            &block.raw_text()[region.start_byte..region.end_byte],
            region.text,
            "region text must be the block's own bytes"
        );
    }
}

#[test]
fn paragraph_separator_renders_as_a_blank_row() {
    // Mirrors AgentNotice::render_text(): lead, then a body whose own
    // paragraphs are separated by a blank line.
    let raw = "Trust changed\nQuarantined pending review.\n\n/trust re-trusts all.";
    let msg = TranscriptMessage::notice(NoticeSeverity::Warning, raw);
    let painted = paint(&msg, 60, 10, SelectionState::None);

    // head + gap + three body lines + one separator = 6 rows.
    assert_eq!(painted.content_lines, 6);
    assert!(painted.row(2, 40).starts_with("  Trust changed"));
    assert!(
        painted
            .row(3, 40)
            .starts_with("  Quarantined pending review.")
    );
    assert!(
        painted.row(4, 40).trim().is_empty(),
        "paragraph separator must render as a blank row: {:?}",
        painted.row(4, 40)
    );
    assert!(painted.row(5, 40).starts_with("  /trust re-trusts all."));
}

#[test]
fn core_notice_head_names_the_topic_and_body_keeps_the_structured_split() {
    let core = nuo_wire::AgentNotice::trust_changed("Workspace configurations changed")
        .with_body("Changed on disk: rules (AGENTS.md / rules) — quarantined pending review.");
    let msg = TranscriptMessage::notice_from_core(&core);
    let painted = paint(&msg, 60, 10, SelectionState::None);

    assert!(painted.row(0, 58).starts_with("▲ trust"));
    assert!(!painted.row(0, 58).contains("notification"));
    assert!(
        painted
            .row(2, 58)
            .starts_with("  Workspace configurations changed")
    );
    assert!(painted.row(3, 58).starts_with("  Changed on disk: rules"));
}

#[test]
fn content_lines_are_viewport_independent() {
    let raw = "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: {\n  \"error\": {\n    \"code\": 429,\n    \"message\": \"Individual quota reached.\",\n    \"status\": \"RESOURCE_EXHAUSTED\"\n  }\n}";
    let msg = TranscriptMessage::notice(NoticeSeverity::Error, raw);

    let large = paint(&msg, 80, 50, SelectionState::None);
    let small = paint(&msg, 80, 3, SelectionState::None);
    assert!(large.content_lines > 3);
    assert_eq!(large.content_lines, small.content_lines);
}

#[test]
fn a_notice_head_carries_only_settled_facts_never_a_ticking_counter() {
    // A notice is a record. Its head shows the severity glyph, the topic and
    // the wall-clock stamp — all frozen at construction. A live countdown used
    // to live here, which made this row re-render every second; the retry now
    // owns no transcript row at all (`crate::retry`), so nothing in this
    // component can tick.
    let mut msg = TranscriptMessage::notice(
        NoticeSeverity::Warning,
        "Google HTTP 503 Service Unavailable: No capacity available for model",
    );
    msg.sent_at_ms = Some(1_700_000_000_000);
    let painted = paint(&msg, 80, 20, SelectionState::None);

    let head = painted.row(0, 80);
    assert!(head.starts_with("▲ notification"), "head: {head:?}");
    assert!(
        !head.contains("retry"),
        "a settled notice carries no live retry counter: {head:?}"
    );

    // The body is the diagnostic text, and nothing else.
    let body: Vec<String> = painted.rows()[2..].to_vec();
    assert!(
        body.iter()
            .any(|r| r.contains("Google HTTP 503 Service Unavailable")),
        "body must carry the failure: {body:?}"
    );
}

#[test]
fn dragging_over_the_body_copies_exactly_what_is_painted() {
    let msg = TranscriptMessage::notice(
        NoticeSeverity::Error,
        "OpenAI HTTP 500 Server Error\n{\"error\":{\"message\":\"boom\"}}",
    );
    // Select the whole entry: from the first body byte to the last.
    let last_block = msg.blocks.len() - 1;
    let selection = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 0),
        head: SemanticCursor::new(0, last_block, usize::MAX),
    };
    let copied = get_selected_text(&selection, std::slice::from_ref(&msg), &|_, _| None, None)
        .expect("a non-empty selection must copy");

    assert!(
        copied.contains("OpenAI HTTP 500 Server Error"),
        "copied: {copied:?}"
    );
    assert!(
        copied.contains("\"message\": \"boom\""),
        "copied: {copied:?}"
    );
    assert_eq!(
        copied, msg.raw,
        "the body must copy the entry's own display text byte for byte"
    );
}

#[test]
fn every_entry_kind_copies_the_same_bytes_it_displays() {
    // The invariant, stated once: for a notice, the copy text is the display
    // text (`raw`), which the painter renders verbatim.
    for (severity, raw) in [
        (NoticeSeverity::Info, "Cancelled via [Esc Esc]"),
        (NoticeSeverity::Warning, "▲ not a glyph, literal text"),
        (
            NoticeSeverity::Error,
            "OpenAI HTTP 429 Too Many Requests: rate limited\n{\"error\":{\"code\":\"1305\"}}",
        ),
    ] {
        let msg = TranscriptMessage::notice(severity, raw);
        let text = crate::model::interactive::copy_text(&msg).expect("notice has copy text");
        assert_eq!(text, msg.raw);
        // And the stored per-line blocks rebuild exactly that text.
        let rebuilt = msg
            .blocks
            .iter()
            .map(|b| match b {
                Block::Break => String::new(),
                other => other.raw_text().to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rebuilt, msg.raw, "blocks must tile the display text");
    }
}
