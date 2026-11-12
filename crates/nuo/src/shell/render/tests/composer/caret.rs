//! Caret placement, highlighting, and scroll excursion.

use super::*;

/// An empty composer must still record a layout-map region for its single
/// text row. Without it a click inside the empty box can't resolve to a
/// cursor, so the click handler can't clear a focused step to hand typing
/// back to the prompt. See `draw_composer` / `composer_wrapped`.

#[test]
fn draw_composer_records_region_for_empty_input() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(30, 5);
    let mut layout_map = LayoutMap::new();
    let input_rect = Rect::new(0, 0, 30, 3);
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
                input: "",
                byte_cursor: 0,
            },
            true,
            true,
            true,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });

    // The empty text row sits one line below the box's top edge.
    let cursor = layout_map
        .cursor_at(
            input_rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16,
            input_rect.y + 1,
        )
        .expect("click inside empty input box must resolve to a cursor");
    assert_eq!(cursor.message_idx, INPUT_MSG_IDX);
    assert_eq!(cursor.byte_offset, 0);
}

/// `draw_composer` must not panic for tricky inputs and should place the caret
/// on the second wrapped line when the cursor sits past the first wrap.
#[test]
fn draw_composer_wraps_and_positions_caret() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(20, 12);
    // "aaaa bbbb cccc dddd eeee" wraps within the ~17-wide inner area; cursor at the
    // very end should be on a later line, not off the box.
    let input = "aaaa bbbb cccc dddd eeee";
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 20, 8),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            true,
            true,
            true,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let cursor = match terminal.cursor() {
        nuotc::CursorState::Visible(x, y) => (x, y),
        other => panic!("caret should be visible, got {other:?}"),
    };
    // The tail of a 24-char input cannot fit the first wrapped line, so the
    // caret must rest on the second one (text row 0 + one wrap).
    assert_eq!(
        cursor.1,
        nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET + 1,
        "caret must sit on the second wrapped line, got {cursor:?}"
    );
}

/// The caret must land flush against the final glyph at the end of the
/// input, measured in display columns — i.e. exactly where the grid painted
/// the text. This is the CJK regression: a buggy grapheme-floor returned the
/// last grapheme *start*, leaving the caret two columns short of a wide
/// glyph (one for ASCII). The caret column must equal the rendered width of
/// the text, for both wide and narrow glyphs.
#[test]
fn draw_composer_caret_flush_against_final_grapheme() {
    let theme = Theme::default();

    for (label, input, expected_cols) in [
        ("cjk", "中文", 4usize),
        ("ascii", "ab", 2),
        ("mixed", "a中", 3),
    ] {
        let mut terminal = nuotc::TestTerminal::new(20, 5);
        terminal.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: Rect::new(0, 0, 20, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText {
                    input,
                    byte_cursor: input.len(),
                },
                true,
                true,
                false,
                0,
                0,
                crate::shell::composer_hints::ComposerHints::default(),
            );
        });
        let cursor = match terminal.cursor() {
            nuotc::CursorState::Visible(x, y) => (x, y),
            other => panic!("{label}: caret should be visible, got {other:?}"),
        };
        // The text row sits one line below the box's top padding row, and
        // the caret follows the `› ` prefix plus the full rendered width.
        assert_eq!(
            cursor,
            (
                (COMPOSER_PROMPT_PREFIX_COLS + expected_cols) as u16,
                nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET,
            ),
            "{label}: caret not flush with end of {input:?}"
        );
    }
}

/// A resolved `/command` token renders in bold + the theme accent color,
/// and the accent stops at the token boundary — the argument tail keeps
/// the normal text color so the two read as command + payload.
#[test]
fn draw_composer_highlighted_accents_only_the_command_token() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(30, 4);
    let input = "/repeat every minute";
    terminal.draw(|f| {
        draw_composer_highlighted(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 30, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            ComposerDrawOptions {
                focused: true,
                show_caret: true,
                follow_caret: true,
                record: false,
                image_count: 0,
                paste_count: 0,
                hints: crate::shell::composer_hints::ComposerHints::default(),
                ghost_text: None,
            },
            "/repeat".len(),
        );
    });
    let buf = terminal.buffer();
    let text_y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    // Every glyph of `/repeat` is bold + brand-colored on the panel bg.
    for (i, ch) in "/repeat".chars().enumerate() {
        let cell = buf.get(text_x + i as u16, text_y).expect("command cell");
        assert_eq!(cell.symbol.as_str(), ch.to_string());
        assert_eq!(
            cell.style.fg,
            theme.brand(),
            "command glyph {ch} lost the accent"
        );
        assert!(
            cell.style.add.contains(nuotc::Modifier::BOLD),
            "command glyph {ch} lost bold"
        );
    }
    // The argument tail (`every minute`) keeps the default text color.
    let arg_start = text_x + "/repeat ".len() as u16;
    let cell = buf.get(arg_start, text_y).expect("argument cell");
    assert_eq!(cell.symbol.as_str(), "e");
    assert_eq!(
        cell.style.fg,
        theme.fg(),
        "argument text must not be accented"
    );
}

