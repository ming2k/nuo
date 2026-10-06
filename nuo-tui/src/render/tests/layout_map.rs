//! Transcript layout-map tests: heights, turn headers, gaps, semantic region recording.

use super::*;

/// The transcript content rect must be recorded after rendering so that
/// clicks on blank transcript space (gap rows, space below the last message,
/// the outer gutters) park keyboard attention on the transcript (ADR-0174
/// browse focus). It must span the **full transcript viewport** — the user's
/// mental model is that the whole transcript area is the browse surface, so
/// no part of it is a dead click.
#[test]
fn transcript_content_rect_spans_band_and_gap_rows() {
    let theme = Theme::default();
    let width = 46u16;
    let mut terminal = nuotc::TestTerminal::new(width, 24);
    // Two assistant text messages so a `MESSAGE_GAP_ROWS` blank row is
    // emitted between them — that row is rendered but never registered.
    let messages = vec![
        TranscriptMessage::new(nuo_wire::Role::Assistant, "first".to_string()),
        TranscriptMessage::new(nuo_wire::Role::Assistant, "second".to_string()),
    ];
    let mut layout_map = LayoutMap::new();
    terminal.draw(|f| {
        draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });

    let rect = layout_map
        .transcript_content_rect()
        .expect("content rect must be recorded when messages are drawn");
    // Full transcript viewport: gutters included, no dead blank space.
    assert_eq!(rect.x, 0);
    assert_eq!(rect.width, width);

    // The whole point of the rect: a gap row between the two messages is
    // rendered but carries no region (clicking it does not resolve to a
    // cursor). It must still fall inside the content rect so the click
    // handler can switch focus to Browse.
    let gap_y = (rect.y..rect.y + rect.height)
        .find(|&y| layout_map.region_at(rect.x, y).is_none())
        .expect("there must be at least one unregistered gap row between the two messages");
    assert!(rect.y <= gap_y && gap_y < rect.y + rect.height);
}
