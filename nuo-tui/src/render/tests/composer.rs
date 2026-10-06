//! Composer/input-box rendering tests: wrapping, caret positioning, grapheme/emoji handling, chips, scroll-block, held/queued rows.

use super::*;

#[test]
fn test_wrap_text() {
    let lines = wrap_text("hello world", 5);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].text, "hello");
    assert_eq!(lines[1].text, " worl");
    assert_eq!(lines[2].text, "d");
}

#[test]
fn test_wrap_with_newlines() {
    let lines = wrap_text("hi\nthere", 10);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "hi");
    assert_eq!(lines[1].text, "there");
}

#[test]
fn wrap_avoids_cjk_punctuation_at_line_start() {
    let lines = wrap_text("人生需要坚持，才能前进。", 12);
    assert!(lines.len() > 1);
    assert!(lines.iter().skip(1).all(|line| {
        line.text
            .chars()
            .next()
            .is_none_or(|ch| !prohibited_line_start(ch))
    }));
    assert!(lines.iter().all(|line| {
        line.text
            .chars()
            .last()
            .is_none_or(|ch| !prohibited_line_end(ch))
    }));
}

/// The input box must reserve only a single content row for a short input
/// but grow to fit wrapped text when the input is long.
#[test]
fn input_box_grows_with_wrapped_content() {
    let theme = Theme::default();
    let messages: Vec<TranscriptMessage> = Vec::new();

    fn render_with(theme: &Theme, messages: &[TranscriptMessage], input: &str) -> Rect {
        let mut terminal = nuotc::TestTerminal::new(46, 24);
        let mut rect = Rect::default();
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let r = draw_transcript(
                f,
                &mut layout_map,
                TranscriptProps {
                    messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    backoff_clause: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input,
                    byte_cursor: input.len(),
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
                    theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
            rect = r.input_rect;
        });
        rect
    }

    // Short input: one content line + three chrome rows = 4.
    let short = render_with(&theme, &messages, "hi");
    assert_eq!(short.height, 4);

    // Long input wraps across many lines on a 40-wide terminal; the box
    // must grow beyond the single-line baseline.
    let long_input = "word ".repeat(40);
    let tall = render_with(&theme, &messages, &long_input);
    assert!(
        tall.height > 4,
        "wrapped input should grow the box, got height {}",
        tall.height
    );
    // ...but never more than half the terminal.
    assert!(tall.height <= 12);
}

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
            crate::components::composer_hints::ComposerHints::default(),
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
            crate::components::composer_hints::ComposerHints::default(),
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
        crate::design::COMPOSER_TEXT_ROW_OFFSET + 1,
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
                crate::components::composer_hints::ComposerHints::default(),
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
                crate::design::COMPOSER_TEXT_ROW_OFFSET,
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
                hints: crate::components::composer_hints::ComposerHints::default(),
            },
            "/repeat".len(),
        );
    });
    let buf = terminal.buffer();
    let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    // Every glyph of `/repeat` is bold + brand-colored on the panel bg.
    for (i, ch) in "/repeat".chars().enumerate() {
        let cell = buf.get(text_x + i as u16, text_y).expect("command cell");
        assert_eq!(cell.symbol(), ch.to_string());
        assert_eq!(cell.fg, theme.brand(), "command glyph {ch} lost the accent");
        assert!(
            cell.style.add.contains(nuotc::Modifier::BOLD),
            "command glyph {ch} lost bold"
        );
    }
    // The argument tail (`every minute`) keeps the default text color.
    let arg_start = text_x + "/repeat ".len() as u16;
    let cell = buf.get(arg_start, text_y).expect("argument cell");
    assert_eq!(cell.symbol(), "e");
    assert_eq!(cell.fg, theme.fg(), "argument text must not be accented");
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
                hints: crate::components::composer_hints::ComposerHints::default(),
            },
            "/sessions".len(),
        );
    });
    let buf = terminal.buffer();
    let row1_y = crate::design::COMPOSER_TEXT_ROW_OFFSET + 1;
    // The continuation row keeps the prefix indent before the wrapped text.
    // Find its first non-blank glyph and require the plain text color — the
    // accent must stop at the wrap boundary.
    let row1 = (crate::design::COMPOSER_PROMPT_PREFIX_COLS as u16..16u16)
        .map(|col| buf.get(col, row1_y).expect("continuation cell"))
        .find(|cell| cell.symbol() != " ")
        .expect("continuation row has visible text");
    assert_eq!(
        row1.fg,
        theme.fg(),
        "accent must not bleed onto the wrapped argument row"
    );
}

