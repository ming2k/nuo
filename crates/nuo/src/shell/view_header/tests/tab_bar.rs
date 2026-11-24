//! Tier 1 tab bar tests: widths, rects, overflow, and styling.

use super::*;

use crate::shell::BAR_LEGEND_GAP_MIN;

#[test]
fn row1_renders_tabs_without_ordinal_prefixes() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
        crate::shell::surfaces::ClientTab::dashboard(),
    ];
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 0,
        unread_faults: 0,
        ..TabBarProps::default()
    };
    let row = render_row1(80, tab_bar);
    assert!(row.contains("thread#b3c4"), "{row}");
    assert!(
        !row.contains("1:thread#b3c4"),
        "tabs must not use ordinal prefix: {row}"
    );
    assert!(row.contains("dashboard"), "{row}");
    assert!(
        !row.contains("2:dashboard"),
        "tabs must not use ordinal prefix: {row}"
    );
    assert!(
        !row.contains("F1"),
        "unbound help_key renders no fabricated keycap (ADR-0131 [INV-AFFORDANCE-08], ADR-0245): {row}"
    );
}

/// The status cluster's rightmost element is the unread-fault badge, and it
/// ends one column before the terminal edge so the row never touches the frame.
#[test]
fn row1_status_cluster_pins_the_notice_badge_to_the_inset_edge() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
        crate::shell::surfaces::ClientTab::dashboard(),
    ];
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 0,
        unread_faults: 3,
        ..TabBarProps::default()
    };
    let mut terminal = graticule::TestTerminal::new(80, 1);
    let mut nav = TabBarNavRects::default();
    terminal.draw(|frame| {
        nav = draw_tab_bar(frame, frame.area(), &tab_bar, &Theme::default());
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(row.contains("[!3]"), "fault badge shown: {row:?}");
    assert_eq!(
        nav.notices_rect.expect("fault badge").right(),
        79,
        "the status cluster ends 1 column before the terminal edge: {row:?}"
    );
}

/// Tier 1 bleeds its tabbar_bg fill to both terminal edges so the top bar
/// forms a continuous frame line. Inside the row, TABBAR_H_INSET (1) leaves
/// 1 column of breathing room at each end.
#[test]
fn row1_bleeds_to_both_terminal_edges() {
    for width in [44u16, 80, 120] {
        let tabs = vec![
            crate::shell::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
            crate::shell::surfaces::ClientTab::dashboard(),
        ];
        let tab_bar = TabBarProps {
            tabs: Some(&tabs),
            active_tab: 0,
            unread_faults: 0,
            ..TabBarProps::default()
        };
        let mut terminal = graticule::TestTerminal::new(width, 1);
        let mut nav = TabBarNavRects::default();
        terminal.draw(|frame| {
            nav = draw_tab_bar(frame, frame.area(), &tab_bar, &Theme::default());
        });
        let row: String = terminal
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect();
        assert_eq!(row.width(), width as usize, "row fills the width: {row:?}");
        assert_eq!(
            nav.tab_rects[0].1.x, 1,
            "the tab rail starts at column 1 (inset by 1): {row:?}"
        );
        assert!(
            nav.notices_rect.is_none(),
            "no fault badge without unread faults: {row:?}"
        );
        let cells = terminal.buffer().cells();
        assert_eq!(
            cells[0].style.bg,
            Theme::default().tabbar_bg(),
            "left edge column 0 is filled with tabbar_bg"
        );
        assert_eq!(
            cells[width as usize - 1].style.bg,
            Theme::default().tabbar_bg(),
            "right edge column is filled with tabbar_bg"
        );
    }
}

/// Edge-to-edge must not mean glyph-on-edge: the first and last *tab*
/// labels keep their own single-column breathing room inside the rail.
#[test]
fn row1_tab_labels_keep_internal_padding_at_the_edges() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
        crate::shell::surfaces::ClientTab::dashboard(),
    ];
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 0,
        unread_faults: 0,
        ..TabBarProps::default()
    };
    let row = render_row1(80, tab_bar);
    assert!(
        row.starts_with("  thread#b3c4"),
        "leading label keeps 1-col edge margin and internal padding: {row:?}"
    );
}

/// The right-pinned status cluster keeps the standing legend gap, so a
/// truncated overflow button can never butt against the fault badge.
#[test]
fn row1_status_cluster_keeps_the_legend_gap_when_tabs_overflow() {
    let tabs: Vec<_> = (0..8)
        .map(|i| crate::shell::surfaces::ClientTab::thread(format!("session-{i}"), None::<String>))
        .collect();
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 7,
        unread_faults: 2,
        ..TabBarProps::default()
    };
    let row = render_row1(60, tab_bar);
    let badge_at = row.find("[!2]").expect("fault badge present");
    let before = &row[..badge_at];
    assert!(
        before.ends_with(&" ".repeat(BAR_LEGEND_GAP_MIN)),
        "at least {BAR_LEGEND_GAP_MIN} columns before the badge: {row:?}"
    );
}

