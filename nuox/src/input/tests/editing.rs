//! Text editing tests: insertion, Backspace/Delete, word deletion, Ctrl/Alt combos, bang-prefix dispatch.

use super::*;

#[test]
fn typing_in_compose_returns_insert_char() {
    // route_event must signal InsertChar (not None) so the event loop
    // can reset the completion-dismissal latch after an Enter commit or
    // Esc dismiss. The char is already spliced into `input` here; the
    // event loop treats the action as a signal only.
    let mut input = "/mc".to_string();
    let mut cursor = 3;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: None,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 2,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::InsertChar('p'));
    assert_eq!(input, "/mcp");
    assert_eq!(cursor, 4);
}

#[test]
fn backspace_in_compose_returns_backspace_action() {
    // Same signal contract as InsertChar: Backspace must be returned so
    // the event loop clears completion_dismissed + suggestion_index.
    let mut input = "/mcp".to_string();
    let mut cursor = 4;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: None,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 1,
            has_exact_suggestion: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "/mc");
    assert_eq!(cursor, 3);
}

#[test]
fn backspace_atomically_deletes_an_image_chip() {
    let chip = crate::composer_attachments::image_chip(1, 0);
    let mut input = format!("look {chip} ");
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "look ");
    assert_eq!(cursor, "look ".chars().count());
}

#[test]
fn backspace_atomically_deletes_a_paste_chip_without_trailing_space() {
    let chip = crate::composer_attachments::paste_chip(1, 5, 0);
    let mut input = format!("see {chip}!");
    let prefix_chars = "see ".chars().count() + chip.chars().count();
    let mut cursor = prefix_chars;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "see !");
    assert_eq!(cursor, "see ".chars().count());
}

#[test]
fn backspace_falls_through_to_single_char_outside_a_chip() {
    let mut input = "hello".to_string();
    let mut cursor = 5;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "hell");
    assert_eq!(cursor, 4);
}

#[test]
fn bang_prefix_variants_dispatch_as_chat() {
    for cmd in ["!git status", "!   ls -la", "!"] {
        let mut input = cmd.to_string();
        assert_eq!(
            enter_shell(&mut input),
            InputAction::SendChat(cmd.to_string())
        );
    }
}

#[test]
fn plain_ctrl_c_maps_to_semantic_ctrl_c() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::CtrlC);
}

#[test]
fn single_key_shortcuts_in_connections_modal() {
    for (key, expected) in [
        ('a', InputAction::OpenPresetChooser),
        ('c', InputAction::OpenCustomConnection),
    ] {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        let action = route_event(
            Event::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            Dispatch {
                overlay: Some(crate::surfaces::OverlaySurface::Dialog(
                    crate::surfaces::DialogKind::Connections,
                )),
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        );
        assert_eq!(action, expected, "key '{key}' in connections modal");
    }
}

#[test]
fn arrows_cycle_custom_provider_selectors_without_editing_text() {
    for (key, forward) in [(KeyCode::Left, false), (KeyCode::Right, true)] {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        let action = route_event(
            Event::Key(KeyEvent::new(key, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            Dispatch {
                // `None` while Protocol or Client Identity is focused.
                overlay: Some(crate::surfaces::OverlaySurface::Sheet(
                    crate::surfaces::SheetKind::CustomProvider,
                )),
                ..Default::default()
            },
            &ModalKeys {
                custom_provider_field: None,
                ..Default::default()
            },
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        );
        assert_eq!(action, InputAction::CycleCustomProviderChoice { forward });
        assert!(input.is_empty());
    }
}

#[test]
fn custom_provider_model_field_accepts_plain_text() {
    let mut input = "GLM-5".to_string();
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: Some(crate::surfaces::OverlaySurface::Sheet(
                crate::surfaces::SheetKind::CustomProvider,
            )),
            ..Default::default()
        },
        &ModalKeys {
            custom_provider_field: Some(3),
            ..Default::default()
        },
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::InsertChar('.'));
    assert_eq!(input, "GLM-5.");
}

#[test]
fn b_and_d_in_preset_chooser_pick_the_login_method() {
    // The preset chooser exposes explicit OAuth login-method selection:
    // `b` = browser PKCE, `d` = device code. Whether the highlighted preset
    // actually supports the method is validated by the dispatcher (which can
    // see the OAuth registration); the input layer only maps the keys.
    for (key, method) in [
        ('b', nuo_contracts::LoginMethod::Browser),
        ('d', nuo_contracts::LoginMethod::Device),
    ] {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        let action = route_event(
            Event::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            Dispatch {
                overlay: Some(crate::surfaces::OverlaySurface::Sheet(
                    crate::surfaces::SheetKind::ProviderPreset,
                )),
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        );
        assert_eq!(
            action,
            InputAction::SelectPresetWithOauthMethod { method },
            "key {key} must select its login method"
        );
    }
}

#[test]
fn ctrl_b_moves_caret_back_one_char() {
    // Ctrl+B is readline backward-char: it moves the caret left and never
    // touches focus. (Focus navigation is Ctrl+↑/↓.)
    let mut input = String::from("abc");
    let mut cursor = 3;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('b'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 2, "Ctrl+B moves the caret back one character");
}

#[test]
fn ctrl_p_and_ctrl_n_drive_step_selection() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
            SurfaceFixture::None,
            false,
        ),
        InputAction::FocusPrevTarget
    );
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
            SurfaceFixture::None,
            true,
        ),
        InputAction::FocusNextTarget
    );
}