/// Attachment chips render as distinct colored "pills": a pasted-text
/// chip in the calm text-block blue and an image chip in the warm amber,
/// each bold on a tinted band, while the surrounding prose keeps the
/// normal text color. The color is the identifier's second channel —
/// kind at a glance, payload size in the label.
#[test]
fn draw_composer_paints_paste_and_image_chips_distinctly() {
    let theme = Theme::default();
    let paste_chip = crate::composer_attachments::paste_chip(1, 3, 2048);
    let image_chip = crate::composer_attachments::image_chip(1, 1536);
    let input = format!("see {paste_chip} plus {image_chip} end");
    let mut terminal = nuotc::TestTerminal::new(120, 5);
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 120, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            true,
            false,
            1,
            1,
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    let interior_bg = theme.input_surface();
    // Chip labels use ASCII metadata; display columns
    // come from `str_len`, never from the raw byte length.
    let paste_width = nuotc::text::str_len(&paste_chip);
    let paste_start = text_x + "see ".len() as u16;
    let paste_end = paste_start + paste_width as u16;
    for col in paste_start..paste_end {
        let cell = buf.get(col, text_y).expect("paste chip cell");
        assert_eq!(
            cell.fg,
            theme.chip_paste_fg(),
            "paste chip glyph lost its blue"
        );
        assert_eq!(
            cell.bg,
            theme.chip_paste_bg(interior_bg),
            "paste chip lost its pill band"
        );
        assert!(
            cell.style.add.contains(nuotc::Modifier::BOLD),
            "paste chip glyph lost bold"
        );
    }

    let image_width = nuotc::text::str_len(&image_chip);
    let image_start = text_x + ("see ".len() + paste_width + " plus ".len()) as u16;
    let image_end = image_start + image_width as u16;
    for col in image_start..image_end {
        let cell = buf.get(col, text_y).expect("image chip cell");
        assert_eq!(
            cell.fg,
            theme.chip_image_fg(),
            "image chip glyph lost its amber"
        );
        assert_eq!(
            cell.bg,
            theme.chip_image_bg(interior_bg),
            "image chip lost its pill band"
        );
        assert!(
            cell.style.add.contains(nuotc::Modifier::BOLD),
            "image chip glyph lost bold"
        );
    }

    // The prose around the chips keeps the normal text color on the panel.
    for col in [
        text_x,
        text_x + 2,
        text_x + ("see ".len() + paste_width) as u16,
    ] {
        let cell = buf.get(col, text_y).expect("prose cell");
        assert_eq!(cell.fg, theme.fg(), "prose next to a chip must stay plain");
        assert_eq!(cell.bg, interior_bg, "prose must not pick up a chip band");
    }
}

/// A chip label with **no staged payload** — typed by hand, or left over
/// after the paste was undone — must render as ordinary text, never as a
/// colored pill. The color marks a real attachment; a literal
/// `[Image #1]` that the user merely typed must not pretend one exists.
#[test]
fn draw_composer_leaves_orphan_chip_labels_as_plain_text() {
    let theme = Theme::default();
    // No payload staged at all: `image_count = 0`, `paste_count = 0`.
    let orphan_image = "[Image #1]".to_string();
    let orphan_paste = "[Pasted text #1 +5 lines]".to_string();
    let input = format!("typed {orphan_image} and {orphan_paste} here");
    let mut terminal = nuotc::TestTerminal::new(100, 5);
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 100, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            true,
            false,
            0,
            0,
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    let interior_bg = theme.input_surface();

    // Every glyph of both orphan labels keeps the plain text color on the
    // plain interior background — no pill band, no kind color, no bold.
    for (offset, label) in [
        ("typed ".len(), &orphan_image),
        ("typed [Image #1] and ".len(), &orphan_paste),
    ] {
        let start = text_x + offset as u16;
        let end = start + nuotc::text::str_len(label) as u16;
        for col in start..end {
            let cell = buf.get(col, text_y).expect("orphan chip cell");
            assert_eq!(
                cell.fg,
                theme.fg(),
                "orphan label {label:?} must keep plain text color at col {col}"
            );
            assert_eq!(
                cell.bg, interior_bg,
                "orphan label {label:?} must not get a pill band at col {col}"
            );
            assert!(
                !cell.style.add.contains(nuotc::Modifier::BOLD),
                "orphan label {label:?} must not be bold at col {col}"
            );
        }
    }
}

/// A real chip (payload staged) is colored while an orphan label typed
/// next to it stays plain — the pill reflects the actual staged state of
/// each block, so one never masks the other.
#[test]
fn draw_composer_colors_only_backed_chips_when_mixed() {
    let theme = Theme::default();
    let real_paste = crate::composer_attachments::paste_chip(1, 3, 2048);
    let orphan_image = "[Image #1]".to_string();
    // One paste payload staged; the image chip is a typed orphan.
    let input = format!("{real_paste} then {orphan_image} end");
    let mut terminal = nuotc::TestTerminal::new(100, 5);
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 100, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            true,
            false,
            0, // image_count: no image payload staged,
            1, // paste_count: one paste payload staged,
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    let interior_bg = theme.input_surface();

    // The backed paste chip gets the blue pill.
    let paste_width = nuotc::text::str_len(&real_paste);
    let paste_end = text_x + paste_width as u16;
    for col in text_x..paste_end {
        let cell = buf.get(col, text_y).expect("backed paste cell");
        assert_eq!(
            cell.fg,
            theme.chip_paste_fg(),
            "backed paste chip lost its blue"
        );
        assert_eq!(
            cell.bg,
            theme.chip_paste_bg(interior_bg),
            "backed paste chip lost its band"
        );
    }

    // The orphan image label stays plain text.
    let orphan_start = text_x + ("".len() + paste_width + " then ".len()) as u16;
    let orphan_end = orphan_start + nuotc::text::str_len(&orphan_image) as u16;
    for col in orphan_start..orphan_end {
        let cell = buf.get(col, text_y).expect("orphan image cell");
        assert_eq!(
            cell.fg,
            theme.fg(),
            "orphan image label must stay plain text"
        );
        assert_eq!(
            cell.bg, interior_bg,
            "orphan image label must not get a pill band"
        );
    }
}

