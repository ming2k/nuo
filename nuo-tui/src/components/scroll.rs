//! Scrollable content components.

use nuotc::{Frame, Line, Rect};

use super::super::Theme;
use super::super::primitives::{BodyRenderOptions, render_body};

pub(crate) struct ScrollBody<'a> {
    pub lines: Vec<Line<'static>>,
    pub scroll: &'a mut usize,
    pub follow: Option<usize>,
    pub edge_margin: usize,
    pub wrap: bool,
}

impl<'a> ScrollBody<'a> {
    pub(crate) fn render(self, frame: &mut Frame, rect: Rect, theme: &Theme) {
        render_body(
            frame,
            rect,
            self.lines,
            self.scroll,
            BodyRenderOptions {
                follow: self.follow,
                edge_margin: self.edge_margin,
                wrap: self.wrap,
            },
            theme,
        );
    }
}