#[test]
fn row1_renders_semantic_tab_titles_with_wider_budget() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread(
            "sess-01a2b3c4",
            Some("Refactor Database Connection"),
        ),
        crate::shell::surfaces::ClientTab::thread("sess-05d6e7f8", None::<String>),
        crate::shell::surfaces::ClientTab::dashboard(),
    ];
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 0,
        unread_faults: 0,
        ..TabBarProps::default()
    };
    let row = render_row1(80, tab_bar);
    assert!(
        row.contains("Refactor Database Co"),
        "semantic title uses the full budget without an ellipsis: {row}"
    );
    assert!(
        !row.contains('…'),
        "overflowing tab titles fade toward the background instead of appending `…`: {row}"
    );
    assert!(!row.contains("1:Refactor"), "no ordinal prefix: {row}");
    assert!(
        row.contains("thread#e7f8"),
        "untitled tab falls back to placeholder thread#tail: {row}"
    );
    assert!(row.contains("dashboard"), "{row}");
}

#[test]
fn row1_tab_rects_and_styling() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
        crate::shell::surfaces::ClientTab::dashboard(),
        crate::shell::surfaces::ClientTab::settings(),
    ];
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 0,
        unread_faults: 0,
        ..TabBarProps::default()
    };
    let mut terminal = graticule::TestTerminal::new(80, 1);
    let theme = Theme::default();
    let mut tab_nav = TabBarNavRects::default();
    terminal.draw(|frame| {
        tab_nav = draw_tab_bar(frame, frame.area(), &tab_bar, &theme);
    });
    let tab_rects = tab_nav.tab_rects;
    assert_eq!(tab_rects.len(), 3);
    assert_eq!(tab_rects[0].0, 0);
    assert_eq!(tab_rects[1].0, 1);
    assert_eq!(tab_rects[2].0, 2);
    assert_eq!(tab_rects[0].1.y, 0);
    assert_eq!(tab_rects[0].1.x, 1, "tab 0 starts after 1-column margin");
    assert_eq!(
        tab_rects[1].1.x,
        tab_rects[0].1.right(),
        "tab 1 starts seamlessly where tab 0 ends"
    );
    assert_eq!(
        tab_rects[2].1.x,
        tab_rects[1].1.right(),
        "tab 2 starts seamlessly where tab 1 ends"
    );
    assert!(tab_rects[0].1.width > 0);
    assert!(tab_rects[1].1.x > tab_rects[0].1.x);

    let cells = terminal.buffer().cells();
    // Edge margins painted with tabbar_bg
    assert_eq!(cells[0].style.bg, theme.tabbar_bg());
    assert_eq!(cells[79].style.bg, theme.tabbar_bg());

    // Tab 0 is active -> selected_bg
    let t0_x = tab_rects[0].1.x as usize;
    assert_eq!(cells[t0_x].style.bg, theme.selected_bg);

    // Tab 1 is inactive, odd index 1 -> theme.body()
    let t1_x = tab_rects[1].1.x as usize;
    assert_eq!(cells[t1_x].style.bg, theme.body());

    // Tab 2 is inactive, even index 2 -> theme.panel()
    let t2_x = tab_rects[2].1.x as usize;
    assert_eq!(cells[t2_x].style.bg, theme.panel());
}

#[test]
fn row1_seamless_tabs_alternate_backgrounds_and_preserve_active_contrast() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread("sess-0", Some("Task 0".to_string())),
        crate::shell::surfaces::ClientTab::thread("sess-1", Some("Task 1".to_string())),
        crate::shell::surfaces::ClientTab::thread("sess-2", Some("Task 2".to_string())),
        crate::shell::surfaces::ClientTab::thread("sess-3", Some("Task 3".to_string())),
        crate::shell::surfaces::ClientTab::dashboard(),
    ];

    // Case 1: active in the middle (tab 2)
    {
        let tab_bar = TabBarProps {
            tabs: Some(&tabs),
            active_tab: 2,
            unread_faults: 0,
            ..TabBarProps::default()
        };
        let mut terminal = graticule::TestTerminal::new(100, 1);
        let theme = Theme::default();
        let mut nav = TabBarNavRects::default();
        terminal.draw(|frame| {
            nav = draw_tab_bar(frame, frame.area(), &tab_bar, &theme);
        });

        assert_eq!(nav.tab_rects.len(), 5);
        // Verify strict 1-column margin at left edge
        assert_eq!(nav.tab_rects[0].1.x, 1);
        let cells = terminal.buffer().cells();
        assert_eq!(cells[0].style.bg, theme.tabbar_bg(), "col 0 has tabbar_bg");
        assert_eq!(
            cells[99].style.bg,
            theme.tabbar_bg(),
            "col 99 has tabbar_bg"
        );
        assert!(
            nav.notices_rect.is_none(),
            "no standing badge on the right edge"
        );

        // Verify seamless tiling without gaps between any adjacent tabs
        for i in 0..4 {
            assert_eq!(
                nav.tab_rects[i + 1].1.x,
                nav.tab_rects[i].1.right(),
                "tab {} and tab {} butt seamlessly",
                i,
                i + 1
            );
        }

        // Verify backgrounds
        assert_eq!(
            cells[nav.tab_rects[0].1.x as usize].style.bg,
            theme.panel(),
            "tab 0 inactive even index -> panel"
        );
        assert_eq!(
            cells[nav.tab_rects[1].1.x as usize].style.bg,
            theme.body(),
            "tab 1 inactive odd index -> body"
        );
        assert_eq!(
            cells[nav.tab_rects[2].1.x as usize].style.bg, theme.selected_bg,
            "tab 2 active -> selected_bg"
        );
        assert_eq!(
            cells[nav.tab_rects[3].1.x as usize].style.bg,
            theme.body(),
            "tab 3 inactive odd index -> body"
        );
        assert_eq!(
            cells[nav.tab_rects[4].1.x as usize].style.bg,
            theme.panel(),
            "tab 4 inactive even index -> panel"
        );
    }

    // Case 2: active at start (tab 0)
    {
        let tab_bar = TabBarProps {
            tabs: Some(&tabs),
            active_tab: 0,
            unread_faults: 0,
            ..TabBarProps::default()
        };
        let mut terminal = graticule::TestTerminal::new(100, 1);
        let theme = Theme::default();
        let mut nav = TabBarNavRects::default();
        terminal.draw(|frame| {
            nav = draw_tab_bar(frame, frame.area(), &tab_bar, &theme);
        });

        let cells = terminal.buffer().cells();
        assert_eq!(
            cells[nav.tab_rects[0].1.x as usize].style.bg, theme.selected_bg,
            "tab 0 active -> selected_bg"
        );
        assert_eq!(
            cells[nav.tab_rects[1].1.x as usize].style.bg,
            theme.body(),
            "tab 1 inactive odd -> body"
        );
        assert_eq!(
            cells[nav.tab_rects[2].1.x as usize].style.bg,
            theme.panel(),
            "tab 2 inactive even -> panel"
        );
        assert_eq!(
            cells[nav.tab_rects[3].1.x as usize].style.bg,
            theme.body(),
            "tab 3 inactive odd -> body"
        );
        assert_eq!(
            cells[nav.tab_rects[4].1.x as usize].style.bg,
            theme.panel(),
            "tab 4 inactive even -> panel"
        );
    }
}