/// Selecting a chip keeps its identity color (so the user can still tell
/// which pasted block is selected) but the selection wins on background —
/// the highlighted slice stays a uniform `selected_bg`.
#[test]
fn draw_composer_chip_keeps_identity_color_under_selection() {
    let theme = Theme::default();
    let paste_chip = crate::composer_attachments::paste_chip(1, 3, 2048);
    let input = format!("see {paste_chip} end");
    let mut terminal = nuotc::TestTerminal::new(80, 5);
    // Select exactly the chip bytes (absolute offsets into `input`).
    let sel_lo = "see ".len();
    let sel_hi = sel_lo + paste_chip.len();
    use crate::model::layout::SemanticCursor;
    let selection = SelectionState::Range {
        anchor: SemanticCursor::new(crate::composer::INPUT_MSG_IDX, 0, sel_lo),
        head: SemanticCursor::new(crate::composer::INPUT_MSG_IDX, 0, sel_hi),
    };
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 80, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &selection,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            false,
            false,
            0,
            1,
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    let chip_start = text_x + sel_lo as u16;
    let chip_end = chip_start + nuotc::text::str_len(&paste_chip) as u16;
    for col in chip_start..chip_end {
        let cell = buf.get(col, text_y).expect("selected chip cell");
        assert_eq!(
            cell.fg,
            theme.chip_paste_fg(),
            "selected chip must keep its identity color"
        );
        assert_eq!(
            cell.bg,
            theme.selected(),
            "selection must win the background"
        );
    }
}

/// A chip split across a wrap boundary paints both fragments with the
/// same pill, so a pasted block stays visually contiguous as it wraps
/// inside the input box.
#[test]
fn draw_composer_chip_pill_continues_across_wrap() {
    let theme = Theme::default();
    let image_chip = crate::composer_attachments::image_chip(1, 1536);
    // Narrow text area (16 - 2 prefix - 2 pad = 12 cols) forces the
    // `[Image #1 (1.5 KB)]` label onto its own wrapped fragment.
    let input = format!("xx {image_chip} yy");
    let mut terminal = nuotc::TestTerminal::new(16, 6);
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 16, 5),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            true,
            false,
            1,
            0,
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let interior_bg = theme.input_surface();
    // Scan every rendered row: every glyph that belongs to the chip label
    // (ignoring spaces, which also appear in the prompt indent and the
    // frame padding) must carry the chip band, proving the pill survives
    // the wrap instead of reverting to plain text on the continuation row.
    let chip_glyphs: Vec<char> = image_chip.chars().filter(|c| *c != ' ').collect();
    // Row 0 is the top border row and row 4 the bottom border row (neither
    // is chip territory); chip text starts on the prompt row (row 1) and
    // may wrap into the rows below, up to the bottom border.
    for row in 1..4u16 {
        for col in 0..16u16 {
            let cell = buf.get(col, row).expect("row cell");
            if chip_glyphs.contains(&cell.symbol().chars().next().unwrap_or('\0')) {
                assert_eq!(
                    cell.bg,
                    theme.chip_image_bg(interior_bg),
                    "wrapped chip fragment at ({col},{row}) lost its band"
                );
                assert_eq!(
                    cell.fg,
                    theme.chip_image_fg(),
                    "wrapped chip fragment at ({col},{row}) lost its amber"
                );
            }
        }
    }
}

/// The composer must forward its resolved cursor coordinate unchanged to
/// the frame for ASCII, CJK, empty, and wrapped inputs.
#[test]
fn cursor_screen_pos_matches_drawn_caret() {
    use crate::composer::cursor_screen_pos;

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
                crate::components::composer_hints::ComposerHints::default(),
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
    use crate::composer::cursor_screen_pos;

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
        .saturating_sub(crate::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
        .max(1);
    let caret_row = (resolved.1 - rect.y - crate::design::COMPOSER_TEXT_ROW_OFFSET) as usize;
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
                hints: crate::components::composer_hints::ComposerHints::default(),
            },
        );
    });

    assert_eq!(scroll, 0, "render must preserve the user's top viewport");
    assert!(
        matches!(terminal.cursor(), nuotc::CursorState::Hidden),
        "a caret outside the preserved viewport must not leak into another surface"
    );
    let text_x = rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16;
    let text_y = rect.y + crate::design::COMPOSER_TEXT_ROW_OFFSET;
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
                hints: crate::components::composer_hints::ComposerHints::default(),
            },
        );
    });
    assert_eq!(follow_scroll, 6, "re-armed following must reveal the caret");
    assert!(matches!(
        follow_terminal.cursor(),
        nuotc::CursorState::Visible(_, _)
    ));
}

/// (head + continuation), cover exactly the selected glyphs, and leave the
/// trailing pad on the panel background — no extra glyph, no half-highlighted
/// wide char. Exercises the full-3-CJK selection the live bug report used.
#[test]
fn composer_cjk_selection_covers_full_width_glyphs() {
    use crate::model::layout::SemanticCursor;
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
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    let g = terminal.buffer();
    let y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
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
            cell.bg, want,
            "{label} at col {col}: bg {:?} expected {:?}",
            cell.bg, want
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
    use crate::model::layout::SemanticCursor;

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
            crate::components::composer_hints::ComposerHints::default(),
        );
    });

    let y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
    let buffer = terminal.buffer();
    let interior_bg = theme.input_surface();

    assert_eq!(buffer.get(2, y).unwrap().symbol(), "你");
    assert_eq!(buffer.get(2, y).unwrap().width, 2);
    assert_eq!(buffer.get(3, y).unwrap().symbol(), " ");
    assert_eq!(buffer.get(3, y).unwrap().width, 0);
    assert_eq!(buffer.get(4, y).unwrap().symbol(), "好");
    assert_eq!(buffer.get(4, y).unwrap().width, 2);
    assert_eq!(buffer.get(5, y).unwrap().symbol(), " ");
    assert_eq!(buffer.get(5, y).unwrap().width, 0);
    assert_eq!(
        buffer.get(6, y).unwrap().symbol(),
        " ",
        "tail cell must not contain a duplicate glyph"
    );

    for col in 2..=5 {
        assert_eq!(
            buffer.get(col, y).unwrap().bg,
            sel_bg,
            "col {col} should be selected"
        );
    }
    assert_eq!(
        buffer.get(6, y).unwrap().bg,
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
            crate::components::composer_hints::ComposerHints::default(),
        );
    });

    let y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
    let buffer = terminal.buffer();
    // Col 0: '›', Col 1: gap, Col 2: 'h' (selected), Col 3: 'e' (unselected)
    assert_eq!(
        buffer.get(2, y).unwrap().bg,
        sel_bg,
        "col 2 ('h') must be selected"
    );
    assert_eq!(
        buffer.get(3, y).unwrap().bg,
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
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    let buffer = terminal.buffer();
    for col in 2..7 {
        assert_eq!(
            buffer.get(col, y).unwrap().bg,
            sel_bg,
            "col {col} in 'hello' must be selected"
        );
    }
    assert_eq!(
        buffer.get(7, y).unwrap().bg,
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
    let text_row = crate::design::COMPOSER_TEXT_ROW_OFFSET;

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
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    let prefix = crate::design::COMPOSER_PROMPT_PREFIX_COLS as u16;
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
                crate::components::composer_hints::ComposerHints::default(),
            );
        });
        (0..10u16)
            .map(|c| t.buffer().get(c, text_row).unwrap().bg)
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

