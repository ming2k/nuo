//! History entry deletion and composer hint density.

use super::*;

#[test]
fn test_delete_selected_history_entry_and_cascade() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "session-test".to_string();

    // 1. Setup 3 history entries
    app.composer.input_history = vec![
        nuo_protocol::HistoryEntry::new(
            "first entry".into(),
            Some("session-test".into()),
            None,
            100,
        ),
        nuo_protocol::HistoryEntry::new(
            "target entry".into(),
            Some("session-test".into()),
            None,
            200,
        ),
        nuo_protocol::HistoryEntry::new(
            "third entry".into(),
            Some("session-test".into()),
            None,
            300,
        ),
    ];
    // Backfill has target entry
    app.composer.session_history_backfill = vec![
        nuo_protocol::HistoryEntry::new(
            "target entry".into(),
            Some("session-test".into()),
            None,
            200,
        ),
        nuo_protocol::HistoryEntry::new(
            "other backfill".into(),
            Some("session-test".into()),
            None,
            250,
        ),
    ];
    // Attachments has target entry
    let identity = ("target entry".to_string(), Some("session-test".to_string()));
    app.composer.history_attachments.insert(
        identity.clone(),
        crate::shell::app::HistoryAttachments {
            images: vec![],
            text_pastes: vec!["some paste".into()],
        },
    );
    app.composer
        .history_attachments_order
        .push_back(identity.clone());

    // History order newest first: "third entry" (idx 2), "target entry" (idx 1), "first entry" (idx 0).
    // Let's select row 1 ("target entry")
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
        .index = 1;
    let removed = app.delete_selected_history_entry();
    assert!(removed.is_some());
    let removed = removed.unwrap();
    assert_eq!(removed.text, "target entry");

    // Verify removed from input_history
    assert_eq!(app.composer.input_history.len(), 2);
    assert!(
        !app.composer
            .input_history
            .iter()
            .any(|e| e.text == "target entry")
    );

    // Verify pruned from session_history_backfill
    assert_eq!(app.composer.session_history_backfill.len(), 1);
    assert_eq!(
        app.composer.session_history_backfill[0].text,
        "other backfill"
    );

    // Verify pruned from history_attachments and order
    assert!(!app.composer.history_attachments.contains_key(&identity));
    assert!(
        !app.composer
            .history_attachments_order
            .iter()
            .any(|k| k == &identity)
    );

    // Verify the panel's index is clamped
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        1
    );

    // Delete remaining entries
    app.delete_selected_history_entry();
    assert_eq!(app.composer.input_history.len(), 1);
    app.delete_selected_history_entry();
    assert_eq!(app.composer.input_history.len(), 0);
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        0
    );

    // Deleting from empty history is a safe no-op
    assert!(app.delete_selected_history_entry().is_none());
}

#[test]
fn test_shift_delete_and_bare_delete_dispatch() {
    use crate::shell::keymap::{Key, KeyCode, KeyModifiers};
    use crate::shell::modal_keys::resolve_modal_key;

    let c = crate::shell::modal_keys::ModalKeys::default();

    // Shift+Delete in HistorySearch resolves to HistoryDeleteSelected
    let shift_del = Key::SHIFT_DELETE;
    let action = resolve_modal_key(
        Some(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::HistorySearch,
        )),
        crate::shell::surfaces::SceneKind::Thread,
        shift_del,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(
        action,
        Some(crate::shell::input::InputAction::HistoryDeleteSelected)
    );

    // Bare Delete in HistorySearch falls through to None (text engine DeleteForward)
    let bare_del = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Delete,
    };
    let action = resolve_modal_key(
        Some(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::HistorySearch,
        )),
        crate::shell::surfaces::SceneKind::Thread,
        bare_del,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(action, None);

    // Ctrl+P / Ctrl+N navigate matches in history search
    let action_p = resolve_modal_key(
        Some(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::HistorySearch,
        )),
        crate::shell::surfaces::SceneKind::Thread,
        Key::CTRL_P,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(action_p, Some(crate::shell::input::InputAction::ModalUp));

    let action_n = resolve_modal_key(
        Some(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::HistorySearch,
        )),
        crate::shell::surfaces::SceneKind::Thread,
        Key::CTRL_N,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(action_n, Some(crate::shell::input::InputAction::ModalDown));

    // Ctrl+R cycles next match and Ctrl+Shift+R cycles previous match (ADR-0051 [INV-COMP-04])
    let action_ctrl_r = resolve_modal_key(
        Some(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::HistorySearch,
        )),
        crate::shell::surfaces::SceneKind::Thread,
        Key::CTRL_R,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(
        action_ctrl_r,
        Some(crate::shell::input::InputAction::ModalDown)
    );

    let action_ctrl_shift_r = resolve_modal_key(
        Some(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::HistorySearch,
        )),
        crate::shell::surfaces::SceneKind::Thread,
        Key::CTRL_SHIFT_R,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(
        action_ctrl_shift_r,
        Some(crate::shell::input::InputAction::ModalUp)
    );
}