#[test]
fn row1_tab_bar_monochrome_renders_brackets_and_reverse_video() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread("sess-01a2b3c4", Some("Refactor".to_string())),
        crate::shell::surfaces::ClientTab::dashboard(),
    ];
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 0,
        unread_faults: 0,
        ..TabBarProps::default()
    };
    let mut terminal = graticule::TestTerminal::new(80, 1);
    let theme = Theme::monochrome();
    terminal.draw(|frame| {
        draw_tab_bar(frame, frame.area(), &tab_bar, &theme);
    });
    let cells = terminal.buffer().cells();
    let row_text: String = cells.iter().map(|c| c.symbol.as_str()).collect();
    // Resting pills do not reserve blank close columns: brackets hug the title.
    assert!(
        row_text.contains("[Refactor]"),
        "active tab must be framed with brackets on monochrome: {row_text}"
    );
    assert!(
        row_text.contains("[dashboard]"),
        "inactive tab must be framed with brackets on monochrome: {row_text}"
    );

    // Find cells belonging to "[Refactor]" and verify they have REVERSE | BOLD
    let refactor_start = row_text.find("[Refactor]").unwrap();
    for (col, cell) in cells
        .iter()
        .enumerate()
        .skip(refactor_start)
        .take("[Refactor]".len())
    {
        assert!(
            cell.style.add.contains(graticule::Modifier::REVERSE),
            "active tab cell at col {col} must have REVERSE video"
        );
        assert!(
            cell.style.add.contains(graticule::Modifier::BOLD),
            "active tab cell at col {col} must have BOLD"
        );
    }
}

#[test]
fn row1_renders_brand_when_no_tabs() {
    let tab_bar = TabBarProps::default();
    let row = render_row1(80, tab_bar);
    assert!(row.contains("nuo"), "brand shown: {row}");
    assert!(
        !row.contains("F1"),
        "unbound help_key renders no fabricated keycap on the brand row (ADR-0245): {row}"
    );
}

#[test]
fn adaptive_tab_title_budget_scales_with_tab_count() {
    assert_eq!(adaptive_tab_title_width(1), 20);
    assert_eq!(adaptive_tab_title_width(3), 20);
    assert_eq!(adaptive_tab_title_width(4), 14);
    assert_eq!(adaptive_tab_title_width(6), 14);
    assert_eq!(adaptive_tab_title_width(7), 10);
    assert_eq!(adaptive_tab_title_width(15), 10);
}

#[test]
fn compute_tab_window_anchors_active_and_computes_overflow() {
    let widths = vec![8; 10];

    // 1. All fit if available width is large
    let (s, e) = compute_tab_window(&widths, 0, 120);
    assert_eq!((s, e), (0, 9));

    // 2. Active at 0 with narrow width: start stays at 0, active is visible
    let (s, e) = compute_tab_window(&widths, 0, 35);
    assert_eq!(s, 0);
    assert!(e < 9);
    assert!(e >= 1);

    // 3. Active in middle (tab 5): active is guaranteed in [s..=e]
    let (s, e) = compute_tab_window(&widths, 5, 35);
    assert!(s <= 5 && 5 <= e, "active tab 5 must be visible: {s}..={e}");
    assert!(s > 0, "should have left overflow: {s}");
    assert!(e < 9, "should have right overflow: {e}");

    // 4. Active at end (tab 9): end reaches 9, active is visible
    let (s, e) = compute_tab_window(&widths, 9, 35);
    assert_eq!(e, 9);
    assert!(s <= 9);
    assert!(s > 0, "should have left overflow: {s}");
}