#[test]
fn user_message_and_composer_keep_symmetric_panel_padding() {
    let theme = Theme::default();
    let user_bg = theme.user_surface();
    let app_bg = theme.surface();
    let input_bg = theme.input_surface();
    let width = 60u16;
    let mut terminal = nuotc::TestTerminal::new(width, 24);

    // A long user message fills the first wrapped line edge to edge, so the
    // right-side padding is only present if the wrap width reserves it.
    let messages = vec![TranscriptMessage::new(
        nuo_wire::Role::User,
        "x".repeat(200),
    )];
    let long_input = "y".repeat(200);

    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        // draw_transcript only computes the input box geometry; the composer
        // itself is drawn separately (as the live app does), using the
        // returned input_rect.
        let render = draw_transcript(
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
                input: &long_input,
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
        let mut input_scroll = 0;
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: render.input_rect,
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut input_scroll,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &long_input,
                byte_cursor: 0,
            },
            true,
            true,
            false,
            0,
            0,
            crate::components::composer_hints::ComposerHints::default(),
        );
    });

    let buffer = terminal.buffer();

    // Find the first user-message text row. Layout (60-col terminal):
    //   cols 0,1  = global app_bg (viewport margin)
    //   cols 2,3  = user_panel_bg inner pad (USER_MESSAGE_TEXT_GAP_COLS)
    //   col  4+   = text
    let user_row = (0..buffer.area().height)
        .find(|&y| {
            let c4 = &buffer[(4, y)];
            c4.symbol() == "x" && c4.bg == user_bg
        })
        .expect("user message row exists");

    // Left: 2-col app_bg outer gutter (viewport margin + entry inset),
    // then 2-col user_panel_bg inner pad.
    assert_eq!(buffer[(0, user_row)].bg, app_bg, "left outer gutter");
    assert_eq!(buffer[(1, user_row)].bg, app_bg, "left outer gutter");
    assert_eq!(
        buffer[(2, user_row)].bg,
        user_bg,
        "left inner padding must be user_panel_bg"
    );
    assert_eq!(
        buffer[(3, user_row)].bg,
        user_bg,
        "left inner padding is 2 cols, not 1"
    );
    assert_eq!(buffer[(4, user_row)].symbol(), "x", "text starts at col 4");

    // Right: 2-col user_panel_bg inner pad, then 2-col app_bg outer gutter.
    // user_text_width = (band_w) - (TEXT_GAP + RIGHT_PAD) = (60-4) - 4 = 52
    // -> text fills cols 4..56.
    assert_eq!(
        buffer[(56, user_row)].symbol(),
        " ",
        "right inner padding must stay clear of wrapped text"
    );
    assert_eq!(buffer[(56, user_row)].bg, user_bg, "right inner padding");
    assert_eq!(buffer[(57, user_row)].bg, user_bg, "right inner padding");
    assert_eq!(buffer[(58, user_row)].bg, app_bg, "right outer gutter");
    assert_eq!(buffer[(59, user_row)].bg, app_bg, "right outer gutter");

    // Composer: the panel starts at x = FOOTER_H_INSET (2) and carries its
    // own tinted bg. `›` at x=2, text from x=4 — the same text column the
    // user panel uses (cols 2,3 are the prompt prefix; text lands at 4) —
    // so sent messages and the live input read as one left margin.
    let composer_row = (0..buffer.area().height)
        .find(|&y| {
            let c4 = &buffer[(4, y)];
            c4.symbol() == "y" && c4.bg == input_bg
        })
        .expect("composer row exists");
    assert_eq!(
        buffer[(2, composer_row)].symbol(),
        "›",
        "composer prompt opens the panel at the footer inset"
    );
    assert_eq!(
        buffer[(2, composer_row)].bg,
        input_bg,
        "composer panel begins at the footer inset"
    );
    assert_eq!(
        buffer[(4, composer_row)].symbol(),
        "y",
        "composer text starts at the same column as user text"
    );
    // full_w (composer panel) = 60 - 2*FOOTER_H_INSET = 56, spanning
    // x=2..58; the last panel column stays panel-tinted air.
    assert_eq!(
        buffer[(57, composer_row)].bg,
        input_bg,
        "composer panel air extends to its last column"
    );
    assert_eq!(
        buffer[(58, composer_row)].bg,
        app_bg,
        "composer right outer gutter"
    );
    assert_eq!(
        buffer[(59, composer_row)].bg,
        app_bg,
        "composer right outer gutter"
    );
}

