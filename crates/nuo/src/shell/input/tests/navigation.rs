//! Cursor movement tests: arrows, word jumps, line-aware movement, Home/End/PageUp/PageDown.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::shell::keymap::Key;

#[test]
fn bare_arrows_walk_steps_when_focused() {
    // When a target is focused, bare ↑/↓ navigate targets.
    // When composer is active, bare ↑/↓ hand off to history recall.
    assert_eq!(key_with_focus(KeyCode::Up), InputAction::FocusPrevTarget);
    assert_eq!(key_with_focus(KeyCode::Down), InputAction::FocusNextTarget);
}

#[test]
fn home_and_end_navigate_line_in_composer_and_scroll_in_focus() {
    // Home/End in the composer move caret to line-start/end (readline convention).
    // In target focus or browse focus, Home/End scroll the transcript to top/bottom.
    let mut input = "hello".to_string();
    let mut cursor = 3;

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Home,
        KeyModifiers::NONE,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 0, "Home moves caret to start of line in composer");

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::End,
        KeyModifiers::NONE,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 5, "End moves caret to end of line in composer");

    // When target or browse focus is active, Home/End scroll transcript
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Home,
        KeyModifiers::NONE,
        SurfaceFixture::None,
        true,
    );
    assert_eq!(action, InputAction::ScrollTop);

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::End,
        KeyModifiers::NONE,
        SurfaceFixture::None,
        true,
    );
    assert_eq!(action, InputAction::ScrollBottom);
}

#[test]
fn home_and_end_scroll_in_browse_zone() {
    // In Browse the thread owns focus, so Home/End drive scrolling
    // instead of moving the (unfocused) input caret.
    let mut input = "hello".to_string();
    let mut cursor = 3;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::NONE,
            SurfaceFixture::None,
            true
        ),
        InputAction::ScrollTop
    );
    assert_eq!(cursor, 3, "Browse Home must not touch the caret");
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::NONE,
            SurfaceFixture::None,
            true
        ),
        InputAction::ScrollBottom
    );
    assert_eq!(cursor, 3);
}

#[test]
fn permission_sheet_left_right_tab_cycle_options() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::Left,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Permission,
            false
        ),
        InputAction::PermissionPrevOption
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::Right,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Permission,
            false
        ),
        InputAction::PermissionNextOption
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::Tab,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Permission,
            false
        ),
        InputAction::PermissionNextOption
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::BackTab,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Permission,
            false
        ),
        InputAction::PermissionPrevOption
    );
}

#[test]
fn question_sheet_tab_and_arrows_navigate_questions() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::Tab,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Question,
            false
        ),
        InputAction::QuestionNext
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::BackTab,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Question,
            false
        ),
        InputAction::QuestionPrevious
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::Right,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Question,
            false
        ),
        InputAction::QuestionNext
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::Left,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Question,
            false
        ),
        InputAction::QuestionPrevious
    );
}

#[test]
fn home_and_end_scroll_in_permission_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Permission,
            false
        ),
        InputAction::ScrollTop
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Permission,
            false
        ),
        InputAction::ScrollBottom
    );
}

#[test]
fn ctrl_home_and_end_scroll_regardless_of_focus() {
    let mut input = "hello".to_string();
    let mut cursor = 3;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::CONTROL,
            SurfaceFixture::None,
            false
        ),
        InputAction::ScrollTop
    );
    assert_eq!(cursor, 3, "Ctrl+Home must not move the caret");
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::CONTROL,
            SurfaceFixture::None,
            false
        ),
        InputAction::ScrollBottom
    );
    assert_eq!(cursor, 3, "Ctrl+End must not move the caret");
}

#[test]
fn page_keys_scroll_question_modal_body() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::PageUp,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Question,
            false
        ),
        InputAction::ScrollPageUp
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::PageDown,
            KeyModifiers::NONE,
            crate::shell::sheet::SheetKind::Question,
            false
        ),
        InputAction::ScrollPageDown
    );
}

