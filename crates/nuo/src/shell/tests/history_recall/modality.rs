//! ADR-0051: canonical quadrant modality and draft capsule restoration.

use super::*;

#[test]
fn adr0051_canonical_quadrant_modality_transitions() {
    use nuo_view::composer_mode::{CompleteKind, ComposerMode};

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-test".to_string();

    // 1. Default state is Edit mode ([INV-COMP-01])
    assert_eq!(app.composer_mode(), ComposerMode::Edit);
    assert_eq!(app.composer_mode().label(), "Edit");

    // 2. In-place history stepping enters Recall mode ([INV-COMP-02])
    app.input_history.push(nuo_protocol::HistoryEntry::new(
        "entry 1".to_string(),
        Some("session-test".to_string()),
        None,
        100,
    ));
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(matches!(
        app.composer_mode(),
        ComposerMode::Recall { history_index: 0 }
    ));
    assert_eq!(app.composer_mode().label(), "Recall");

    // Stepping back past newest restores draft and returns to Edit
    assert!(!app.history_next(&rows));
    assert_eq!(app.composer_mode(), ComposerMode::Edit);

    // 3. Opening HistorySearch transitions to Search mode ([INV-COMP-04])
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    app.input = "query".to_string();
    assert!(matches!(
        app.composer_mode(),
        ComposerMode::Search {
            selected_index: 0,
            ..
        }
    ));
    assert_eq!(app.composer_mode().label(), "Search");

    // Dismissing search returns to Edit mode
    assert!(app.dismiss_surface());
    assert_eq!(app.composer_mode(), ComposerMode::Edit);

    // 4. Slash command autocomplete enters Complete mode
    app.input = "/m".to_string();
    app.cursor_position = 2;
    app.completion_dismissed = false;
    assert!(matches!(
        app.composer_mode(),
        ComposerMode::Complete {
            kind: CompleteKind::Command,
            ..
        }
    ));
    assert_eq!(app.composer_mode().label(), "Command");
}

#[tokio::test]
async fn adr0051_ctrl_r_continuous_cycling_in_history_search() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-test".to_string();

    // Populate multiple history entries
    app.record_input_history("command 1".to_string(), Vec::new(), Vec::new());
    app.record_input_history("command 2".to_string(), Vec::new(), Vec::new());
    app.record_input_history("command 3".to_string(), Vec::new(), Vec::new());

    // 1. Enter HistorySearch via OpenHistory
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch)
    );
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        0
    );

    // 2. In HistorySearch, Ctrl-R resolves to ModalDown (cycling downward)
    let c = crate::shell::modal_keys::ModalKeys::default();
    let action_ctrl_r = crate::shell::modal_keys::resolve_modal_key(
        Some(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::HistorySearch,
        )),
        crate::shell::surfaces::SceneKind::Thread,
        crate::shell::keymap::Key::CTRL_R,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(
        action_ctrl_r,
        Some(crate::shell::input::InputAction::ModalDown)
    );

    // 3. Dispatching ModalDown rotates index 0 -> 1 -> 2 -> 0 ([INV-COMP-04])
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ModalDown,
        "session-test",
    )
    .await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        1
    );

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ModalDown,
        "session-test",
    )
    .await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        2
    );

    // Wrapping around
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ModalDown,
        "session-test",
    )
    .await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        0
    );

    // 4. Ctrl+Shift+R steps backwards (ModalUp)
    let action_ctrl_shift_r = crate::shell::modal_keys::resolve_modal_key(
        Some(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::HistorySearch,
        )),
        crate::shell::surfaces::SceneKind::Thread,
        crate::shell::keymap::Key::CTRL_SHIFT_R,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(
        action_ctrl_shift_r,
        Some(crate::shell::input::InputAction::ModalUp)
    );

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ModalUp,
        "session-test",
    )
    .await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        2
    );
}

