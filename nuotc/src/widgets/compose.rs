//! Declarative layout and composition widgets: `Column`, `Row`, `Stack`, `Container`, `Padding`, `Spacer`, and `Divider`.
//!
//! Provides a modern, declarative API for composing rich terminal interfaces
//! without manual coordinate math or widget lifecycle tracking.

use crate::buffer::cell::{Cell, Style};
use crate::buffer::grid::Grid;
use crate::layout::rect::{Constraint, Direction, Layout, Margin, Rect};
use crate::terminal::frame::Widget;

/// A type-erased boxed widget for heterogeneous declarative composition.
pub trait BoxedWidget {
    fn render_boxed(self: Box<Self>, area: Rect, grid: &mut Grid);
}

impl<W: Widget> BoxedWidget for W {
    fn render_boxed(self: Box<Self>, area: Rect, grid: &mut Grid) {
        (*self).render(area, grid);
    }
}

impl Widget for Box<dyn BoxedWidget> {
    fn render(self, area: Rect, grid: &mut Grid) {
        self.render_boxed(area, grid);
    }
}

/// Type alias for a boxed heterogeneous widget.
pub type AnyWidget = Box<dyn BoxedWidget>;

/// Four-sided padding specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Padding {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl Padding {
    /// Zero padding on all sides.
    pub const ZERO: Self = Self {
        top: 0,
        right: 0,
        bottom: 0,
        left: 0,
    };

    /// Uniform padding on all four sides.
    pub const fn all(value: u16) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    /// Symmetric vertical and horizontal padding.
    pub const fn symmetric(vertical: u16, horizontal: u16) -> Self {
        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }

    /// Explicit four-sided padding.
    pub const fn new(top: u16, right: u16, bottom: u16, left: u16) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    /// Total horizontal padding (`left + right`).
    pub const fn horizontal(self) -> u16 {
        self.left.saturating_add(self.right)
    }

    /// Total vertical padding (`top + bottom`).
    pub const fn vertical(self) -> u16 {
        self.top.saturating_add(self.bottom)
    }

    /// Shrink a rectangle by this padding.
    pub fn shrink(self, rect: Rect) -> Rect {
        let x = rect.x.saturating_add(self.left);
        let y = rect.y.saturating_add(self.top);
        let width = rect.width.saturating_sub(self.horizontal());
        let height = rect.height.saturating_sub(self.vertical());
        Rect::new(x, y, width, height)
    }
}

/// A vertical flex container (`Column`) that arranges children sequentially from top to bottom.
pub struct Column {
    children: Vec<(Option<Constraint>, AnyWidget)>,
    spacing: u16,
}

impl Default for Column {
    fn default() -> Self {
        Self::new()
    }
}

impl Column {
    /// Create a new empty column container.
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            spacing: 0,
        }
    }

    /// Set spacing (in lines) between consecutive children.
    pub fn spacing(mut self, spacing: u16) -> Self {
        self.spacing = spacing;
        self
    }

    /// Append a child widget with automatic flexible sizing.
    pub fn child(mut self, widget: impl Widget + 'static) -> Self {
        self.children.push((None, Box::new(widget)));
        self
    }

    /// Append a child widget with an explicit size constraint.
    pub fn child_with_constraint(
        mut self,
        constraint: Constraint,
        widget: impl Widget + 'static,
    ) -> Self {
        self.children.push((Some(constraint), Box::new(widget)));
        self
    }
}

impl Widget for Column {
    fn render(self, area: Rect, grid: &mut Grid) {
        if self.children.is_empty() || area.is_empty() {
            return;
        }

        let child_count = self.children.len();

        // Build constraints for Layout
        let mut constraints = Vec::with_capacity(child_count * 2);
        for (idx, (c, _)) in self.children.iter().enumerate() {
            if idx > 0 && self.spacing > 0 {
                constraints.push(Constraint::Length(self.spacing));
            }
            match c {
                Some(constraint) => constraints.push(*constraint),
                None => constraints.push(Constraint::Min(0)),
            }
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let mut chunk_idx = 0;
        for (idx, (_, child)) in self.children.into_iter().enumerate() {
            if idx > 0 && self.spacing > 0 {
                chunk_idx += 1; // Skip spacing chunk
            }
            if chunk_idx < chunks.len() {
                child.render(chunks[chunk_idx], grid);
                chunk_idx += 1;
            }
        }
    }
}

/// A horizontal flex container (`Row`) that arranges children sequentially from left to right.
pub struct Row {
    children: Vec<(Option<Constraint>, AnyWidget)>,
    spacing: u16,
}

impl Default for Row {
    fn default() -> Self {
        Self::new()
    }
}

impl Row {
    /// Create a new empty row container.
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            spacing: 0,
        }
    }

    /// Set spacing (in columns) between consecutive children.
    pub fn spacing(mut self, spacing: u16) -> Self {
        self.spacing = spacing;
        self
    }

    /// Append a child widget with automatic flexible sizing.
    pub fn child(mut self, widget: impl Widget + 'static) -> Self {
        self.children.push((None, Box::new(widget)));
        self
    }

    /// Append a child widget with an explicit size constraint.
    pub fn child_with_constraint(
        mut self,
        constraint: Constraint,
        widget: impl Widget + 'static,
    ) -> Self {
        self.children.push((Some(constraint), Box::new(widget)));
        self
    }
}