/// The input box's tinted panel owns two surface tokens — active (the box
/// owns the keyboard) and inactive (a transcript step owns it). The two
/// backgrounds must be visibly different colors, so "where does typing
/// land" is legible from the panel alone, and neither may melt into the
/// app background. Regression guard for the activated/deactivated input
/// being indistinguishable.
#[test]
fn composer_focused_and_unfocused_panels_render_distinct_backgrounds() {
    let theme = Theme::default();
    let active_bg = theme.input_surface();
    let inactive_bg = theme.input_surface_inactive();
    let app_bg = theme.surface();
    assert_ne!(active_bg, inactive_bg, "pair must be distinct colors");

    let panel_at = |focused: bool| -> nuotc::Color {
        let mut terminal = nuotc::TestTerminal::new(30, 6);
        terminal.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: Rect::new(0, 0, 30, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText {
                    input: "hello",
                    byte_cursor: 5,
                },
                focused,
                false,
                false,
                0,
                0,
                crate::components::composer_hints::ComposerHints::default(),
            );
        });
        let buffer = terminal.buffer();
        // The top breathing row: painted unconditionally in panel bg, so its
        // background carries the surface color either way.
        let cell = &buffer[(0, 0)];
        assert_eq!(cell.symbol(), " ", "top breathing row is blank");
        cell.bg
    };

    let rendered_active = panel_at(true);
    let rendered_inactive = panel_at(false);
    assert_eq!(
        rendered_active, active_bg,
        "focused box must paint the input-surface color"
    );
    assert_eq!(
        rendered_inactive, inactive_bg,
        "unfocused box must paint the input-surface-inactive color"
    );
    assert_ne!(
        rendered_active, app_bg,
        "focused panel must not melt into the app background"
    );
    assert_ne!(
        rendered_inactive, app_bg,
        "unfocused panel must not melt into the app background"
    );
    // The whole box carries the tint, not just an edge: every row of the
    // four-row panel is the same surface.
    let mut terminal = nuotc::TestTerminal::new(30, 6);
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 30, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: "hello",
                byte_cursor: 5,
            },
            true,
            false,
            false,
            0,
            0,
            crate::components::composer_hints::ComposerHints::default(),
        );
    });
    for y in 0..4u16 {
        assert_eq!(
            terminal.buffer()[(15, y)].bg,
            active_bg,
            "every composer row carries the input surface"
        );
    }
}