#[tokio::test]
async fn test_history_delete_selected_action_dispatch() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.composer
        .input_history
        .push(nuo_protocol::HistoryEntry::new(
            "entry 1".to_string(),
            Some("s1".to_string()),
            None,
            100,
        ));
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let flow = crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::HistoryDeleteSelected,
        "s1",
    )
    .await;
    assert_eq!(flow, crate::shell::event_loop::actions::ActionFlow::Handled);
    assert_eq!(app.composer.input_history.len(), 0);
}

#[test]
fn test_composer_hints_history_search_density() {
    use crate::shell::composer_hints::{ActionDensity, ComposeTarget, hint_row_parts};
    use crate::shell::render::Theme;

    let theme = Theme::default();
    let key = crate::shell::keymap::Key::TAB;

    // Full density: includes close, navigate, delete, insert
    let (left, right) = hint_row_parts(
        false,
        ActionDensity::Full,
        ComposeTarget::HistorySearch,
        &theme,
        theme.panel(),
        key,
        None,
    );
    let left_str: String = left.iter().map(|s| s.content.as_ref()).collect();
    let right_str: String = right.iter().map(|s| s.content.as_ref()).collect();
    assert!(left_str.contains("close"));
    assert!(left_str.contains("navigate"));
    assert!(left_str.contains("delete"));
    assert!(left_str.contains("Shift-Del"));
    assert!(right_str.contains("Tab"));
    assert!(right_str.contains("Enter"));
    assert!(right_str.contains("insert"));

    // Compact density: drops navigate and Tab
    let (left_c, right_c) = hint_row_parts(
        false,
        ActionDensity::Compact,
        ComposeTarget::HistorySearch,
        &theme,
        theme.panel(),
        key,
        None,
    );
    let left_c_str: String = left_c.iter().map(|s| s.content.as_ref()).collect();
    let right_c_str: String = right_c.iter().map(|s| s.content.as_ref()).collect();
    assert!(left_c_str.contains("close"));
    assert!(!left_c_str.contains("navigate"));
    assert!(left_c_str.contains("delete"));
    assert!(!right_c_str.contains("Tab"));
    assert!(right_c_str.contains("Enter"));

    // Tiny density: drops delete as well, only close and Enter insert
    let (left_t, right_t) = hint_row_parts(
        false,
        ActionDensity::Tiny,
        ComposeTarget::HistorySearch,
        &theme,
        theme.panel(),
        key,
        None,
    );
    let left_t_str: String = left_t.iter().map(|s| s.content.as_ref()).collect();
    let right_t_str: String = right_t.iter().map(|s| s.content.as_ref()).collect();
    assert!(left_t_str.contains("close"));
    assert!(!left_t_str.contains("delete"));
    assert!(!left_t_str.contains("navigate"));
    assert!(right_t_str.contains("Enter"));
}

#[tokio::test]
async fn test_ctrl_c_in_history_search() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::mpsc;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch)
    );

    // Case 1: Filter query is non-empty -> Ctrl+C clears the filter and resets cursor
    app.composer.input = "my search query".to_string();
    app.composer.cursor_position = 5;
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
        .index = 2;

    let (copy_tx, _copy_rx) = mpsc::unbounded_channel();
    let copy_pending = Arc::new(AtomicUsize::new(0));

    crate::shell::event_loop::handle_ctrl_c(&mut app, "test-session", &copy_tx, &copy_pending);

    // Dialog remains open, input query cleared, index reset
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch)
    );
    assert_eq!(app.composer.input, "");
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
            .index,
        0
    );

    // Case 2: Filter query is empty -> Ctrl+C dismisses history dialog
    crate::shell::event_loop::handle_ctrl_c(&mut app, "test-session", &copy_tx, &copy_pending);

    // Dialog dismissed
    assert_ne!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::HistorySearch)
    );
}
