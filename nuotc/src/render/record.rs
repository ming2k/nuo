//! Recording backend: DisplayList, DrawOp, Canvas, RenderNode, and Rasterizer.
//!
//! Separates high-level widget drawing declarations from the rasterization
//! target. Widgets record intent into an intermediate [`DisplayList`] via
//! [`Canvas`], which can then be cached across frames via [`RenderNode`],
//! transformed, clipped without memory cloning, and played back by the
//! [`Rasterizer`] onto a double-buffered [`Grid`].

use crate::buffer::cell::{Cell, Style};
use crate::buffer::grid::{Fit, Grid};
use crate::buffer::text::{grapheme_width, graphemes};
use crate::layout::Rect;

/// A primitive drawing command recorded by the canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrawOp {
    /// Write a styled text run starting at `(x, y)`.
    PutText {
        x: u16,
        y: u16,
        fit: Fit,
        style: Style,
        text: String,
    },
    /// Direct cell placement at `(x, y)`.
    SetCell {
        x: u16,
        y: u16,
        cell: Cell,
    },
    /// Fill a rectangular region with a given style (space glyphs with background).
    FillRect {
        rect: Rect,
        style: Style,
    },
    /// Clear a rectangular region back to default terminal cells.
    ClearRect {
        rect: Rect,
    },
    /// Push a clipping boundary onto the clip stack.
    PushClip(Rect),
    /// Pop the top clipping boundary from the clip stack.
    PopClip,
    /// A nested, pre-recorded sub-display-list (e.g. from a cached [`RenderNode`]).
    SubList(DisplayList),
}

/// An ordered sequence of recorded drawing operations representing a frame or a sub-tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayList {
    ops: Vec<DrawOp>,
}

impl DisplayList {
    /// Create a new empty display list.
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Number of recorded operations.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Returns `true` if the display list contains no operations.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Push an operation to the list.
    pub fn push(&mut self, op: DrawOp) {
        self.ops.push(op);
    }

    /// Extend this list with operations from another list.
    pub fn extend(&mut self, other: DisplayList) {
        self.ops.extend(other.ops);
    }

    /// Clear all operations in this display list.
    pub fn clear(&mut self) {
        self.ops.clear();
    }

    /// Read-only slice of recorded operations.
    pub fn ops(&self) -> &[DrawOp] {
        &self.ops
    }
}

/// Canvas recorder for declarative and immediate-mode widgets.
///
/// Manages clipping boundaries and accumulates commands into an underlying [`DisplayList`].
pub struct Canvas {
    list: DisplayList,
    clip_stack: Vec<Rect>,
    viewport: Rect,
}

impl Canvas {
    /// Construct a new Canvas with the specified root viewport.
    pub fn new(viewport: Rect) -> Self {
        Self {
            list: DisplayList::new(),
            clip_stack: vec![viewport],
            viewport,
        }
    }

    /// Current effective clipping rectangle.
    pub fn current_clip(&self) -> Rect {
        self.clip_stack.last().copied().unwrap_or(self.viewport)
    }

    /// Push a new clip rectangle, intersecting it with the current active clip.
    pub fn push_clip(&mut self, clip: Rect) {
        let effective = self.current_clip().intersection(clip);
        self.clip_stack.push(effective);
        self.list.push(DrawOp::PushClip(clip));
    }

    /// Pop the most recent clip rectangle.
    pub fn pop_clip(&mut self) {
        if self.clip_stack.len() > 1 {
            self.clip_stack.pop();
            self.list.push(DrawOp::PopClip);
        }
    }

    /// Record a styled string starting at `(x, y)` with default clipping fit.
    pub fn put(&mut self, x: u16, y: u16, style: Style, text: &str) {
        self.put_fitted(x, y, Fit::Clip, style, text);
    }

    /// Record a styled string starting at `(x, y)` with a specific fit strategy.
    pub fn put_fitted(&mut self, x: u16, y: u16, fit: Fit, style: Style, text: &str) {
        self.list.push(DrawOp::PutText {
            x,
            y,
            fit,
            style,
            text: text.to_string(),
        });
    }

    /// Record setting a single cell at `(x, y)`.
    pub fn set_cell(&mut self, x: u16, y: u16, cell: Cell) {
        self.list.push(DrawOp::SetCell { x, y, cell });
    }

    /// Record filling a rectangular region with a given style.
    pub fn fill_rect(&mut self, rect: Rect, style: Style) {
        self.list.push(DrawOp::FillRect { rect, style });
    }

    /// Record clearing a rectangular region.
    pub fn clear_rect(&mut self, rect: Rect) {
        self.list.push(DrawOp::ClearRect { rect });
    }