#[test]
fn typing_while_focused_is_inert() {
    let action = key_with_focus(KeyCode::Char('a'));
    assert_eq!(action, InputAction::None);
}

/// Ctrl+↑ / Ctrl+↓ inside any scrollable modal advance the body by a page
/// — the chord a pager/editor binds to a page jump, useful on keyboards
/// without dedicated Page keys and consistent across every modal. Mirrors
/// PageUp / PageDown.
#[test]
fn ctrl_arrows_page_scroll_modal_body() {
    let scrollable = [
        SurfaceFixture::UsageStats,
        SurfaceFixture::Config,
        SurfaceFixture::Telemetry,
        SurfaceFixture::Sessions,
        SurfaceFixture::Queue,
        SurfaceFixture::HistorySearch,
        SurfaceFixture::Models,
        SurfaceFixture::Connections,
        SurfaceFixture::Skills,
    ];
    for fixture in scrollable {
        let mut input = String::new();
        let mut cursor = 0;
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::Up,
                KeyModifiers::CONTROL,
                fixture,
                false
            ),
            InputAction::ScrollPageUp,
            "Ctrl+Up should page-scroll the {fixture:?} modal body"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::Down,
                KeyModifiers::CONTROL,
                fixture,
                false
            ),
            InputAction::ScrollPageDown,
            "Ctrl+Down should page-scroll the {fixture:?} modal body"
        );
    }
}

/// On the no-modal baseline, Alt+↑ / Alt+↓ drive transcript step selection
/// (ADR-0173: the step walk is verb-owned; PgUp/PgDn page the transcript).
#[test]
fn ctrl_p_and_ctrl_n_drive_transcript_focus_on_no_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
            SurfaceFixture::None,
            false,
        ),
        InputAction::FocusPrevTarget
    );
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
            SurfaceFixture::None,
            true,
        ),
        InputAction::FocusNextTarget
    );
}

#[test]
fn ctrl_a_and_ctrl_e_move_caret_in_compose_zone() {
    let mut input = "hello".to_string();
    let mut cursor = 2;

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 0);
    assert_eq!(input, "hello", "Ctrl+A must not insert a literal 'a'");

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('e'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 5);
    assert_eq!(input, "hello");
}

#[test]
fn ctrl_a_and_ctrl_e_are_noop_in_browse_zone() {
    // Browse has no input editing; the keys fall through to no-ops rather
    // than scrolling or inserting characters.
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
            SurfaceFixture::None,
            true
        ),
        InputAction::None
    );
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
            SurfaceFixture::None,
            true
        ),
        InputAction::None
    );
}

#[test]
fn ctrl_w_deletes_previous_word() {
    // "hello world" with the caret after "world" (char index 11).
    let mut input = "hello world".to_string();
    let mut cursor = 11;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "hello ");
    assert_eq!(cursor, 6);
}

#[test]
fn ctrl_w_eats_trailing_whitespace_and_previous_word() {
    // Caret sits after the trailing spaces following "world"; Ctrl+W
    // (readline `unix-word-rubout`) eats both the trailing whitespace
    // AND the preceding word in one stroke, leaving "hello ".
    let mut input = "hello world   ".to_string();
    let mut cursor = 14;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(input, "hello ");
    assert_eq!(cursor, 6);
}

#[test]
fn ctrl_w_is_noop_at_line_start() {
    let mut input = "hello world".to_string();
    let mut cursor = 0;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "hello world");
    assert_eq!(cursor, 0);
}

