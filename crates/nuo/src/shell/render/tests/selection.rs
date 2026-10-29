//! Semantic selection tests: wrapped-line intersection, block coverage, virtual index geometry.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn virtual_index_selects_only_chunks_intersecting_the_viewport() {
    let messages = (0..4)
        .map(|i| TranscriptMessage::new(nuo_wire::Role::Assistant, format!("m{i}")))
        .collect::<Vec<_>>();
    let mut cache = HeightCache::default();
    cache.prepare(80);
    // Top scroll padding (1 row), four-line bodies, inter-message gap (1 row),
    // and bottom scroll padding (1 row): chunks begin at 0, 5, 10, and 15.
    for message in &messages {
        cache.set(message.id, 4);
    }

    let window = cache
        .virtual_window(&messages, nuo_view::layout::Strategy::TurnBand, 6, 3)
        .expect("all message heights are cached");
    assert_eq!(window.message_start, 1);
    assert_eq!(window.message_end, 2);
    assert_eq!(window.prefix_lines, 5);
    assert_eq!(window.skip_rows, 1);
    assert_eq!(window.total_lines, 21);
}

#[test]
fn virtual_index_uses_segmented_same_turn_geometry() {
    let mut thinking = TranscriptMessage::reasoning("reasoning").with_turn(3);
    thinking.set_reasoning_duration(1);
    let first = TranscriptMessage::tool_step("a", "read_text", r#"{"path":"a"}"#).with_turn(3);
    let second = TranscriptMessage::tool_step("b", "read_text", r#"{"path":"b"}"#).with_turn(3);
    let messages = vec![thinking, first, second];
    let mut cache = HeightCache::default();
    cache.prepare(80);
    for message in &messages {
        cache.set(message.id, 2);
    }

    let window = cache
        .virtual_window(&messages, nuo_view::layout::Strategy::TurnBand, 0, 20)
        .expect("all message heights are cached");
    assert_eq!(window.message_start, 0);
    assert_eq!(window.message_end, 3);
    assert_eq!(
        window.total_lines, 11,
        "top gap + header + header gap + thinking + segment gap + flush tool batch + bottom gap"
    );
}

#[test]
fn virtual_index_prefix_skips_settled_history_during_streaming_tail() {
    // 5 settled messages forming separate chunks, plus a 6th unmeasured streaming tail message.
    let mut messages = Vec::new();
    for i in 0..5 {
        messages.push(TranscriptMessage::new(
            nuo_wire::Role::User,
            format!("user prompt {i}"),
        ));
    }
    // 6th message is actively streaming (not in HeightCache)
    let streaming_tail =
        TranscriptMessage::new(nuo_wire::Role::Assistant, "streaming in progress...");
    messages.push(streaming_tail);

    let mut cache = HeightCache::default();
    cache.prepare(80);
    // Only the first 5 messages have cached heights.
    for m in &messages[..5] {
        cache.set(m.id, 3);
    }

    // When scrolled to the bottom (viewing the streaming tail), settled history must be skipped in O(1).
    let window = cache
        .virtual_window(&messages, nuo_view::layout::Strategy::TurnBand, 100, 20)
        .expect("prefix virtual index must resolve even with live unmeasured tail");

    assert_eq!(window.message_start, 5, "must skip all 5 settled messages");
    assert_eq!(window.message_end, 6, "must target the streaming tail");
    assert!(!window.is_full);
}

#[test]
fn line_selection_intersects_wrapped_lines() {
    use nuo_view::model::layout::SemanticCursor;
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 2),
        head: SemanticCursor::new(0, 0, 8),
    };
    let range = block_selection_range(&sel, 0, 0);

    // Line covering bytes 0..5 ("hello"): selected from 2 to end.
    let first = WrappedLine {
        text: "hello".to_string(),
        start_byte: 0,
        end_byte: 5,
    };
    assert_eq!(line_selection(range, &first), Some((2, 5)));

    // Line covering bytes 5..10 ("world"): selected up to head char (8 → rel 3, inclusive → 4).
    let second = WrappedLine {
        text: "world".to_string(),
        start_byte: 5,
        end_byte: 10,
    };
    assert_eq!(line_selection(range, &second), Some((0, 4)));

    // A line after the selection has no overlap.
    let third = WrappedLine {
        text: "after".to_string(),
        start_byte: 10,
        end_byte: 15,
    };
    assert_eq!(line_selection(range, &third), None);
}