/// A queued user message (one staged in the send queue waiting for the
/// in-flight turn to finish) must render with the dimmer
/// `user_panel_bg_queued` band and a visible "⏸ Queued" badge so the user
/// can tell their message is pending, not delivered.
#[test]
fn queued_user_message_renders_badge_and_dimmer_bg() {
    let theme = Theme::default();
    let _queued_bg = theme.user_surface_queued();
    let delivered_bg = theme.user_surface();
    let width = 46u16;
    let mut terminal = nuotc::TestTerminal::new(width, 20);

    let messages = vec![
        TranscriptMessage::new(nuo_wire::Role::User, "first steer message").queued(),
        TranscriptMessage::new(nuo_wire::Role::User, "second steer message").queued(),
    ];
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        let _ = draw_transcript(
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

    let buffer = terminal.buffer();

    // Both queued panels must carry the queued bg, never the delivered bg.
    // Scan the inner-pad columns (2,3) of every row for any cell painted
    // with the delivered bg — that would mean a queued message leaked the
    // wrong surface.
    for y in 0..buffer.area().height {
        for x in 2..4 {
            let bg = buffer[(x, y)].bg;
            assert_ne!(
                bg, delivered_bg,
                "queued panels must never carry the delivered bg, found at ({},{})",
                x, y
            );
        }
    }

    // Each queued user message renders one queued status strip OUTSIDE
    // the panel (on plain `surface`, above the panel's top transition).
    let row_text = |y: u16| -> String {
        (0..buffer.area().width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    };
    let queued_headers = (0..buffer.area().height)
        .filter(|&y| row_text(y).contains("queued"))
        .count();
    assert_eq!(
        queued_headers, 2,
        "each queued user message must render one queued header row, got {}",
        queued_headers
    );
}

/// ADR-0126: a *held* insert — one whose round ended (naturally or by an
/// Esc Esc interrupt) before admission — renders the same pending panel
/// as a queued message, with a label that spells out the different fate:
/// `⏸ Held for next round`, not the plain `⏸ Queued`.
#[test]
fn held_insert_renders_the_held_label_and_dimmer_bg() {
    use crate::model::document::DeliveryStatus;
    let theme = Theme::default();
    let delivered_bg = theme.user_surface();
    let width = 56u16;
    let mut terminal = nuotc::TestTerminal::new(width, 16);

    let mut held = TranscriptMessage::new(nuo_wire::Role::User, "held steer");
    held.delivery = DeliveryStatus::HeldNextRound;
    held.origin = crate::model::document::UserMessageOrigin::Steer;
    let messages = vec![held];

    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        let _ = draw_transcript(
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

    let buffer = terminal.buffer();
    // The held panel carries the dimmer pending band, never the delivered
    // one.
    for y in 0..buffer.area().height {
        for x in 2..4 {
            assert_ne!(
                buffer[(x, y)].bg,
                delivered_bg,
                "a held panel must never carry the delivered bg, found at ({},{})",
                x,
                y
            );
        }
    }
    // The full label renders (spelled out as upright status).
    let row_text = |y: u16| -> String {
        (0..buffer.area().width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    };
    let rendered = (0..buffer.area().height)
        .map(row_text)
        .any(|row| row.contains("held for next round"));
    assert!(
        rendered,
        "the held entry must spell out its fate (held for next round)"
    );
}

/// The dropdown shares the composer's surface language, not the permission
/// sheet's: it opens and closes with full panel-bg padding rows and never
/// paints a full-height brand-colored left column (which would read as
/// selection/severity). The top and bottom rows must be solid panel
/// background (no half-block `▄`/`▀` glyphs), and the left column must NOT
/// be brand-colored.
#[test]
fn history_panel_uses_composer_padding_not_brand_column() {
    let theme = Theme::default();
    let history: Vec<nuo_wire::HistoryEntry> = ["one", "two", "three"]
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            nuo_wire::HistoryEntry::new(
                text.to_string(),
                Some(format!("s{i}")),
                None,
                i as u64,
            )
        })
        .collect();
    let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
    let ranked = crate::fuzzy::rank(&texts, "");
    let input_rect = nuotc::Rect::new(0, 40, 30, 2);
    let mut terminal = nuotc::TestTerminal::new(30, 42);
    let mut panel: Option<nuotc::Rect> = None;
    terminal.draw(|f| {
        panel = draw_history_panel(
            f,
            crate::overlays::history::HistoryPanelProps {
                history: &history,
                ranked: &ranked,
                modal_index: 0,
                scroll: &mut 0,
                follow_selection: true,
                input_rect,
                activity_height: 0,
            },
            &theme,
        )
    });
    let panel = panel.expect("panel should render");
    let buf = terminal.buffer();

    // Top row is a full panel-bg padding row (no half-block glyph).
    let top_left = buf.get(panel.x, panel.y).expect("top-left cell");
    assert_eq!(
        top_left.bg,
        theme.panel(),
        "top edge must be a solid panel-bg row, matching the composer's padding"
    );
    assert_eq!(
        top_left.symbol(),
        " ",
        "top edge must be blank (no ▄ transition glyph)"
    );
    // Bottom row is likewise a solid panel-bg padding row.
    let bottom_left = buf
        .get(panel.x, panel.y + panel.height - 1)
        .expect("bottom-left cell");
    assert_eq!(
        bottom_left.bg,
        theme.panel(),
        "bottom edge must be a solid panel-bg row, matching the composer's padding"
    );
    assert_eq!(
        bottom_left.symbol(),
        " ",
        "bottom edge must be blank (no ▀ transition glyph)"
    );

    // No full-height brand column: the background of the left column on the
    // header row (which is never selection-tinted) must NOT be the brand
    // color. A brand column would paint every left-edge cell, including the
    // header's, with brand as its background. The header sits one row below
    // the top transition edge.
    let header_left = buf.get(panel.x, panel.y + 1).expect("header left cell");
    assert_ne!(
        header_left.bg,
        theme.brand(),
        "no full-height brand left column — the composer edge language has none"
    );
}

/// Same clamp check with a multi-codepoint emoji grapheme (ZWJ family) in
/// the heading: `wrap_text` measures per-char (overcounting the sequence)
/// while the grid renders per-grapheme, so this guards the underline width
/// against the char-vs-grapheme measurement split.
#[test]
fn h1_underline_clamps_with_emoji_grapheme() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(60, 12);
    let messages = vec![TranscriptMessage::new(
        nuo_wire::Role::Assistant,
        "# 👨‍👩‍👧 OKX\n\nbody\n",
    )];
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
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
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    let underline = nuotc::Modifier::UNDERLINE;

    let mut x_pos = None;
    'outer: for y in 0..buffer.area().height {
        for x in 0..width {
            if buffer[(x, y)].symbol() == "X" {
                x_pos = Some((x, y));
                break 'outer;
            }
        }
    }
    let (xx, xy) = x_pos.expect("heading 'X' cell exists");

    assert!(
        buffer[(xx, xy)].style.add.contains(underline),
        "heading 'X' text cell must be UNDERLINED"
    );
    let trailing = xx + 1;
    assert!(trailing < width, "trailing cell within grid");
    assert!(
        !buffer[(trailing, xy)].style.add.contains(underline),
        "underline must not bleed past emoji heading at x={trailing}"
    );
}

// Tinted-panel composer geometry (the four-row redesign)

fn frame_row_text(terminal: &mut nuotc::TestTerminal, y: u16) -> String {
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    (0..width).map(|x| buffer[(x, y)].symbol()).collect()
}

fn draw_frame_composer(
    input: &str,
    focused: bool,
    hints: crate::components::composer_hints::ComposerHints,
) -> nuotc::TestTerminal {
    let mut terminal = nuotc::TestTerminal::new(60, 6);
    let theme = Theme::default();
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 60, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            focused,
            false,
            false,
            0,
            0,
            hints,
        );
    });
    terminal
}

/// The composer is a tinted panel laid out as four rows for a one-line
/// draft: blank breathing row, the text row (opened by `›`), a blank gap
/// row, and the meta row carrying the Enter sentence. The right side of
/// the meta row carries nothing while every wrapped row is visible — the
/// position readout only exists once the box clips rows (see
/// `composer_overflow_indicates_hidden_lines_and_position`).
#[test]
fn composer_panel_carries_the_hint_row() {
    let mut terminal = draw_frame_composer("hello", true, Default::default());

    let row1 = frame_row_text(&mut terminal, 0);
    assert!(
        row1.trim().is_empty(),
        "row 1 is a blank breathing row: {row1:?}"
    );

    let row2 = frame_row_text(&mut terminal, 1);
    assert!(row2.starts_with("› hello"), "prompt + text: {row2:?}");

    let row3 = frame_row_text(&mut terminal, 2);
    assert!(row3.trim().is_empty(), "row 3 is a blank gap row: {row3:?}");

    let row4 = frame_row_text(&mut terminal, 3);
    assert!(
        row4.contains("Enter send"),
        "hint row carries the Enter sentence: {row4:?}"
    );
    assert!(
        !row4.contains("lines") && !row4.contains("chars"),
        "a fully visible draft carries no right-side readout: {row4:?}"
    );
}

