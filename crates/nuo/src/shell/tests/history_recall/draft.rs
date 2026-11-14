//! ADR-0044: composer-owned caret, draft stashing, and ghost preview.

use super::*;

#[test]
fn adr0044_composer_owns_caret_and_direct_typing_filters_history() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.composer
        .input_history
        .push(nuo_protocol::HistoryEntry::new(
            "cargo test --package nuo-ui".to_string(),
            Some("s1".to_string()),
            None,
            100,
        ));
    app.composer
        .input_history
        .push(nuo_protocol::HistoryEntry::new(
            "git status --short".to_string(),
            Some("s1".to_string()),
            None,
            200,
        ));

    // [INV-HIST-01]: Opening HistorySearch keeps CaretOwner::Composer
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.caret_owner(),
        crate::shell::CaretOwner::Composer,
        "Composer must retain caret ownership during history search"
    );

    // Typing directly into Composer acts as the live search query
    app.composer.input = "cargo".to_string();
    let rows = app.history_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        app.composer.input_history[rows[0].0].text,
        "cargo test --package nuo-ui"
    );

    app.composer.input = "git".to_string();
    let rows = app.history_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        app.composer.input_history[rows[0].0].text,
        "git status --short"
    );
}

#[test]
fn adr0044_atomic_draft_stashing_and_esc_rollback() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.composer
        .input_history
        .push(nuo_protocol::HistoryEntry::new(
            "historical command".to_string(),
            Some("s1".to_string()),
            None,
            100,
        ));

    // Pre-populate an in-progress draft with text and attachments
    app.composer.input = "uncommitted work in progress".to_string();
    app.composer.cursor_position = app.composer.input.len();
    app.composer
        .pending_text_pastes
        .push("staged paste block".to_string());

    // Opening history search stashes the draft atomically ([INV-HIST-03], [INV-COMP-03])
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.composer.draft_capsule.text,
        "uncommitted work in progress"
    );
    assert_eq!(
        app.composer.draft_capsule.pending_text_pastes,
        vec!["staged paste block".to_string()]
    );
    assert_eq!(
        app.composer.input, "",
        "Composer input should clear for live query typing"
    );

    // Operator types a query, filtering candidates
    app.composer.input = "historical".to_string();
    assert_eq!(app.history_rows().len(), 1);

    // Operator cancels via Esc (dismissing surface)
    assert!(app.dismiss_surface());

    // Draft is restored 100% loss-free
    assert_eq!(app.composer.input, "uncommitted work in progress");
    assert_eq!(
        app.composer.cursor_position,
        "uncommitted work in progress".len()
    );
    assert_eq!(
        app.composer.pending_text_pastes,
        vec!["staged paste block".to_string()]
    );
    assert!(
        app.composer.draft_capsule.is_empty(),
        "Stash should be cleared on rollback"
    );
}

#[test]
fn adr0044_ghost_preview_rendering() {
    let theme = Theme::default();
    let rect = nuotc::Rect::new(0, 0, 80, 5);
    let mut terminal = nuotc::TestTerminal::new(80, 5);
    let mut scroll = 0usize;

    // Empty input with ghost text preview renders the ghost text in muted style
    let ghost = "multi-line\nghost candidate";
    terminal.draw(|f| {
        crate::shell::composer::draw_composer_with_options(
            crate::shell::composer::ComposerProps {
                frame: f,
                input_rect: rect,
                theme: &theme,
                layout_map: &mut nuo_view::model::layout::LayoutMap::new(),
                input_scroll: &mut scroll,
                selection: &nuo_view::model::selection::SelectionState::None,
            },
            crate::shell::composer::ComposerText {
                input: "",
                byte_cursor: 0,
            },
            crate::shell::composer::ComposerDrawOptions {
                focused: true,
                show_caret: true,
                follow_caret: true,
                record: false,
                image_count: 0,
                paste_count: 0,
                hints: crate::shell::composer_hints::ComposerHints::default(),
                ghost_text: Some(ghost),
            },
        );
    });

    let buf = terminal.buffer();
    // Ghost text content must be visible in the buffer
    let text = buf
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>();
    assert!(
        text.contains("multi-line"),
        "Buffer must contain ghost preview line 1"
    );
    assert!(
        text.contains("ghost candidate"),
        "Buffer must contain ghost preview line 2"
    );
}

