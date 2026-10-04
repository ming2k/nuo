//! Universal Reusable Anchored Popover Container Component for TUI.
//!
//! Provides a clean floating popover shell with auto-flip geometry, viewport boundary
//! clamping, elevation styling, title and footer affordances, and inner layout solving.

use nuotc::{
    Alignment, Block as RtBlock, BorderType, Borders, Clear, Frame, Line, Modifier, Paragraph,
    Rect, Span, Style,
    anchor::{
        AnchorAlignment, AnchorConstraints, AnchorPlacement, AnchorTarget, compute_anchored_rect,
    },
};

use crate::elevation::ElevationContainer;
use crate::render::Theme;

/// Configuration for an anchored popover shell.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct AnchoredPopover {
    pub target: AnchorTarget<Rect>,
    pub placement: AnchorPlacement,
    pub alignment: AnchorAlignment,
    pub constraints: AnchorConstraints,
    pub title: Option<String>,
    pub footer_hint: Option<String>,
    pub border_type: BorderType,
}

#[allow(dead_code)]
impl AnchoredPopover {
    /// Create a new anchored popover relative to a target rectangle.
    pub fn new(target_rect: Rect) -> Self {
        Self {
            target: AnchorTarget::Rect(target_rect),
            placement: AnchorPlacement::AutoVertical,
            alignment: AnchorAlignment::Start,
            constraints: AnchorConstraints::new()
                .with_width_bounds(36, 80)
                .with_height_bounds(5, 20),
            title: None,
            footer_hint: None,
            border_type: BorderType::Plain,
        }
    }

    /// Create a center-screen popover.
    pub fn center_screen() -> Self {
        Self {
            target: AnchorTarget::Rect(Rect::default()),
            placement: AnchorPlacement::CenterViewport,
            alignment: AnchorAlignment::Center,
            constraints: AnchorConstraints::new()
                .with_width_bounds(44, 84)
                .with_height_bounds(5, 22),
            title: None,
            footer_hint: None,
            border_type: BorderType::Plain,
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

    pub fn with_constraints(mut self, constraints: AnchorConstraints) -> Self {
        self.constraints = constraints;
        self
    }

    pub fn with_width_bounds(mut self, min: u16, max: u16) -> Self {
        self.constraints = self.constraints.with_width_bounds(min, max);
        self
    }

    pub fn with_max_height(mut self, max_height: u16) -> Self {
        self.constraints = self.constraints.with_max_height(max_height);
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_footer_hint(mut self, hint: impl Into<String>) -> Self {
        self.footer_hint = Some(hint.into());
        self
    }

    pub fn with_border_type(mut self, border_type: BorderType) -> Self {
        self.border_type = border_type;
        self
    }

    /// Compute the outer bounding rectangle for this popover given a content size.
    pub fn compute_rect(&self, viewport: Rect, content_size: (u16, u16)) -> Rect {
        let anchor_rect = match self.target {
            AnchorTarget::Rect(r) => r,
            AnchorTarget::Node(r) => r,
        };
        let chrome_height = 2
            + if self.title.is_some() { 1 } else { 0 }
            + if self.footer_hint.is_some() { 1 } else { 0 };
        let chrome_width = 4;

        let requested_size = (
            content_size.0.saturating_add(chrome_width),
            content_size.1.saturating_add(chrome_height),
        );

        compute_anchored_rect(
            anchor_rect,
            viewport,
            requested_size,
            self.placement,
            self.alignment,
            &self.constraints,
        )
    }

    /// Render the popover background, border, title, and footer, returning the inner content area.
    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        viewport: Rect,
        content_size: (u16, u16),
        theme: &Theme,
    ) -> PopoverFrame {
        let outer_rect = self.compute_rect(viewport, content_size);
        if outer_rect.width < 4 || outer_rect.height < 3 {
            return PopoverFrame {
                outer_rect,
                inner_rect: Rect::default(),
            };
        }

        // Clear underlying cells
        frame.render_widget(Clear, outer_rect);

        // Render elevation container background
        let _ = ElevationContainer::overlay().render(frame, outer_rect, theme);

        // Frame borders
        let block = RtBlock::default()
            .borders(Borders::ALL)
            .border_type(self.border_type)
            .border_style(Style::default().fg(theme.brand()))
            .style(Style::default().bg(theme.panel()));

        let inner_rect = block.inner(outer_rect);
        frame.render_widget(block, outer_rect);

        let mut actual_inner = inner_rect;

        // Title row if present
        if let Some(ref title) = self.title
            && actual_inner.height > 1
        {
            let title_rect = Rect::new(actual_inner.x, actual_inner.y, actual_inner.width, 1);
            let p = Paragraph::new(Line::from(Span::styled(
                title.as_str(),
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            )));
            frame.render_widget(p, title_rect);

            actual_inner = Rect::new(
                actual_inner.x,
                actual_inner.y + 1,
                actual_inner.width,
                actual_inner.height.saturating_sub(1),
            );
        }

        // Optional footer hint
        if let Some(ref hint) = self.footer_hint
            && actual_inner.height > 1
        {
            let footer_y = actual_inner.bottom().saturating_sub(1);
            let footer_rect = Rect::new(actual_inner.x, footer_y, actual_inner.width, 1);
            let p = Paragraph::new(Line::from(Span::styled(
                hint.as_str(),
                Style::default().fg(theme.dim()),
            )))
            .alignment(Alignment::Right);
            frame.render_widget(p, footer_rect);

            actual_inner = Rect::new(
                actual_inner.x,
                actual_inner.y,
                actual_inner.width,
                actual_inner.height.saturating_sub(1),
            );
        }

        PopoverFrame {
            outer_rect,
            inner_rect: actual_inner,
        }
    }
}

/// Resolved outer and inner frames after popover layout rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub struct PopoverFrame {
    pub outer_rect: Rect,
    pub inner_rect: Rect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anchored_popover_flips_above_when_below_overflows() {
        let viewport = Rect::new(0, 0, 80, 24);
        let target = Rect::new(10, 20, 30, 2);
        let popover = AnchoredPopover::new(target);

        let outer = popover.compute_rect(viewport, (25, 5));
        assert!(outer.bottom() <= target.y);
    }

    #[test]
    fn test_popover_center_screen() {
        let viewport = Rect::new(0, 0, 100, 40);
        let popover = AnchoredPopover::center_screen();
        let outer = popover.compute_rect(viewport, (40, 10));

        assert_eq!(outer.x, (100 - outer.width) / 2);
        assert_eq!(outer.y, (40 - outer.height) / 2);
    }

    #[test]
    fn test_popover_render_produces_inner_content_rect() {
        use nuotc::TestTerminal;

        let mut terminal = TestTerminal::new(80, 24);
        let theme = Theme::default();
        let target = Rect::new(10, 5, 20, 2);
        let popover = AnchoredPopover::new(target)
            .with_title("Options")
            .with_footer_hint("Esc Cancel");

        let mut frame_rects = None;
        terminal.draw(|frame| {
            let res = popover.render(frame, Rect::new(0, 0, 80, 24), (30, 6), &theme);
            frame_rects = Some(res);
        });

        let fr = frame_rects.expect("frame rects should be returned");
        assert!(fr.outer_rect.width >= 30);
        assert!(fr.outer_rect.height >= 8);
        assert!(fr.inner_rect.width < fr.outer_rect.width);
        assert!(fr.inner_rect.height < fr.outer_rect.height);
    }
}
