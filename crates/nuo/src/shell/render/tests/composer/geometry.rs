//! Composer geometry: layout rects, heights, and divider chrome.

use super::*;

#[test]
fn draw_composer_records_composer_rect_enabling_whole_component_focus() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(40, 6);
    let mut layout_map = LayoutMap::new();
    let input_rect = Rect::new(0, 1, 40, 4);

    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect,
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: "hello world",
                byte_cursor: 11,
            },
            true,
            true,
            true,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });

    // Verify composer_rect is recorded
    assert_eq!(layout_map.composer_rect(), Some(input_rect));

    // 1. Click on Row 0 (top chrome row): y = 1 -> snaps to start of input
    let target_row0 = crate::shell::interaction::classify_click(&layout_map, 10, 1);
    assert_eq!(
        target_row0,
        crate::shell::interaction::ClickTarget::InputBox {
            cursor: nuo_view::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 0)
        }
    );

    // 2. Click on Row 1 (text row): y = 2 -> precise text cursor
    let target_row1 = crate::shell::interaction::classify_click(
        &layout_map,
        COMPOSER_PROMPT_PREFIX_COLS as u16 + 2,
        2,
    );
    assert!(matches!(
        target_row1,
        crate::shell::interaction::ClickTarget::InputBox { .. }
    ));

    // 3. Click on Row 2 (gap row): y = 3 -> snaps to end of input
    let target_row2 = crate::shell::interaction::classify_click(&layout_map, 10, 3);
    assert_eq!(
        target_row2,
        crate::shell::interaction::ClickTarget::InputBox {
            cursor: nuo_view::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 11)
        }
    );

    // 4. Click on Row 3 (hint row): y = 4 -> snaps to end of input
    let target_row3 = crate::shell::interaction::classify_click(&layout_map, 10, 4);
    assert_eq!(
        target_row3,
        crate::shell::interaction::ClickTarget::InputBox {
            cursor: nuo_view::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 11)
        }
    );

    // 5. Click outside the composer component (e.g. y = 0 or y = 5) -> Dead
    assert_eq!(
        crate::shell::interaction::classify_click(&layout_map, 10, 0),
        crate::shell::interaction::ClickTarget::Dead
    );
    assert_eq!(
        crate::shell::interaction::classify_click(&layout_map, 10, 5),
        crate::shell::interaction::ClickTarget::Dead
    );
}

#[test]
fn calculate_composer_height_matches_draw_transcript_geometry() {
    use crate::shell::render::calculate_composer_height;
    use nuotc::Rect;

    let viewport = Rect::new(0, 0, 80, 24);

    // 1. Single line: 1 wrapped line + 3 chrome rows = 4 rows
    let h1 = calculate_composer_height(viewport, "single line prompt");
    assert_eq!(h1, 4);

    // 2. Multiline prompt with newlines
    let multiline = "line 1\nline 2\nline 3";
    let h2 = calculate_composer_height(viewport, multiline);
    assert_eq!(h2, 6);

    // 3. Overflowing prompt caps at viewport / 2 (24 / 2 = 12)
    let overflow = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15";
    let h3 = calculate_composer_height(viewport, overflow);
    assert_eq!(h3, 12);
}

#[test]
fn calculate_composer_height_matches_actual_rendered_transcript_rect() {
    use crate::shell::render::{
        QueueBarProps, TabBarProps, ThreadContext, TranscriptProps, ViewHints, ViewKind,
        calculate_composer_height, draw_transcript,
    };
    use nuo_view::empty_state::EmptyStateGuidance;
    use nuo_view::model::layout::LayoutMap;
    use nuo_view::model::selection::SelectionState;

    let width = 80u16;
    let height = 24u16;
    let mut terminal = nuotc::TestTerminal::new(width, height);
    let theme = Theme::default();

    let test_inputs = [
        "short",
        "line 1\nline 2",
        "line 1\nline 2\nline 3\nline 4",
        "a very long overflowing prompt 1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16",
    ];

    for input in test_inputs {
        let mut actual_h = 0;
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let rendered = draw_transcript(
                f,
                &mut layout_map,
                TranscriptProps {
                    messages: &[],
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    backoff: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input,
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        blocked: false,
                        expand_key: None,
                    },
                    tasks_bar: Default::default(),
                    interrupt_key: None,
                    subagent_bar: None,
                    page_hints: Some(ViewHints {
                        kind: ViewKind::Thread,
                        context: None,
                        context_warn: false,
                        breadcrumbs: None,
                        can_back: false,
                        can_forward: false,
                    }),
                    tab_bar: Some(TabBarProps::default()),
                    thread_id: Some("test-id"),
                    thread_context: Some(ThreadContext {
                        workspace: "~/test",
                        role: None,
                        unattended: false,
                        confined: true,
                    }),
                    round_started_at: None,
                    hovered_step: None,
                    focused_entry: None,
                    focus_plane: crate::shell::render::FocusPlane::Composer,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: nuo_view::layout::Strategy::default(),
                    height_cache: None,
                },
            );
            actual_h = rendered.input_rect.height;
        });

        let expected_h = calculate_composer_height(
            nuotc::Rect::new(
                0,
                0,
                width,
                height.saturating_sub(nuo_ui_primitives::design::PAGE_HEADER_ROWS),
            ),
            input,
        );
        assert_eq!(
            actual_h, expected_h,
            "calculate_composer_height must exactly match rendered input_rect for input {:?}",
            input
        );
    }
}

/// The painter half of the Ctrl-J empty-line contract: the caret glyph lands on
/// the empty row the newline created, and that row exists whether or not the
/// caret is resting on it.
///
/// Both halves come from one grid (`composer_grid::TextGrid`): a row's byte
/// range is closed, so an empty line's single byte is a real caret cell instead
/// of a gap the caret falls through into the row below.
#[test]
fn composer_non_chromatic_renders_top_divider_border() {
    let mut theme = Theme::monochrome();
    theme.glyphs = nuotc::ASCII_GLYPHS;
    let mut terminal = nuotc::TestTerminal::new(60, 6);
    let mut layout_map = LayoutMap::new();
    let mut scroll = 0;
    terminal.draw(|f| {
        crate::shell::composer::draw_composer(
            crate::shell::composer::ComposerProps {
                frame: f,
                input_rect: Rect::new(2, 1, 56, 4),
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut scroll,
                selection: &SelectionState::None,
            },
            crate::shell::composer::ComposerText {
                input: "hello",
                byte_cursor: 5,
            },
            true,
            true,
            false,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let top_row: String = (2..58)
        .map(|x| {
            terminal
                .buffer()
                .get(x, 1)
                .map(|c| c.symbol.as_str())
                .unwrap_or(" ")
        })
        .collect();
    assert!(
        top_row.contains('-'),
        "top chrome row in monochrome mode must render horizontal divider border: {top_row}"
    );
}
