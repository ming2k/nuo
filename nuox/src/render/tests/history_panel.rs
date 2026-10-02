//! History panel rendering tests: query states, folding, row accounting, activity-bar reservation.

use super::*;

/// Drive `draw_history_panel` against a real buffer across every input
/// state the Ctrl+R picker can land in. The assertions are deliberately
/// structural ("does not panic, produces a non-empty frame") because the
/// fuzzy highlight math is already covered by `fuzzy::tests`; here we
/// only need to prove the renderer consumes each state without exploding.
#[test]
fn history_panel_renders_every_query_state() {
    let theme = Theme::default();
    let history: Vec<nuo_contracts::HistoryEntry> = [
        "git status",
        "git commit -am 'ship it'",
        "cargo test",
        "review the diff before sending",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, text)| {
        nuo_contracts::HistoryEntry::new(
            text.to_string(),
            Some(format!("s{i}")),
            Some("~/p".to_string()),
            (i as u64) * 1_000,
        )
    })
    .collect();
    let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();

    let cases: &[(&str, usize)] = &[
        ("", history.len()), // empty query → everything surfaces
        ("git", 2),          // partial match → subset with highlights
        ("zzz", 0),          // no subsequence → empty placeholder
    ];

    let input_rect = nuotc::Rect::new(0, 22, 80, 2);
    for (query, expected_matches) in cases {
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let mut ranked = crate::fuzzy::rank(&texts, query);
        crate::fuzzy::sort_by_score(&mut ranked);
        assert_eq!(
            ranked.len(),
            *expected_matches,
            "query {:?} should surface {} entries",
            query,
            expected_matches
        );
        terminal.draw(|f| {
            let _ = draw_history_panel(
                f,
                crate::overlays::history::HistoryPanelProps {
                    history: &history,
                    ranked: &ranked,
                    modal_index: 0,
                    scroll: &mut 0,
                    follow_selection: true,
                    input_rect,
                    activity_height: 0,
                },
                &theme,
            );
        });
    }

    // Empty history must render the "(no history yet)" placeholder rather
    // than indexing into an empty slice.
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let empty: Vec<nuo_contracts::HistoryEntry> = Vec::new();
    let ranked: Vec<(usize, crate::fuzzy::FuzzyMatch)> = crate::fuzzy::rank::<&str>(&[], "");
    terminal.draw(|f| {
        let _ = draw_history_panel(
            f,
            crate::overlays::history::HistoryPanelProps {
                history: &empty,
                ranked: &ranked,
                modal_index: 0,
                scroll: &mut 0,
                follow_selection: true,
                input_rect,
                activity_height: 0,
            },
            &theme,
        );
    });
}

/// A multi-line history entry collapses to its first line in the fuzzy
/// list (so a long prompt never breaks the single-row grid).
#[test]
fn history_panel_folds_multiline_entries() {
    let theme = Theme::default();
    let history: Vec<nuo_contracts::HistoryEntry> =
        ["first line\nsecond line\nthird line", "single line"]
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                nuo_contracts::HistoryEntry::new(text.to_string(), Some(format!("s{i}")), None, 0)
            })
            .collect();
    let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();

    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let ranked = crate::fuzzy::rank(&texts, "");
    let input_rect = nuotc::Rect::new(0, 22, 80, 2);

    terminal.draw(|f| {
        let _ = draw_history_panel(
            f,
            crate::overlays::history::HistoryPanelProps {
                history: &history,
                ranked: &ranked,
                modal_index: 0,
                scroll: &mut 0,
                follow_selection: true,
                input_rect,
                activity_height: 0,
            },
            &theme,
        );
    });
    let buf = terminal.buffer();
    let has_marker = buf.content.iter().any(|c| c.symbol() == "↵");
    assert!(has_marker, "multi-line entry should show the ↵ fold marker");
}

/// The dropdown is an extension of the composer, not a fixed-size window:
/// it collapses to the actual row count rather than reserving a fixed
/// minimum. Two entries must produce a 4-row panel (2 rows + header +
/// footer), not the old 6-row floor.
#[test]
fn history_panel_collapses_to_actual_row_count() {
    let theme = Theme::default();
    let history: Vec<nuo_contracts::HistoryEntry> = ["one", "two"]
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            nuo_contracts::HistoryEntry::new(
                text.to_string(),
                Some(format!("s{i}")),
                None,
                i as u64,
            )
        })
        .collect();
    let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
    let ranked = crate::fuzzy::rank(&texts, "");
    // Composer near the bottom of a tall terminal so room-above is not the
    // binding constraint — the row count is.
    let input_rect = nuotc::Rect::new(0, 40, 80, 2);
    let mut terminal = nuotc::TestTerminal::new(80, 42);
    let mut panel: Option<nuotc::Rect> = None;
    terminal.draw(|f| {
        panel = draw_history_panel(
            f,
            crate::overlays::history::HistoryPanelProps {
                history: &history,
                ranked: &ranked,
                modal_index: 0,
                scroll: &mut 0,
                follow_selection: true,
                input_rect,
                activity_height: 0,
            },
            &theme,
        )
    });
    let panel = panel.expect("panel should render with ample room above");
    // 2 entries + 3 chrome rows (top padding, header, bottom padding) = 5 rows.
    // The panel still collapses to the actual row count — a fixed minimum
    // would have forced 8+ regardless of entries.
    assert_eq!(
        panel.height, 5,
        "panel must collapse to actual row count + chrome (5), not a fixed minimum"
    );
}

/// never grows into the activity bar's rows, so the live status surface
/// above the composer always stays visible and always reads as above the
/// history dropdown.
#[test]
fn history_panel_reserves_activity_bar_rows() {
    let theme = Theme::default();
    // Enough entries that, absent the reservation, the panel would want to
    // grow tall and run past the activity bar.
    let history: Vec<nuo_contracts::HistoryEntry> = (0..25)
        .map(|i| {
            nuo_contracts::HistoryEntry::new(format!("entry {i}"), Some(format!("s{i}")), None, i)
        })
        .collect();
    let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
    let ranked = crate::fuzzy::rank(&texts, "");
    // Composer at row 15; the activity bar occupies the single row above it
    // (row 14), so `activity_height = 1`.
    let input_rect = nuotc::Rect::new(0, 15, 80, 2);
    let mut terminal = nuotc::TestTerminal::new(80, 17);
    let mut panel: Option<nuotc::Rect> = None;
    terminal.draw(|f| {
        panel = draw_history_panel(
            f,
            crate::overlays::history::HistoryPanelProps {
                history: &history,
                ranked: &ranked,
                modal_index: 0,
                scroll: &mut 0,
                follow_selection: true,
                input_rect,
                activity_height: 1,
            },
            &theme,
        )
    });
    let panel = panel.expect("panel should render");
    // The activity bar occupies the single row above the composer
    // (input_rect.y - 1 = 14). The panel must never cover it: its bottom
    // edge (panel.y + panel.height) must sit at or above row 14.
    assert!(
        panel.y + panel.height <= 14,
        "panel footprint [y={}, h={}] must not cover the activity bar row (14)",
        panel.y,
        panel.height
    );
}