#[test]
fn row1_renders_overflow_indicators_when_tabs_exceed_available_width() {
    let tabs: Vec<_> = (0..10)
        .map(|i| {
            crate::shell::surfaces::ClientTab::thread(
                format!("session-{i}"),
                Some(format!("Task {i}")),
            )
        })
        .collect();
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 5,
        unread_faults: 0,
        ..TabBarProps::default()
    };
    let row = render_row1(60, tab_bar);
    assert!(
        row.contains('‹'),
        "must display left overflow indicator: {row}"
    );
    assert!(
        row.contains('›'),
        "must display right overflow indicator: {row}"
    );
    assert!(
        row.contains("[▾]"),
        "must display overview switcher button: {row}"
    );
    assert!(
        row.contains("Task 5"),
        "active tab 5 must be rendered: {row}"
    );
    assert!(
        !row.contains("F1"),
        "unbound help_key renders no fabricated keycap on overflow rows (ADR-0245): {row}"
    );
}

#[test]
fn row1_overflow_rects_mounted_for_hit_testing() {
    // Ten tabs at 50 columns overflow on both sides of the active tab once the
    // status cluster and the legend gap have taken their columns.
    let tabs: Vec<_> = (0..10)
        .map(|i| {
            crate::shell::surfaces::ClientTab::thread(
                format!("session-{i}"),
                Some(format!("Tab {i}")),
            )
        })
        .collect();
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 4,
        unread_faults: 0,
        ..TabBarProps::default()
    };
    let mut terminal = graticule::TestTerminal::new(50, 1);
    let theme = Theme::default();
    let mut nav = TabBarNavRects::default();
    terminal.draw(|frame| {
        nav = draw_tab_bar(frame, frame.area(), &tab_bar, &theme);
    });

    assert!(
        nav.prev_rect.is_some(),
        "must have prev_rect for left indicator"
    );
    assert_eq!(
        nav.prev_rect.unwrap().x,
        1,
        "prev indicator starts at column 1"
    );
    assert!(
        nav.next_rect.is_some(),
        "must have next_rect for right indicator"
    );
    assert!(
        nav.overview_rect.is_some(),
        "must have overview_rect for switcher button"
    );
    assert!(
        nav.tab_rects.iter().any(|(idx, _)| *idx == 4),
        "active tab 4 must have rect"
    );
}

// ---- ADR-0235: tab lifecycle affordances (hover close, add, hover reveal) ----

fn lifecycle_tabs() -> Vec<crate::shell::surfaces::ClientTab> {
    vec![
        crate::shell::surfaces::ClientTab::thread(
            "sess-01a2b3c4",
            Some("Refactor Database Connection"),
        ),
        crate::shell::surfaces::ClientTab::dashboard(),
        crate::shell::surfaces::ClientTab::settings(),
    ]
}

/// A pill only wears its `✕` while the pointer is on it, and the mark is the
/// *last* column it owns — the pill's width does not change, so the strip cannot
/// reflow under the pointer ([INV-TAB-16]).
#[test]
fn row1_hovered_pill_reveals_its_close_mark_without_growing() {
    let tabs = lifecycle_tabs();
    let hovered = |hover| TabBarProps {
        tabs: Some(&tabs),
        active_tab: 0,
        unread_faults: 0,
        hover,
        hover_elapsed_ms: 0,
        ..TabBarProps::default()
    };
    let mut plain = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        plain = draw_tab_bar(frame, frame.area(), &hovered(None), &Theme::default());
    });
    assert!(plain.close_rects.is_empty(), "no pointer, no close mark");
    let rest_row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(!rest_row.contains('✕'), "{rest_row:?}");

    let mut hover = TabBarNavRects::default();
    terminal.draw(|frame| {
        hover = draw_tab_bar(
            frame,
            frame.area(),
            &hovered(Some(TabBarHover::Tab(0))),
            &Theme::default(),
        );
    });
    let hover_row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(
        hover_row.contains('✕'),
        "hovered pill shows its mark: {hover_row:?}"
    );
    assert_eq!(
        hover.tab_rects[0].1, plain.tab_rects[0].1,
        "the pill keeps its geometry on hover: {hover_row:?}"
    );
    let (index, mark) = hover.close_rects[0];
    assert_eq!(index, 0);
    assert_eq!(
        mark.right(),
        hover.tab_rects[0].1.right() - 1,
        "the mark sits one column in from the pill's trailing edge"
    );
    assert!(
        hover.tab_rects[0].1.contains(mark.x, mark.y),
        "the mark lives inside its own pill"
    );
}

/// Moving from a pill onto its own `✕` is one continuous visit: the close glyph
/// lights up, the title window does not rewind ([INV-TAB-16]).
#[test]
fn row1_close_hover_is_the_same_anchor_as_its_pill() {
    assert_eq!(TabBarHover::Tab(2).anchor(), TabBarHover::Tab(2));
    assert_eq!(TabBarHover::Close(2).anchor(), TabBarHover::Tab(2));
    assert_eq!(TabBarHover::Add.anchor(), TabBarHover::Add);
    assert_ne!(
        TabBarHover::Tab(2).anchor(),
        TabBarHover::Tab(3).anchor(),
        "a different pill is a different visit"
    );
}

/// Close and add are only clickable where they are painted. A hit target with
/// nothing on screen is the phantom click region ADR-0219 deleted.
#[test]
fn row1_lifecycle_hit_rects_exist_only_when_painted() {
    let tabs = lifecycle_tabs();
    let mut nav = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 1,
                unread_faults: 0,
                hover: Some(TabBarHover::Tab(0)),
                hover_elapsed_ms: 0,
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    assert_eq!(
        nav.close_rects.len(),
        1,
        "only the hovered pill carries a close target"
    );
    assert_eq!(nav.close_rects[0].0, 0);
    assert!(
        nav.add_rect.is_some(),
        "the add affordance is always painted on a populated strip"
    );
}