#[test]
fn ctrl_w_crosses_newline() {
    // Ctrl+W now crosses newline boundaries. "line1\nworld" with caret
    // at the end → first Ctrl+W deletes "world", second deletes "line1".
    let mut input = "line1\nworld".to_string();
    let mut cursor = 11;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(input, "line1\n");
    assert_eq!(cursor, 6);

    // Second Ctrl+W eats the newline and "line1".
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(input, "");
    assert_eq!(cursor, 0);
}

#[test]
fn ctrl_w_is_noop_in_question_modal() {
    // Ctrl+W must never leak as a literal 'w' or close the modal in the
    // question modal; it should be a silent no-op there.
    let mut input = "abc".to_string();
    let mut cursor = 3;
    let action = run_sheet_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        crate::sheet::SheetKind::Question,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "abc");
    assert_eq!(cursor, 3);
}

#[test]
fn ctrl_u_deletes_to_line_start() {
    let mut input = "hello world".to_string();
    let mut cursor = 7;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "orld");
    assert_eq!(cursor, 0);
}

#[test]
fn ctrl_u_keeps_other_lines_in_multiline_draft() {
    // Multi-line draft: Ctrl+U on line 2 only wipes the part of line 2
    // before the caret, leaving line 1 untouched.
    let mut input = "keep me\nwipe me".to_string();
    let mut cursor = 15;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(input, "keep me\n");
    assert_eq!(cursor, 8);
}

#[test]
fn ctrl_k_deletes_to_line_end() {
    let mut input = "hello world".to_string();
    let mut cursor = 5;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "hello");
    assert_eq!(cursor, 5, "Ctrl+K keeps the caret put");
}

#[test]
fn ctrl_k_does_not_eat_next_line_on_first_press() {
    let mut input = "first\nsecond".to_string();
    let mut cursor = 3;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(input, "fir\nsecond");
    assert_eq!(cursor, 3);
}

#[test]
fn ctrl_k_eats_newline_when_already_at_line_end() {
    let mut input = "fir\nsecond".to_string();
    let mut cursor = 3;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "firsecond");
    assert_eq!(cursor, 3);
}

#[test]
fn ctrl_k_at_buffer_end_is_noop() {
    let mut input = "hello".to_string();
    let mut cursor = 5;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "hello");
    assert_eq!(cursor, 5);
}

#[test]
fn alt_d_deletes_next_word() {
    // Caret at index 5 (the space); Alt+D should eat "world".
    let mut input = "hello world".to_string();
    let mut cursor = 5;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('d'),
        KeyModifiers::ALT,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "hello");
    assert_eq!(cursor, 5, "Alt+D keeps the caret put");
}

#[test]
fn alt_b_jumps_back_one_word() {
    let mut input = "the quick fox".to_string();
    let mut cursor = 13;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('b'),
        KeyModifiers::ALT,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 10);
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('b'),
        KeyModifiers::ALT,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 4);
}

#[test]
fn alt_f_jumps_forward_one_word() {
    let mut input = "the quick fox".to_string();
    let mut cursor = 0;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('f'),
        KeyModifiers::ALT,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 3);
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('f'),
        KeyModifiers::ALT,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 9);
}

#[test]
fn ctrl_left_right_move_word_by_word() {
    // "alpha bravo charlie" — char indices:
    // alpha=0..4, ' '=5, bravo=6..10, ' '=11, charlie=12..18 (len 19).
    let mut input = "alpha bravo charlie".to_string();
    let mut cursor = 19;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Left,
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 12, "Ctrl+Left snaps to the start of 'charlie'");
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Left,
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 6, "Ctrl+Left snaps to the start of 'bravo'");
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Right,
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 11, "Ctrl+Right snaps to the end of 'bravo'");
}

#[test]
fn alt_backspace_deletes_previous_word() {
    let mut input = "foo bar baz".to_string();
    let mut cursor = 11;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Backspace,
        KeyModifiers::ALT,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "foo bar ");
    assert_eq!(cursor, 8);
}

#[test]
fn ctrl_backspace_deletes_previous_word() {
    // Ctrl+Backspace is the same word-rubout motion on terminals that
    // deliver it; mirror the Alt+Backspace behaviour.
    let mut input = "foo bar baz".to_string();
    let mut cursor = 11;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Backspace,
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(input, "foo bar ");
    assert_eq!(cursor, 8);
}