#[test]
fn block_selection_covers_middle_blocks_fully() {
    use nuo_view::model::layout::SemanticCursor;
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 3),
        head: SemanticCursor::new(0, 2, 1),
    };
    assert_eq!(block_selection_range(&sel, 0, 0), Some((3, None)));
    assert_eq!(block_selection_range(&sel, 0, 1), Some((0, None)));
    assert_eq!(block_selection_range(&sel, 0, 2), Some((0, Some(1))));
    assert_eq!(block_selection_range(&sel, 0, 3), None);
    assert_eq!(block_selection_range(&sel, 1, 0), None);
}

#[test]
fn line_selection_does_not_bleed_into_next_line_at_exact_boundary() {
    use nuo_view::model::layout::SemanticCursor;
    // Selection covers bytes 2..5 (exactly to the end of the first line).
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 2),
        head: SemanticCursor::new(0, 0, 5),
    };
    let range = block_selection_range(&sel, 0, 0);

    let first = WrappedLine {
        text: "hello".to_string(),
        start_byte: 0,
        end_byte: 5,
    };
    assert_eq!(line_selection(range, &first), Some((2, 5)));

    // Following line starting at byte 5 must NOT get any selection.
    let second = WrappedLine {
        text: "world".to_string(),
        start_byte: 5,
        end_byte: 10,
    };
    assert_eq!(line_selection(range, &second), None);
}

#[test]
fn extract_selection_text_extracts_modal_document_selections() {
    use crate::shell::event_loop::transcript::extract_selection_text;
    use nuo_view::model::layout::{BlockRegion, LayoutMap, MODAL_DOC_MSG_IDX, SemanticCursor};
    use nuotc::Rect;

    let mut layout_map = LayoutMap::new();
    layout_map.push(BlockRegion {
        message_idx: MODAL_DOC_MSG_IDX,
        block_idx: 1,
        start_byte: 0,
        end_byte: 25,
        text: "https://example.com/auth".to_string(),
        prefix_cols: 0,
        rect: Rect::new(0, 0, 30, 1),
        hidden_ranges: Vec::new(),
    });
    layout_map.push(BlockRegion {
        message_idx: MODAL_DOC_MSG_IDX,
        block_idx: 1,
        start_byte: 25,
        end_byte: 45,
        text: "?client_id=123456789".to_string(),
        prefix_cols: 0,
        rect: Rect::new(0, 1, 30, 1),
        hidden_ranges: Vec::new(),
    });

    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(MODAL_DOC_MSG_IDX, 1, 0),
        head: SemanticCursor::new(MODAL_DOC_MSG_IDX, 1, 45),
    };

    let extracted = extract_selection_text(&sel, &[], "", &layout_map, None);
    assert_eq!(
        extracted,
        Some("https://example.com/auth?client_id=123456789".to_string())
    );
}