/// The accent must not bleed past the first wrapped row: when the command
/// token itself fits but the highlight length would cover the wrap
/// boundary, the continuation row renders in the normal text color.
#[test]
fn draw_composer_highlight_clamps_at_wrap_boundary() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(15, 6);
    // 9-column text area (15 - 4 prefix - 2 right pad): `/sessions` wraps
    // mid-token; the continuation row must render in the normal text color.
    let input = "/sessions abc";
    terminal.draw(|f| {
        draw_composer_highlighted(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 15, 5),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            ComposerDrawOptions {
                focused: true,
                show_caret: true,
                follow_caret: true,
                record: false,
                image_count: 0,
                paste_count: 0,
                hints: crate::shell::composer_hints::ComposerHints::default(),
                ghost_text: None,
            },
            "/sessions".len(),
        );
    });
    let buf = terminal.buffer();
    let row1_y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET + 1;
    // The continuation row keeps the prefix indent before the wrapped text.
    // Find its first non-blank glyph and require the plain text color — the
    // accent must stop at the wrap boundary.
    let row1 = (nuo_ui_primitives::design::COMPOSER_PROMPT_PREFIX_COLS as u16..16u16)
        .map(|col| buf.get(col, row1_y).expect("continuation cell"))
        .find(|cell| cell.symbol.as_str() != " ")
        .expect("continuation row has visible text");
    assert_eq!(
        row1.style.fg,
        theme.fg(),
        "accent must not bleed onto the wrapped argument row"
    );
}

/// Attachment chips render as distinct colored "pills": a pasted-text
/// chip in the calm text-block blue and an image chip in the warm amber,
/// each bold on a tinted band, while the surrounding prose keeps the
/// normal text color. The color is the identifier's second channel —
/// kind at a glance, payload size in the label.
/// The composer must forward its resolved cursor coordinate unchanged to
/// the frame for ASCII, CJK, empty, and wrapped inputs.

#[test]
fn cursor_screen_pos_matches_drawn_caret() {
    use crate::shell::composer::cursor_screen_pos;

    let theme = Theme::default();
    // Composer rect must fit inside the test terminal (24×8): a 4-row box
    // at y=0..4, x=0..20.
    let rect = Rect::new(0, 0, 20, 4);

    // (label, input, byte cursor) spanning ASCII, CJK (wide), mid-string,
    // empty, and a cursor that rests past the last wrapped line.
    let cases: &[(&str, &str, usize)] = &[
        ("ascii end", "hello", 5),
        ("ascii mid", "hello", 2),
        ("empty", "", 0),
        ("cjk end", "中文测试", 12),
        ("cjk mid", "中文测试", 6),
        ("mixed", "a中b文", 5),
        ("past wrap", "aaaa bbbb cccc dd", 16),
    ];

    for (label, input, byte_cursor) in cases {
        let byte_cursor = *byte_cursor;
        // What the draw path places.
        let mut terminal = nuotc::TestTerminal::new(24, 8);
        terminal.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: rect,
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText { input, byte_cursor },
                true,
                true,
                false,
                0,
                0,
                crate::shell::composer_hints::ComposerHints::default(),
            );
        });
        let drawn = match terminal.cursor() {
            nuotc::CursorState::Visible(x, y) => (x, y),
            other => panic!("{label}: caret should be visible, got {other:?}"),
        };

        // What the authoritative geometry function resolves.
        let mut scroll = 0usize;
        let resolved = cursor_screen_pos(rect, input, byte_cursor, &mut scroll, true)
            .unwrap_or_else(|| panic!("{label}: cursor_screen_pos returned None"));

        assert_eq!(
            drawn, resolved,
            "{label} (input={input:?}, byte={byte_cursor}): \
                 draw path did not forward the resolved caret"
        );
    }
}

