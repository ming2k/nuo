//! Shared terminal and geometry fixtures for the composer tests.

use super::super::*;

// Tinted-panel composer geometry (the four-row redesign)

pub(super) fn frame_row_text(terminal: &mut nuotc::TestTerminal, y: u16) -> String {
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    (0..width)
        .map(|x| buffer.get(x, y).unwrap().symbol.as_str())
        .collect()
}

pub(super) fn draw_frame_composer(
    input: &str,
    focused: bool,
    hints: crate::shell::composer_hints::ComposerHints,
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