#[test]
fn f1_is_inert() {
    // F1 no longer opens a help surface; it must be a no-op.
    let mut input = String::new();
    let mut cursor = 0;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::F(1),
        KeyModifiers::NONE,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::None);
}

#[test]
fn ctrl_w_works_in_history_modal() {
    // Free-text modals (history search, models, provider editor) accept the
    // same line-editing vocabulary as the main prompt so the user is
    // never trapped mid-query.
    let mut input = "fuzzy query".to_string();
    let mut cursor = 11;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        SurfaceFixture::HistorySearch,
        false,
    );
    assert_eq!(input, "fuzzy ");
    assert_eq!(cursor, 6);
}

#[test]
fn ctrl_keys_do_not_insert_literal_chars() {
    // Regression guard: none of the new Ctrl/Alt shortcuts may fall
    // through to the `Char(c)` insertion path. Each must leave the
    // buffer text untouched when there is nothing to delete.
    let mut input = String::new();
    let mut cursor = 0;
    for (code, mods) in [
        (KeyCode::Char('w'), KeyModifiers::CONTROL),
        (KeyCode::Char('u'), KeyModifiers::CONTROL),
        (KeyCode::Char('k'), KeyModifiers::CONTROL),
        (KeyCode::Char('b'), KeyModifiers::ALT),
        (KeyCode::Char('f'), KeyModifiers::ALT),
        (KeyCode::Char('d'), KeyModifiers::ALT),
    ] {
        let action = run_key(
            &mut input,
            &mut cursor,
            code,
            mods,
            SurfaceFixture::None,
            false,
        );
        assert_eq!(action, InputAction::None);
        assert!(input.is_empty());
        assert_eq!(cursor, 0);
    }
}

#[test]
fn typing_in_history_modal_appends_to_query() {
    // The history modal borrows the input line as the fuzzy query, so each
    // printable char must insert into `input` exactly like the ApiKey /
    // Endpoint / ModelName modals do.
    let mut input = String::new();
    let mut cursor = 0;
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('g'),
        KeyModifiers::NONE,
    );
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('i'),
        KeyModifiers::NONE,
    );
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('t'),
        KeyModifiers::NONE,
    );
    assert_eq!(input, "git");
    assert_eq!(cursor, 3);
}

#[test]
fn backspace_in_history_modal_trims_query() {
    let mut input = "rust".to_string();
    let mut cursor = 4;
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Backspace,
        KeyModifiers::NONE,
    );
    assert_eq!(input, "rus");
    assert_eq!(cursor, 3);
}

#[test]
fn typing_in_history_panel_inserts_into_filter() {
    // The composer is always the live filter: a printable key splices into
    // the query buffer and narrows the list. There is no inert browse mode.
    let mut input = String::new();
    let mut cursor = 0;
    let action = run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('g'),
        KeyModifiers::NONE,
    );
    assert_eq!(action, InputAction::InsertChar('g'));
    assert_eq!(input, "g");
    assert_eq!(cursor, 1);
}

#[test]
fn slash_in_history_panel_inserts_literal() {
    // `/` is just another query character — the composer is always the
    // filter, so it splices into the buffer rather than toggling a mode.
    let mut input = String::new();
    let mut cursor = 0;
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('/'),
        KeyModifiers::NONE,
    );
    assert_eq!(input, "/");
    assert_eq!(cursor, 1);
}

#[test]
fn ctrl_r_opens_history_modal_when_no_modal_is_open() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        )),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::OpenHistory);

    let action = route_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        )),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: Some(crate::surfaces::OverlaySurface::Dialog(
                crate::surfaces::DialogKind::HistorySearch,
            )),
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
}

#[test]
fn ctrl_v_returns_paste_in_free_text_modals() {
    // Ctrl+V routes to InputAction::Paste on the main prompt and in
    // every free-text modal (provider editor, provider picker filter,
    // history search). Other modals drop it so a paste never leaks into
    // a read-only overlay or the permission sheet.
    let free_text_modals = [
        SurfaceFixture::None,
        SurfaceFixture::ModelEditor,
        SurfaceFixture::Models,
        SurfaceFixture::Connections,
        SurfaceFixture::HistorySearch,
    ];
    for modal in free_text_modals {
        let mut input = String::new();
        let mut cursor = 0;
        let action = run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
            modal,
            false,
        );
        assert_eq!(
            action,
            InputAction::Paste,
            "Ctrl+V should paste in free-text modal"
        );
        assert!(input.is_empty(), "Ctrl+V must not mutate the buffer itself");
    }

    for modal in [SurfaceFixture::UsageStats, SurfaceFixture::Sessions] {
        let mut input = String::new();
        let mut cursor = 0;
        let action = run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
            modal,
            false,
        );
        assert_eq!(
            action,
            InputAction::None,
            "Ctrl+V should be a no-op in non-text modal"
        );
    }
}

