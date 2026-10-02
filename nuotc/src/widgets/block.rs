//! Block widget: bordered and styled rectangular panel.

use crate::buffer::cell::Cell;
use crate::buffer::glyph::GlyphSet;
use crate::buffer::grid::Grid;
use crate::buffer::Color;
use crate::buffer::Style;
use crate::layout::rect::Rect;
use super::paragraph::Line;

/// Border sides bitflags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Borders(pub u16);

impl Borders {
    pub const NONE: Borders = Borders(0);
    pub const LEFT: Borders = Borders(1);
    pub const RIGHT: Borders = Borders(2);
    pub const TOP: Borders = Borders(4);
    pub const BOTTOM: Borders = Borders(8);
    pub const ALL: Borders = Borders(1 | 2 | 4 | 8);

    pub const fn union(self, other: Borders) -> Borders {
        Borders(self.0 | other.0)
    }
}

impl std::ops::BitOr for Borders {
    type Output = Borders;
    fn bitor(self, rhs: Borders) -> Borders {
        Borders(self.0 | rhs.0)
    }
}

/// Border rendering style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderType {
    #[default]
    Plain,
    Thick,
    Ascii,
}

/// A block (bordered panel). Supports a background fill, custom borders,
/// and glyph sets.
#[derive(Debug, Clone, Default)]
pub struct Block<'a> {
    pub style: Style,
    pub borders: Borders,
    pub border_type: BorderType,
    pub border_style: Style,
    pub title: Option<Line<'a>>,
    pub glyph_v: Option<&'static str>,
    pub glyph_set: Option<&'static GlyphSet>,
}

impl<'a> Block<'a> {
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
    pub fn borders(mut self, b: Borders) -> Self {
        self.borders = b;
        self
    }
    pub fn border_type(mut self, t: BorderType) -> Self {
        self.border_type = t;
        self
    }
    pub fn border_style(mut self, s: Style) -> Self {
        self.border_style = s;
        self
    }
    pub fn glyph_v(mut self, sym: &'static str) -> Self {
        self.glyph_v = Some(sym);
        self
    }
    pub fn glyph_set(mut self, set: &'static GlyphSet) -> Self {
        self.glyph_set = Some(set);
        self
    }

    /// Calculate inner rect after removing configured borders.
    pub fn inner(&self, area: Rect) -> Rect {
        if area.width == 0 || area.height == 0 {
            return area;
        }
        let left = if self.borders.0 & Borders::LEFT.0 != 0 {
            1
        } else {
            0
        };
        let right = if self.borders.0 & Borders::RIGHT.0 != 0 {
            1
        } else {
            0
        };
        let top = if self.borders.0 & Borders::TOP.0 != 0 {
            1
        } else {
            0
        };
        let bottom = if self.borders.0 & Borders::BOTTOM.0 != 0 {
            1
        } else {
            0
        };

        Rect {
            x: area.x.saturating_add(left),
            y: area.y.saturating_add(top),
            width: area.width.saturating_sub(left + right),
            height: area.height.saturating_sub(top + bottom),
        }
    }

    /// Render the block's background and borders into `grid`.
    pub fn render(&self, area: Rect, grid: &mut Grid) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // Background fill.
        grid.fill_rect(area.x, area.y, area.width, area.height, self.style);

        let bar_style = Style {
            fg: if self.border_style.fg != Color::Reset {
                self.border_style.fg
            } else {
                self.style.fg
            },
            bg: self.style.bg,
            add: self.style.add | self.border_style.add,
        };

        let (v, h, tl, tr, bl, br) = if let Some(gs) = self.glyph_set {
            (
                gs.border_v,
                gs.border_h,
                gs.corner_tl,
                gs.corner_tr,
                gs.corner_bl,
                gs.corner_br,
            )
        } else {
            match self.border_type {
                BorderType::Ascii => ("|", "-", "+", "+", "+", "+"),
                BorderType::Thick => ("┃", "━", "┏", "┓", "┗", "┛"),
                BorderType::Plain => ("│", "─", "┌", "┐", "└", "┘"),
            }
        };
        let sym_v = self.glyph_v.unwrap_or(v);

        // Top border
        if self.borders.0 & Borders::TOP.0 != 0 {
            for x in area.x..area.x + area.width {
                grid.set(x, area.y, Cell::narrow(h, bar_style));
            }
        }

        // Bottom border
        if self.borders.0 & Borders::BOTTOM.0 != 0 && area.height > 1 {
            let by = area.y + area.height - 1;
            for x in area.x..area.x + area.width {
                grid.set(x, by, Cell::narrow(h, bar_style));
            }
        }

        // Left border
        if self.borders.0 & Borders::LEFT.0 != 0 {
            for y in area.y..area.y + area.height {
                grid.set(area.x, y, Cell::narrow(sym_v, bar_style));
            }
        }

        // Right border
        if self.borders.0 & Borders::RIGHT.0 != 0 && area.width > 1 {
            let rx = area.x + area.width - 1;
            for y in area.y..area.y + area.height {
                grid.set(rx, y, Cell::narrow(sym_v, bar_style));
            }
        }

        // Corners
        if self.borders.0 & (Borders::TOP.0 | Borders::LEFT.0) == (Borders::TOP.0 | Borders::LEFT.0)
        {
            grid.set(area.x, area.y, Cell::narrow(tl, bar_style));
        }
        if self.borders.0 & (Borders::TOP.0 | Borders::RIGHT.0)
            == (Borders::TOP.0 | Borders::RIGHT.0)
            && area.width > 1
        {
            grid.set(area.x + area.width - 1, area.y, Cell::narrow(tr, bar_style));
        }
        if self.borders.0 & (Borders::BOTTOM.0 | Borders::LEFT.0)
            == (Borders::BOTTOM.0 | Borders::LEFT.0)
            && area.height > 1
        {
            grid.set(
                area.x,
                area.y + area.height - 1,
                Cell::narrow(bl, bar_style),
            );
        }
        if self.borders.0 & (Borders::BOTTOM.0 | Borders::RIGHT.0)
            == (Borders::BOTTOM.0 | Borders::RIGHT.0)
            && area.width > 1
            && area.height > 1
        {
            grid.set(
                area.x + area.width - 1,
                area.y + area.height - 1,
                Cell::narrow(br, bar_style),
            );
        }
    }
}