#[test]
fn a_strip_without_columns_paints_no_add_target() {
    let tabs = lifecycle_tabs();
    let mut nav = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(4, 1);
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                unread_faults: 0,
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    assert!(
        nav.add_rect.is_none(),
        "a hit target exists only where its affordance is painted ([INV-TAB-18])"
    );
}

/// The add affordance is *reserved* out of the rail's budget: a wall of long
/// titles truncates itself rather than starving the strip's own creation verb
/// ([INV-TAB-17]).
#[test]
fn row1_add_affordance_survives_a_wall_of_long_titles() {
    let tabs: Vec<_> = (0..12)
        .map(|i| {
            crate::shell::surfaces::ClientTab::thread(
                format!("session-{i}"),
                Some(format!("A very long tab title number {i}")),
            )
        })
        .collect();
    let mut nav = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(44, 1);
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                unread_faults: 0,
                hover: None,
                hover_elapsed_ms: 0,
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(
        row.contains('+'),
        "the creation affordance survives at the minimum width: {row:?}"
    );
    assert!(
        nav.add_rect.expect("add rect").right() <= 43,
        "the add affordance stays inside the row's inset: {row:?}"
    );
}

/// Hovering an overflowing title slides it toward the tail for one bounded pass,
/// and the strip reports that the pass is live so the loop keeps painting
/// ([INV-TAB-15]).
#[test]
fn row1_hover_reveals_an_overflowing_title_for_one_bounded_pass() {
    let tabs = vec![crate::shell::surfaces::ClientTab::thread(
        "sess-01a2b3c4",
        Some("Refactor Database Connection Layer"),
    )];
    let draw = |elapsed_ms: u64| {
        let mut terminal = graticule::TestTerminal::new(44, 1);
        let mut nav = TabBarNavRects::default();
        terminal.draw(|frame| {
            nav = draw_tab_bar(
                frame,
                frame.area(),
                &TabBarProps {
                    tabs: Some(&tabs),
                    active_tab: 0,
                    unread_faults: 0,
                    hover: Some(TabBarHover::Tab(0)),
                    hover_elapsed_ms: elapsed_ms,
                    ..TabBarProps::default()
                },
                &Theme::default(),
            );
        });
        let row: String = terminal
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect();
        (row, nav.reveal_active)
    };

    let (at_arrival, live_at_arrival) = draw(0);
    assert!(
        live_at_arrival,
        "the pass is owed from the moment of arrival"
    );
    assert!(
        at_arrival.contains("Refactor"),
        "the pass opens on the head: {at_arrival:?}"
    );

    assert!(
        !at_arrival.contains('…'),
        "overflow reveal uses edge fade instead of `…`: {at_arrival:?}"
    );

    let (later, live_later) = draw(4_000);
    assert!(
        !live_later,
        "a finished pass stops asking for animation frames"
    );
    assert!(
        !later.contains("Refactor"),
        "the pass ends on the tail the reader waited for: {later:?}"
    );
    assert!(
        later.contains("Layer") && later.contains('✕') && !later.contains('…'),
        "the pill rests on the crisp tail beside its close mark without `…`: {later:?}"
    );
}

/// A title that fits at rest never opens a reveal pass on hover: the close mark
/// replaces its rightmost two columns in place without starting an animation.
#[test]
fn row1_a_title_that_fits_never_opens_a_reveal_pass() {
    let tabs = vec![crate::shell::surfaces::ClientTab::dashboard()];
    let mut nav = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                unread_faults: 0,
                hover: Some(TabBarHover::Tab(0)),
                hover_elapsed_ms: 0,
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(
        row.contains("dashboa ✕"),
        "hover replaces the trailing two title columns with the close mark: {row:?}"
    );
    assert!(!row.contains('…'), "no ellipsis on clipped title: {row:?}");
    assert!(!nav.reveal_active, "a fitting title costs no animation");
}

/// The add affordance is drawn on the empty strip too, so a client with no tabs
/// open can still create one from the row itself.
#[test]
fn row1_brand_row_still_offers_the_add_affordance() {
    let mut nav = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                hover: Some(TabBarHover::Add),
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(row.contains("nuo"), "{row:?}");
    assert!(
        row.contains('+'),
        "creation is reachable with no tabs: {row:?}"
    );
    assert!(nav.add_rect.is_some());
}

/// On the framed glyph set the close mark stays one column and replaces the
/// rightmost title columns in place without growing the pill.
#[test]
fn row1_ascii_close_mark_is_one_column() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread("sess-01a2b3c4", Some("Refactor DB")),
        crate::shell::surfaces::ClientTab::dashboard(),
    ];
    let mut rest_terminal = graticule::TestTerminal::new(80, 1);
    let mut rest_nav = TabBarNavRects::default();
    rest_terminal.draw(|frame| {
        rest_nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                unread_faults: 0,
                hover: None,
                hover_elapsed_ms: 0,
                ..TabBarProps::default()
            },
            &Theme::monochrome(),
        );
    });
    let mut terminal = graticule::TestTerminal::new(80, 1);
    let mut nav = TabBarNavRects::default();
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                unread_faults: 0,
                hover: Some(TabBarHover::Tab(0)),
                hover_elapsed_ms: 0,
                ..TabBarProps::default()
            },
            &Theme::monochrome(),
        );
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(
        row.contains("[Refactor  x]"),
        "the framed close mark degrades to a 1-column `x` replacing the tail in place: {row:?}"
    );
    assert_eq!(
        nav.tab_rects[0].1, rest_nav.tab_rects[0].1,
        "hover replacement keeps pill width unchanged"
    );
    assert_eq!(nav.close_rects.len(), 1);
    assert_eq!(nav.close_rects[0].1.width, 1);
}

