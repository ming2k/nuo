//! Paste and escape routing tests.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn esc_closes_slash_completion_menu() {
    let mut input = "/mc".to_string();
    let mut cursor = 3;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: None,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            completion_kind: crate::shell::CompletionKind::Slash,
            suggestion_count: 2,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseCompletion);
    assert_eq!(input, "/mc");
}

#[test]
fn esc_closes_path_completion_menu() {
    let mut input = "@src".to_string();
    let mut cursor = 4;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: None,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            completion_kind: crate::shell::CompletionKind::Path,
            suggestion_count: 3,
            suggestion_index: Some(1),
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseCompletion);
}

#[test]
fn esc_falls_through_when_no_completion_is_open() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
}

#[test]
fn escape_returns_from_always_confirmation() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        Dispatch {
            sheet: Some(crate::shell::sheet::SheetKind::Permission),
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys {
            permission_confirm_always: true,
            ..Default::default()
        },
        &SceneKeys {
            is_responding: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::PermissionBack);
}

#[test]
fn esc_in_models_browse_closes_the_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: Some(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Models,
            )),
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn esc_in_connections_browse_closes_the_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: Some(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Connections,
            )),
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn escape_clears_focus() {
    assert_eq!(
        key_with_focus(KeyCode::Esc),
        InputAction::ClearFocusedTarget
    );
}

/// Esc does not close a Scene. On the TaskInspection zoom it is
/// inert once a completion and step/browse focus are clear — the scene's own
/// exit is `q` (or `C-x w`).
#[test]
fn escape_never_exits_subagent_view() {
    let mut input = String::new();
    assert_eq!(
        key_in_view(KeyCode::Esc, true, &mut input),
        InputAction::None
    );
    assert_eq!(
        key_in_view(KeyCode::Esc, false, &mut input),
        InputAction::None
    );
    // The zoom has no exit chord of its own: leaving is the C-x namespace.
    assert_ne!(
        key_in_view(KeyCode::Char('q'), true, &mut input),
        InputAction::CloseTab,
        "`q` never closes the tab"
    );
}

/// Esc in a background view interrupts its round and
/// never leaves the view; `Ctrl+C` is the detach gesture.
#[test]
fn raw_bracket_keys_type_characters_while_alt_brackets_navigate_scene_history() {
    let mut input = String::new();
    key_in_view(KeyCode::Char('['), true, &mut input);
    assert_eq!(
        input, "[",
        "raw `[` always types character even when empty (ADR-0092)"
    );

    let mut other = String::new();
    key_in_view(KeyCode::Char(']'), false, &mut other);
    assert_eq!(other, "]");

    // Alt+[ and Alt+] navigate scene history regardless of input contents (ADR-0092 [INV-NAV-03]):
    let mut draft = "in flight draft".to_string();
    assert_eq!(
        super::key_with_mod_in_view(KeyCode::Char('['), KeyModifiers::ALT, true, &mut draft),
        InputAction::TabHistoryBack
    );
    assert_eq!(
        super::key_with_mod_in_view(KeyCode::Char(']'), KeyModifiers::ALT, true, &mut draft),
        InputAction::TabHistoryForward
    );
}

#[test]
fn esc_in_history_panel_closes_modal_directly() {
    let mut input = "git".to_string();
    let mut cursor = 3;
    let action = run_history_key(&mut input, &mut cursor, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn bracketed_paste_routes_in_free_text_modals() {
    let payload = "sk-test-1234";
    for fixture in [
        SurfaceFixture::None,
        SurfaceFixture::ModelEditor,
        SurfaceFixture::Models,
        SurfaceFixture::Connections,
        SurfaceFixture::HistorySearch,
    ] {
        let mut input = String::new();
        let mut cursor = 0;
        let action = run_paste(payload, &mut input, &mut cursor, fixture);
        match action {
            InputAction::BracketedPaste(text) => assert_eq!(
                text, payload,
                "bracketed paste payload should pass through in free-text modal"
            ),
            other => panic!("expected BracketedPaste in free-text modal, got {other:?}"),
        }
        assert!(
            input.is_empty(),
            "BracketedPaste must not mutate the buffer itself"
        );
    }

    let mut input = String::new();
    let mut cursor = 0;
    let action = run_paste(payload, &mut input, &mut cursor, SurfaceFixture::UsageStats);
    assert_eq!(
        action,
        InputAction::None,
        "bracketed paste should be dropped in Usage stats"
    );

    let config_context = Dispatch {
        scene: crate::shell::surfaces::SceneKind::Settings,
        ..Default::default()
    };
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = nuo_view::model::selection::SelectionDrag::default();
    let action = crate::shell::input::route_event(
        crossterm::event::Event::Paste(payload.to_string()),
        &mut input,
        &mut cursor,
        config_context,
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::None,
        "bracketed paste should be dropped in Config"
    );
}
