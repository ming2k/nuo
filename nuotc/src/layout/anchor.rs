//! Generic anchor-based positioning and layout primitives.
//!
//! Provides geometric anchoring calculations and alignment strategies for
//! floating popovers, tooltips, flyouts, and dropdown menus. Anchoring supports
//! both explicit screen rectangles and scene node references, with automatic
//! axis-flipping (above/below or left/right) and boundary clamping against
//! the terminal viewport.

use crate::Rect;

/// Placement strategy relative to the anchor target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnchorPlacement {
    /// Directly above the anchor target.
    Top,
    /// Directly below the anchor target.
    Bottom,
    /// To the left of the anchor target.
    Left,
    /// To the right of the anchor target.
    Right,
    /// Automatically flip Top or Bottom depending on available vertical space.
    #[default]
    AutoVertical,
    /// Automatically flip Left or Right depending on available horizontal space.
    AutoHorizontal,
    /// Center within the viewport, ignoring the anchor bounds.
    CenterViewport,
}

/// Alignment along the cross-axis of the placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnchorAlignment {
    /// Align start/leading edges (left for vertical, top for horizontal).
    #[default]
    Start,
    /// Center along the cross-axis.
    Center,
    /// Align end/trailing edges (right for vertical, bottom for horizontal).
    End,
}

/// Size bounds and offsets applied during anchor resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorConstraints {
    pub min_width: u16,
    pub max_width: u16,
    pub min_height: u16,
    pub max_height: u16,
    pub offset_x: i16,
    pub offset_y: i16,
    pub match_anchor_width: bool,
}

impl Default for AnchorConstraints {
    fn default() -> Self {
        Self {
            min_width: 1,
            max_width: u16::MAX,
            min_height: 1,
            max_height: u16::MAX,
            offset_x: 0,
            offset_y: 0,
            match_anchor_width: false,
        }
    }
}

impl AnchorConstraints {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_min_width(mut self, min_width: u16) -> Self {
        self.min_width = min_width;
        self
    }

    pub fn with_max_width(mut self, max_width: u16) -> Self {
        self.max_width = max_width;
        self
    }

    pub fn with_width_bounds(mut self, min: u16, max: u16) -> Self {
        self.min_width = min;
        self.max_width = max;
        self
    }

    pub fn with_min_height(mut self, min_height: u16) -> Self {
        self.min_height = min_height;
        self
    }

    pub fn with_max_height(mut self, max_height: u16) -> Self {
        self.max_height = max_height;
        self
    }

    pub fn with_height_bounds(mut self, min: u16, max: u16) -> Self {
        self.min_height = min;
        self.max_height = max;
        self
    }

    pub fn with_offset(mut self, offset_x: i16, offset_y: i16) -> Self {
        self.offset_x = offset_x;
        self.offset_y = offset_y;
        self
    }

    pub fn with_match_anchor_width(mut self, match_anchor_width: bool) -> Self {
        self.match_anchor_width = match_anchor_width;
        self
    }
}

/// Target reference for an anchored element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorTarget<K = Rect> {
    Rect(Rect),
    Node(K),
}

impl<K> From<Rect> for AnchorTarget<K> {
    fn from(rect: Rect) -> Self {
        AnchorTarget::Rect(rect)
    }
}

/// Full specification for resolving an anchored box layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredBox<K = ()> {
    pub target: AnchorTarget<K>,
    pub placement: AnchorPlacement,
    pub alignment: AnchorAlignment,
    pub content_size: (u16, u16),
    pub constraints: AnchorConstraints,
}

impl<K: Copy> Copy for AnchoredBox<K> {}

