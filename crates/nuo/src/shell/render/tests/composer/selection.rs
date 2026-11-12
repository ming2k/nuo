//! Selection highlight geometry over CJK and exact input ranges.

use super::*;

/// (head + continuation), cover exactly the selected glyphs, and leave the
/// trailing pad on the panel background — no extra glyph, no half-highlighted
/// wide char. Exercises the full-3-CJK selection the live bug report used.

#[test]
fn composer_cjk_selection_covers_full_width_glyphs() {
    use nuo_view::model::layout::SemanticCursor;
    let theme = Theme::default();
    let interior_bg = theme.input_surface();
    let sel_bg = theme.selected();
    let input = "中文测"; // 3 wide glyphs = 6 cols (cols 4..10)
    // Select all three. Head points AT 测 (byte 6); the inclusive-head model
    // includes the glyph under the head, so the range is [0, 9) = "中文测".
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
        head: SemanticCursor::new(INPUT_MSG_IDX, 0, 6),
    };
    let mut terminal = nuotc::TestTerminal::new(20, 5);
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 20, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &sel,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            true,
            false,
            false,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let g = terminal.buffer();
    let y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    // Cols: 0='›', 1=gap, 2-7='中文测'(sel), 8+=panel tail.
    for (col, label, expect_sel) in [
        (2usize, "中 head", true),
        (3, "中 cont", true),
        (4, "文 head", true),
        (5, "文 cont", true),
        (6, "测 head", true),
        (7, "测 cont", true),
        (8, "tail 0", false),
        (9, "tail 1", false),
    ] {
        let cell = g.get(col as u16, y).unwrap();
        let want = if expect_sel { sel_bg } else { interior_bg };
        assert_eq!(
            cell.style.bg, want,
            "{label} at col {col}: bg {:?} expected {:?}",
            cell.style.bg, want
        );
    }
    // While a selection is active the caller passes `show_caret = false`
    // (see the event loop), so no terminal caret is placed on top of the
    // highlighted glyphs — the "appended flickering character" symptom.
    assert!(
        matches!(terminal.cursor(), nuotc::CursorState::Hidden),
        "caret must be hidden while a selection is active"
    );
}

#[test]
fn composer_two_cjk_select_all_has_no_extra_glyph_or_tail_highlight() {
    use nuo_view::model::layout::SemanticCursor;

    let theme = Theme::default();
    let sel_bg = theme.selected();
    let input = "你好";
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
        head: SemanticCursor::new(INPUT_MSG_IDX, 0, input.len()),
    };
    let mut terminal = nuotc::TestTerminal::new(16, 5);

    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 16, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &sel,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            true,
            false,
            false,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });

    let y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    let buffer = terminal.buffer();
    let interior_bg = theme.input_surface();

    assert_eq!(buffer.get(2, y).unwrap().symbol.as_str(), "你");
    assert_eq!(buffer.get(2, y).unwrap().width, nuotc::CellWidth::Wide);
    assert_eq!(buffer.get(3, y).unwrap().symbol.as_str(), " ");
    assert_eq!(
        buffer.get(3, y).unwrap().width,
        nuotc::CellWidth::Continuation
    );
    assert_eq!(buffer.get(4, y).unwrap().symbol.as_str(), "好");
    assert_eq!(buffer.get(4, y).unwrap().width, nuotc::CellWidth::Wide);
    assert_eq!(buffer.get(5, y).unwrap().symbol.as_str(), " ");
    assert_eq!(
        buffer.get(5, y).unwrap().width,
        nuotc::CellWidth::Continuation
    );
    assert_eq!(
        buffer.get(6, y).unwrap().symbol.as_str(),
        " ",
        "tail cell must not contain a duplicate glyph"
    );

    for col in 2..=5 {
        assert_eq!(
            buffer.get(col, y).unwrap().style.bg,
            sel_bg,
            "col {col} should be selected"
        );
    }
    assert_eq!(
        buffer.get(6, y).unwrap().style.bg,
        interior_bg,
        "tail cell must remain on the panel's background"
    );
    assert!(
        matches!(terminal.cursor(), nuotc::CursorState::Hidden),
        "caret must be hidden while a selection is active"
    );
}