/// The hint sentence names the delivery group the buffer will land in, and
/// mid-round it also carries the Tab toggle.
#[test]
fn composer_hint_sentence_names_the_delivery_group() {
    use crate::components::composer_hints::{ComposeTarget, ComposerHints};

    let hints = ComposerHints {
        compose_target: ComposeTarget::Running(crate::app::ComposerSendMode::Steer),
        ..Default::default()
    };
    let mut terminal = draw_frame_composer("wait", true, hints);
    let row4 = frame_row_text(&mut terminal, 3);
    assert!(row4.contains("send steer"), "steer verb: {row4:?}");
    assert!(
        row4.contains("follow-up mode"),
        "follow-up mode affordance: {row4:?}"
    );

    let hints_followup = ComposerHints {
        compose_target: ComposeTarget::Running(crate::app::ComposerSendMode::FollowUp),
        ..Default::default()
    };
    let mut terminal = draw_frame_composer("wait", true, hints_followup);
    let row4 = frame_row_text(&mut terminal, 3);
    assert!(row4.contains("queue follow-up"), "follow-up verb: {row4:?}");
    assert!(
        row4.contains("steer mode"),
        "steer mode affordance: {row4:?}"
    );
}

/// Once the draft outgrows the box, the three-part overflow affordance
/// lights up: the breathing row above the text carries `↑ N lines`, the gap
/// row below carries `↓ N lines`, and the meta row's right side swaps in a
/// `first–last/total lines` position readout. The box height never changes
/// crossing the threshold — the indicators reuse existing chrome rows.
#[test]
fn composer_overflow_indicates_hidden_lines_and_position() {
    let theme = Theme::default();
    // 12 words ≈ 65 cols: three wrapped rows on a 60-col panel whose height
    // leaves room for exactly one visible text row.
    let input = "one two three four five six seven eight nine ten eleven twelve ";
    let draw = |byte_cursor: usize, scroll: &mut usize| {
        let mut terminal = nuotc::TestTerminal::new(60, 6);
        terminal.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: Rect::new(0, 0, 60, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: scroll,
                    selection: &SelectionState::None,
                },
                ComposerText { input, byte_cursor },
                true,
                false,
                false,
                0,
                0,
                crate::components::composer_hints::ComposerHints::default(),
            );
        });
        terminal
    };

    // Caret at the end: the caret-follow clamp scrolls the last row into
    // view, hiding the rows above → the ↑ indicator + position readout.
    let mut scroll = 0usize;
    let mut terminal = draw(input.len(), &mut scroll);
    assert!(scroll > 0, "caret-follow scroll hides rows above: {scroll}");
    let top = frame_row_text(&mut terminal, 0);
    assert!(
        top.contains('↑') && top.trim_end().ends_with("lines"),
        "breathing row carries the ↑ indicator: {top:?}"
    );
    let gap = frame_row_text(&mut terminal, 2);
    assert!(
        gap.trim().is_empty(),
        "nothing hides below when the caret rests on the last row: {gap:?}"
    );
    let meta = frame_row_text(&mut terminal, 3);
    assert!(
        meta.contains(&format!("–{}/{} lines", scroll + 1, scroll + 1)),
        "position readout names the visible span: {meta:?}"
    );

    // Caret at the start instead: the viewport stays at the top, hiding
    // rows below → the ↓ indicator, and the readout names the first row.
    let mut scroll_top = 0usize;
    let mut terminal = draw(0, &mut scroll_top);
    assert_eq!(
        scroll_top, 0,
        "viewport rests at the top with the caret there"
    );
    let top = frame_row_text(&mut terminal, 0);
    assert!(
        top.trim().is_empty(),
        "no ↑ while nothing hides above: {top:?}"
    );
    let gap = frame_row_text(&mut terminal, 2);
    assert!(
        gap.contains('↓') && gap.trim_end().ends_with("lines"),
        "gap row carries the ↓ indicator: {gap:?}"
    );
    let meta = frame_row_text(&mut terminal, 3);
    assert!(
        meta.contains("1–1/"),
        "readout names the first visible row: {meta:?}"
    );
}

/// While blurred the panel keeps its tint but sheds the hint sentence — the
/// meta text never competes with a step-focused transcript.
#[test]
fn composer_panel_blurred_sheds_the_hint_row() {
    let mut terminal = draw_frame_composer("hello", false, Default::default());
    let row4 = frame_row_text(&mut terminal, 3);
    assert!(!row4.contains("Enter"), "blurred row sheds the keys");
    assert!(
        !row4.contains("lines"),
        "blurred row sheds the position readout"
    );
    let buffer = terminal.buffer();
    assert_eq!(
        buffer[(0, 3)].bg,
        Theme::default().input_surface_inactive(),
        "blurred panel keeps its recessed tint"
    );
}