/// Every modal that paints its own scrollable body must route PageUp /
/// PageDown to a body page-scroll action — not just the four modals the
/// old gate covered (None / Permission / Question / OauthPending). This
/// is the regression guard for "any modal should support scroll".
#[test]
fn page_keys_scroll_every_scrollable_modal_body() {
    let scrollable = [
        SurfaceFixture::UsageStats,
        SurfaceFixture::Permissions,
        SurfaceFixture::Config,
        SurfaceFixture::ThreadStats,
        SurfaceFixture::SessionTrace,
        SurfaceFixture::OauthPending,
        SurfaceFixture::ProviderPreset,
        SurfaceFixture::CustomProvider,
        SurfaceFixture::Tools,
        SurfaceFixture::Mcp,
        SurfaceFixture::Skills,
        SurfaceFixture::Sessions,
        SurfaceFixture::Queue,
        SurfaceFixture::HistorySearch,
        SurfaceFixture::Connections,
        SurfaceFixture::Models,
    ];
    for fixture in scrollable {
        let mut input = String::new();
        let mut cursor = 0;
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageUp,
                KeyModifiers::NONE,
                fixture,
                false
            ),
            InputAction::ScrollPageUp,
            "PageUp should page-scroll the {fixture:?} modal body"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                fixture,
                false
            ),
            InputAction::ScrollPageDown,
            "PageDown should page-scroll the {fixture:?} modal body"
        );
    }
}

/// The caret-owning text editors (ModelEditor, InputInjection) have no body
/// scroll, so PageUp / PageDown and Ctrl+↑ / Ctrl+↓ must be inert there
/// (no-op), not a stray page-scroll or transcript focus gesture.
#[test]
fn page_keys_are_inert_in_caret_editors() {
    {
        let mut input = String::new();
        let mut cursor = 0;
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageUp,
                KeyModifiers::NONE,
                SurfaceFixture::ModelEditor,
                false
            ),
            InputAction::None,
            "PageUp should be a no-op in ModelEditor"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                SurfaceFixture::ModelEditor,
                false
            ),
            InputAction::None,
            "PageDown should be a no-op in ModelEditor"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::Up,
                KeyModifiers::CONTROL,
                SurfaceFixture::ModelEditor,
                false
            ),
            InputAction::None,
            "Ctrl+Up should be a no-op in ModelEditor"
        );
    }
}

#[test]
fn home_and_end_move_caret_in_free_text_modals() {
    // The unified provider editor borrows the input line for one field at a
    // time; Home/End should edit there too, not be swallowed.
    for fixture in [SurfaceFixture::ModelEditor, SurfaceFixture::HistorySearch] {
        let mut input = "abc".to_string();
        let mut cursor = 2;
        let action = run_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::NONE,
            fixture,
            false,
        );
        assert_eq!(action, InputAction::None);
        assert_eq!(cursor, 0, "Home should reach line start");

        let action = run_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::NONE,
            fixture,
            false,
        );
        assert_eq!(action, InputAction::None);
        assert_eq!(cursor, 3, "End should reach line end");
    }
}

#[test]
fn line_aware_movement_respects_newlines() {
    // Multi-line input: Home/End/Ctrl+A/Ctrl+E operate on the current
    // logical line, not the whole buffer.
    let mut input = "line1\nline2\nline3".to_string();
    // Place the caret in the middle of the second line ("line2").
    // "line1\n" = 6 chars, then 2 more into "line2" -> char index 8.
    let mut cursor = 8;

    // Ctrl+A -> start of "line2" (char index 6, just past the first '\n').
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 6, "Ctrl+A should land at start of current line");

    // Ctrl+E -> end of "line2" (char index 11, just before the second '\n').
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('e'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 11, "Ctrl+E should land at end of current line");

    // Ctrl+A snaps back to the line start.
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 6);
    // Ctrl+E snaps back to the line end without running off the buffer.
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('e'),
        KeyModifiers::CONTROL,
        SurfaceFixture::None,
        false,
    );
    assert_eq!(cursor, 11);
}

