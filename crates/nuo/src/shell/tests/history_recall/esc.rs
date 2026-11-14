//! Esc behavior around recalled history and running rounds.

use super::*;

#[test]
fn esc_preserves_recalled_history_and_interrupts_when_running() {
    use crate::shell::keymap::Key;
    use crate::shell::session::{SceneKeys, resolve_chat_surface_key};

    let mut keys = SceneKeys {
        is_responding: false,
        composer_send_mode: Default::default(),
        completion_kind: crate::shell::completion::CompletionKind::None,
        completion_dismissed: false,
        has_trigger_text: false,
        suggestion_count: 0,
        suggestion_index: None,
        has_exact_suggestion: false,
        in_history_recall: true,
        composer_text_width: 0,
        surface_overrides: Default::default(),
        focus: crate::shell::focus::FocusPlane::Composer,
        entry_focus: None,
        focused_subagent_running: false,
    };

    let mut input = "recalled command with edits".to_string();
    let mut cursor = input.len();

    // Idle session: Esc does NOT clear or cancel recall
    let action = resolve_chat_surface_key(Key::ESC, &keys, &mut input, &mut cursor);
    assert_eq!(action, None, "Esc must not clear or cancel history recall");
    assert_eq!(input, "recalled command with edits");

    // Responding session: Esc resolves to Interrupt rather than clearing recall
    keys.is_responding = true;
    let action = resolve_chat_surface_key(Key::ESC, &keys, &mut input, &mut cursor);
    assert_eq!(
        action,
        Some(crate::shell::input::InputAction::Interrupt),
        "Esc while running must interrupt rather than clear history recall"
    );
    assert_eq!(input, "recalled command with edits");
}

#[tokio::test]
async fn history_search_overlay_does_not_dim_composer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "session-a".to_string();
    app.record_input_history("existing entry".to_string(), Vec::new(), Vec::new());

    // 1. Draw normal frame (no dialog)
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-a");
    });
    // The composer bottom row is within the last rows (e.g. y = 22)
    let normal_cell = terminal.buffer().get(10, 22).cloned().expect("cell");

    // 2. Open HistorySearch
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    let mut terminal_hist = nuotc::TestTerminal::new(80, 24);
    terminal_hist.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-a");
    });
    let hist_cell = terminal_hist.buffer().get(10, 22).cloned().expect("cell");

    // Composer cells must not be dimmed when history search is open
    assert_eq!(
        hist_cell.style.bg, normal_cell.style.bg,
        "composer background should not be dimmed when history search is open"
    );
}

#[tokio::test]
async fn test_history_search_keeps_composer_active_and_shows_hints() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "session-a".to_string();
    app.composer.input = "live draft text".to_string();
    app.composer.cursor_position = "live draft text".chars().count();
    app.record_input_history("existing entry".to_string(), Vec::new(), Vec::new());

    // Enter HistorySearch (simulating Ctrl+R)
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch)
    );
    assert_eq!(
        app.active_composer_extension(),
        Some(crate::shell::composer_extension::ComposerExtensionKind::HistorySearch)
    );

    let mut terminal = nuotc::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-a");
    });

    let buffer = terminal.buffer();
    let screen = (0..buffer.area().height)
        .map(|y| {
            (0..buffer.area().width)
                .map(|x| buffer.get(x, y).unwrap().symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    // The composer must NOT be marked as handed over to the transcript: the
    // history-search extension keeps the composer plane live (ADR-0145).
    assert!(
        !screen.contains("Shift-Tab composer"),
        "the composer plane must stay live in history search: {screen}"
    );

    // Composer top row must show history search badge
    assert!(
        screen.contains("[history search | draft saved]"),
        "top row must display history search badge: {screen}"
    );

    // Composer hint row must show history search action hints
    assert!(
        screen.contains("close") && screen.contains("insert"),
        "hint row must display history hints: {screen}"
    );
}

#[tokio::test]
async fn oldest_history_entry_up_arrow_does_not_cycle_cursor() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "session-a".to_string();
    app.session.current_workspace = "~/p".to_string();
    app.record_input_history(
        "multiline\nentry\nbottom".to_string(),
        Vec::new(),
        Vec::new(),
    );
    let rows = app.current_session_history();
    assert_eq!(rows.len(), 1);

    // Initial draft
    app.composer.input = "".to_string();
    assert!(app.history_prev(&rows));
    assert_eq!(app.composer.input, "multiline\nentry\nbottom");
    assert_eq!(
        app.composer.cursor_position,
        "multiline\nentry\nbottom".chars().count()
    );

    // Walk the caret up row-by-row over the composer's visual grid.
    let grid = crate::shell::composer_grid::TextGrid::unwrapped(&app.composer.input);
    let mut motion = crate::shell::composer_grid::CaretMotion::default();
    for _ in 0..2 {
        let target = grid
            .shift_row(
                app.composer.cursor_position,
                crate::shell::composer_grid::Vertical::Up,
                &mut motion,
            )
            .expect("multi-line draft has a row above");
        app.composer.cursor_position = target;
    }
    // Cursor is now on the top line, in the same display column it held on
    // the bottom one (the sticky column survives the shorter middle row).
    let top_line_cursor = app.composer.cursor_position;
    assert_eq!(
        top_line_cursor, 6,
        "two rows up from the bottom is the top line, column preserved"
    );
    assert_eq!(
        grid.shift_row(
            app.composer.cursor_position,
            crate::shell::composer_grid::Vertical::Up,
            &mut motion
        ),
        None,
        "the top row has no row above it"
    );

    // Pressing Up at the top line tries history_prev: already at oldest entry, must be a no-op!
    assert!(!app.history_prev(&rows));
    // Cursor position and text must NOT change (must not reset cursor to end of message)
    assert_eq!(app.composer.cursor_position, top_line_cursor);
    assert_eq!(app.composer.input, "multiline\nentry\nbottom");
    assert_eq!(app.composer.history_index, Some(0));
}

#[tokio::test]
async fn history_rows_scales_to_100k_entries_without_lag() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "session-scale".to_string();

    let count = 100_000;
    app.composer.input_history.reserve(count);
    for i in 0..count {
        let text = if i % 1000 == 0 {
            format!("git commit -m 'release {i}'")
        } else if i % 200 == 0 {
            format!("cargo test package_{i}")
        } else {
            format!("random prompt message line number {i}")
        };
        app.composer
            .input_history
            .push(nuo_protocol::HistoryEntry::new(
                text,
                Some("session-scale".to_string()),
                Some("~/work".to_string()),
                i as u64,
            ));
    }

    assert_eq!(app.composer.input_history.len(), count);

    // 1. Empty query returns all 100,000 items newest-first
    let t0 = std::time::Instant::now();
    app.composer.input.clear();
    let rows_empty = app.history_rows();
    let d0 = t0.elapsed();
    assert_eq!(rows_empty.len(), count);
    assert!(
        d0.as_millis() < 500,
        "empty query on 100k items took {:?}, expected < 500ms in debug mode",
        d0
    );

    // 2. Filtered search with pre-filter pruning
    let t1 = std::time::Instant::now();
    app.composer.input = "gcm".to_string();
    let rows_filtered = app.history_rows();
    let d1 = t1.elapsed();
    // 100 matches of "git commit -m 'release {i}'"
    assert_eq!(rows_filtered.len(), 100);
    assert!(
        d1.as_millis() < 250,
        "search query on 100k items took {:?}, expected < 250ms in debug mode",
        d1
    );

    // Verified results are ranked with highest scores first
    for w in rows_filtered.windows(2) {
        assert!(w[0].1.score >= w[1].1.score);
    }
}
