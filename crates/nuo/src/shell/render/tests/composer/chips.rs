//! Paste and image chip rendering.

use super::*;

#[test]
fn draw_composer_paints_paste_and_image_chips_distinctly() {
    let theme = Theme::default();
    let paste_chip = nuo_view::composer_attachments::paste_chip(1, 3, 2048);
    let image_chip = nuo_view::composer_attachments::image_chip(1, 1536);
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
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let text_y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    let interior_bg = theme.input_surface();
    // Chip labels use ASCII metadata; display columns
    // come from `str_len`, never from the raw byte length.
    let paste_width = nuotc::str_len(&paste_chip);
    let paste_start = text_x + "see ".len() as u16;
    let paste_end = paste_start + paste_width as u16;
    for col in paste_start..paste_end {
        let cell = buf.get(col, text_y).expect("paste chip cell");
        assert_eq!(
            cell.style.fg,
            theme.chip_paste_fg(),
            "paste chip glyph lost its blue"
        );
        assert_eq!(
            cell.style.bg,
            theme.chip_paste_bg(interior_bg),
            "paste chip lost its pill band"
        );
        assert!(
            cell.style.add.contains(nuotc::Modifier::BOLD),
            "paste chip glyph lost bold"
        );
    }

    let image_width = nuotc::str_len(&image_chip);
    let image_start = text_x + ("see ".len() + paste_width + " plus ".len()) as u16;
    let image_end = image_start + image_width as u16;
    for col in image_start..image_end {
        let cell = buf.get(col, text_y).expect("image chip cell");
        assert_eq!(
            cell.style.fg,
            theme.chip_image_fg(),
            "image chip glyph lost its amber"
        );
        assert_eq!(
            cell.style.bg,
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
        assert_eq!(
            cell.style.fg,
            theme.fg(),
            "prose next to a chip must stay plain"
        );
        assert_eq!(
            cell.style.bg, interior_bg,
            "prose must not pick up a chip band"
        );
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
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let text_y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    let interior_bg = theme.input_surface();

    // Every glyph of both orphan labels keeps the plain text color on the
    // plain interior background — no pill band, no kind color, no bold.
    for (offset, label) in [
        ("typed ".len(), &orphan_image),
        ("typed [Image #1] and ".len(), &orphan_paste),
    ] {
        let start = text_x + offset as u16;
        let end = start + nuotc::str_len(label) as u16;
        for col in start..end {
            let cell = buf.get(col, text_y).expect("orphan chip cell");
            assert_eq!(
                cell.style.fg,
                theme.fg(),
                "orphan label {label:?} must keep plain text color at col {col}"
            );
            assert_eq!(
                cell.style.bg, interior_bg,
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
    let real_paste = nuo_view::composer_attachments::paste_chip(1, 3, 2048);
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
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let text_y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    let interior_bg = theme.input_surface();

    // The backed paste chip gets the blue pill.
    let paste_width = nuotc::str_len(&real_paste);
    let paste_end = text_x + paste_width as u16;
    for col in text_x..paste_end {
        let cell = buf.get(col, text_y).expect("backed paste cell");
        assert_eq!(
            cell.style.fg,
            theme.chip_paste_fg(),
            "backed paste chip lost its blue"
        );
        assert_eq!(
            cell.style.bg,
            theme.chip_paste_bg(interior_bg),
            "backed paste chip lost its band"
        );
    }

    // The orphan image label stays plain text.
    let orphan_start = text_x + ("".len() + paste_width + " then ".len()) as u16;
    let orphan_end = orphan_start + nuotc::str_len(&orphan_image) as u16;
    for col in orphan_start..orphan_end {
        let cell = buf.get(col, text_y).expect("orphan image cell");
        assert_eq!(
            cell.style.fg,
            theme.fg(),
            "orphan image label must stay plain text"
        );
        assert_eq!(
            cell.style.bg, interior_bg,
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
    let paste_chip = nuo_view::composer_attachments::paste_chip(1, 3, 2048);
    let input = format!("see {paste_chip} end");
    let mut terminal = nuotc::TestTerminal::new(80, 5);
    // Select exactly the chip bytes (absolute offsets into `input`).
    let sel_lo = "see ".len();
    let sel_hi = sel_lo + paste_chip.len();
    use nuo_view::model::layout::SemanticCursor;
    let selection = SelectionState::Range {
        anchor: SemanticCursor::new(crate::shell::composer::INPUT_MSG_IDX, 0, sel_lo),
        head: SemanticCursor::new(crate::shell::composer::INPUT_MSG_IDX, 0, sel_hi),
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
            crate::shell::composer_hints::ComposerHints::default(),
        );
    });
    let buf = terminal.buffer();
    let text_y = nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
    let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
    let chip_start = text_x + sel_lo as u16;
    let chip_end = chip_start + nuotc::str_len(&paste_chip) as u16;
    for col in chip_start..chip_end {
        let cell = buf.get(col, text_y).expect("selected chip cell");
        assert_eq!(
            cell.style.fg,
            theme.chip_paste_fg(),
            "selected chip must keep its identity color"
        );
        assert_eq!(
            cell.style.bg,
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
    let image_chip = nuo_view::composer_attachments::image_chip(1, 1536);
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
            crate::shell::composer_hints::ComposerHints::default(),
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
            if chip_glyphs.contains(&cell.symbol.as_str().chars().next().unwrap_or('\0')) {
                assert_eq!(
                    cell.style.bg,
                    theme.chip_image_bg(interior_bg),
                    "wrapped chip fragment at ({col},{row}) lost its band"
                );
                assert_eq!(
                    cell.style.fg,
                    theme.chip_image_fg(),
                    "wrapped chip fragment at ({col},{row}) lost its amber"
                );
            }
        }
    }
}