    /// Append a pre-recorded display list directly into this canvas.
    pub fn append_list(&mut self, list: DisplayList) {
        if !list.is_empty() {
            self.list.push(DrawOp::SubList(list));
        }
    }

    /// Draw a retained [`RenderNode`]. If cached, its recorded commands are appended directly.
    pub fn draw_node(&mut self, node: &RenderNode) {
        if let Some(cached) = node.display_list() {
            self.append_list(cached.clone());
        }
    }

    /// Consume the canvas and extract the recorded [`DisplayList`].
    pub fn finish(self) -> DisplayList {
        self.list
    }
}

/// A retained render node that caches its recorded [`DisplayList`] across frames.
///
/// Analogous to Android's `RenderNode`: if input revision and allocated area
/// have not changed, re-recording is skipped and the cached display list is reused.
#[derive(Debug, Clone, Default)]
pub struct RenderNode {
    key: String,
    revision: u64,
    area: Rect,
    cached: Option<DisplayList>,
}

impl RenderNode {
    /// Create a new render node with an identifying key.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            revision: 0,
            area: Rect::default(),
            cached: None,
        }
    }

    /// Node key identifier.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Current cached revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Check if this node is dirty given the incoming revision and area.
    pub fn is_dirty(&self, new_revision: u64, new_area: Rect) -> bool {
        self.cached.is_none() || self.revision != new_revision || self.area != new_area
    }

    /// Read-only access to cached display list.
    pub fn display_list(&self) -> Option<&DisplayList> {
        self.cached.as_ref()
    }

    /// Record into this node only if dirty; otherwise retain the existing display list.
    pub fn record_if_dirty<F>(&mut self, revision: u64, area: Rect, record_fn: F)
    where
        F: FnOnce(&mut Canvas),
    {
        if self.is_dirty(revision, area) {
            let mut canvas = Canvas::new(area);
            record_fn(&mut canvas);
            self.cached = Some(canvas.finish());
            self.revision = revision;
            self.area = area;
        }
    }

    /// Invalidate the cached display list, forcing a re-record on next use.
    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    /// Emit this node's cached display list to a canvas.
    pub fn emit(&self, canvas: &mut Canvas) {
        canvas.draw_node(self);
    }
}

/// Deterministic, zero-allocation rasterizer that replays a [`DisplayList`] onto a [`Grid`].
///
/// Enforces clipping dynamically without requiring full-grid clones.
pub struct Rasterizer;

impl Rasterizer {
    /// Rasterize a [`DisplayList`] onto a target [`Grid`] within the grid's boundaries.
    pub fn rasterize(display_list: &DisplayList, target: &mut Grid) {
        let (w, h) = target.size();
        let root_viewport = Rect::new(0, 0, w, h);
        let mut clip_stack = vec![root_viewport];
        Self::replay_ops(display_list.ops(), target, &mut clip_stack);
    }

    /// Rasterize a [`DisplayList`] with an initial clip boundary.
    pub fn rasterize_with_clip(display_list: &DisplayList, target: &mut Grid, initial_clip: Rect) {
        let (w, h) = target.size();
        let grid_rect = Rect::new(0, 0, w, h);
        let mut clip_stack = vec![grid_rect.intersection(initial_clip)];
        Self::replay_ops(display_list.ops(), target, &mut clip_stack);
    }

    fn replay_ops(ops: &[DrawOp], target: &mut Grid, clip_stack: &mut Vec<Rect>) {
        for op in ops {
            let current_clip = clip_stack.last().copied().unwrap_or_default();
            if current_clip.is_empty() && !matches!(op, DrawOp::PopClip) {
                continue;
            }

            match op {
                DrawOp::PushClip(rect) => {
                    let next_clip = current_clip.intersection(*rect);
                    clip_stack.push(next_clip);
                }
                DrawOp::PopClip => {
                    if clip_stack.len() > 1 {
                        clip_stack.pop();
                    }
                }
                DrawOp::SubList(sub) => {
                    Self::replay_ops(sub.ops(), target, clip_stack);
                }
                DrawOp::ClearRect { rect } => {
                    let target_rect = rect.intersection(current_clip);
                    if !target_rect.is_empty() {
                        let default_style = Style::default();
                        for y in target_rect.y..target_rect.bottom() {
                            for x in target_rect.x..target_rect.right() {
                                target.set(x, y, Cell::default());
                            }
                            target.mark(target_rect.x, y);
                            target.mark(target_rect.right().saturating_sub(1), y);
                        }
                        let _ = default_style;
                    }
                }
                DrawOp::FillRect { rect, style } => {
                    let target_rect = rect.intersection(current_clip);
                    if !target_rect.is_empty() {
                        target.fill_rect(
                            target_rect.x,
                            target_rect.y,
                            target_rect.width,
                            target_rect.height,
                            *style,
                        );
                    }
                }
                DrawOp::SetCell { x, y, cell } => {
                    if current_clip.contains(*x, *y) {
                        target.set(*x, *y, cell.clone());
                    }
                }
                DrawOp::PutText {
                    x,
                    y,
                    fit,
                    style,
                    text,
                } => {
                    Self::rasterize_text(*x, *y, *fit, *style, text, target, current_clip);
                }
            }
        }
    }

