use super::common::{draw_picker_search_row, place_picker_search_cursor};
use super::*;
use crate::shell::render::Theme;
use nuo_ui_primitives::components::columns::{ColumnSpec, Grid, MAX_JUSTIFIED_GAP, column};
use nuo_ui_primitives::components::row::{GROUP_GAP, GUTTER};
use nuotc::Rect;
use unicode_width::UnicodeWidthStr;

/// Render the whole frame buffer back to a single string (rows joined by
/// `\n`), the standard readback helper for layout-level modal assertions.
pub(super) fn buffer_text(terminal: &nuotc::TestTerminal) -> String {
    let buf = terminal.buffer();
    let area = buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.get(x, y).unwrap().symbol.as_str().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

mod connections;
mod editor;
mod models;
mod oauth;
