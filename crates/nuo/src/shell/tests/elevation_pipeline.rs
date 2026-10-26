//! End-to-end integration tests for ADR-0053: Capability-First Visual Archetypes,
//! Structural Elevation, and Responsive Layout Pipeline.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuotc::{CellGrid, Damage, Frame, Modifier, Rect, TerminalProfile};

use nuo_ui_primitives::elevation::ElevationArchetype;
use nuo_ui_primitives::primitives::{ElevationContainer, LayoutTier, modal_frame};
use nuo_ui_primitives::theme::Theme;

#[test]
fn test_profile_to_elevation_archetype_mapping() {
    let direct_color = TerminalProfile::direct_color();
    assert_eq!(
        ElevationArchetype::for_profile(&direct_color),
        ElevationArchetype::Chromatic
    );

    let indexed256 = TerminalProfile::indexed256();
    assert_eq!(
        ElevationArchetype::for_profile(&indexed256),
        ElevationArchetype::Chromatic
    );

    let ansi16 = TerminalProfile::ecma48_ansi16();
    assert_eq!(
        ElevationArchetype::for_profile(&ansi16),
        ElevationArchetype::Hybrid
    );

    let mono = TerminalProfile::dec_vt100_monochrome();
    assert_eq!(
        ElevationArchetype::for_profile(&mono),
        ElevationArchetype::Structured
    );
}

#[test]
fn test_structured_archetype_deducts_the_frame_from_inner_bounds() {
    let full = Rect::new(0, 0, 20, 10);
    // Chromatic / Hybrid separate by luminance, so they spend no space.
    assert_eq!(ElevationArchetype::Chromatic.inner_bounds(full), full);
    assert_eq!(ElevationArchetype::Hybrid.inner_bounds(full), full);
    // Structured draws an explicit box, costing one cell on every side.
    assert_eq!(
        ElevationArchetype::Structured.inner_bounds(full),
        Rect::new(1, 1, 18, 8)
    );
}

#[test]
fn test_responsive_breakpoint_with_spatial_cost() {
    let viewport_90 = Rect::new(0, 0, 90, 24);

    // Modern terminal (TrueColor DirectColor): 90 columns has 0 border cost -> Wide
    let tier_chromatic = LayoutTier::from_rect(viewport_90, ElevationArchetype::Chromatic);
    assert_eq!(tier_chromatic, LayoutTier::Wide);
    assert!(tier_chromatic.is_wide());

    // Constrained terminal (DEC VT100 / Linux VT Monochrome):
    // 2-column border cost drops effective width to 88 columns -> Compact (vertical stack)
    let tier_structured = LayoutTier::from_rect(viewport_90, ElevationArchetype::Structured);
    assert_eq!(tier_structured, LayoutTier::Compact);
    assert!(!tier_structured.is_wide());

    // Constrained terminal with 92 columns: 92 - 2 = 90 columns -> Wide
    let viewport_92 = Rect::new(0, 0, 92, 24);
    let tier_structured_wide = LayoutTier::from_rect(viewport_92, ElevationArchetype::Structured);
    assert_eq!(tier_structured_wide, LayoutTier::Wide);
}

#[test]
fn test_elevation_container_monochrome_ascii_framing() {
    let theme_mono = Theme::monochrome();
    assert_eq!(theme_mono.elevation, ElevationArchetype::Structured);

    let mut grid = CellGrid::new(20, 10);
    let mut damage = Damage::new(10);
    let mut frame = Frame::new(&mut grid, &mut damage);
    let container_rect = Rect::new(0, 0, 20, 10);

    let inner = ElevationContainer::panel().render(&mut frame, container_rect, &theme_mono);
    assert_eq!(inner, Rect::new(1, 1, 18, 8));

    // Top-left, top-right, bottom-left, bottom-right corners are ASCII "+"
    assert_eq!(grid.get(0, 0).unwrap().symbol.as_str(), "+");
    assert_eq!(grid.get(19, 0).unwrap().symbol.as_str(), "+");
    assert_eq!(grid.get(0, 9).unwrap().symbol.as_str(), "+");
    assert_eq!(grid.get(19, 9).unwrap().symbol.as_str(), "+");

    // Horizontal bars are "-"
    assert_eq!(grid.get(1, 0).unwrap().symbol.as_str(), "-");
    assert_eq!(grid.get(18, 0).unwrap().symbol.as_str(), "-");
    assert_eq!(grid.get(1, 9).unwrap().symbol.as_str(), "-");
    assert_eq!(grid.get(18, 9).unwrap().symbol.as_str(), "-");

    // Vertical bars are "|"
    assert_eq!(grid.get(0, 1).unwrap().symbol.as_str(), "|");
    assert_eq!(grid.get(0, 8).unwrap().symbol.as_str(), "|");
    assert_eq!(grid.get(19, 1).unwrap().symbol.as_str(), "|");
    assert_eq!(grid.get(19, 8).unwrap().symbol.as_str(), "|");
}

#[test]
fn test_elevation_container_focused_reverse_video() {
    let theme_mono = Theme::monochrome();
    let mut grid = CellGrid::new(12, 6);
    let mut damage = Damage::new(6);
    let mut frame = Frame::new(&mut grid, &mut damage);
    let container_rect = Rect::new(0, 0, 12, 6);

    ElevationContainer::card()
        .focused(true)
        .render(&mut frame, container_rect, &theme_mono);

    // Under Structured, focused container carries REVERSE modifier
    let corner = grid.get(0, 0).unwrap();
    assert!(corner.style.add.contains(Modifier::REVERSE));
    let border = grid.get(1, 0).unwrap();
    assert!(border.style.add.contains(Modifier::REVERSE));
}

#[test]
fn test_modal_frame_monochrome_bounding_box() {
    let theme_mono = Theme::monochrome();
    let mut grid = CellGrid::new(50, 20);
    let mut damage = Damage::new(20);
    let mut frame = Frame::new(&mut grid, &mut damage);
    let modal_area = Rect::new(10, 5, 30, 10);

    let mf = modal_frame(&mut frame, modal_area, &theme_mono, true, true);
    assert!(mf.header.is_some());
    assert!(mf.footer.is_some());

    // Perimeter cells are ASCII borders
    assert_eq!(grid.get(10, 5).unwrap().symbol.as_str(), "+");
    assert_eq!(grid.get(39, 5).unwrap().symbol.as_str(), "+");
    assert_eq!(grid.get(10, 14).unwrap().symbol.as_str(), "+");
    assert_eq!(grid.get(39, 14).unwrap().symbol.as_str(), "+");
}

#[test]
fn test_theme_selection_style_monochrome_reversal() {
    let theme_mono = Theme::monochrome();
    let style = theme_mono.selection_style(true);
    assert!(style.add.contains(Modifier::REVERSE));

    let focus = theme_mono.focus_style(true);
    assert!(focus.add.contains(Modifier::REVERSE));

    // Unselected / unfocused has no modifier
    assert!(
        !theme_mono
            .selection_style(false)
            .add
            .contains(Modifier::REVERSE)
    );
    assert!(
        !theme_mono
            .focus_style(false)
            .add
            .contains(Modifier::REVERSE)
    );

    // Chromatic does not use reverse video by default
    let theme_chromatic = Theme::default();
    assert!(
        !theme_chromatic
            .selection_style(true)
            .add
            .contains(Modifier::REVERSE)
    );
}
