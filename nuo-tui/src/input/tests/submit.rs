//! Submit action tests: chat dispatch, slash commands, modal activation, focus.

use super::*;

#[test]
fn enter_in_compose_sends_chat() {
    let mut input = "hello world".to_string();
    assert_eq!(
        enter(&mut input, false),
        InputAction::SendChat("hello world".to_string())
    );
    assert_eq!(input, "");
}

#[test]
fn enter_in_compose_while_busy_steers_immediate_by_default() {
    let mut input = "steer message".to_string();
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            is_responding: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::SteerImmediate("steer message".to_string())
    );
    assert_eq!(input, "");
}

#[test]
fn enter_in_compose_while_busy_queues_follow_up_in_follow_up_mode() {
    let mut input = "follow up message".to_string();
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            is_responding: true,
            composer_send_mode: crate::app::ComposerSendMode::FollowUp,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::QueueFollowUp("follow up message".to_string())
    );
    assert_eq!(input, "");
}

#[test]
fn enter_with_slash_command_dispatches_recognized_commands() {
    for (cmd, expected) in [
        ("/sessions", InputAction::OpenSessions),
        ("/models", InputAction::OpenModels),
        ("/connections", InputAction::OpenConnections),
        ("/permissions", InputAction::OpenPermissions),
        ("/tools", InputAction::OpenTools),
        ("/usage", InputAction::OpenUsage),
        ("/mcp", InputAction::OpenMcp),
        ("/skills", InputAction::OpenSkills),
        ("/settings", InputAction::OpenConfig),
        ("/config", InputAction::OpenConfig),
        ("/exit", InputAction::Quit),
    ] {
        let mut input = cmd.to_string();
        assert_eq!(enter(&mut input, false), expected, "failed for {cmd}");
        assert_eq!(input, "");
    }
}

#[test]
fn enter_with_unknown_slash_command_dispatches_send_slash() {
    let mut input = "/custom-command arg1".to_string();
    assert_eq!(
        enter(&mut input, false),
        InputAction::SendSlash("/custom-command arg1".to_string())
    );
    assert_eq!(input, "");
}

#[test]
fn enter_with_active_suggestion_commits_suggestion() {
    let mut input = "/m".to_string();
    assert_eq!(
        enter_with_completion(&mut input, crate::CompletionKind::Slash, 2, Some(1), false),
        InputAction::CommitSuggestion("1".to_string())
    );
}

#[test]
fn enter_with_unique_slash_suggestion_auto_commits() {
    let mut input = "/mod".to_string();
    assert_eq!(
        enter_with_completion(&mut input, crate::CompletionKind::Slash, 1, None, false),
        InputAction::CommitSuggestion("0".to_string())
    );
}

#[test]
fn enter_activates_focused_target() {
    assert_eq!(
        key_with_focus(KeyCode::Enter),
        InputAction::ActivateFocusedTarget
    );
}

#[test]
fn space_in_transcript_is_inert() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            focused_target: true,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "");
    assert_eq!(cursor, 0);
}

#[test]
fn enter_in_history_modal_emits_history_insert() {
    let mut input = "go".to_string();
    let mut cursor = 2;
    let action = run_history_key(&mut input, &mut cursor, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(action, InputAction::HistoryInsert);
}