/// Narrow panels degrade instead of overflowing: the counter goes first,
/// then the sentence's optional clauses, and every row stays exactly the
/// panel width.
#[test]
fn composer_panel_degrades_gracefully_on_narrow_widths() {
    let theme = Theme::default();
    for width in [12u16, 9, 7] {
        let mut terminal = nuotc::TestTerminal::new(width, 6);
        terminal.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: Rect::new(0, 0, width, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText {
                    input: "hi",
                    byte_cursor: 2,
                },
                true,
                false,
                false,
                0,
                0,
                crate::components::composer_hints::ComposerHints::default(),
            );
        });
        for y in 0..4u16 {
            let row = frame_row_text(&mut terminal, y);
            assert_eq!(
                row.chars().count(),
                width as usize,
                "w={width} y={y}: rows never exceed the panel width"
            );
        }
        // The recessed/active tint still covers the full box on every row.
        let buffer = terminal.buffer();
        for y in 0..4u16 {
            assert_eq!(
                buffer[(width - 1, y)].bg,
                theme.input_surface(),
                "w={width} y={y}: panel tint reaches the last column"
            );
        }
    }
}

#[test]
fn composer_top_row_renders_history_mode_badges() {
    use crate::components::composer_hints::{ComposeTarget, ComposerHints};

    // HistorySearch mode badge on row 0 right side
    let search_hints = ComposerHints {
        compose_target: ComposeTarget::HistorySearch,
        ..Default::default()
    };
    let mut terminal = draw_frame_composer("query", true, search_hints);
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.contains("[history search · draft saved]"),
        "top row has history search badge: {row0:?}"
    );
}

/// The inline ↑/↓ recall badge (ADR-0192): when the recall pointer sits on a
/// history row the top breathing row declares it — 1-based position over the
/// session slice, the `draft saved` reassurance while the stash is intact,
/// the `edited` clause once the buffer forks from the loaded row. Draft mode
/// renders the plain breathing row (no badge, zero indication tax).
#[test]
fn composer_top_row_declares_the_history_recall_pointer() {
    use crate::components::composer_hints::{ComposeTarget, ComposerHints};

    let mut hints = ComposerHints {
        compose_target: ComposeTarget::HistoryRecall,
        history_recall: Some((1, 17, false)),
        recall_draft_saved: true,
        ..Default::default()
    };
    let mut terminal = draw_frame_composer("recalled prompt", true, hints);
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.contains("[history 1/17 · draft saved]"),
        "full-width badge names position, total, and the draft reassurance: {row0:?}"
    );

    // The edited clause replaces the reassurance once the buffer forks.
    hints.history_recall = Some((3, 17, true));
    let mut terminal = draw_frame_composer("recalled prompt EDITED", true, hints);
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.contains("[history 3/17 · edited]"),
        "an edited buffer is declared on the badge: {row0:?}"
    );

    // Draft mode: no badge at all.
    let mut terminal = draw_frame_composer("live draft", true, ComposerHints::default());
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.trim().is_empty(),
        "draft mode renders the plain breathing row, no badge: {row0:?}"
    );
}

/// The badge's width ladder: the full clause set on comfortable panels, the
/// bare `position/total` pointer when space runs out — the pointer is the
/// last element to go because it is the badge's entire point.
#[test]
fn composer_recall_badge_degrades_with_width() {
    use crate::components::composer_hints::{ComposeTarget, ComposerHints};

    let hints = ComposerHints {
        compose_target: ComposeTarget::HistoryRecall,
        history_recall: Some((2, 9, false)),
        recall_draft_saved: true,
        ..Default::default()
    };

    // Narrow panel: pointer only.
    let mut terminal = nuotc::TestTerminal::new(20, 6);
    let theme = Theme::default();
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
                input: "row",
                byte_cursor: 3,
            },
            true,
            false,
            false,
            0,
            0,
            hints,
        );
    });
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.contains("[2/9]"),
        "narrow panel degrades to the bare pointer: {row0:?}"
    );
    assert!(
        !row0.contains("draft saved"),
        "the reassurance clause drops first: {row0:?}"
    );
}

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
            crate::components::composer_hints::ComposerHints::default(),
        );
    });

    // Verify composer_rect is recorded
    assert_eq!(layout_map.composer_rect(), Some(input_rect));

    // 1. Click on Row 0 (top chrome row): y = 1 -> snaps to start of input
    let target_row0 = crate::interaction::classify_click(&layout_map, 10, 1);
    assert_eq!(
        target_row0,
        crate::interaction::ClickTarget::InputBox {
            cursor: crate::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 0)
        }
    );

    // 2. Click on Row 1 (text row): y = 2 -> precise text cursor
    let target_row1 =
        crate::interaction::classify_click(&layout_map, COMPOSER_PROMPT_PREFIX_COLS as u16 + 2, 2);
    assert!(matches!(
        target_row1,
        crate::interaction::ClickTarget::InputBox { .. }
    ));

    // 3. Click on Row 2 (gap row): y = 3 -> snaps to end of input
    let target_row2 = crate::interaction::classify_click(&layout_map, 10, 3);
    assert_eq!(
        target_row2,
        crate::interaction::ClickTarget::InputBox {
            cursor: crate::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 11)
        }
    );

    // 4. Click on Row 3 (hint row): y = 4 -> snaps to end of input
    let target_row3 = crate::interaction::classify_click(&layout_map, 10, 4);
    assert_eq!(
        target_row3,
        crate::interaction::ClickTarget::InputBox {
            cursor: crate::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 11)
        }
    );

    // 5. Click outside the composer component (e.g. y = 0 or y = 5) -> Dead
    assert_eq!(
        crate::interaction::classify_click(&layout_map, 10, 0),
        crate::interaction::ClickTarget::Dead
    );
    assert_eq!(
        crate::interaction::classify_click(&layout_map, 10, 5),
        crate::interaction::ClickTarget::Dead
    );
}