impl Widget for Row {
    fn render(self, area: Rect, grid: &mut Grid) {
        if self.children.is_empty() || area.is_empty() {
            return;
        }

        let child_count = self.children.len();

        let mut constraints = Vec::with_capacity(child_count * 2);
        for (idx, (c, _)) in self.children.iter().enumerate() {
            if idx > 0 && self.spacing > 0 {
                constraints.push(Constraint::Length(self.spacing));
            }
            match c {
                Some(constraint) => constraints.push(*constraint),
                None => constraints.push(Constraint::Min(0)),
            }
        }

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);

        let mut chunk_idx = 0;
        for (idx, (_, child)) in self.children.into_iter().enumerate() {
            if idx > 0 && self.spacing > 0 {
                chunk_idx += 1;
            }
            if chunk_idx < chunks.len() {
                child.render(chunks[chunk_idx], grid);
                chunk_idx += 1;
            }
        }
    }
}

/// A layered container (`Stack`) that renders children on top of each other in declaration order.
pub struct Stack {
    children: Vec<AnyWidget>,
}

impl Default for Stack {
    fn default() -> Self {
        Self::new()
    }
}

impl Stack {
    /// Create a new empty stack container.
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// Append a child layer to the stack.
    pub fn child(mut self, widget: impl Widget + 'static) -> Self {
        self.children.push(Box::new(widget));
        self
    }
}

impl Widget for Stack {
    fn render(self, area: Rect, grid: &mut Grid) {
        for child in self.children {
            child.render(area, grid);
        }
    }
}

/// A decorative container providing padding, optional margins, and background styling.
pub struct Container {
    child: AnyWidget,
    padding: Padding,
    margin: Margin,
    background: Option<Style>,
}

impl Container {
    /// Wrap a child widget inside a container.
    pub fn new(child: impl Widget + 'static) -> Self {
        Self {
            child: Box::new(child),
            padding: Padding::ZERO,
            margin: Margin::default(),
            background: None,
        }
    }

    /// Set internal padding.
    pub fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }

    /// Set external margin.
    pub fn margin(mut self, margin: Margin) -> Self {
        self.margin = margin;
        self
    }

    /// Fill the container background with a styled surface.
    pub fn background(mut self, style: Style) -> Self {
        self.background = Some(style);
        self
    }
}

impl Widget for Container {
    fn render(self, area: Rect, grid: &mut Grid) {
        let area = area.inner(self.margin);
        if area.is_empty() {
            return;
        }

        if let Some(style) = self.background {
            grid.fill_rect(area.x, area.y, area.width, area.height, style);
        }

        let inner_area = self.padding.shrink(area);
        if !inner_area.is_empty() {
            self.child.render(inner_area, grid);
        }
    }
}

/// An empty widget that claims space or enforces a gap.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spacer;

impl Spacer {
    pub fn new() -> Self {
        Self
    }
}

impl Widget for Spacer {
    fn render(self, _area: Rect, _grid: &mut Grid) {}
}

/// A visual separator line (horizontal or vertical).
#[derive(Debug, Clone, Copy)]
pub struct Divider {
    direction: Direction,
    style: Style,
    symbol: &'static str,
}

impl Default for Divider {
    fn default() -> Self {
        Self::horizontal()
    }
}

impl Divider {
    /// Create a horizontal divider line.
    pub const fn horizontal() -> Self {
        Self {
            direction: Direction::Horizontal,
            style: Style::RESET,
            symbol: "─",
        }
    }