#[test]
fn sgr_guard_drops_split_sgr_mouse_sequence() {
    // The exact symptom from the field: a mouse report crossterm split into
    // individual chars. `ESC [ < 0 ; 3 5 ; 4 6 M` — the `[ < … M` payload
    // is dropped, but the leading Esc is *delivered* (it is a real control
    // key, never inserted as text, and is the double-Esc interrupt path).
    let seq: Vec<Event> = [
        leaked_esc(),
        leaked_char('['),
        leaked_char('<'),
        leaked_char('0'),
        leaked_char(';'),
        leaked_char('3'),
        leaked_char('5'),
        leaked_char(';'),
        leaked_char('4'),
        leaked_char('6'),
        leaked_char('M'),
    ]
    .into_iter()
    .collect();
    let (accepted, dropped) = drain_guard(&seq);
    assert_eq!(
        accepted, 1,
        "the leading Esc is delivered, the rest dropped"
    );
    assert_eq!(dropped, seq.len() - 1);
}

#[test]
fn sgr_guard_drops_release_variant_lowercase_m() {
    // SGR release uses lowercase `m`. Same coverage as the press variant:
    // only the leading Esc is delivered.
    let seq: Vec<Event> = [
        leaked_esc(),
        leaked_char('['),
        leaked_char('<'),
        leaked_char('3'),
        leaked_char('5'),
        leaked_char(';'),
        leaked_char('5'),
        leaked_char('6'),
        leaked_char('m'),
    ]
    .into_iter()
    .collect();
    let (accepted, _) = drain_guard(&seq);
    assert_eq!(accepted, 1);
}

#[test]
fn sgr_guard_drops_run_of_split_sequences() {
    // The real complaint showed *many* sequences back to back (resize drag).
    // The guard must resync to idle after each terminating M/m and catch
    // the next one too. Each sequence's leading Esc is delivered; the
    // remaining payload bytes are swallowed.
    let one = |b: char| {
        vec![
            leaked_esc(),
            leaked_char('['),
            leaked_char('<'),
            leaked_char(b),
            leaked_char(';'),
            leaked_char('1'),
            leaked_char('M'),
        ]
    };
    let seq: Vec<Event> = [one('0'), one('3'), one('5')]
        .into_iter()
        .flatten()
        .collect();
    let (accepted, _) = drain_guard(&seq);
    assert_eq!(accepted, 3, "each sequence delivers its leading Esc");
}

#[test]
fn sgr_guard_passes_through_normal_typing() {
    // Ordinary typing must be unaffected — the guard never enters a
    // tracking state and hands every char back as Accept.
    let seq: Vec<Event> = ['h', 'e', 'l', 'l', 'o']
        .into_iter()
        .map(leaked_char)
        .collect();
    let (accepted, dropped) = drain_guard(&seq);
    assert_eq!(accepted, seq.len());
    assert_eq!(dropped, 0);
}

#[test]
fn sgr_guard_delivers_lone_esc() {
    // Regression: a standalone Esc (e.g. the first of a double-Esc
    // interrupt) must reach the app. The previous guard dropped it as a
    // suspected leak prefix, which broke double-Esc interrupt entirely.
    let mut g = SgrLeakGuard::default();
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    // Not idle: it armed the tracker so a following `[` still opens a leak.
    assert!(!g.is_idle());
    // A subsequent normal char aborts the tracking and is delivered too.
    assert!(matches!(g.feed(&leaked_char('x')), Feed::Accept));
    assert!(g.is_idle());
}

#[test]
fn sgr_guard_delivers_double_esc() {
    // The double-Esc interrupt path: two Escs with nothing between them.
    // Neither is part of an SGR sequence (a real leak has `[` next, never
    // another Esc), so both must be delivered.
    let mut g = SgrLeakGuard::default();
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(!g.is_idle());
}

