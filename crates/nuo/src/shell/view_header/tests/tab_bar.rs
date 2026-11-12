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
        row.contains("F1 help"),
        "compact help affordance on TabBar: {row}"
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
        };
        let mut terminal = nuotc::TestTerminal::new(width, 1);
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
        assert_eq!(
            nav.help_rect.expect("help affordance").right(),
            width - 1,
            "the status cluster ends 1 column before the terminal edge: {row:?}"
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
    };
    let row = render_row1(80, tab_bar);
    assert!(
        row.starts_with("  thread#b3c4"),
        "leading label keeps 1-col edge margin and internal padding: {row:?}"
    );
}

/// The right-pinned status cluster keeps the standing legend gap, so a
/// truncated overflow button can never butt against the `F1 help` keycap.
#[test]
fn row1_status_cluster_keeps_the_legend_gap_when_tabs_overflow() {
    let tabs: Vec<_> = (0..8)
        .map(|i| crate::shell::surfaces::ClientTab::thread(format!("session-{i}"), None::<String>))
        .collect();
    let tab_bar = TabBarProps {
        tabs: Some(&tabs),
        active_tab: 7,
        unread_faults: 0,
    };
    let row = render_row1(60, tab_bar);
    let help_at = row.find("F1 help").expect("help affordance present");
    let before = &row[..help_at];
    assert!(
        before.ends_with(&" ".repeat(BAR_LEGEND_GAP_MIN)),
        "at least {BAR_LEGEND_GAP_MIN} columns before the keycap: {row:?}"
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
    };
    let row = render_row1(80, tab_bar);
    assert!(
        row.contains("Refactor Database C…"),
        "semantic title projected with wider budget: {row}"
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
    };
    let mut terminal = nuotc::TestTerminal::new(80, 1);
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
        };
        let mut terminal = nuotc::TestTerminal::new(100, 1);
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
        assert_eq!(
            nav.help_rect.expect("help").right(),
            99,
            "help affordance ends 1 col before terminal right edge"
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
        };
        let mut terminal = nuotc::TestTerminal::new(100, 1);
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
    };
    let mut terminal = nuotc::TestTerminal::new(80, 1);
    let theme = Theme::monochrome();
    terminal.draw(|frame| {
        draw_tab_bar(frame, frame.area(), &tab_bar, &theme);
    });
    let cells = terminal.buffer().cells();
    let row_text: String = cells.iter().map(|c| c.symbol.as_str()).collect();
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
            cell.style.add.contains(nuotc::Modifier::REVERSE),
            "active tab cell at col {col} must have REVERSE video"
        );
        assert!(
            cell.style.add.contains(nuotc::Modifier::BOLD),
            "active tab cell at col {col} must have BOLD"
        );
    }
}

#[test]
fn row1_renders_brand_when_no_tabs() {
    let tab_bar = TabBarProps::default();
    let row = render_row1(80, tab_bar);
    assert!(row.contains("nuo"), "brand shown: {row}");
    assert!(row.contains("F1 help"), "client menu shown: {row}");
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
    assert!(row.contains("F1 help"), "client menu preserved: {row}");
}

#[test]
fn row1_overflow_rects_mounted_for_hit_testing() {
    let tabs: Vec<_> = (0..8)
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
    };
    let mut terminal = nuotc::TestTerminal::new(50, 1);
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