fn rgb_dist_sq(a: graticule::Color, b: graticule::Color) -> i32 {
    let (graticule::Color::Rgb(ar, ag, ab), graticule::Color::Rgb(br, bg, bb)) = (a, b) else {
        panic!("expected Rgb colors, got {a:?} and {b:?}");
    };
    let dr = ar as i32 - br as i32;
    let dg = ag as i32 - bg as i32;
    let db = ab as i32 - bb as i32;
    dr * dr + dg * dg + db * db
}

/// Overflowing titles dissolve smoothly toward the pill background over their
/// trailing fade columns instead of terminating in `…`.
#[test]
fn row1_overflowing_title_fades_toward_pill_background_without_ellipsis() {
    let tabs = vec![crate::shell::surfaces::ClientTab::thread(
        "sess-01a2b3c4",
        Some("Refactor Database Connection"),
    )];
    let theme = Theme::default();
    let mut terminal = graticule::TestTerminal::new(80, 1);
    let mut nav = TabBarNavRects::default();
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                unread_faults: 0,
                hover: None,
                hover_elapsed_ms: 0,
                ..TabBarProps::default()
            },
            &theme,
        );
    });
    let cells = terminal.buffer().cells();
    let row: String = cells.iter().map(|c| c.symbol.as_str()).collect();
    assert!(
        !row.contains('…'),
        "no ellipsis on overflowing tab: {row:?}"
    );

    let pill = nav.tab_rects[0].1;
    // Active pill width: 20 title cols + 2 chrome cols = 22 (no resting close reserve).
    assert_eq!(pill.width, 22, "pill spends zero resting columns on close");
    let title_start = pill.x as usize + 1;
    let title_end = pill.right() as usize - 1;
    let bg = theme.selected_bg;
    let base_fg = theme.heading();

    // Unfaded head stays at full foreground color.
    assert_eq!(cells[title_start].style.fg, base_fg);
    assert_eq!(cells[title_end - 5].style.fg, base_fg);

    // The last 4 columns monotonically approach the pill background color.
    let mut prev_dist = rgb_dist_sq(base_fg, bg);
    for (col, cell) in cells.iter().enumerate().take(title_end).skip(title_end - 4) {
        let fg = cell.style.fg;
        let dist = rgb_dist_sq(fg, bg);
        assert!(
            dist < prev_dist,
            "col {col} ({:?}) must fade closer to bg ({bg:?}) than previous step",
            cell.symbol
        );
        prev_dist = dist;
    }
}

/// Hovering a scrolled overflowing title fades only the hidden edge(s): trailing
/// at arrival, both mid-pass, and leading-only once the reveal reaches the tail.
#[test]
fn row1_hover_reveal_fades_hidden_edges_without_ellipsis() {
    let tabs = vec![crate::shell::surfaces::ClientTab::thread(
        "sess-01a2b3c4",
        Some("Refactor Database Connection Layer"),
    )];
    let theme = Theme::default();
    let draw = |elapsed_ms: u64| {
        let mut terminal = graticule::TestTerminal::new(60, 1);
        let mut nav = TabBarNavRects::default();
        terminal.draw(|frame| {
            nav = draw_tab_bar(
                frame,
                frame.area(),
                &TabBarProps {
                    tabs: Some(&tabs),
                    active_tab: 0,
                    unread_faults: 0,
                    hover: Some(TabBarHover::Tab(0)),
                    hover_elapsed_ms: elapsed_ms,
                    ..TabBarProps::default()
                },
                &theme,
            );
        });
        (terminal.buffer().cells().to_vec(), nav)
    };

    let bg = theme.selected_bg;
    let base_fg = theme.heading();

    // Mid-pass: both leading and trailing edges are hidden, so both fade.
    let (mid_cells, mid_nav) = draw(1_000);
    let mid_row: String = mid_cells.iter().map(|c| c.symbol.as_str()).collect();
    assert!(!mid_row.contains('…'), "no ellipsis mid-pass: {mid_row:?}");
    let pill = mid_nav.tab_rects[0].1;
    let title_start = pill.x as usize + 1;
    let title_end = pill.right() as usize - 3; // before " ✕ "
    assert!(
        rgb_dist_sq(mid_cells[title_start].style.fg, bg) < rgb_dist_sq(base_fg, bg),
        "leading edge fades when head is scrolled out"
    );
    assert!(
        rgb_dist_sq(mid_cells[title_end - 1].style.fg, bg) < rgb_dist_sq(base_fg, bg),
        "trailing edge fades when tail is still hidden"
    );
    assert_eq!(
        mid_cells[(title_start + title_end) / 2].style.fg,
        base_fg,
        "center of the title remains crisp"
    );

    // End of pass: tail reached, so leading edge fades while tail stays crisp.
    let (tail_cells, tail_nav) = draw(4_000);
    let tail_row: String = tail_cells.iter().map(|c| c.symbol.as_str()).collect();
    assert!(!tail_row.contains('…'), "no ellipsis at tail: {tail_row:?}");
    assert!(!tail_nav.reveal_active);
    assert!(
        rgb_dist_sq(tail_cells[title_start].style.fg, bg) < rgb_dist_sq(base_fg, bg),
        "leading edge fades at the tail"
    );
    assert_eq!(
        tail_cells[title_end - 1].style.fg,
        base_fg,
        "final visible tail character is crisp once nothing remains hidden on the right"
    );
}