#[test]
fn sgr_guard_recovers_from_aborted_prefix() {
    // `ESC [` followed by something other than `<` is a real CSI (e.g. an
    // arrow key's payload). The leading Esc is delivered; `[` is dropped
    // (it can only be leak noise from this state); the aborting char is
    // delivered and the guard returns to idle.
    let mut g = SgrLeakGuard::default();
    // ESC [ A = Up arrow, delivered as separate chars by a broken read.
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(matches!(g.feed(&leaked_char('[')), Feed::Drop));
    // 'A' is not the SGR intro: the guard aborts and *this* event is
    // accepted (returned to the caller to deal with), then goes idle.
    assert!(matches!(g.feed(&leaked_char('A')), Feed::Accept));
    assert!(g.is_idle());
    // Subsequent normal typing is accepted.
    assert!(matches!(g.feed(&leaked_char('x')), Feed::Accept));
}

#[test]
fn sgr_guard_resets_on_non_key_events() {
    // A genuine parsed mouse event or a resize resyncs the tracker, so a
    // half-buffered prefix can't poison the next real interaction.
    let mut g = SgrLeakGuard::default();
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(matches!(g.feed(&leaked_char('[')), Feed::Drop));
    assert!(matches!(g.feed(&Event::Resize(80, 24)), Feed::Accept));
    assert!(g.is_idle());
    assert!(matches!(g.feed(&leaked_char('x')), Feed::Accept));
}

#[test]
fn sgr_guard_survives_garbage_inside_payload() {
    // A malformed payload (non-digit, non-;) while inside an SGR sequence
    // resyncs to idle instead of swallowing arbitrary following text.
    let mut g = SgrLeakGuard::default();
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(matches!(g.feed(&leaked_char('[')), Feed::Drop));
    assert!(matches!(g.feed(&leaked_char('<')), Feed::Drop));
    // A letter that is not the terminator: abort and resync.
    assert!(matches!(g.feed(&leaked_char('Z')), Feed::Accept));
    assert!(g.is_idle());
}

#[test]
fn text_modal_commands_resolve_and_consume_composer() {
    // Slash commands that open an interactive modal are intercepted locally:
    // they resolve to a data-less `Open*` action (not `SendSlash`) and consume
    // the composer text. The event loop snapshots the composer before dispatch
    // and relies on `is_text_modal_command` to recover that text for input
    // history + transcript recording — so these must stay in sync with the
    // intercepted set in `route_event`.
    for (cmd, expected) in [
        ("/sessions", InputAction::OpenSessions),
        ("/models", InputAction::OpenModels),
        ("/connections", InputAction::OpenConnections),
        ("/permissions", InputAction::OpenPermissions),
        ("/tools", InputAction::OpenTools),
        ("/mcp", InputAction::OpenMcp),
        ("/skills", InputAction::OpenSkills),
        ("/settings", InputAction::OpenConfig),
        ("/config", InputAction::OpenConfig),
    ] {
        let mut input = cmd.to_string();
        let action = enter(&mut input, true);
        assert_eq!(action, expected, "modal command {cmd}");
        assert!(
            input.is_empty(),
            "composer must be consumed for {cmd}, got {input:?}"
        );
        assert!(
            action.is_text_modal_command(),
            "{cmd} must be flagged as a text modal command so its \
             invocation is recorded in history + transcript"
        );
    }
}

#[test]
fn keybinding_modals_are_not_text_commands() {
    // Ctrl+R opens a modal via a keybinding, not by typing a slash
    // command, so it must NOT be flagged: it consumes no composer text and
    // therefore has nothing to record in input history.
    assert!(!InputAction::OpenHistory.is_text_modal_command());
    // `/exit` resolves to Quit — it is not a replayable input, so it is
    // deliberately excluded from the recorded set.
    assert!(!InputAction::Quit.is_text_modal_command());
    // Notification-style slash commands carry their text on the action, so the
    // event loop records it from the action itself rather than via this hint.
    assert!(!InputAction::SendSlash("/pursue".to_string()).is_text_modal_command());
}

#[test]
fn ctrl_l_opens_command_palette() {
    let mut input = "draft".to_string();
    let mut cursor = 5;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent {
            code: KeyCode::Char('l'),
            modifiers: KeyModifiers::CONTROL,
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
    assert_eq!(action, InputAction::ViewSwitcherToggle);
    assert_eq!(input, "draft");
}

#[test]
fn sessions_modal_n_key_triggers_create_new_session() {
    let mut input = String::new();
    let mut cursor = 0;
    let action_n = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('n'),
        KeyModifiers::NONE,
        SurfaceFixture::Sessions,
        false,
    );
    assert_eq!(action_n, InputAction::CreateNewSession);

    let action_big_n = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('N'),
        KeyModifiers::NONE,
        SurfaceFixture::Sessions,
        false,
    );
    assert_eq!(action_big_n, InputAction::CreateNewSession);
}