#[test]
fn adr0051_draft_capsule_atomic_stashing_and_restoration() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-test".to_string();

    // Draft with text and pending attachments
    app.input = "in-flight prompt".to_string();
    app.cursor_position = 8;
    let img = nuo_protocol::ImagePart {
        mime: "image/png".to_string(),
        data: "base64data".to_string(),
    };
    app.pending_images.push(img.clone());
    app.pending_text_pastes.push("paste payload".to_string());

    // 1. Open history search -> draft is captured into draft_capsule ([INV-COMP-03])
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert!(!app.draft_capsule.is_empty());
    assert_eq!(app.draft_capsule.text, "in-flight prompt");
    assert_eq!(app.draft_capsule.cursor_position, 8);
    assert_eq!(app.draft_capsule.pending_images, vec![img.clone()]);
    assert_eq!(
        app.draft_capsule.pending_text_pastes,
        vec!["paste payload".to_string()]
    );
    assert_eq!(app.input, "");

    // 2. Dismiss history search -> draft is 100% restored and capsule cleared
    assert!(app.dismiss_surface());
    assert_eq!(app.input, "in-flight prompt");
    assert_eq!(app.pending_images, vec![img]);
    assert_eq!(app.pending_text_pastes, vec!["paste payload".to_string()]);
    assert!(app.draft_capsule.is_empty());
}

#[tokio::test]
async fn test_history_search_up_down_arrow_navigation_through_event_loop() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-test".to_string();

    // Populate multiple history entries
    app.record_input_history("command 1".to_string(), Vec::new(), Vec::new());
    app.record_input_history("command 2".to_string(), Vec::new(), Vec::new());
    app.record_input_history("command 3".to_string(), Vec::new(), Vec::new());

    // 1. Enter HistorySearch via OpenHistory
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch)
    );
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        0
    );

    // 2. Press Down arrow -> index advances to 1
    let down_event = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    crate::shell::event_loop::process_event_for_test(&mut app, &down_event, "session-test").await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        1,
        "Down arrow must advance candidate index to 1 without being reset to 0"
    );

    // 3. Press Down arrow again -> index advances to 2
    crate::shell::event_loop::process_event_for_test(&mut app, &down_event, "session-test").await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        2,
        "Down arrow must advance candidate index to 2"
    );

    // 4. Press Up arrow -> index moves back to 1
    let up_event = Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    crate::shell::event_loop::process_event_for_test(&mut app, &up_event, "session-test").await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        1,
        "Up arrow must move candidate index back to 1"
    );

    // 5. Typing query text resets selection index to 0
    let type_event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    crate::shell::event_loop::process_event_for_test(&mut app, &type_event, "session-test").await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        0,
        "Typing query text must reset candidate index to 0"
    );
    assert_eq!(app.input, "c");

    // 6. Press Down arrow with filter active -> index advances to 1
    crate::shell::event_loop::process_event_for_test(&mut app, &down_event, "session-test").await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        1,
        "Down arrow must advance index on filtered results"
    );

    // 7. Press Enter -> inserts selected candidate and closes history search
    let enter_event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    crate::shell::event_loop::process_event_for_test(&mut app, &enter_event, "session-test").await;
    assert_eq!(app.active_dialog(), None);
    // Newest-first order: index 0 is "command 3", index 1 is "command 2", index 2 is "command 1"
    assert_eq!(app.input, "command 2");
    assert!(
        app.draft_capsule.is_empty(),
        "Draft capsule must be cleared on successful history insertion"
    );
}