#[test]
fn pageup_scrolls_transcript_page() {
    assert_eq!(pageup_key(), InputAction::ScrollPageUp);
}

#[test]
fn arrows_navigate_completion_menu_while_command_is_partial() {
    // A partially-typed `/` command keeps the completion menu interactive:
    // ↑/↓ cycle its candidates (SuggestPrev/SuggestNext) rather than
    // walking history, so the user can keep switching toward the command
    // they want.
    let kind = crate::shell::CompletionKind::Slash;
    assert_eq!(
        compose_key_with_completion(KeyCode::Down, kind, 5, false),
        InputAction::SuggestNext
    );
    assert_eq!(
        compose_key_with_completion(KeyCode::Up, kind, 5, false),
        InputAction::SuggestPrev
    );
}

#[test]
fn arrows_recall_history_once_command_is_fully_typed() {
    // Once a command is resolved (exact match), completion popup closes and
    // arrows hand off at the draft's edges to inline history recall
    // a single-line command draft is all edge.
    let kind = crate::shell::CompletionKind::Slash;
    assert_eq!(
        compose_key_with_completion(KeyCode::Down, kind, 1, true),
        InputAction::HistoryNext,
        "↓ on an exact-match command recalls the next history entry"
    );
    assert_eq!(
        compose_key_with_completion(KeyCode::Up, kind, 1, true),
        InputAction::HistoryPrev,
        "↑ on an exact-match command recalls the previous history entry"
    );
}

#[test]
fn up_arrow_in_browse_hands_off_to_history() {
    // The queued-message recall only fires from Compose (where the user can
    // actually edit the recalled draft). In Browse (a step selected, no
    // completion), a single-line draft's ↑ still hands off to inline
    // history recall at the edge; step walking stays verb-owned
    // (Alt+↑).
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: None,
            focused_target: false,
            ..Default::default()
        },
        &ModalKeys {
            session_info_detail: false,
            connection_info_detail: false,
            ..Default::default()
        },
        &SheetKeys {
            permission_confirm_always: false,
            ..Default::default()
        },
        &SceneKeys {
            is_responding: false,
            completion_kind: crate::shell::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::HistoryPrev);
}

#[test]
fn up_arrow_walks_lines_in_multiline_and_hands_off_at_top_line() {
    // In a multi-line draft, ↑ moves the caret up a line. From the top
    // line, it hands off to inline history recall.
    let seed = "hello\nworld";
    // Caret at end of second line: ↑ should move to the same column on
    // the first line ("hello", col 5) and stay a caret motion.
    let (action, cur) = multiline_arrow(seed, "hello\nworld".chars().count(), KeyCode::Up);
    assert_eq!(action, InputAction::None);
    assert_eq!(cur, 5, "up should land at col 5 on the first line");

    // Sitting on the first line: ↑ hands off to history recall.
    let (action, _) = multiline_arrow(seed, 5, KeyCode::Up);
    assert_eq!(action, InputAction::HistoryPrev);
}

#[test]
fn down_arrow_walks_lines_in_multiline_and_hands_off_at_bottom_line() {
    let seed = "hello\nworld";
    // Caret at start of first line: ↓ moves to the same column on the
    // second line and stays a caret motion.
    let (action, cur) = multiline_arrow(seed, 0, KeyCode::Down);
    assert_eq!(action, InputAction::None);
    assert_eq!(cur, 6, "down should land at col 0 of the second line");

    // Caret at end of the second line: ↓ hands off to history recall
    // walking forward, or restoring the stashed draft.
    let (action, _) = multiline_arrow(seed, "hello\nworld".chars().count(), KeyCode::Down);
    assert_eq!(action, InputAction::HistoryNext);
}