#[test]
fn digit_on_effort_field_jumps_to_that_ladder_rung() {
    // The flat segmented selector makes direct selection natural: `1`..=`7`
    // map to ladder rungs (0-indexed here), and the key never lands in the
    // borrowed input line.
    let mut input = "high".to_string();
    assert_eq!(
        editor_key(KeyCode::Char('1'), 1, &mut input),
        InputAction::ModelEditorEffortJump { index: 0 }
    );
    assert_eq!(
        editor_key(KeyCode::Char('7'), 1, &mut input),
        InputAction::ModelEditorEffortJump { index: 6 }
    );
    assert_eq!(input, "high", "digit must not edit the effort field's line");
}

#[test]
fn digit_jump_is_scoped_to_the_effort_field() {
    // `0` is not a tier (ladder rungs are 1-based), and digits on the API-key
    // field stay ordinary typed characters (a key can contain digits).
    let mut input = "high".to_string();
    assert_eq!(
        editor_key(KeyCode::Char('0'), 1, &mut input),
        InputAction::InsertChar('0'),
        "`0` is not a rung, so it falls through to the borrowed line"
    );

    let mut key = "sk-".to_string();
    assert_eq!(
        editor_key(KeyCode::Char('5'), 0, &mut key),
        InputAction::InsertChar('5'),
        "digits on the API-key field type normally"
    );
    assert_eq!(key, "sk-5");
}

#[test]
fn delete_key_removes_char_after_cursor_and_inert_at_end() {
    // Forward delete removes character after caret; inert when at buffer end.
    let mut input = "hello".to_string();
    let mut cursor = 2;
    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::DeleteForward
    );
    assert_eq!(input, "helo");
    assert_eq!(cursor, 2, "forward delete must not move the caret");

    let mut input = "hello".to_string();
    let mut cursor = 5;
    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::None
    );
    assert_eq!(input, "hello");
    assert_eq!(cursor, 5);
}

#[test]
fn delete_key_removes_full_grapheme_cluster() {
    // A CJK char occupies 3 bytes but must vanish as one user-visible glyph;
    // the byte math must never split what the user sees as one character.
    let mut input = "中abc".to_string();
    let mut cursor = 0;
    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::DeleteForward
    );
    assert_eq!(input, "abc");
    assert_eq!(cursor, 0);
}

#[test]
fn horizontal_motion_skips_entire_zwj_grapheme() {
    let family = "👨‍👩‍👧‍👦";
    let mut input = format!("a{family}b");
    let mut cursor = 1 + family.chars().count();

    assert_eq!(
        compose_key(KeyCode::Left, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::None
    );
    assert_eq!(cursor, 1, "Left lands before the whole family emoji");

    compose_key(KeyCode::Right, KeyModifiers::NONE, &mut input, &mut cursor);
    assert_eq!(
        cursor,
        1 + family.chars().count(),
        "Right lands after the whole family emoji"
    );
}

#[test]
fn backspace_removes_entire_zwj_grapheme() {
    let family = "👨‍👩‍👧‍👦";
    let mut input = format!("a{family}b");
    let mut cursor = 1 + family.chars().count();

    assert_eq!(
        compose_key(
            KeyCode::Backspace,
            KeyModifiers::NONE,
            &mut input,
            &mut cursor,
        ),
        InputAction::Backspace
    );
    assert_eq!(input, "ab");
    assert_eq!(cursor, 1);
}

#[test]
fn delete_removes_entire_combining_grapheme() {
    let mut input = "ae\u{301}b".to_string();
    let mut cursor = 1;

    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::DeleteForward
    );
    assert_eq!(input, "ab");
    assert_eq!(cursor, 1);
}

#[test]
fn delete_key_eats_whole_attachment_chip() {
    // Forward-deleting the `[` of an attachment chip removes the whole chip
    // (plus the trailing space a paste inserts) in one keystroke, mirroring
    // the chip-aware Backspace.
    let chip = crate::composer_attachments::image_chip(1, 42);
    let mut input = format!("hello {chip} world");
    let mut cursor = "hello ".len();
    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::DeleteForward
    );
    assert_eq!(input, "hello world", "chip + trailing space both go");
    assert_eq!(cursor, "hello ".len());
}