#[test]
fn composer_input_range_highlights_exact_characters() {
    let theme = Theme::default();
    let sel_bg = theme.selected();
    let interior_bg = theme.input_surface();
    let input = "hello world";
    // Select only 'h' (bytes 0..1)
    let sel = SelectionState::InputRange {
        anchor_byte: 0,
        head_byte: 1,
    };
    let mut terminal = nuotc::TestTerminal::new(20, 5);

    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 20, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &sel,
            },
            ComposerText {
                input,
                byte_cursor: 1,
            },
            true,
            false,
            false,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });

    let y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    let buffer = terminal.buffer();
    // Col 0: '›', Col 1: gap, Col 2: 'h' (selected), Col 3: 'e' (unselected)
    assert_eq!(
        buffer.get(2, y).unwrap().style.bg,
        sel_bg,
        "col 2 ('h') must be selected"
    );
    assert_eq!(
        buffer.get(3, y).unwrap().style.bg,
        interior_bg,
        "col 3 ('e') must not be selected"
    );

    // Select "hello" (bytes 0..5)
    let sel = SelectionState::InputRange {
        anchor_byte: 0,
        head_byte: 5,
    };
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 20, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &sel,
            },
            ComposerText {
                input,
                byte_cursor: 5,
            },
            true,
            false,
            false,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let buffer = terminal.buffer();
    for col in 2..7 {
        assert_eq!(
            buffer.get(col, y).unwrap().style.bg,
            sel_bg,
            "col {col} in 'hello' must be selected"
        );
    }
    assert_eq!(
        buffer.get(7, y).unwrap().style.bg,
        interior_bg,
        "col 7 (space) must not be selected"
    );
}

/// Regression for the input-select bug: a click that starts a selection
/// (anchor == head, a collapsed range) must highlight NOTHING, and a drag
/// through the real click pipeline (layout_map → cursor_at) must highlight
/// exactly the dragged glyphs with the correct background. The prior
/// `inclusive_grapheme_end`-on-a-point logic lit up one glyph on every
/// click and flickered as the drag moved — "an extra changing character
/// appears and the selection background misbehaves".
#[test]
fn composer_collapsed_click_highlights_nothing_drag_highlights_cleanly() {
    let theme = Theme::default();
    let panel_bg = theme.input_surface();
    let sel_bg = theme.selected();
    let input = "中文测";
    let rect = Rect::new(0, 0, 20, 4);
    let text_row = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;

    // Record input regions so cursor_at can resolve real drag positions.
    let mut layout_map = LayoutMap::new();
    let mut rec = nuotc::TestTerminal::new(20, 5);
    rec.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: rect,
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            true,
            false,
            true,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let prefix = nuo_ui_primitives::design::COMPOSER_PROMPT_PREFIX_COLS as u16;
    let anchor = layout_map
        .cursor_at(rect.x + prefix, rect.y + text_row)
        .unwrap();
    assert_eq!(anchor.byte_offset, 0);

    fn row_bgs(
        input: &str,
        rect: Rect,
        text_row: u16,
        theme: &Theme,
        sel: &SelectionState,
    ) -> Vec<nuotc::Color> {
        let mut t = nuotc::TestTerminal::new(20, 5);
        t.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: rect,
                    theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: sel,
                },
                ComposerText {
                    input,
                    byte_cursor: input.len(),
                },
                true,
                false,
                false,
                0,
                0,
                crate::shell::composer_hints::ComposerHints::default(),
            );
        });
        (0..10u16)
            .map(|c| t.buffer().get(c, text_row).unwrap().style.bg)
            .collect()
    }

    // 1) Collapsed click (anchor == head): no glyph may carry the selection bg.
    let collapsed = SelectionState::Range {
        anchor,
        head: anchor,
    };
    for (col, bg) in row_bgs(input, rect, text_row, &theme, &collapsed)
        .into_iter()
        .enumerate()
    {
        assert_ne!(bg, sel_bg, "collapsed click lit up col {col}");
        let _ = panel_bg;
    }

    // 2) Drag onto 测's first column (byte 6): inclusive head selects all
    //    three glyphs.
    let head = layout_map.cursor_at(rect.x + 6, rect.y + text_row).unwrap();
    assert_eq!(head.byte_offset, 6);
    let drag = SelectionState::Range { anchor, head };
    let bgs = row_bgs(input, rect, text_row, &theme, &drag);
    // cols 0-1 = prompt prefix; 2..8 = "中文测" (selected).
    for (col, &bg) in bgs[2..8].iter().enumerate() {
        assert_eq!(bg, sel_bg, "col {} should be selected", col + 2);
    }

    // 3) Drag to the second visual column of 中. The hit-test cursor maps
    // both columns of a wide glyph to that glyph's byte start; with an
    // inclusive head this selects 中 only, not the next glyph.
    let head = layout_map.cursor_at(rect.x + 3, rect.y + text_row).unwrap();
    assert_eq!(head.byte_offset, 1);
    let drag = SelectionState::Range { anchor, head };
    let bgs = row_bgs(input, rect, text_row, &theme, &drag);
    for (col, &bg) in bgs[2..4].iter().enumerate() {
        assert_eq!(bg, sel_bg, "col {} should select 中", col + 2);
    }
    for (col, &bg) in bgs[4..10].iter().enumerate() {
        assert_eq!(bg, panel_bg, "col {} should remain unselected", col + 4);
    }
}
