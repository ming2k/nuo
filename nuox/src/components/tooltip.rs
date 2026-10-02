//! Universal Reusable Anchored Tooltip Component for TUI.
//!
//! Provides a compact, non-stealing, pointer-transparent informational balloon
//! that anchors to any screen element or cursor point.

use nuotc::{
    Block as RtBlock, BorderType, Borders, Clear, Frame, Line, Paragraph, Rect, Span, Style,
    anchor::{
        AnchorAlignment, AnchorConstraints, AnchorPlacement, AnchorTarget, compute_anchored_rect,
    },
};
use unicode_width::UnicodeWidthStr;

use crate::primitives::contrast_fg;
use crate::render::Theme;

/// Configuration for an anchored tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct AnchoredTooltip {
    pub target: AnchorTarget<Rect>,
    pub text: String,
    pub placement: AnchorPlacement,
    pub alignment: AnchorAlignment,
    pub bordered: bool,
}

#[allow(dead_code)]
impl AnchoredTooltip {
    /// Create a tooltip anchored to a target rectangle.
    pub fn new(target_rect: Rect, text: impl Into<String>) -> Self {
        Self {
            target: AnchorTarget::Rect(target_rect),
            text: text.into(),
            placement: AnchorPlacement::Top,
            alignment: AnchorAlignment::Center,
            bordered: true,
        }
    }

    pub fn with_placement(mut self, placement: AnchorPlacement) -> Self {
        self.placement = placement;
        self
    }

    pub fn with_alignment(mut self, alignment: AnchorAlignment) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn with_bordered(mut self, bordered: bool) -> Self {
        self.bordered = bordered;
        self
    }

    /// Compute tooltip screen rectangle.
    pub fn compute_rect(&self, viewport: Rect) -> Rect {
        let text_w = self.text.as_str().width() as u16;
        let (req_w, req_h) = if self.bordered {
            (text_w.saturating_add(4), 3)
        } else {
            (text_w.saturating_add(2), 1)
        };

        let constraints = AnchorConstraints::new()
            .with_width_bounds(3, viewport.width.saturating_sub(2))
            .with_height_bounds(1, 5);

        let anchor_rect = match self.target {
            AnchorTarget::Rect(r) => r,
            AnchorTarget::Node(r) => r,
        };

        compute_anchored_rect(
            anchor_rect,
            viewport,
            (req_w, req_h),
            self.placement,
            self.alignment,
            &constraints,
        )
    }

    /// Render tooltip onto the frame.
    pub fn render(&self, frame: &mut Frame<'_>, viewport: Rect, theme: &Theme) -> Rect {
        let rect = self.compute_rect(viewport);
        if rect.width == 0 || rect.height == 0 {
            return rect;
        }

        frame.render_widget(Clear, rect);

        let bg = theme.raised();
        let fg = contrast_fg(bg);

        if self.bordered && rect.height >= 3 {
            let block = RtBlock::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(theme.dim()))
                .style(Style::default().bg(bg));
            let inner = block.inner(rect);
            frame.render_widget(block, rect);

            let p = Paragraph::new(Line::from(Span::styled(
                self.text.as_str(),
                Style::default().fg(fg).bg(bg),
            )));
            frame.render_widget(p, inner);
        } else {
            let p = Paragraph::new(Line::from(vec![
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(self.text.as_str(), Style::default().fg(fg).bg(bg)),
                Span::styled(" ", Style::default().bg(bg)),
            ]))
            .style(Style::default().bg(bg));
            frame.render_widget(p, rect);
        }

        rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tooltip_computes_tight_rect_above_target() {
        let viewport = Rect::new(0, 0, 80, 24);
        let target = Rect::new(20, 10, 15, 1);
        let tooltip = AnchoredTooltip::new(target, "Short hint");

        let rect = tooltip.compute_rect(viewport);
        assert_eq!(rect.height, 3);
        assert!(rect.bottom() <= target.y);
    }

    #[test]
    fn test_tooltip_renders_cleanly() {
        use nuotc::TestTerminal;

        let mut terminal = TestTerminal::new(80, 24);
        let theme = Theme::default();
        let target = Rect::new(20, 10, 15, 1);
        let tooltip = AnchoredTooltip::new(target, "Short hint");

        let mut rendered_rect = None;
        terminal.draw(|frame| {
            let r = tooltip.render(frame, Rect::new(0, 0, 80, 24), &theme);
            rendered_rect = Some(r);
        });

        let r = rendered_rect.expect("rendered rect should be returned");
        assert_eq!(r.height, 3);
        assert!(r.width >= "Short hint".len() as u16);
    }
}