#[test]
fn delete_key_inert_outside_free_text() {
    // In a read-only modal (Usage stats) the Del key must do nothing — the
    // `edits_input_field` gate keeps it from mutating the borrowed composer.
    let mut input = "hello".to_string();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    assert_eq!(
        route_event(
            Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            Dispatch {
                overlay: Some(crate::surfaces::OverlaySurface::Dialog(
                    crate::surfaces::DialogKind::UsageStats
                )),
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        ),
        InputAction::None
    );
    assert_eq!(input, "hello");
}

#[test]
fn delete_key_closes_selected_view_in_switcher() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    assert_eq!(
        route_event(
            Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            Dispatch {
                overlay: Some(crate::surfaces::OverlaySurface::Dialog(
                    crate::surfaces::DialogKind::Switcher
                )),
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        ),
        InputAction::ViewCloseSelected
    );
}

#[test]
fn host_prompt_delete_key_removes_forward_char() {
    // The /host dashboard's inline prompt borrows the composer line; Del
    // there deletes forward too (the branch swallows the key and returns
    // None — the mutation is the whole effect).
    let mut input = "abc".to_string();
    let mut cursor = 1;
    let mut drag = SelectionDrag::default();
    assert_eq!(
        route_event(
            Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            Dispatch {
                scene: crate::surfaces::SceneKind::Dashboard,
                ..Default::default()
            },
            &ModalKeys {
                host_prompting: true,
                ..Default::default()
            },
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        ),
        InputAction::None
    );
    assert_eq!(input, "ac");
    assert_eq!(cursor, 1);
}

#[test]
fn tab_is_inert_without_a_completion() {
    // ADR-0173: Tab belongs to completion only — no plane switching. With no
    // completion up it is inert in both compose and "browse" (the focused
    // step is a transient selection, not a mode).
    for focused in [false, true] {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        let action = route_event(
            Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            Dispatch {
                focused_target: focused,
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        );
        assert_eq!(action, InputAction::None);
    }
}

#[test]
fn enter_while_running_in_steer_mode_emits_steer_immediate() {
    let mut input = "steer command".to_string();
    let mut cursor = 13;
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
            composer_send_mode: crate::app::ComposerSendMode::Steer,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::SteerImmediate("steer command".to_string())
    );
    assert_eq!(input, "");
    assert_eq!(cursor, 0);
}

#[test]
fn tab_while_running_emits_toggle_send_mode() {
    let mut input = "some text".to_string();
    let mut cursor = 9;
    let mut drag = SelectionDrag::default();

    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
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
    assert_eq!(action, InputAction::ToggleSendMode);
}

#[test]
fn enter_while_running_in_follow_up_mode_emits_queue_follow_up() {
    let mut input = "followup text".to_string();
    let mut cursor = 13;
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
        InputAction::QueueFollowUp("followup text".to_string())
    );
    assert_eq!(input, "");
    assert_eq!(cursor, 0);
}

#[test]
fn printable_char_in_transcript_is_inert_and_does_not_mutate_composer() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();

    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
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
fn key_release_events_are_ignored() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();

    // Key release for typing char
    let char_release = KeyEvent::new_with_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    let action = route_event(
        Event::Key(char_release),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "");
    assert_eq!(cursor, 0);

    // Key release for Ctrl+C
    let ctrl_c_release = KeyEvent::new_with_kind(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        KeyEventKind::Release,
    );
    let action = route_event(
        Event::Key(ctrl_c_release),
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
fn key_repeat_events_are_processed() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();

    let char_repeat =
        KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat);
    let action = route_event(
        Event::Key(char_repeat),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::InsertChar('a'));
    assert_eq!(input, "a");
    assert_eq!(cursor, 1);
}

#[test]
fn resolve_block_resolves_composer_chrome_to_input_sentinel() {
    let mut map = LayoutMap::new();
    map.set_composer_rect(nuotc::Rect::new(0, 10, 50, 4));

    // Middle-click on chrome row (e.g. y = 10 or 13) resolves to composer block
    assert_eq!(
        resolve_block(&map, 5, 10),
        Some((crate::model::layout::INPUT_MSG_IDX, 0))
    );
    assert_eq!(
        resolve_block(&map, 5, 13),
        Some((crate::model::layout::INPUT_MSG_IDX, 0))
    );
    // Outside composer
    assert_eq!(resolve_block(&map, 5, 15), None);
}
