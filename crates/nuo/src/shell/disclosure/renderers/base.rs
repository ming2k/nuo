//! Base cursor and rendering primitives for disclosure step renderers.
use nuotc::{Frame, Line, Paragraph, Rect};

use crate::shell::render::Theme;
use nuo_view::model::layout::{BlockRegion, LayoutMap};
use nuo_view::text_layout::WrappedLine;

pub(crate) const MARKER_COLLAPSED: &str = "+";
pub(crate) const MARKER_EXPANDED: &str = "-";

/// Cursor + environment carried through the tool-step body renderers.
pub struct RenderCtx<'a, 'f: 'a> {
    pub frame: &'a mut Frame<'f>,
    pub area: Rect,
    pub full_width: usize,
    pub theme: &'a Theme,
    pub layout_map: &'a mut LayoutMap,
    pub skip_rows: &'a mut usize,
    pub y: &'a mut u16,
    pub content_lines: &'a mut usize,
    /// Content-addressed wrap cache — see `crate::shell::render::BlockWrapCache`.
    pub wrap: &'a mut crate::shell::render::BlockWrapCache,
    /// The ambient session workspace root.
    pub workspace_root: Option<&'a std::path::Path>,
}

impl<'a, 'f: 'a> RenderCtx<'a, 'f> {
    #[allow(clippy::too_many_arguments)]
    pub fn from_cursor(
        frame: &'a mut Frame<'f>,
        area: Rect,
        full_width: usize,
        theme: &'a Theme,
        layout_map: &'a mut LayoutMap,
        skip_rows: &'a mut usize,
        y: &'a mut u16,
        content_lines: &'a mut usize,
        wrap: &'a mut crate::shell::render::BlockWrapCache,
    ) -> Self {
        Self {
            frame,
            area,
            full_width,
            theme,
            layout_map,
            skip_rows,
            y,
            content_lines,
            wrap,
            workspace_root: None,
        }
    }

    /// Attach an optional session workspace root.
    pub fn with_workspace_root(mut self, root: Option<&'a std::path::Path>) -> Self {
        self.workspace_root = root;
        self
    }

    pub fn advance_blank_rows(&mut self, rows: usize) {
        for _ in 0..rows {
            *self.content_lines += 1;
            if *self.skip_rows > 0 {
                *self.skip_rows = self.skip_rows.saturating_sub(1);
            } else if *self.y < self.area.y + self.area.height {
                *self.y += 1;
            }
        }
    }

    pub fn paint(&mut self, line: Line<'static>) -> Option<Rect> {
        *self.content_lines += 1;
        if *self.skip_rows > 0 {
            *self.skip_rows = self.skip_rows.saturating_sub(1);
            return None;
        }
        if *self.y >= self.area.y + self.area.height {
            return None;
        }
        let rect = Rect::new(self.area.x, *self.y, self.area.width, 1);
        self.frame.render_widget(&mut Paragraph::new(line), rect);
        *self.y += 1;
        Some(rect)
    }

    pub fn paint_text_row(
        &mut self,
        line: Line<'static>,
        mi: usize,
        block_idx: usize,
        wl: &WrappedLine,
        prefix_cols: u16,
        hidden_ranges: &[(usize, usize)],
    ) {
        if let Some(rect) = self.paint(line) {
            self.layout_map.push(BlockRegion {
                message_idx: mi,
                block_idx,
                start_byte: wl.start_byte,
                end_byte: wl.end_byte,
                text: wl.text.clone(),
                prefix_cols,
                rect,
                hidden_ranges: hidden_ranges.to_vec(),
            });
        }
    }
}

pub(crate) fn nonempty_wrapped(wrapped: Vec<WrappedLine>) -> Vec<WrappedLine> {
    if wrapped.is_empty() {
        vec![WrappedLine {
            text: String::new(),
            start_byte: 0,
            end_byte: 0,
        }]
    } else {
        wrapped
    }
}

pub(crate) fn advance_plain_blank_rows(
    transcript_area: Rect,
    rows: usize,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
) {
    for _ in 0..rows {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows = skip_rows.saturating_sub(1);
        } else if *current_y < transcript_area.y + transcript_area.height {
            *current_y += 1;
        }
    }
}

/// Clamp a fold summary to one line of `max_width` columns.
///
/// A fold summary is prose that must be cut at its first line break rather than
/// flattened — splicing two unrelated sentences into one run-on would misreport
/// what the fold hides. That is exactly
/// [`fit_line`](nuo_ui_primitives::components::overflow::fit_line), so this is a
/// thin alias over the substrate's overflow vocabulary rather than a fourth
/// private copy of it (ADR-0152 `[INV-TEXT-OVERFLOW-01]`).
pub(crate) fn truncate_to_width(text: &str, max_width: usize) -> String {
    nuo_ui_primitives::components::overflow::fit_line(
        text,
        max_width,
        nuo_ui_primitives::components::overflow::Overflow::End,
    )
    .text
}