#[test]
fn up_arrow_clamps_column_to_shorter_line() {
    // Moving up to a shorter line clamps the column to that line's
    // length rather than overshooting into the newline.
    let seed = "hi\nlonger line";
    // Caret at col 7 of the second line ("longer line").
    let start = "hi\n".chars().count() + 7;
    let (action, cur) = multiline_arrow(seed, start, KeyCode::Up);
    assert_eq!(action, InputAction::None);
    assert_eq!(cur, 2, "column should clamp to the first line's length");
}

#[test]
fn resize_event_routes_to_terminal_resized_with_dimensions() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Resize(120, 42),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::TerminalResized {
            cols: 120,
            rows: 42
        }
    );
}

#[test]
fn ctrl_x_arms_the_scene_namespace() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::SetSceneNamespaceArmed(true));
}

/// Every second stroke resolves through the namespace's own verb
/// table (`keymap::scene_namespace`) — the same table the which-key card
/// renders. Case is folded, because a leader chord is typed blind.
#[test]
fn scene_namespace_second_strokes_resolve_through_the_verb_table() {
    use crate::shell::keymap::scene_namespace::{SceneVerb, strokes_of};

    let armed = Dispatch {
        scene_namespace_armed: true,
        notice_viewer_open: false,
        ..Default::default()
    };
    use crate::shell::keymap::{KeyCode as ChordCode, KeyModifiers as ChordMods};
    let press = |code: ChordCode, mods: ChordMods, dispatch: Dispatch| {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        route_event(
            Event::Key(crate::shell::input::keystroke::event_from_chord(Key {
                code,
                modifiers: mods,
            })),
            &mut input,
            &mut cursor,
            dispatch,
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        )
    };

    // Every declared stroke maps to the action its verb names.
    for stroke in strokes_of() {
        let verb = SceneVerb::from_stroke(stroke).expect("declared stroke resolves");
        let expected = match verb {
            SceneVerb::Leave => InputAction::CloseTab,
            SceneVerb::Switcher => InputAction::ViewSwitcherToggle,
            SceneVerb::Threads => InputAction::OpenSessions,
            SceneVerb::Dashboard => InputAction::NavigateDashboard,
            SceneVerb::Settings => InputAction::NavigateSettings,
        };
        if let ChordCode::Char(c) = stroke.code {
            // Lowercase spelling.
            assert_eq!(
                press(ChordCode::Char(c), stroke.modifiers, armed.clone()),
                expected,
                "{verb:?} lowercase"
            );
            // Uppercase spelling (`C-x W`) folds to the same verb.
            if !stroke.modifiers.contains(ChordMods::CONTROL) {
                assert_eq!(
                    press(
                        ChordCode::Char(c.to_ascii_uppercase()),
                        stroke.modifiers,
                        armed.clone()
                    ),
                    expected,
                    "{verb:?} uppercase folds"
                );
            }
        }
    }

    // `Esc` and `C-g` cancel; so does any unclaimed stroke (including `k`, `c`, `C-c`
    // excised by ADR-0077) — a half-typed chord must never fall through and fire a global.
    for (code, mods) in [
        (ChordCode::Esc, ChordMods::NONE),
        (ChordCode::Char('g'), ChordMods::CONTROL),
        (ChordCode::Char('k'), ChordMods::NONE),
        (ChordCode::Char('c'), ChordMods::CONTROL),
        (ChordCode::Char('c'), ChordMods::NONE),
        (ChordCode::Char('z'), ChordMods::NONE),
        (ChordCode::Enter, ChordMods::NONE),
    ] {
        assert_eq!(
            press(code, mods, armed.clone()),
            InputAction::CancelSceneNamespace,
            "{code:?}+{mods:?} cancels"
        );
    }

    // Re-pressing the opener re-arms rather than cancelling.
    assert_eq!(
        press(ChordCode::Char('x'), ChordMods::CONTROL, armed),
        InputAction::SetSceneNamespaceArmed(true)
    );
}

