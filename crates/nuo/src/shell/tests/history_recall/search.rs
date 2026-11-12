//! History listing, search ranking, and recency ordering.

use super::*;

#[test]
fn history_rows_lists_newest_first_then_ranks_search() {
    // The App-level view of the Ctrl+R panel. With no query the whole
    // cross-session history is listed newest-first (by created_at_ms),
    // unhighlighted; once the user types, only the fuzzy subsequence matches
    // surface, ordered by score with newest-first order as the stable
    // tiebreaker.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let mk = |text: &str, sid: &str, ts: u64| {
        nuo_protocol::HistoryEntry::new(
            text.to_string(),
            Some(sid.to_string()),
            Some("~/p".to_string()),
            ts,
        )
    };
    app.input_history = vec![
        mk("scatter", "s1", 10),     // idx 0 — 'cat' mid-word, lowest score
        mk("catalog", "s1", 20),     // idx 1 — 'cat' at boundary, high score
        mk("cargo build", "s1", 30), // idx 2 — 'cat' is not a subsequence
        mk("the cat sat", "s1", 40), // idx 3 — 'cat' at boundary, high score
    ];

    // Empty query → newest-first by timestamp, score 0, no highlights.
    app.input.clear();
    let rows = app.history_rows();
    let indices: Vec<usize> = rows.iter().map(|(i, _)| *i).collect();
    assert_eq!(indices, vec![3, 2, 1, 0], "newest first by timestamp");
    for (_, m) in &rows {
        assert_eq!(m.score, 0);
        assert!(m.positions.is_empty());
    }

    // Search "cat" → matches catalog, "the cat sat", and scatter; not
    // "cargo build" (no 't' after the 'ca'). Boundary matches outrank
    // scatter; among the tied boundary matches the newest-first order wins
    // (idx 3 "the cat sat" ts=40 before idx 1 "catalog" ts=20).
    app.input = "cat".to_string();
    let rows = app.history_rows();
    let indices: Vec<usize> = rows.iter().map(|(i, _)| *i).collect();
    assert_eq!(
        indices,
        vec![3, 1, 0],
        "boundary matches first (newest-first on ties), then scatter"
    );
    assert!(rows[0].1.score > rows[2].1.score);
    for (_, m) in &rows {
        assert_eq!(m.positions.len(), 3);
    }

    // Query with no subsequence match → empty list (the renderer turns this
    // into the "no matches" placeholder).
    app.input = "xyz".to_string();
    assert!(app.history_rows().is_empty());
}

#[test]
fn history_modal_never_borrows_the_draft_and_resets_its_own_query() {
    // `[INV-HIST-03]`: draft is stashed on open and restored on dismiss.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "my draft".to_string();
    app.cursor_position = "my draft".chars().count();
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(app.input, "", "composer input cleared for search query");
    assert_eq!(
        app.draft_capsule.text, "my draft",
        "composer draft stashed in capsule"
    );
    app.input = "git".to_string();
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
        .index = 4;

    assert!(app.dismiss_surface());

    assert_eq!(app.input, "my draft", "composer draft restored");
    assert_eq!(app.cursor_position, "my draft".chars().count());
    assert!(app.surfaces.active_overlay().is_none());
}

#[test]
fn history_insert_clears_search_query_buffer_and_places_entry() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input_history.push(nuo_protocol::HistoryEntry::new(
        "history row 1".to_string(),
        Some("s1".to_string()),
        None,
        100,
    ));
    app.input_history.push(nuo_protocol::HistoryEntry::new(
        "history row 2".to_string(),
        Some("s1".to_string()),
        None,
        200,
    ));

    app.input = "draft before search".to_string();
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    app.input = "row".to_string();

    // Simulate HistoryInsert action (Tab / Enter accept)
    let ranked = app.history_rows();
    let pick = ranked.first();
    assert!(pick.is_some());
    let (orig_idx, _) = *pick.unwrap();
    let text = app.input_history[orig_idx].text.clone();
    app.adopt_as_draft(
        text,
        vec![],
        vec![],
        crate::shell::app::DraftAdoption::Replace,
    );
    app.clear_stashed_draft();
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
        .index = 0;
    app.surfaces.dismiss_all_overlays();

    // Composer now holds the selected history entry, not the search query or old draft
    assert_eq!(app.input, "history row 2");
}

#[test]
fn test_history_ranking_prefers_exact_word_over_scattered_and_applies_recency() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "live-session".to_string();

    let hour_ms = 3_600_000;
    let base_time = 1_000_000_000;

    app.input_history = vec![
        // Scattered acronym initials across sentence: "all dogs run in the park" (ts: newest!)
        nuo_protocol::HistoryEntry::new(
            "all dogs run in the park".into(),
            Some("other-session".into()),
            None,
            base_time + 5 * hour_ms,
        ),
        // Exact whole word "adr", but typed 4 hours ago in other session
        nuo_protocol::HistoryEntry::new(
            "let's write the adr".into(),
            Some("other-session".into()),
            None,
            base_time + hour_ms,
        ),
        // Word prefix "adroit approach"
        nuo_protocol::HistoryEntry::new(
            "adroit approach to problems".into(),
            Some("other-session".into()),
            None,
            base_time + 2 * hour_ms,
        ),
        // Exact whole word "adr" typed recently in CURRENT session
        nuo_protocol::HistoryEntry::new(
            "review the adr now".into(),
            Some("live-session".into()),
            None,
            base_time + 5 * hour_ms,
        ),
    ];

    app.input = "adr".to_string();
    let rows = app.history_rows();
    assert_eq!(rows.len(), 4, "all 4 match the subsequence 'adr'");

    // Row 0: "review the adr now" (exact word + current session + newest)
    assert_eq!(app.input_history[rows[0].0].text, "review the adr now");

    // Row 1: "let's write the adr" (exact word, older)
    assert_eq!(app.input_history[rows[1].0].text, "let's write the adr");

    // Row 2: "adroit approach to problems" (word prefix)
    assert_eq!(
        app.input_history[rows[2].0].text,
        "adroit approach to problems"
    );

    // Row 3: "all dogs run in the park" (scattered acronym, MUST be lowest rank despite newer timestamp!)
    assert_eq!(
        app.input_history[rows[3].0].text,
        "all dogs run in the park"
    );

    // Verify exact word scores strictly higher than scattered acronym
    assert!(rows[0].1.score > rows[3].1.score);
    assert!(rows[1].1.score > rows[3].1.score);
}

#[tokio::test]
async fn ctrl_c_clears_history_recall_and_resets_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("prior command".to_string(), Vec::new(), Vec::new());
    let rows = app.current_session_history();

    app.input = "draft text".to_string();
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "prior command");
    assert!(app.history_index.is_some());
    assert_eq!(app.draft_capsule.text, "draft text");

    // Edit the recalled text
    app.input.push_str(" --flag");

    let (copy_tx, _copy_rx) = tokio::sync::mpsc::unbounded_channel();
    let copy_pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Pressing Ctrl-C clears the recalled / edited text and exits recall mode
    crate::shell::event_loop::handle_ctrl_c(&mut app, "session-a", &copy_tx, &copy_pending);

    assert_eq!(app.input, "");
    assert_eq!(app.history_index, None);
    assert_eq!(app.draft_capsule.text, "");
}