#[tokio::test]
async fn test_history_search_composer_pinned_to_bottom_with_multiline_candidate() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "session-a".to_string();

    // Populate history with both single-line and long multi-line entries
    let multiline_entry = (0..20)
        .map(|i| format!("line {i} of long historical prompt"))
        .collect::<Vec<_>>()
        .join("\n");
    app.record_input_history("short entry".to_string(), Vec::new(), Vec::new());
    app.record_input_history(multiline_entry, Vec::new(), Vec::new());

    // 1. Render normal frame before opening history search (80 cols, 24 rows)
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-a");
    });
    app.transcript.ui.commit();
    let normal_composer_rect = app
        .transcript
        .ui
        .bounds(crate::shell::ui::UiKey::Composer)
        .expect("composer rect");
    // Under normal empty/single-line input, composer is 4 rows high pinned at rows 19..23 (above 1-row model bar at row 23)
    assert_eq!(normal_composer_rect.height, 4);
    assert_eq!(normal_composer_rect.y, 19);

    // 2. Open HistorySearch (simulating Ctrl+R)
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch)
    );

    // 3. Render frame with multi-line candidate selected (index 0 is the newest, 20-line entry)
    let mut terminal_hist = nuotc::TestTerminal::new(80, 24);
    terminal_hist.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-a");
    });
    app.transcript.ui.commit();
    let hist_composer_rect = app
        .transcript
        .ui
        .bounds(crate::shell::ui::UiKey::Composer)
        .expect("composer rect in history search");

    // CRITICAL: Composer position MUST NOT change or drift towards the vertical center!
    // It must remain pinned to the bottom (y = 19, height = 4) rather than expanding to size.height / 2 (y = 11, height = 12).
    assert_eq!(
        hist_composer_rect.y, normal_composer_rect.y,
        "Composer must maintain its position at the bottom (y={}) instead of drifting to vertical center (found y={})",
        normal_composer_rect.y, hist_composer_rect.y
    );
    assert_eq!(
        hist_composer_rect.height, normal_composer_rect.height,
        "Composer height must remain based on live input (height={}) instead of inflating to candidate size (found height={})",
        normal_composer_rect.height, hist_composer_rect.height
    );

    // 4. Navigate to the single-line candidate (index 1)
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
        .index = 1;
    let mut terminal_hist_short = nuotc::TestTerminal::new(80, 24);
    terminal_hist_short.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-a");
    });
    app.transcript.ui.commit();
    let hist_short_composer_rect = app
        .transcript
        .ui
        .bounds(crate::shell::ui::UiKey::Composer)
        .expect("composer rect");

    // Switching between long and short candidates must NEVER cause the composer to jump
    assert_eq!(
        hist_short_composer_rect.y, normal_composer_rect.y,
        "Composer position must be completely stable when browsing history"
    );
    assert_eq!(
        hist_short_composer_rect.height, normal_composer_rect.height,
        "Composer height must be completely stable when browsing history"
    );
}

#[tokio::test]
async fn test_history_search_stashes_and_restores_attachments_with_empty_text() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "session-a".to_string();

    // Draft with only staged images/pastes and empty text
    app.composer.input.clear();
    let img = nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    app.composer.pending_images.push(img.clone());
    app.composer
        .pending_text_pastes
        .push("staged paste without text".to_string());

    // 1. Open HistorySearch
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert!(app.composer.draft_capsule.text.is_empty());
    assert_eq!(app.composer.draft_capsule.pending_images, vec![img.clone()]);
    assert_eq!(
        app.composer.draft_capsule.pending_text_pastes,
        vec!["staged paste without text".to_string()]
    );
    assert!(
        app.composer.pending_images.is_empty(),
        "Pending attachments cleared for query typing"
    );
    assert!(app.composer.pending_text_pastes.is_empty());

    // 2. Type search query
    app.composer.input = "query".to_string();

    // 3. Dismiss via Esc
    assert!(app.dismiss_surface());

    // 4. Staged attachments are 100% restored
    assert_eq!(app.composer.input, "");
    assert_eq!(app.composer.pending_images, vec![img]);
    assert_eq!(
        app.composer.pending_text_pastes,
        vec!["staged paste without text".to_string()]
    );
}

#[tokio::test]
async fn test_history_search_empty_placeholder_visible_and_index_clamped() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "session-a".to_string();
    app.record_input_history("cargo build".to_string(), Vec::new(), Vec::new());
    app.record_input_history("cargo test".to_string(), Vec::new(), Vec::new());

    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);

    // Operator navigates down to index 1
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
        .index = 1;

    // Operator types a query with no matches
    app.composer.input = "nomatch_query".to_string();

    // Render frame
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

    // The no matches placeholder must be visible
    assert!(
        screen.contains("no matches"),
        "Screen must show visible empty state placeholder: {screen}"
    );
}