/// Cursor resolution updates `input_scroll` to keep the final caret inside
/// the visible composer rows.
#[test]
fn cursor_screen_pos_clamps_scroll_like_draw() {
    use crate::shell::composer::cursor_screen_pos;

    // A 20-wide box (text width ~16) with a long input; the box shows only
    // a couple of rows, so a caret near the end forces a scroll.
    let rect = Rect::new(0, 0, 20, 4);
    let input = "word ".repeat(20); // ~100 chars, wraps many times
    let byte_cursor = input.len();

    let mut scroll = 0usize;
    let resolved = cursor_screen_pos(rect, &input, byte_cursor, &mut scroll, true)
        .expect("caret position resolves");

    // The resolved caret must sit on a visible row (within the box's text
    // rows), proving scroll advanced to track it.
    let visible_rows = (rect.height as usize)
        .saturating_sub(nuo_ui_primitives::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
        .max(1);
    let caret_row =
        (resolved.1 - rect.y - nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET) as usize;
    assert!(
        caret_row < visible_rows,
        "resolved caret row {caret_row} outside the {visible_rows} visible rows"
    );
    assert!(scroll > 0, "scroll should have advanced to track the caret");
}

/// A render must preserve a viewport that the user moved away from the
/// logical caret. This is the integration boundary the scroll-state tests do
/// not cover: before this regression test existed, wheel/drag code advanced
/// `input_scroll`, then the very next draw silently snapped it back.
#[test]
fn composer_render_preserves_scroll_excursion_until_caret_follow_rearms() {
    let theme = Theme::default();
    let rect = Rect::new(0, 0, 24, 5); // two visible text rows + chrome
    let input = "a\nb\nc\nd\ne\nf\ng\nh";

    let mut scroll = 0usize;
    let mut layout_map = LayoutMap::new();
    let mut terminal = nuotc::TestTerminal::new(rect.width, rect.height);
    terminal.draw(|f| {
        draw_composer_with_options(
            ComposerProps {
                frame: f,
                input_rect: rect,
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut scroll,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            ComposerDrawOptions {
                focused: true,
                show_caret: true,
                follow_caret: false,
                record: true,
                image_count: 0,
                paste_count: 0,
                hints: crate::shell::composer_hints::ComposerHints::default(),
                ghost_text: None,
            },
        );
    });

    assert_eq!(scroll, 0, "render must preserve the user's top viewport");
    assert!(
        matches!(terminal.cursor(), nuotc::CursorState::Hidden),
        "a caret outside the preserved viewport must not leak into another surface"
    );
    let text_x = rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16;
    let text_y = rect.y + nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    assert_eq!(
        layout_map
            .cursor_at(text_x, text_y)
            .expect("first preserved input row")
            .byte_offset,
        0
    );
    assert_eq!(
        layout_map
            .cursor_at(text_x, text_y + 1)
            .expect("second preserved input row")
            .byte_offset,
        2
    );

    let mut follow_scroll = 0usize;
    let mut follow_terminal = nuotc::TestTerminal::new(rect.width, rect.height);
    follow_terminal.draw(|f| {
        draw_composer_with_options(
            ComposerProps {
                frame: f,
                input_rect: rect,
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut follow_scroll,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            ComposerDrawOptions {
                focused: true,
                show_caret: true,
                follow_caret: true,
                record: false,
                image_count: 0,
                paste_count: 0,
                hints: crate::shell::composer_hints::ComposerHints::default(),
                ghost_text: None,
            },
        );
    });
    assert_eq!(follow_scroll, 6, "re-armed following must reveal the caret");
    assert!(matches!(
        follow_terminal.cursor(),
        nuotc::CursorState::Visible(_, _)
    ));
}

/// The painter half of the Ctrl-J empty-line contract: the caret glyph lands on
/// the empty row the newline created, and that row exists whether or not the
/// caret is resting on it.
///
/// Both halves come from one grid (`composer_grid::TextGrid`): a row's byte
/// range is closed, so an empty line's single byte is a real caret cell instead
/// of a gap the caret falls through into the row below.
#[test]
fn caret_paints_on_the_empty_line_a_newline_creates() {
    let theme = Theme::default();
    // "a\n\nb" is the draft Ctrl-J leaves when it opens a line between "a" and
    // "b"; byte 2 is the empty row itself.
    let input = "a\n\nb";
    let mut terminal = nuotc::TestTerminal::new(20, 6);
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 20, 6),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: 2,
            },
            true,
            true,
            true,
            0,
            0,
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let (x, y) = match terminal.cursor() {
        nuotc::CursorState::Visible(x, y) => (x, y),
        other => panic!("caret should be visible, got {other:?}"),
    };
    assert_eq!(
        y,
        nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET + 1,
        "the caret must sit on the empty middle row"
    );
    assert_eq!(
        x, COMPOSER_PROMPT_PREFIX_COLS as u16,
        "at column 0 of that row"
    );
}

/// A trailing newline is a row, not a caret artifact: the box reserves the same
/// height for the empty line whether the caret is on it or up on the text.
#[test]
fn empty_trailing_line_owns_a_row_independent_of_the_caret() {
    use crate::shell::render::calculate_composer_height;

    let viewport = Rect::new(0, 0, 60, 24);
    let one = calculate_composer_height(viewport, "abc");
    let two = calculate_composer_height(viewport, "abc\n");
    assert_eq!(two - one, 1, "Ctrl-J must grow the box by exactly one row");
    // The layout has no caret input at all any more, so no caret position can
    // change the answer.
    assert_eq!(two, calculate_composer_height(viewport, "abc\n"));
}