/// Wide CJK titles slice cleanly on grapheme boundaries, never emit `…`, and
/// fade their trailing characters toward the pill background.
#[test]
fn row1_cjk_overflowing_title_fades_without_splitting_or_ellipsis() {
    let tabs = vec![crate::shell::surfaces::ClientTab::thread(
        "sess-01a2b3c4",
        Some("数据库连接池重构优化方案设计与验证"),
    )];
    let theme = Theme::default();
    let mut terminal = graticule::TestTerminal::new(80, 1);
    let mut nav = TabBarNavRects::default();
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                unread_faults: 0,
                hover: None,
                hover_elapsed_ms: 0,
                ..TabBarProps::default()
            },
            &theme,
        );
    });
    let cells = terminal.buffer().cells();
    let row: String = cells.iter().map(|c| c.symbol.as_str()).collect();
    let compact: String = cells
        .iter()
        .map(|c| c.symbol.as_str())
        .filter(|s| *s != " ")
        .collect();
    assert!(!row.contains('…'), "no ellipsis on CJK title: {row:?}");
    assert!(
        compact.contains("数据库连接池重构优化"),
        "CJK head intact without splitting graphemes: {row:?}"
    );

    let pill = nav.tab_rects[0].1;
    assert_eq!(pill.width, 22);
    let title_start = pill.x as usize + 1;
    let last_cjk_lead_col = pill.right() as usize - 3; // width-2 char occupies -3..-1
    assert_eq!(cells[title_start].style.fg, theme.heading());
    assert!(
        rgb_dist_sq(cells[last_cjk_lead_col].style.fg, theme.selected_bg)
            < rgb_dist_sq(theme.heading(), theme.selected_bg),
        "trailing CJK grapheme fades toward the pill background"
    );
}

// ---- ADR-0245: yield-first elastic help portal ([INV-TAB-19], [INV-TAB-20]) ----