/// ADR-0077 [INV-TUI-SURF-01]: Neither `c`, `C-c`, nor `k` carry any meaning inside the namespace;
/// they all cancel safely instead of quitting or closing tabs.
#[test]
fn scene_namespace_excised_strokes_cancel_safely() {
    use crate::shell::keymap::{KeyCode as ChordCode, KeyModifiers as ChordMods};
    for (code, mods) in [
        (ChordCode::Char('c'), ChordMods::NONE),
        (ChordCode::Char('c'), ChordMods::CONTROL),
        (ChordCode::Char('k'), ChordMods::NONE),
    ] {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        let action = route_event(
            Event::Key(crate::shell::input::keystroke::event_from_chord(Key {
                code,
                modifiers: mods,
            })),
            &mut input,
            &mut cursor,
            Dispatch {
                scene_namespace_armed: true,
                notice_viewer_open: false,
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        );
        assert_eq!(action, InputAction::CancelSceneNamespace);
    }
}

#[test]
fn scene_namespace_s_opens_sessions() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            scene_namespace_armed: true,
            notice_viewer_open: false,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::OpenSessions);
}

#[test]
fn scene_namespace_d_navigates_to_dashboard() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            scene_namespace_armed: true,
            notice_viewer_open: false,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::NavigateDashboard);
}

#[test]
fn scene_namespace_comma_navigates_to_settings() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char(','), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            scene_namespace_armed: true,
            notice_viewer_open: false,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::NavigateSettings);
}

/// The scenes have **no** `q` exit. `q` is an ordinary printable on
/// the Dashboard (it seeds the console composer) and an unclaimed char on
/// Settings, so leaving either is the `C-x` namespace alone.
#[test]
fn q_is_not_a_scene_exit_on_dashboard() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let dispatch = Dispatch {
        scene: crate::shell::surfaces::SceneKind::Dashboard,
        ..Default::default()
    };
    let modal_keys = ModalKeys {
        host_prompting: false,
        ..Default::default()
    };
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        dispatch,
        &modal_keys,
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::HostPromptSeed('q'),
        "`q` types like every other unclaimed letter on the console"
    );
}

#[test]
fn q_is_not_a_scene_exit_on_settings() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let dispatch = Dispatch {
        scene: crate::shell::surfaces::SceneKind::Settings,
        ..Default::default()
    };
    let modal_keys = ModalKeys {
        config_focus: crate::shell::overlays::ConfigFocus::Categories,
        ..Default::default()
    };
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        dispatch,
        &modal_keys,
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::None,
        "the Settings scene owns no exit chord, so `q` is unclaimed"
    );
}

/// Esc on a scene never leaves it. On the Dashboard and Settings
/// scenes it resolves to the scene-local step-back verb, whatever sub-layer is
/// open — the scene's own exit is `C-x w` (or `q`) only.
#[test]
fn esc_on_dashboard_and_settings_is_scene_local_back() {
    for scene in [
        crate::shell::surfaces::SceneKind::Dashboard,
        crate::shell::surfaces::SceneKind::Settings,
    ] {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        let action = route_event(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            Dispatch {
                scene,
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        );
        assert_eq!(
            action,
            InputAction::SceneBack,
            "Esc is a step back, never a scene exit, on {scene:?}"
        );
    }
}

#[test]
fn alt_digits_route_to_select_tab() {
    for digit in '1'..='9' {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        let action = route_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char(digit),
                KeyModifiers::ALT,
            )),
            &mut input,
            &mut cursor,
            Dispatch::default(),
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        );
        let expected_idx = (digit as usize) - ('1' as usize);
        assert_eq!(action, InputAction::SelectTab(expected_idx));
    }
}

#[test]
fn alt_w_routes_to_close_tab() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::ALT,
        )),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseTab);
}

