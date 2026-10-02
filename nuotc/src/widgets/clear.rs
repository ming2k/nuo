//! Clear widget: resets an area to blank cells.

use crate::buffer::grid::Grid;
use crate::buffer::Style;
use crate::layout::rect::Rect;

/// A clear operation: reset the target area to blank cells.
#[derive(Debug, Clone, Copy, Default)]
pub struct Clear;

impl Clear {
    pub fn render(self, area: Rect, grid: &mut Grid) {
        grid.fill_rect(area.x, area.y, area.width, area.height, Style::RESET);
    }
}