impl<K> AnchoredBox<K> {
    pub fn new(target: AnchorTarget<K>, content_size: (u16, u16)) -> Self {
        Self {
            target,
            placement: AnchorPlacement::AutoVertical,
            alignment: AnchorAlignment::Start,
            content_size,
            constraints: AnchorConstraints::default(),
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
}

/// Compute the bounded screen rectangle for an element anchored to a target rectangle.
pub fn compute_anchored_rect(
    anchor: Rect,
    viewport: Rect,
    requested_size: (u16, u16),
    placement: AnchorPlacement,
    alignment: AnchorAlignment,
    constraints: &AnchorConstraints,
) -> Rect {
    if viewport.width == 0 || viewport.height == 0 {
        return Rect::default();
    }

    // 1. Calculate effective width
    let base_width = if constraints.match_anchor_width {
        anchor.width.max(requested_size.0)
    } else {
        requested_size.0
    };
    let width = base_width
        .max(constraints.min_width)
        .min(constraints.max_width)
        .min(viewport.width)
        .max(1);

    // 2. Calculate effective height
    let height = requested_size
        .1
        .max(constraints.min_height)
        .min(constraints.max_height)
        .min(viewport.height)
        .max(1);

    // 3. Center in viewport shortcut
    if placement == AnchorPlacement::CenterViewport {
        let x = viewport.x + (viewport.width.saturating_sub(width)) / 2;
        let y = viewport.y + (viewport.height.saturating_sub(height)) / 2;
        return Rect::new(x, y, width, height);
    }

    // 4. Resolve placement coordinates
    match placement {
        AnchorPlacement::Top | AnchorPlacement::Bottom | AnchorPlacement::AutoVertical => {
            let space_below =
                (viewport.y + viewport.height).saturating_sub(anchor.y + anchor.height);
            let space_above = anchor.y.saturating_sub(viewport.y);

            let place_below = match placement {
                AnchorPlacement::Bottom => true,
                AnchorPlacement::Top => false,
                AnchorPlacement::AutoVertical => {
                    space_below >= height || space_below >= space_above
                }
                _ => unreachable!(),
            };

            let y = if place_below {
                (anchor.y + anchor.height)
                    .saturating_add_signed(constraints.offset_y)
                    .min((viewport.y + viewport.height).saturating_sub(height))
            } else {
                anchor
                    .y
                    .saturating_sub(height)
                    .saturating_add_signed(-constraints.offset_y)
                    .max(viewport.y)
            };

            let raw_x = match alignment {
                AnchorAlignment::Start => anchor.x.saturating_add_signed(constraints.offset_x),
                AnchorAlignment::Center => (anchor.x + anchor.width / 2).saturating_sub(width / 2),
                AnchorAlignment::End => (anchor.x + anchor.width)
                    .saturating_sub(width)
                    .saturating_add_signed(constraints.offset_x),
            };
            let x = raw_x
                .min((viewport.x + viewport.width).saturating_sub(width))
                .max(viewport.x);

            Rect::new(x, y, width, height)
        }
        AnchorPlacement::Left | AnchorPlacement::Right | AnchorPlacement::AutoHorizontal => {
            let space_right = (viewport.x + viewport.width).saturating_sub(anchor.x + anchor.width);
            let space_left = anchor.x.saturating_sub(viewport.x);

            let place_right = match placement {
                AnchorPlacement::Right => true,
                AnchorPlacement::Left => false,
                AnchorPlacement::AutoHorizontal => {
                    space_right >= width || space_right >= space_left
                }
                _ => unreachable!(),
            };

            let x = if place_right {
                (anchor.x + anchor.width)
                    .saturating_add_signed(constraints.offset_x)
                    .min((viewport.x + viewport.width).saturating_sub(width))
            } else {
                anchor
                    .x
                    .saturating_sub(width)
                    .saturating_add_signed(-constraints.offset_x)
                    .max(viewport.x)
            };

            let raw_y = match alignment {
                AnchorAlignment::Start => anchor.y.saturating_add_signed(constraints.offset_y),
                AnchorAlignment::Center => {
                    (anchor.y + anchor.height / 2).saturating_sub(height / 2)
                }
                AnchorAlignment::End => (anchor.y + anchor.height)
                    .saturating_sub(height)
                    .saturating_add_signed(constraints.offset_y),
            };
            let y = raw_y
                .min((viewport.y + viewport.height).saturating_sub(height))
                .max(viewport.y);

            Rect::new(x, y, width, height)
        }
        AnchorPlacement::CenterViewport => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_vertical_flips_when_space_below_is_insufficient() {
        let viewport = Rect::new(0, 0, 80, 24);
        // Anchor near the bottom of viewport (y: 20, h: 2 -> bottom: 22)
        let anchor = Rect::new(10, 20, 20, 2);
        let requested_size = (30, 8);

        let result = compute_anchored_rect(
            anchor,
            viewport,
            requested_size,
            AnchorPlacement::AutoVertical,
            AnchorAlignment::Start,
            &AnchorConstraints::default(),
        );

        // Only 2 rows below anchor, but 20 rows above anchor -> should flip above!
        assert_eq!(result.y, 20 - 8);
        assert_eq!(result.x, 10);
        assert_eq!(result.width, 30);
        assert_eq!(result.height, 8);
    }

    #[test]
    fn test_auto_vertical_places_below_when_space_is_abundant() {
        let viewport = Rect::new(0, 0, 80, 24);
        let anchor = Rect::new(10, 5, 20, 2);
        let requested_size = (30, 6);

        let result = compute_anchored_rect(
            anchor,
            viewport,
            requested_size,
            AnchorPlacement::AutoVertical,
            AnchorAlignment::Start,
            &AnchorConstraints::default(),
        );

        // Plenty of space below (from y: 7 to 24 = 17 rows) -> should place below!
        assert_eq!(result.y, 7);
        assert_eq!(result.x, 10);
        assert_eq!(result.width, 30);
        assert_eq!(result.height, 6);
    }

    #[test]
    fn test_anchored_box_clamps_to_viewport_edges() {
        let viewport = Rect::new(0, 0, 80, 24);
        // Anchor near right edge (x: 70, w: 10)
        let anchor = Rect::new(70, 5, 10, 2);
        let requested_size = (25, 6);

        let result = compute_anchored_rect(
            anchor,
            viewport,
            requested_size,
            AnchorPlacement::Bottom,
            AnchorAlignment::Start,
            &AnchorConstraints::default(),
        );

        // Width 25 cannot start at 70 (70 + 25 = 95 > 80). Clamped to 80 - 25 = 55.
        assert_eq!(result.x, 55);
        assert_eq!(result.y, 7);
        assert_eq!(result.width, 25);
    }

    #[test]
    fn test_center_viewport_placement() {
        let viewport = Rect::new(0, 0, 100, 40);
        let anchor = Rect::new(10, 10, 5, 5);
        let requested_size = (40, 20);

        let result = compute_anchored_rect(
            anchor,
            viewport,
            requested_size,
            AnchorPlacement::CenterViewport,
            AnchorAlignment::Center,
            &AnchorConstraints::default(),
        );

        assert_eq!(result.x, 30);
        assert_eq!(result.y, 10);
        assert_eq!(result.width, 40);
        assert_eq!(result.height, 20);
    }

    #[test]
    fn test_match_anchor_width() {
        let viewport = Rect::new(0, 0, 100, 40);
        let anchor = Rect::new(15, 10, 45, 2);
        let requested_size = (20, 10);
        let constraints = AnchorConstraints::default().with_match_anchor_width(true);

        let result = compute_anchored_rect(
            anchor,
            viewport,
            requested_size,
            AnchorPlacement::Bottom,
            AnchorAlignment::Start,
            &constraints,
        );

        assert_eq!(result.x, 15);
        assert_eq!(result.width, 45);
    }
}
