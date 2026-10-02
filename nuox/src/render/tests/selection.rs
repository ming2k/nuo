//! Semantic selection tests: wrapped-line intersection, block coverage, virtual index geometry.

use super::*;

#[test]
fn virtual_index_selects_only_chunks_intersecting_the_viewport() {
    let messages = (0..4)
        .map(|i| TranscriptMessage::new(nuo_contracts::Role::Assistant, format!("m{i}")))
        .collect::<Vec<_>>();
    let mut cache = HeightCache::default();
    cache.prepare(80);
    // Top scroll padding (1 row), four-line bodies, inter-message gap (1 row),
    // and bottom scroll padding (1 row): chunks begin at 0, 5, 10, and 15.
    for message in &messages {
        cache.set(message.id, 4);
    }

    let window = cache
        .virtual_window(&messages, crate::layout::Strategy::TurnBand, 6, 3)
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
        .virtual_window(&messages, crate::layout::Strategy::TurnBand, 0, 20)
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
            nuo_contracts::Role::User,
            format!("user prompt {i}"),
        ));
    }
    // 6th message is actively streaming (not in HeightCache)
    let streaming_tail =
        TranscriptMessage::new(nuo_contracts::Role::Assistant, "streaming in progress...");
    messages.push(streaming_tail);

    let mut cache = HeightCache::default();
    cache.prepare(80);
    // Only the first 5 messages have cached heights.
    for m in &messages[..5] {
        cache.set(m.id, 3);
    }

    // When scrolled to the bottom (viewing the streaming tail), settled history must be skipped in O(1).
    let window = cache
        .virtual_window(&messages, crate::layout::Strategy::TurnBand, 100, 20)
        .expect("prefix virtual index must resolve even with live unmeasured tail");

    assert_eq!(window.message_start, 5, "must skip all 5 settled messages");
    assert_eq!(window.message_end, 6, "must target the streaming tail");
    assert!(!window.is_full);
}

#[test]
fn line_selection_intersects_wrapped_lines() {
    use crate::model::layout::SemanticCursor;
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
    use crate::model::layout::SemanticCursor;
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
    use crate::model::layout::SemanticCursor;
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
    use crate::event_loop::transcript::extract_selection_text;
    use crate::model::layout::{BlockRegion, LayoutMap, MODAL_DOC_MSG_IDX, SemanticCursor};
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