#[test]
fn alt_brackets_route_to_tab_history() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();

    let back = route_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('['),
            KeyModifiers::ALT,
        )),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(back, InputAction::TabHistoryBack);

    let fwd = route_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char(']'),
            KeyModifiers::ALT,
        )),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(fwd, InputAction::TabHistoryForward);
}

/// The reported defect: after Ctrl-J opens an empty line, the arrows must be
/// able to *stand* on it.
///
/// Caret motion walks the composer's **visual rows** (what the panel paints),
/// not the buffer's `\n`-separated lines, and every row — including a zero-width
/// empty one — is a legal caret cell. Before the grid owned both projections,
/// the empty row was painted but no caret byte could resolve onto it, so ↑/↓
/// skipped straight over it.
#[test]
fn ctrl_j_empty_line_is_a_caret_row() {
    fn press(
        input: &mut String,
        cursor: &mut usize,
        motion: &mut crate::shell::composer_grid::CaretMotion,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> InputAction {
        let mut drag = SelectionDrag::default();
        route_event_motion(
            Event::Key(KeyEvent::new(code, modifiers)),
            input,
            cursor,
            motion,
            Dispatch::default(),
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        )
    }

    // Type "a\nb", put the caret at the end of the first line, press Ctrl-J.
    let mut input = String::from("a\nb");
    let mut cursor = 1;
    let mut motion = crate::shell::composer_grid::CaretMotion::default();

    assert_eq!(
        press(
            &mut input,
            &mut cursor,
            &mut motion,
            KeyCode::Char('j'),
            KeyModifiers::CONTROL
        ),
        InputAction::None
    );
    assert_eq!(input, "a\n\nb", "Ctrl-J opens a line between the two");
    assert_eq!(cursor, 2, "the caret sits in the line Ctrl-J opened");

    // ↓ leaves the empty line for the text below it…
    assert_eq!(
        press(
            &mut input,
            &mut cursor,
            &mut motion,
            KeyCode::Down,
            KeyModifiers::NONE
        ),
        InputAction::None
    );
    assert_eq!(cursor, 3, "down reaches the line below the empty one");
    // …and ↑ comes back to it, one row at a time.
    assert_eq!(
        press(
            &mut input,
            &mut cursor,
            &mut motion,
            KeyCode::Up,
            KeyModifiers::NONE
        ),
        InputAction::None
    );
    assert_eq!(cursor, 2, "up stops on the empty line, not past it");
    assert_eq!(
        press(
            &mut input,
            &mut cursor,
            &mut motion,
            KeyCode::Up,
            KeyModifiers::NONE
        ),
        InputAction::None
    );
    assert_eq!(cursor, 0, "and then on the first line");
    // Only the true top row hands off to history recall.
    assert_eq!(
        press(
            &mut input,
            &mut cursor,
            &mut motion,
            KeyCode::Up,
            KeyModifiers::NONE
        ),
        InputAction::HistoryPrev
    );
}

/// ↑/↓ walk the rows the panel *paints*, so a soft-wrapped line is climbed one
/// visual row at a time instead of being jumped over in a single logical step.
#[test]
fn arrows_walk_soft_wrapped_rows_not_logical_lines() {
    // 14 columns in a 10-column text area wrap onto two rows: [0..10), [10..14).
    let mut input = String::from("abcdefghijklmn");
    let mut cursor = input.len();
    let mut motion = crate::shell::composer_grid::CaretMotion::default();
    let mut drag = SelectionDrag::default();
    let keys = SceneKeys {
        composer_text_width: 10,
        ..Default::default()
    };

    let action = route_event_motion(
        Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        &mut motion,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &keys,
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(
        cursor, 4,
        "up moves one painted row (column 4 of the first row), not to the line start"
    );

    // ↓ returns to the same column on the continuation row.
    let action = route_event_motion(
        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        &mut motion,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &keys,
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(
        cursor, 14,
        "down restores the sticky column on the row below"
    );
}