    fn rasterize_text(
        x: u16,
        y: u16,
        fit: Fit,
        style: Style,
        text: &str,
        target: &mut Grid,
        clip: Rect,
    ) {
        let mut cx = x;
        let mut cy = y;

        for piece in graphemes(text) {
            let grapheme = piece.text;
            if grapheme == "\n" || grapheme == "\r\n" {
                cy = match cy.checked_add(1) {
                    Some(next) if next < clip.bottom() => next,
                    _ => break,
                };
                cx = x;
                continue;
            }
            if grapheme.chars().any(char::is_control) {
                continue;
            }
            let w = grapheme_width(grapheme);

            // Line overflow handling relative to clip bounds
            if cx + w as u16 > clip.right() {
                match fit {
                    Fit::Wrap => {
                        cy = match cy.checked_add(1) {
                            Some(next) if next < clip.bottom() => next,
                            _ => break,
                        };
                        cx = x;
                    }
                    Fit::Clip => {
                        if w as u16 > clip.width {
                            continue;
                        }
                        continue;
                    }
                }
            }

            // Visible cell placement inside active clip
            if cy >= clip.y && cy < clip.bottom() && cx >= clip.x && cx + w as u16 <= clip.right() {
                let mut cell_style = style;
                if cell_style.bg == crate::buffer::cell::Color::Reset
                    && let Some(existing) = target.get(cx, cy)
                {
                    cell_style.bg = existing.bg;
                }
                let head = Cell {
                    symbol: grapheme.into(),
                    width: w,
                    fg: cell_style.fg,
                    bg: cell_style.bg,
                    style: cell_style,
                };
                target.set(cx, cy, head);

                if w == 2 {
                    target.set(
                        cx + 1,
                        cy,
                        Cell::wide_continuation(cell_style),
                    );
                }
            }

            cx = match cx.checked_add(w as u16) {
                Some(next) => next,
                None => break,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::cell::Color;

    #[test]
    fn test_canvas_recording_and_rasterization() {
        let mut canvas = Canvas::new(Rect::new(0, 0, 20, 5));
        canvas.put(0, 0, Style::default().fg(Color::Green), "Hello");
        canvas.fill_rect(Rect::new(0, 1, 5, 1), Style::default().bg(Color::Blue));
        let list = canvas.finish();

        assert_eq!(list.len(), 2);
        let mut grid = Grid::new(20, 5);
        Rasterizer::rasterize(&list, &mut grid);

        assert_eq!(grid.get(0, 0).map(|c| c.symbol.as_str()), Some("H"));
        assert_eq!(grid.get(4, 0).map(|c| c.symbol.as_str()), Some("o"));
        assert_eq!(grid.get(0, 1).map(|c| c.bg), Some(Color::Blue));
    }

    #[test]
    fn test_render_node_caching() {
        let mut node = RenderNode::new("test_header");
        let area = Rect::new(0, 0, 10, 2);

        // First pass: dirty, should record
        let mut record_count = 0;
        node.record_if_dirty(1, area, |c| {
            record_count += 1;
            c.put(0, 0, Style::default(), "Cached");
        });
        assert_eq!(record_count, 1);
        assert!(!node.is_dirty(1, area));

        // Second pass with same revision and area: should skip recording
        node.record_if_dirty(1, area, |_c| {
            record_count += 1;
        });
        assert_eq!(record_count, 1);

        // Third pass with updated revision: should re-record
        node.record_if_dirty(2, area, |c| {
            record_count += 1;
            c.put(0, 0, Style::default(), "Updated");
        });
        assert_eq!(record_count, 2);
    }

    #[test]
    fn test_clipping_scissoring() {
        let mut canvas = Canvas::new(Rect::new(0, 0, 20, 5));
        canvas.push_clip(Rect::new(2, 1, 5, 2));
        canvas.put(0, 0, Style::default(), "Outside clip");
        canvas.put(2, 1, Style::default(), "Inside");
        canvas.pop_clip();
        let list = canvas.finish();

        let mut grid = Grid::new(20, 5);
        Rasterizer::rasterize(&list, &mut grid);

        // (0,0) was outside the clip stack when drawn
        assert_eq!(grid.get(0, 0).map(|c| c.symbol.as_str()), Some(" "));
        // (2,1) was inside the clip
        assert_eq!(grid.get(2, 1).map(|c| c.symbol.as_str()), Some("I"));
    }
}
