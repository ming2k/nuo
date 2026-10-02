//! Geometry primitives: `Rect`, `Margin`, `Constraint`, `Direction`, and
//! `Layout`.
//!
//! These mirror ratatui's layout API surface exactly (same field names, same
//! `Layout::split` semantics) so the migrated widget code needs no geometry
//! changes — only an import path swap from `ratatui::layout` to `nuotc`.

/// A rectangular region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns a rect that has been shrunk by `margin.horizontal` on each
    /// side and `margin.vertical` on top and bottom.
    pub fn inner(self, margin: Margin) -> Rect {
        if self.width < 2 * margin.horizontal || self.height < 2 * margin.vertical {
            return Rect::new(self.x, self.y, 0, 0);
        }
        Rect::new(
            self.x + margin.horizontal,
            self.y + margin.vertical,
            self.width - 2 * margin.horizontal,
            self.height - 2 * margin.vertical,
        )
    }

    /// Clamp a point to be inside this rect. Used by the app to normalize
    /// mouse coordinates.
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Intersection in terminal coordinates, including empty rectangles.
    pub fn intersection(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Self::new(
            x,
            y,
            self.right().min(other.right()).saturating_sub(x),
            self.bottom().min(other.bottom()).saturating_sub(y),
        )
    }

    /// Area (width × height) in cells.
    pub const fn area(self) -> u32 {
        self.width as u32 * self.height as u32
    }

    /// Whether this rectangle covers zero area.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Right edge x coordinate (exclusive).
    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Bottom edge y coordinate (exclusive).
    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// Split this rect horizontally at `offset` from the left, returning
    /// `(left, right)`.
    pub fn split_horizontal(self, offset: u16) -> (Rect, Rect) {
        let left_w = offset.min(self.width);
        let right_x = self.x + left_w;
        let right_w = self.width - left_w;
        (
            Rect::new(self.x, self.y, left_w, self.height),
            Rect::new(right_x, self.y, right_w, self.height),
        )
    }
}

/// A margin to apply when computing an inner rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Margin {
    pub horizontal: u16,
    pub vertical: u16,
}

impl Margin {
    pub const fn new(horizontal: u16, vertical: u16) -> Self {
        Self {
            horizontal,
            vertical,
        }
    }
}

/// A layout constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constraint {
    /// Fill all remaining space.
    Min(u16),
    /// A fixed length.
    Length(u16),
    /// A percentage of the available space (0–100).
    Percentage(u16),
}

/// Layout direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Vertical,
    Horizontal,
}

/// A layout solver: given a `Rect` and a list of `Constraint`s, `split`
/// returns the sub-rects. Mirrors ratatui's `Layout::default().direction()
/// .constraints().split()` API.
///
/// The solver implements the constraint-resolution algorithm ratatui uses:
/// `Length` is fixed; `Percentage` is the given fraction of total; `Min` fills
/// whatever is left. Multiple `Min` constraints split the remainder equally.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub direction: Direction,
    pub constraints: Vec<Constraint>,
}

impl Layout {
    /// Create a default layout (vertical, no constraints).
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Self {
        Self {
            direction: Direction::Vertical,
            constraints: Vec::new(),
        }
    }

    pub fn direction(mut self, dir: Direction) -> Self {
        self.direction = dir;
        self
    }

    pub fn constraints(mut self, cs: impl IntoIterator<Item = Constraint>) -> Self {
        self.constraints = cs.into_iter().collect();
        self
    }

    /// Split `area` into sub-rects according to the constraints.
    ///
    /// Semantically equivalent to mapping the constraints onto flex children
    /// and solving with [`crate::flex::Flex`]: `Length(l)`/`Percentage(p)`
    /// → a fixed basis; `Min(m)` → basis m, grow 1, shrink 1 (surplus split
    /// evenly, remainder to the earlier `Min` — matching the legacy
    /// implementation). One legacy behavior is preserved: a zero width or
    /// height yields an empty list.
    pub fn split(self, area: Rect) -> RcRects {
        let n = self.constraints.len();
        if n == 0 || area.width == 0 || area.height == 0 {
            return RcRects { rects: Vec::new() };
        }
        let total = match self.direction {
            Direction::Vertical => area.height,
            Direction::Horizontal => area.width,
        };
        let items: Vec<crate::flex::FlexItem> = self
            .constraints
            .iter()
            .map(|c| match c {
                Constraint::Length(l) => crate::flex::FlexItem::fixed(*l),
                Constraint::Percentage(p) => {
                    crate::flex::FlexItem::fixed(((*p as u32 * total as u32 + 50) / 100) as u16)
                }
                // Min fills the remainder: basis m + an even split of the
                // surplus (grow = 1), shrinkable.
                Constraint::Min(m) => crate::flex::FlexItem::fixed(*m).with_grow(1).with_shrink(1),
            })
            .collect();
        let flex = crate::flex::Flex {
            direction: match self.direction {
                Direction::Vertical => crate::flex::FlexDirection::Column,
                Direction::Horizontal => crate::flex::FlexDirection::Row,
            },
            ..crate::flex::Flex::default()
        };
        let solved = flex.solve_with(area, &items, &|_, _| 0);
        RcRects {
            rects: solved.iter().copied().collect(),
        }
    }
}

/// The result of `Layout::split`. Indexable like `Rc<[Rect]>` in ratatui:
/// the rect list is computed once in `split` and never mutated afterwards, so
/// a plain `Vec` behind an [`Index`][std::ops::Index] impl gives call sites the
/// same `chunks[i]` pattern with no interior mutability.
pub struct RcRects {
    pub(crate) rects: Vec<Rect>,
}

impl std::ops::Index<usize> for RcRects {
    type Output = Rect;
    fn index(&self, i: usize) -> &Rect {
        &self.rects[i]
    }
}

impl RcRects {
    pub fn iter(&self) -> std::vec::IntoIter<Rect> {
        self.rects.clone().into_iter()
    }
    pub fn len(&self) -> usize {
        self.rects.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }
}