#[tokio::test]
async fn test_ctrl_r_does_not_open_history_when_composer_inactive() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use nuo_view::model::layout::InteractiveTarget;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-test".to_string();
    app.record_input_history("existing entry".to_string(), Vec::new(), Vec::new());

    // Initially, composer is focused and active
    assert!(app.is_composer_focused());
    assert!(app.dialog_available(crate::shell::surfaces::DialogKind::HistorySearch));

    // 1. Simulate step focus: composer becomes inactive
    app.focus
        .enter_transcript(crate::shell::focus::TranscriptFocus::Entry(
            InteractiveTarget::tool_step(0),
        ));
    assert!(!app.is_composer_focused());
    assert!(!app.dialog_available(crate::shell::surfaces::DialogKind::HistorySearch));
    assert!(!app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch));
    assert_eq!(app.active_dialog(), None);

    // Pressing Ctrl-R while step-focused must NOT open HistorySearch
    let ctrl_r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    crate::shell::event_loop::process_event_for_test(&mut app, &ctrl_r, "session-test").await;
    assert_eq!(
        app.active_dialog(),
        None,
        "Ctrl-r must not open history search when step is focused"
    );

    // 2. Shift-Tab crosses back to the composer (Esc deliberately does not — it is
    // the interrupt chord and never moves the keyboard; ADR-0145).
    let back_tab = Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
    crate::shell::event_loop::process_event_for_test(&mut app, &back_tab, "session-test").await;
    assert!(app.focus.entry().is_none());
    assert!(app.is_composer_focused());
    assert!(app.dialog_available(crate::shell::surfaces::DialogKind::HistorySearch));

    // 3. Simulate transcript browse focus: composer becomes inactive
    app.focus
        .enter_transcript(crate::shell::focus::TranscriptFocus::Browse);
    assert!(!app.is_composer_focused());
    assert!(!app.dialog_available(crate::shell::surfaces::DialogKind::HistorySearch));
    assert!(!app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch));
    assert_eq!(app.active_dialog(), None);

    // Pressing Ctrl-R while transcript-focused must NOT open HistorySearch
    crate::shell::event_loop::process_event_for_test(&mut app, &ctrl_r, "session-test").await;
    assert_eq!(
        app.active_dialog(),
        None,
        "Ctrl-r must not open history search when transcript is focused"
    );

    // 4. Shift-Tab crosses back from browse focus too.
    crate::shell::event_loop::process_event_for_test(&mut app, &back_tab, "session-test").await;
    assert!(app.focus.is_composer());
    assert!(app.is_composer_focused());

    // 5. With composer active, Ctrl-R opens HistorySearch
    crate::shell::event_loop::process_event_for_test(&mut app, &ctrl_r, "session-test").await;
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch),
        "Ctrl-r must open history search when composer is active"
    );
}

#[tokio::test]
async fn history_search_opening_preserves_scene_bar_and_context_bar() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-test".to_string();
    app.record_input_history("existing entry".to_string(), Vec::new(), Vec::new());

    // 1. Render normal frame
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-test");
    });
    let buffer_normal = terminal.buffer().clone();
    let row1_normal: String = (0..80)
        .map(|x| buffer_normal.get(x, 1).unwrap().symbol.as_str())
        .collect();
    let row2_normal: String = (0..80)
        .map(|x| buffer_normal.get(x, 2).unwrap().symbol.as_str())
        .collect();
    assert!(
        row1_normal.contains("THREAD"),
        "Row 1 must be SceneBar carrying THREAD tag: {row1_normal:?}"
    );

    // 2. Open HistorySearch
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch)
    );

    // 3. Render frame during HistorySearch
    let mut terminal_hist = nuotc::TestTerminal::new(80, 24);
    terminal_hist.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-test");
    });
    let buffer_hist = terminal_hist.buffer();
    let row1_hist: String = (0..80)
        .map(|x| buffer_hist.get(x, 1).unwrap().symbol.as_str())
        .collect();
    let row2_hist: String = (0..80)
        .map(|x| buffer_hist.get(x, 2).unwrap().symbol.as_str())
        .collect();
    assert!(
        row1_hist.contains("THREAD"),
        "Row 1 SceneBar must NOT be hidden when HistorySearch is opened: {row1_hist:?}"
    );
    assert_eq!(
        row1_hist, row1_normal,
        "SceneBar must remain identical during HistorySearch"
    );
    assert_eq!(
        row2_hist, row2_normal,
        "ThreadContextBar must remain identical during HistorySearch"
    );
}