    /// Create a vertical divider line.
    pub const fn vertical() -> Self {
        Self {
            direction: Direction::Vertical,
            style: Style::RESET,
            symbol: "│",
        }
    }

    /// Set divider style.
    pub const fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set divider line character symbol.
    pub const fn symbol(mut self, symbol: &'static str) -> Self {
        self.symbol = symbol;
        self
    }
}

impl Widget for Divider {
    fn render(self, area: Rect, grid: &mut Grid) {
        if area.is_empty() {
            return;
        }
        match self.direction {
            Direction::Horizontal => {
                let cy = area.y;
                for cx in area.x..area.right() {
                    let mut head = Cell::blank_styled(self.style);
                    head.symbol = self.symbol.into();
                    grid.set(cx, cy, head);
                }
                grid.mark(area.x, cy);
                grid.mark(area.right().saturating_sub(1), cy);
            }
            Direction::Vertical => {
                let cx = area.x;
                for cy in area.y..area.bottom() {
                    let mut head = Cell::blank_styled(self.style);
                    head.symbol = self.symbol.into();
                    grid.set(cx, cy, head);
                    grid.mark(cx, cy);
                }
            }
        }
    }
}

/// Extension trait enabling fluent declarative chaining on all widgets.
pub trait WidgetExt: Widget + Sized + 'static {
    /// Box this widget into an [`AnyWidget`] for heterogeneous composition.
    fn boxed(self) -> AnyWidget {
        Box::new(self)
    }

    /// Wrap this widget with padding inside a [`Container`].
    fn with_padding(self, padding: Padding) -> Container {
        Container::new(self).padding(padding)
    }

    /// Wrap this widget with a background fill inside a [`Container`].
    fn with_background(self, style: Style) -> Container {
        Container::new(self).background(style)
    }
}

impl<T: Widget + Sized + 'static> WidgetExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::cell::Color;
    use crate::widgets::{Block, Borders, Paragraph};

    #[test]
    fn test_column_layout_composition() {
        let mut grid = Grid::new(20, 10);
        let col = Column::new()
            .child_with_constraint(Constraint::Length(1), Paragraph::new("Header"))
            .child_with_constraint(Constraint::Min(0), Paragraph::new("Body"));

        col.render(grid.area(), &mut grid);

        assert_eq!(grid.get(0, 0).map(|c| c.symbol.as_str()), Some("H"));
        assert_eq!(grid.get(0, 1).map(|c| c.symbol.as_str()), Some("B"));
    }

    #[test]
    fn test_row_layout_composition() {
        let mut grid = Grid::new(20, 5);
        let row = Row::new()
            .child_with_constraint(Constraint::Length(10), Paragraph::new("Left"))
            .child_with_constraint(Constraint::Length(10), Paragraph::new("Right"));

        row.render(grid.area(), &mut grid);

        assert_eq!(grid.get(0, 0).map(|c| c.symbol.as_str()), Some("L"));
        assert_eq!(grid.get(10, 0).map(|c| c.symbol.as_str()), Some("R"));
    }

    #[test]
    fn test_container_padding_and_background() {
        let mut grid = Grid::new(10, 5);
        let container = Paragraph::new("Padded")
            .with_background(Style::default().bg(Color::Blue))
            .padding(Padding::all(1));

        container.render(grid.area(), &mut grid);

        // (0,0) is padding filled with background
        assert_eq!(grid.get(0, 0).map(|c| c.bg), Some(Color::Blue));
        // (1,1) has the padded text
        assert_eq!(grid.get(1, 1).map(|c| c.symbol.as_str()), Some("P"));
    }

    #[test]
    fn test_stack_overlapping_layers() {
        let mut grid = Grid::new(10, 5);
        let stack = Stack::new()
            .child(Block::default().borders(Borders::ALL))
            .child(Paragraph::new("Over"));

        stack.render(grid.area(), &mut grid);

        // Top-left was rendered as 'Over' on top of border '┌'
        assert_eq!(grid.get(0, 0).map(|c| c.symbol.as_str()), Some("O"));
    }

    #[test]
    fn test_divider() {
        let mut grid = Grid::new(5, 3);
        let div = Divider::horizontal();
        div.render(Rect::new(0, 1, 5, 1), &mut grid);

        assert_eq!(grid.get(0, 1).map(|c| c.symbol.as_str()), Some("─"));
        assert_eq!(grid.get(4, 1).map(|c| c.symbol.as_str()), Some("─"));
    }
}