/// When all tabs fit with idle slack covering `BAR_LEGEND_GAP_MIN`, the TabBar
/// paints the expanded `{key} help` portal at the right inset without causing
/// any tab overflow ([INV-TAB-19]).
#[test]
fn row1_help_portal_expands_in_idle_slack_and_never_triggers_overflow() {
    let tabs = vec![
        crate::shell::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
        crate::shell::surfaces::ClientTab::dashboard(),
    ];
    let mut nav = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                unread_faults: 0,
                help_key: Some(crate::shell::keymap::Key::F1),
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(row.contains("F1 help"), "expanded portal shown: {row:?}");
    assert!(
        nav.prev_rect.is_none() && nav.next_rect.is_none(),
        "expanded portal must never trigger tab overflow: {row:?}"
    );
    let help = nav.help_rect.expect("help hit rect painted");
    assert_eq!(help.right(), 79, "pinned to right inset: {row:?}");
    assert_eq!(help.width, "F1 help".len() as u16);
    let painted: String = (help.x..help.right())
        .map(|x| terminal.buffer().cells()[x as usize].symbol.as_str())
        .collect();
    assert_eq!(painted, "F1 help");

    // Brand row (no tabs) also expands cleanly.
    let mut brand_nav = TabBarNavRects::default();
    terminal.draw(|frame| {
        brand_nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                help_key: Some(crate::shell::keymap::Key::F1),
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let brand_row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(brand_row.contains("nuo") && brand_row.contains("F1 help"));
    assert_eq!(brand_nav.help_rect.expect("brand help rect").right(), 79);
}

/// Under tab-count or width pressure, the help portal shrinks from `{key} help`
/// to compact `{key}`, sharing the right cluster with `[!N]` cleanly while
/// preserving `[!N]`'s `BAR_LEGEND_GAP_MIN` ([INV-TAB-19]).
#[test]
fn row1_help_portal_shrinks_to_compact_under_tab_pressure() {
    let tabs: Vec<_> = (0..10)
        .map(|i| {
            crate::shell::surfaces::ClientTab::thread(
                format!("session-{i}"),
                Some(format!("Task {i}")),
            )
        })
        .collect();

    // 1. Overflowing tabs without unread faults: compact `F1` separated by TAB_HELP_GAP_MIN.
    let mut nav = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(60, 1);
    terminal.draw(|frame| {
        nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 5,
                unread_faults: 0,
                help_key: Some(crate::shell::keymap::Key::F1),
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(
        row.contains("F1") && !row.contains("F1 help"),
        "overflowing strip shrinks portal to compact key: {row:?}"
    );
    let help = nav.help_rect.expect("compact help rect");
    assert_eq!(help.width, 2);
    assert_eq!(help.right(), 59);
    let add = nav.add_rect.expect("add rect survives");
    assert!(
        help.x >= add.right() + ui_primitives::design::TAB_HELP_GAP_MIN as u16,
        "compact portal keeps TAB_HELP_GAP_MIN after rail: {row:?}"
    );

    // 2. Minimum terminal width (44) with unread faults + overflowing tabs:
    // `[!2]  F1` both fit, `[!2]` keeps BAR_LEGEND_GAP_MIN before it.
    let mut min_nav = TabBarNavRects::default();
    let mut min_term = graticule::TestTerminal::new(44, 1);
    min_term.draw(|frame| {
        min_nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 5,
                unread_faults: 2,
                help_key: Some(crate::shell::keymap::Key::F1),
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let min_row: String = min_term
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    let notices = min_nav.notices_rect.expect("fault badge survives at 44");
    let min_help = min_nav.help_rect.expect("compact F1 survives at 44");
    assert_eq!(min_help.right(), 43);
    assert_eq!(
        notices.right() + JOIN_ENUMERATE_COLS as u16,
        min_help.x,
        "fault badge and compact help separate by JOIN_ENUMERATE_COLS: {min_row:?}"
    );
    let badge_at = min_row.find("[!2]").expect("fault badge painted");
    assert!(
        min_row[..badge_at].ends_with(&" ".repeat(BAR_LEGEND_GAP_MIN)),
        "fault badge keeps BAR_LEGEND_GAP_MIN before it: {min_row:?}"
    );
}

/// Remapping `OpenHelp` updates the portal text; unbinding it omits the portal;
/// starving the strip drops the portal before starving `[!N]`, the active tab,
/// or `+` ([INV-TAB-19], [INV-AFFORDANCE-08]).
#[test]
fn row1_help_portal_yields_when_unbound_or_starved_and_follows_remaps() {
    let tabs: Vec<_> = (0..10)
        .map(|i| {
            crate::shell::surfaces::ClientTab::thread(
                format!("session-{i}"),
                Some(format!("A very long tab title number {i}")),
            )
        })
        .collect();

    // Remapped key renders its resolved display string.
    let mut remapped_nav = TabBarNavRects::default();
    let mut terminal = graticule::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        remapped_nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs[..1]),
                active_tab: 0,
                unread_faults: 0,
                help_key: Some(crate::shell::keymap::Key::ALT_H),
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let remapped_row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(
        remapped_row.contains("Alt-h help") && !remapped_row.contains("F1"),
        "portal follows remapped chord: {remapped_row:?}"
    );
    assert_eq!(
        remapped_nav.help_rect.expect("remapped help rect").width,
        "Alt-h help".len() as u16
    );

    // Unbound key paints nothing and mounts no hit rect.
    let mut unbound_nav = TabBarNavRects::default();
    terminal.draw(|frame| {
        unbound_nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs[..1]),
                active_tab: 0,
                unread_faults: 0,
                help_key: None,
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    assert!(unbound_nav.help_rect.is_none());

    // Starved at 44 cols with faults + long remapped chord: portal omits itself
    // so `[!2]`, the active tab, and `+` stay intact.
    let mut starved_nav = TabBarNavRects::default();
    let mut narrow = graticule::TestTerminal::new(44, 1);
    narrow.draw(|frame| {
        starved_nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 5,
                unread_faults: 2,
                help_key: Some(crate::shell::keymap::Key::CTRL_SHIFT_P),
                ..TabBarProps::default()
            },
            &Theme::default(),
        );
    });
    let starved_row: String = narrow
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();
    assert!(
        starved_nav.help_rect.is_none() && !starved_row.contains("Ctrl-Shift-p"),
        "starved row yields the help portal cleanly: {starved_row:?}"
    );
    assert!(
        starved_nav.notices_rect.is_some() && starved_nav.add_rect.is_some(),
        "fault badge and add affordance outrank the help portal: {starved_row:?}"
    );
}

/// Hovering the help portal lifts its background onto `theme.raised()` without
/// changing its geometry, and its hit rect exists if and only if painted
/// ([INV-TAB-20]).
#[test]
fn row1_help_portal_hit_rect_matches_painted_state() {
    let tabs = lifecycle_tabs();
    let theme = Theme::default();

    let mut rest_nav = TabBarNavRects::default();
    let mut rest_term = graticule::TestTerminal::new(80, 1);
    rest_term.draw(|frame| {
        rest_nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                help_key: Some(crate::shell::keymap::Key::F1),
                hover: None,
                ..TabBarProps::default()
            },
            &theme,
        );
    });

    let mut hover_nav = TabBarNavRects::default();
    let mut hover_term = graticule::TestTerminal::new(80, 1);
    hover_term.draw(|frame| {
        hover_nav = draw_tab_bar(
            frame,
            frame.area(),
            &TabBarProps {
                tabs: Some(&tabs),
                active_tab: 0,
                help_key: Some(crate::shell::keymap::Key::F1),
                hover: Some(TabBarHover::Help),
                ..TabBarProps::default()
            },
            &theme,
        );
    });

    let rest_rect = rest_nav.help_rect.expect("help rect at rest");
    let hover_rect = hover_nav.help_rect.expect("help rect on hover");
    assert_eq!(rest_rect, hover_rect, "hover must never reflow the portal");
    assert_eq!(
        rest_term.buffer().cells()[rest_rect.x as usize].style.bg,
        theme.tabbar_bg()
    );
    assert_eq!(
        hover_term.buffer().cells()[hover_rect.x as usize].style.bg,
        theme.raised()
    );
    assert_eq!(TabBarHover::Help.anchor(), TabBarHover::Help);
}