/// Paint one transcript through the real renderer and hand back the layout
/// map — the same shape `layout_map.rs` uses, so these tests read the regions
/// the pointer path will read.
fn paint_transcript(
    messages: &[TranscriptMessage],
    selection: &SelectionState,
) -> nuo_view::model::layout::LayoutMap {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let mut layout_map = LayoutMap::new();
    terminal.draw(|f| {
        draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages,
                scroll: 0,
                selection,
                cell_selection: None,
                backoff: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::shell::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                notice_dock: None,
                subagent_bar: None,
                page_hints: None,
                tab_bar: None,
                thread_id: None,
                thread_context: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: nuo_view::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    layout_map
}

/// A notice entry's body is real document text: dragging from its first line to
/// its last must produce exactly the bytes the entry displays. This is the
/// regression that motivated the chrome/body split — the head used to register
/// every one of its rows against a sentinel block index, so a drag resolved to
/// "no text" and a whole-block select resolved to an empty slice.
#[test]
fn a_notice_body_is_drag_selectable_end_to_end() {
    use nuo_view::model::document::NoticeSeverity;
    use nuo_view::model::layout::SemanticCursor;

    let raw = "OpenAI HTTP 429 Too Many Requests: rate limited\n{\"error\":{\"code\":\"1305\"}}";
    let msg = TranscriptMessage::notice(NoticeSeverity::Error, raw);
    let messages = vec![msg.clone()];

    let layout_map = paint_transcript(&messages, &SelectionState::None);

    // The head is chrome: it registers so a click can resolve the entry, but it
    // must carry no text.
    let head = layout_map
        .regions()
        .iter()
        .find(|r| r.block_idx == nuo_view::model::layout::NOTICE_BLOCK_IDX)
        .expect("the entry head is registered so clicks resolve the entry");
    assert!(
        !head.is_text_region(),
        "the head must not be copyable text: {head:?}"
    );

    // Body rows address real block bytes.
    let body_regions: Vec<_> = layout_map
        .regions()
        .iter()
        .filter(|r| r.is_text_region())
        .collect();
    assert!(
        body_regions.len() >= 2,
        "both body lines must register as text: {body_regions:?}"
    );
    for region in body_regions {
        assert!(
            region.block_idx < msg.blocks.len(),
            "a body region must name a real block: {region:?}"
        );
        assert_eq!(
            &msg.blocks[region.block_idx].raw_text()[region.start_byte..region.end_byte],
            region.text,
            "region text must be the block's own bytes: {region:?}"
        );
    }

    // A drag over the whole body copies exactly what is drawn.
    let last_block = msg.blocks.len() - 1;
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 0),
        head: SemanticCursor::new(0, last_block, usize::MAX),
    };
    let copied = crate::shell::event_loop::transcript::extract_selection_text(
        &sel,
        &messages,
        "",
        &layout_map,
        None,
    )
    .expect("a drag over the body must copy");
    assert_eq!(copied, msg.raw, "drag-copy must equal the displayed text");
}

/// A whole-entry selection over a notice copies that entry's own block text —
/// never an empty string. The two gestures (drag, whole-block) must agree about
/// where the entry's text lives.
#[test]
fn a_notice_whole_block_selection_copies_real_text() {
    use nuo_view::model::document::NoticeSeverity;

    let raw = "Claude HTTP 500 Server Error\nDetails:\nhost unreachable";
    let msg = TranscriptMessage::notice(NoticeSeverity::Error, raw);
    let messages = vec![msg.clone()];
    let layout_map = paint_transcript(&messages, &SelectionState::None);

    let block = crate::shell::event_loop::transcript::extract_selection_text(
        &SelectionState::Block {
            message_idx: 0,
            block_idx: 0,
        },
        &messages,
        "",
        &layout_map,
        None,
    )
    .expect("block select copies");
    assert_eq!(block, msg.blocks[0].raw_text());
    assert!(
        !block.is_empty(),
        "a whole-block select over a notice must never resolve to an empty slice"
    );
}

/// The pointer cascade must treat a notice's two halves differently: the head
/// focuses/activates the entry, the body starts a text drag. That distinction
/// is what lets the same entry be both an affordance and copyable text — and it
/// is exactly what broke when every notice row carried the sentinel index, since
/// the body then classified as a step summary and a drag could never start.
#[test]
fn a_notice_head_focuses_the_entry_while_its_body_starts_a_drag() {
    use crate::shell::interaction::{ClickTarget, classify_click};
    use nuo_view::model::document::NoticeSeverity;
    use nuo_view::model::layout::NOTICE_BLOCK_IDX;

    let msg = TranscriptMessage::notice(
        NoticeSeverity::Error,
        "OpenAI HTTP 429 Too Many Requests: rate limited\n{\"error\":{\"code\":\"1305\"}}",
    );
    let messages = vec![msg];
    let layout_map = paint_transcript(&messages, &SelectionState::None);

    let head = layout_map
        .regions()
        .iter()
        .find(|r| r.block_idx == NOTICE_BLOCK_IDX)
        .expect("head region");
    assert!(
        matches!(
            classify_click(&layout_map, head.rect.x + 2, head.rect.y),
            ClickTarget::StepSummary { .. }
        ),
        "the entry head must resolve to the entry itself"
    );

    let body = layout_map
        .regions()
        .iter()
        .find(|r| r.is_text_region())
        .expect("body region");
    match classify_click(&layout_map, body.rect.x + body.prefix_cols, body.rect.y) {
        ClickTarget::Content { cursor } => {
            assert_eq!(cursor.message_idx, body.message_idx);
            assert_eq!(cursor.block_idx, body.block_idx);
        }
        other => panic!("a body click must start a text drag, got {other:?}"),
    }
}
