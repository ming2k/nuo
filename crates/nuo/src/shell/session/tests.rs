use super::*;
use crate::shell::input::route_event_motion;
use crate::shell::surfaces::SceneKind;
use crossterm::event::{Event, KeyEvent, KeyEventKind, KeyEventState};

/// Run states used to build a context for resolver tests. Test-local so
/// the shipped module carries no dead types (the resolver is
/// the scheme; the mode table is a test concern).
#[derive(Clone, Copy, Debug)]
enum Mode {
    Idle,
    Running,
    FocusedTarget,
    /// The transcript plane with no entry highlighted (pointer-parked
    /// attention).
    Browsing,
    Completion,
    Subagent,
}

/// A mode-appropriate chat-surface scene-keys bundle for direct resolver
/// tests.
fn ctx(mode: Mode, tune: impl FnOnce(&mut SceneKeys)) -> SceneKeys {
    let mut c = SceneKeys::default();
    match mode {
        Mode::Idle => {}
        Mode::Running => c.is_responding = true,
        Mode::FocusedTarget => {
            c.focus = crate::shell::focus::FocusPlane::Transcript;
            c.entry_focus = Some(nuo_view::model::layout::InteractiveTarget::tool_step(0));
        }
        Mode::Browsing => {
            c.focus = crate::shell::focus::FocusPlane::Transcript;
        }
        Mode::Completion => {
            c.completion_kind = crate::shell::completion::CompletionKind::Slash;
            c.suggestion_count = 3;
            c.suggestion_index = Some(1);
        }
        Mode::Subagent => {}
    }
    tune(&mut c);
    c
}

/// The scene a Mode stands in (surface dispatch keys off the explicit
/// scene).
fn scene_of(mode: Mode) -> SceneKind {
    match mode {
        Mode::Subagent => SceneKind::Subagent,
        _ => SceneKind::Thread,
    }
}

/// Route a chord through the real `route_event` pipeline to confirm the
/// resolver is reached for the chat surface (layer wiring).
use crate::shell::keymap::Key;

/// Resolve `key` through the **whole** input pipeline (global chords, then
/// sheet/modal arms, then the scene scheme) for `mode`, which is what a
/// user's keypress actually traverses.
///
/// The hint-resolvability checks use this rather than the scene resolver
/// alone: a hint may legitimately be served by any layer — `Shift-Tab` is a
/// Global-scope chord that the scene scheme deliberately declines — and a
/// test that only exercised one layer would report that as a dead key.
fn resolves_end_to_end(
    key: crate::shell::keymap::Key,
    mode: Mode,
    seed: &str,
    tune: impl FnOnce(&mut SceneKeys),
) -> (InputAction, String) {
    let mut input = seed.to_string();
    let mut cursor = input.chars().count();
    let mut drag = nuo_view::model::selection::SelectionDrag::default();
    let keys = ctx(mode, tune);
    let action = crate::shell::input::route_event_motion(
        Event::Key(crate::shell::input::keystroke::event_from_chord(key)),
        &mut input,
        &mut cursor,
        &mut crate::shell::composer_grid::CaretMotion::default(),
        crate::shell::input::Dispatch {
            scene: scene_of(mode),
            focus: keys.focus,
            ..Default::default()
        },
        &crate::shell::modal_keys::ModalKeys::default(),
        &crate::shell::sheet::SheetKeys::default(),
        &keys,
        &mut drag,
    );
    (action, input)
}

fn process(scene: SceneKind, code: KeyCode, modifiers: KeyModifiers, mode: Mode) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = nuo_view::model::selection::SelectionDrag::default();
    let keys = ctx(mode, |_| {});
    let dispatch = crate::shell::input::Dispatch {
        scene,
        ..Default::default()
    };
    crate::shell::input::route_event_motion(
        Event::Key(crate::shell::input::keystroke::event_from_chord(Key {
            code,
            modifiers,
        })),
        &mut input,
        &mut cursor,
        &mut crate::shell::composer_grid::CaretMotion::default(),
        dispatch,
        &crate::shell::modal_keys::ModalKeys::default(),
        &crate::shell::sheet::SheetKeys::default(),
        &keys,
        &mut drag,
    )
}

#[test]
fn every_owned_chord_resolves_in_its_mode() {
    // The chat surface's owned chords, paired with the run state they act
    // in. Each must resolve (an owned chord is never dead).
    // Esc is an *interrupt* on every scene, so its owned entries are the
    // running states — a scene exit is not an Esc arm, and since ADR-0145
    // a plane crossing is not one either.
    let owned: &[(crate::shell::keymap::Key, Mode)] = &[
        (crate::shell::keymap::Key::ENTER, Mode::Idle),
        (crate::shell::keymap::Key::ENTER, Mode::Running),
        (crate::shell::keymap::Key::ENTER, Mode::FocusedTarget),
        (crate::shell::keymap::Key::ESC, Mode::Running),
        (crate::shell::keymap::Key::TAB, Mode::Running),
        (crate::shell::keymap::Key::CTRL_R, Mode::Idle),
        (crate::shell::keymap::Key::ALT_P, Mode::Idle),
        (crate::shell::keymap::Key::ALT_N, Mode::Idle),
        (crate::shell::keymap::Key::CTRL_P, Mode::Idle),
        (crate::shell::keymap::Key::CTRL_N, Mode::FocusedTarget),
    ];
    for (key, mode) in owned {
        let c = ctx(*mode, |_| {});
        let mut input = String::from("hi");
        let mut cursor = input.chars().count();
        let resolved = resolve_scene_key(scene_of(*mode), *key, &c, &mut input, &mut cursor);
        assert!(
            resolved.is_some(),
            "owned chord {key:?} did not resolve in {mode:?}"
        );
    }

    // The subagent scene owns Esc only while its *viewed* round is
    // interruptible; idle, it is inert (it has no Esc exit). It reads its
    // own scene-local liveness — the viewed child's
    // `focused_subagent_running` — never the primary's `is_responding`.
    let (mode, expected) = (Mode::Subagent, InputAction::InterruptSubagent);
    {
        let mut c = ctx(mode, |c| {
            c.is_responding = true;
            c.focused_subagent_running = true;
        });
        let mut input = String::new();
        let mut cursor = 0;
        let resolved = resolve_scene_key(
            scene_of(mode),
            crate::shell::keymap::Key::ESC,
            &c,
            &mut input,
            &mut cursor,
        );
        assert_eq!(
            resolved,
            Some(expected),
            "a running {mode:?} scene owns Esc as its *scene-scoped* interrupt"
        );
        c.is_responding = false;
        c.focused_subagent_running = false;
        assert!(
            resolve_scene_key(
                scene_of(mode),
                crate::shell::keymap::Key::ESC,
                &c,
                &mut input,
                &mut cursor
            )
            .is_none(),
            "an idle {mode:?} scene has no Esc arm at all"
        );
    }
}

#[test]
fn global_and_modal_keys_are_not_session_owned() {
    // Readline/editing and navigation chords are shared, not owned by the
    // chat surface — the resolver must leave them for the router.
    for key in [
        crate::shell::keymap::Key::CTRL_A,
        crate::shell::keymap::Key::CTRL_G,
        crate::shell::keymap::Key::CTRL_W,
        crate::shell::keymap::Key::CTRL_L,
        crate::shell::keymap::Key::PAGE_UP,
        crate::shell::keymap::Key::PAGE_DOWN,
    ] {
        let c = ctx(Mode::Idle, |_| {});
        let mut input = String::from("x");
        let mut cursor = 1;
        assert!(
            resolve_chat_surface_key(key, &c, &mut input, &mut cursor).is_none(),
            "{key:?} must not be owned by the chat surface"
        );
    }

    // Ctrl+P and Ctrl+N are canonical focus navigation verbs owned by the chat surface.
    let c = ctx(Mode::Idle, |_| {});
    let mut input = String::from("x");
    let mut cursor = 1;
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::CTRL_P,
            &c,
            &mut input,
            &mut cursor
        ),
        Some(InputAction::FocusPrevTarget)
    );
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::CTRL_N,
            &c,
            &mut input,
            &mut cursor
        ),
        Some(InputAction::FocusNextTarget)
    );
}

#[test]
fn enter_multi_mode_reactions() {
    // Idle + text → send.
    let action = {
        let mut input = String::from("hello");
        let mut cursor = 5;
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ENTER,
            &ctx(Mode::Idle, |_| {}),
            &mut input,
            &mut cursor,
        )
    };
    assert_eq!(action, Some(InputAction::SendChat("hello".into())));

    // Running + text in Steer mode (default) → steer immediate.
    let action = {
        let mut input = String::from("next");
        let mut cursor = 4;
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ENTER,
            &ctx(Mode::Running, |_| {}),
            &mut input,
            &mut cursor,
        )
    };
    assert_eq!(action, Some(InputAction::SteerImmediate("next".into())));

    // Running + text in FollowUp mode → queue follow-up.
    let action = {
        let mut input = String::from("next");
        let mut cursor = 4;
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ENTER,
            &ctx(Mode::Running, |c| {
                c.composer_send_mode = crate::shell::app::ComposerSendMode::FollowUp;
            }),
            &mut input,
            &mut cursor,
        )
    };
    assert_eq!(action, Some(InputAction::QueueFollowUp("next".into())));

    // Focused step → activate, ignoring the draft.
    let action = {
        let mut input = String::from("draft");
        let mut cursor = 5;
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ENTER,
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut input,
            &mut cursor,
        )
    };
    assert_eq!(action, Some(InputAction::ActivateFocusedTarget));

    // Highlighted completion → commit the highlighted item.
    let action = resolve_chat_surface_key(
        crate::shell::keymap::Key::ENTER,
        &ctx(Mode::Completion, |_| {}),
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(action, Some(InputAction::CommitSuggestion("1".into())));
}

#[test]
fn focus_navigation_and_interrupt_react_to_mode() {
    // Esc interrupts only while running.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ESC,
            &ctx(Mode::Running, |_| {}),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::Interrupt)
    );
    // Esc never crosses planes, even while a step is focused and a round
    // runs: it is the interrupt and nothing else (ADR-0145).
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ESC,
            &ctx(Mode::FocusedTarget, |c| c.is_responding = true),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::Interrupt),
        "Esc interrupts regardless of which plane holds the keyboard"
    );
    // …and with nothing running it is spent, leaving the plane untouched.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ESC,
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut String::new(),
            &mut 0,
        ),
        None,
        "a spent Esc does not move the keyboard off the transcript"
    );
    // Shift-Tab is the crossing, and it is *not* a scene-owned chord: the
    // router's global arm claims it before scene dispatch, so the scene
    // scheme must decline it (one gesture, one producer).
    for mode in [Mode::Idle, Mode::FocusedTarget] {
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::BACKTAB,
                &ctx(mode, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            None,
            "the scene scheme leaves the crossing to the global chord layer ({mode:?})"
        );
    }
    // Tab without completion is inert (completion-only).
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::TAB,
            &ctx(Mode::Idle, |_| {}),
            &mut String::new(),
            &mut 0,
        ),
        None
    );
    // While a step is focused, bare ↑ steps to the previous target.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::UP,
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::FocusPrevTarget)
    );
    // While a step is focused, bare ↓ steps to the next target.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::DOWN,
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::FocusNextTarget)
    );
    // While transcript browse focus is active, bare ↑/↓ scroll the transcript.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::UP,
            &ctx(Mode::Idle, |c| c.focus =
                crate::shell::focus::FocusPlane::Transcript),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::ScrollUp)
    );
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::DOWN,
            &ctx(Mode::Idle, |c| c.focus =
                crate::shell::focus::FocusPlane::Transcript),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::ScrollDown)
    );
    // While idle in composer, bare ↑ hands off to inline history recall.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::UP,
            &ctx(Mode::Idle, |_| {}),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::HistoryPrev)
    );
    // Tab toggles send mode only while running.
    assert!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::TAB,
            &ctx(Mode::Idle, |_| {}),
            &mut String::from("steer"),
            &mut 5,
        )
        .is_none()
    );
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::TAB,
            &ctx(Mode::Running, |_| {}),
            &mut String::from("steer"),
            &mut 5,
        ),
        Some(InputAction::ToggleSendMode)
    );
}

#[test]
/// `[INV-FOCUS-02]`: a printable key on the transcript plane is inert in
/// both sub-states, and is never inserted into the draft the user is not
/// looking at (ADR-0145).
fn typed_character_on_the_transcript_plane_is_inert() {
    for focus in [Mode::FocusedTarget, Mode::Browsing] {
        let mut input = String::from("");
        let mut cursor = 0;
        let action = resolve_chat_surface_key(
            crate::shell::keymap::Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('x'),
            },
            &ctx(focus, |_| {}),
            &mut input,
            &mut cursor,
        );
        assert_eq!(action, Some(InputAction::None));
        assert_eq!(
            input, "",
            "a typed char must never pollute an unfocused draft ({focus:?})"
        );
    }
}

/// The two ends of the entry walk are the *implicit* crossing: with nothing
/// left to scroll, walking forward hands the keyboard back, and walking
/// backward wraps the entry list instead of moving the keyboard.
#[test]
fn walking_off_the_entry_list_is_the_implicit_crossing() {
    // `↓` at the last visible entry with the viewport already at the
    // bottom resolves to the walk (the app layer performs the crossing);
    // the resolver never claims an Esc-style exit.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::DOWN,
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::FocusNextTarget)
    );
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::UP,
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut String::new(),
            &mut 0,
        ),
        Some(InputAction::FocusPrevTarget)
    );
}

#[test]
fn copy_keys_act_on_focused_target() {
    let mut input = String::from("");
    let mut cursor = 0;
    // 'y' copies focused target content without bouncing
    let action = resolve_chat_surface_key(
        crate::shell::keymap::Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char('y'),
        },
        &ctx(Mode::FocusedTarget, |_| {}),
        &mut input,
        &mut cursor,
    );
    assert_eq!(action, Some(InputAction::CopyFocusedTarget));
    assert_eq!(input, "", "yank key must not pollute composer input");
}

/// The view verb is spelled `v` — the letter the fold block's sentence
/// prints — and nothing else. `o` and `c` were unadvertised aliases: a proxy
/// for a key that works but is never shown cannot be learned from the UI, and
/// each alias costs a neighbour chord (`Alt-o`, `Ctrl-c`).
#[test]
fn only_the_advertised_letters_reach_the_entry_verbs() {
    let press = |c: char| {
        resolve_chat_surface_key(
            crate::shell::keymap::Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char(c),
            },
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut String::new(),
            &mut 0,
        )
    };
    assert_eq!(press('v'), Some(InputAction::ViewFocusedTargetOutput));
    for alias in ['o', 'O', 'V', 'c', 'C'] {
        assert_eq!(
            press(alias),
            Some(InputAction::None),
            "`{alias}` is not printed anywhere, so it must not act"
        );
    }
    assert_eq!(press('Y'), Some(InputAction::CopyFocusedDiagnostic));
}

/// `Shift-y` is the diagnostic-copy chord, and it is the *only* difference
/// from `y`: the lowercase verb keeps its "give me the text" meaning, so a
/// muscle-memory `y` is never silently upgraded into a different payload.
/// `Shift-C` was its duplicate spelling and is gone: one verb, one chord, one
/// word to print.
#[test]
fn shift_y_emits_a_diagnostic_for_the_focused_target() {
    let mut input = String::from("");
    let mut cursor = 0;
    let action = resolve_chat_surface_key(
        crate::shell::keymap::Key {
            modifiers: KeyModifiers::SHIFT,
            code: KeyCode::Char('Y'),
        },
        &ctx(Mode::FocusedTarget, |_| {}),
        &mut input,
        &mut cursor,
    );
    assert_eq!(action, Some(InputAction::CopyFocusedDiagnostic));
    assert_eq!(input, "", "copy keys must never pollute the composer");
}

/// Hints and dispatch share one origin: every chord the entry-focus row
/// advertises must actually resolve to an action while an entry is focused.
/// Without this, the newly-discoverable copy verbs could advertise a dead
/// key the moment the resolver changed.
#[test]
fn focused_entry_hints_are_resolvable() {
    for kind in [
        HintState::EntryFocused { subagent: false },
        HintState::EntryFocused { subagent: true },
    ] {
        for hint in live_chat_hints(kind, crate::shell::keymap::Key::TAB) {
            let (resolved, input) = resolves_end_to_end(hint.key, Mode::FocusedTarget, "", |_| {});
            assert_ne!(
                resolved,
                InputAction::None,
                "entry-focus hint {key:?} did not resolve while an entry is focused",
                key = hint.key
            );
            assert_eq!(
                input,
                "",
                "hint chord {key:?} leaked into the composer",
                key = hint.key
            );
        }
    }
}

/// The entry-focus row must describe what the focused entry actually does.
///
/// A subagent task has no inline body — activation returns `EnterSubagent`,
/// never a disclosure toggle — so promising `Space expand` there sent the
/// user hunting for a body the renderer never draws.
#[test]
fn focused_entry_hints_name_the_verb_the_entry_supports() {
    let labels = |subagent: bool| -> Vec<&'static str> {
        live_chat_hints(HintState::EntryFocused { subagent }, Key::TAB)
            .iter()
            .map(|hint| hint.label)
            .collect()
    };
    let disclosable = labels(false);
    assert!(
        disclosable.contains(&"expand"),
        "a collapsible entry expands: {disclosable:?}"
    );
    let subagent = labels(true);
    assert!(
        !subagent.contains(&"expand"),
        "a subagent task has no inline body to expand: {subagent:?}"
    );
    assert!(
        subagent.iter().any(|label| label.contains("subagent")),
        "the row names the drill-in verb instead: {subagent:?}"
    );
}

#[test]
fn router_offers_chat_keys_only_on_chat_surfaces() {
    use crate::shell::input::InputAction;
    // Tab on the Thread scene without a completion is inert (
    // the chord belongs to completion, not to plane switching).
    assert_eq!(
        process(
            SceneKind::Thread,
            KeyCode::Tab,
            KeyModifiers::NONE,
            Mode::Idle
        ),
        InputAction::None
    );
    // On the Settings scene the resolver is never consulted: Tab is inert.
    assert_eq!(
        process(
            SceneKind::Settings,
            KeyCode::Tab,
            KeyModifiers::NONE,
            Mode::Idle
        ),
        InputAction::None
    );
    // A subagent step's Enter still activates through the shared path.
    assert_eq!(
        process(
            SceneKind::Subagent,
            KeyCode::Enter,
            KeyModifiers::NONE,
            Mode::FocusedTarget
        ),
        InputAction::ActivateFocusedTarget
    );
}

/// Read the current key-handling region's Enter behavior as a canary:
/// after the extraction the central match must no longer send the draft.
#[test]
fn advertised_hints_are_resolvable_in_their_state() {
    // Every chord `live_chat_hints` advertises for a run state must be
    // consumable by the resolver in that state (hints and
    // dispatch share a semantic origin).
    use crate::shell::keymap::Key;

    // Idle.
    let c = ctx(Mode::Idle, |_| {});
    let mut input = String::from("hi");
    assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut input, &mut 2).is_some());

    // Running.
    let c = ctx(Mode::Running, |_| {});
    let mut input = String::from("next");
    assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut input, &mut 4).is_some());
    let mut input = String::from("steer");
    assert!(resolve_chat_surface_key(Key::TAB, &c, &mut input, &mut 5).is_some());

    // Completion.
    let c = ctx(Mode::Completion, |_| {});
    assert!(resolve_chat_surface_key(Key::ESC, &c, &mut String::new(), &mut 0).is_some());
    assert!(resolve_chat_surface_key(Key::TAB, &c, &mut String::new(), &mut 0).is_some());
    assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut String::new(), &mut 0).is_some());
}

#[test]
fn surface_overrides_remap_verbs_and_kill_canonical_chords() {
    use crate::shell::keymap::{Key, SurfaceOverrides};

    let mut map = std::collections::HashMap::new();
    map.insert("open_history".to_string(), "ctrl+shift+r".to_string());
    map.insert("toggle_send_mode".to_string(), "ctrl+t".to_string());
    map.insert("scene_back".to_string(), "alt+b".to_string());
    let ov = SurfaceOverrides::from_config(&map);
    let c = ctx(Mode::Idle, |c| c.surface_overrides = ov.clone());

    // The assigned chord fires; the canonical chord is dead.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::CTRL_SHIFT_R,
            &c,
            &mut String::new(),
            &mut 0
        ),
        Some(InputAction::OpenHistory)
    );
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::CTRL_R,
            &c,
            &mut String::new(),
            &mut 0
        ),
        None,
        "the canonical Ctrl+R must go inactive once open_history is remapped"
    );

    // ToggleSendMode's guard still applies on the remapped chord: fires only while
    // running.
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::CTRL_T,
            &ctx(Mode::Running, |c| c.surface_overrides = ov.clone()),
            &mut String::from("steer"),
            &mut 5
        ),
        Some(InputAction::ToggleSendMode)
    );
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::CTRL_T,
            &c,
            &mut String::from("steer"),
            &mut 5
        ),
        None,
        "toggle_send_mode on a remapped chord must still require a running round"
    );

    // Subagent scene back follows the remapped chord, same guards.
    let rc = ctx(Mode::Subagent, |c| c.surface_overrides = ov);
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            Key::alt('b'),
            &rc,
            &mut String::new(),
            &mut 0
        ),
        Some(InputAction::TabHistoryBack)
    );
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            Key::ALT_BRACKET_LEFT,
            &rc,
            &mut String::new(),
            &mut 0
        ),
        None,
        "the canonical Alt+[ must go inactive once scene_back is remapped"
    );
}

#[test]
fn subagent_and_side_never_exit_on_esc_and_keep_their_navigation() {
    use crate::shell::keymap::Key;

    // Subagent: Esc clears step focus, and otherwise resolves to nothing —
    // it never exits the scene ([INV-TUI-CLEAN-02]). Alt+[ / Alt+] is the canonical navigation.
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            Key::ESC,
            &ctx(Mode::Subagent, |_| {}),
            &mut String::new(),
            &mut 0
        ),
        None,
        "an idle zoom has no Esc arm; leaving is the `C-x` namespace"
    );
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            Key::ALT_BRACKET_LEFT,
            &ctx(Mode::Subagent, |_| {}),
            &mut String::new(),
            &mut 0
        ),
        Some(InputAction::TabHistoryBack),
        "`Alt+[` in subagent view steps back to parent thread (ADR-0092)"
    );
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            Key::ALT_BRACKET_RIGHT,
            &ctx(Mode::Subagent, |_| {}),
            &mut String::new(),
            &mut 0
        ),
        Some(InputAction::TabHistoryForward),
        "`Alt+]` in subagent view steps forward in scene history (ADR-0092)"
    );
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            Key::ESC,
            &ctx(Mode::Subagent, |c| {
                c.focus = crate::shell::focus::FocusPlane::Transcript;
                c.entry_focus = Some(nuo_view::model::layout::InteractiveTarget::tool_step(0));
            }),
            &mut String::new(),
            &mut 0
        ),
        None,
        "subagent Esc never crosses planes; the zoom has no Esc exit"
    );
    // Scene scope (the regression this guards): a Subagent-scene Esc must
    // NEVER resolve to the primary `Interrupt`. When the viewed child has
    // finished but the *primary* round still runs, the zoom's Esc is inert —
    // it must not penetrate the scene boundary and stop the outer round.
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            Key::ESC,
            &ctx(Mode::Subagent, |c| {
                c.is_responding = true; // the *primary* is running
                c.focused_subagent_running = false; // the viewed child finished
            }),
            &mut String::new(),
            &mut 0
        ),
        None,
        "a finished viewed child under a running primary owns no Esc: the \
         primary interrupt must not leak through the Subagent scene"
    );
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            Key::ESC,
            &ctx(Mode::Subagent, |c| {
                c.is_responding = true;
                c.focused_subagent_running = true;
            }),
            &mut String::new(),
            &mut 0
        ),
        Some(InputAction::InterruptSubagent),
        "a running viewed child owns Esc as the scene-scoped interrupt"
    );
    // Raw `[` and `q` no longer trigger scene navigation (ADR-0092 [INV-NAV-03])
    let bracket = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char('['),
    };
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            bracket,
            &ctx(Mode::Subagent, |_| {}),
            &mut String::new(),
            &mut 0
        ),
        None,
        "raw `[` falls through to normal input"
    );
    let q_key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char('q'),
    };
    assert_eq!(
        resolve_scene_key(
            SceneKind::Subagent,
            q_key,
            &ctx(Mode::Subagent, |_| {}),
            &mut String::new(),
            &mut 0
        ),
        None,
        "raw `q` falls through to normal input"
    );

    // The Thread scene never emits the subagent exits.
    let c = ctx(Mode::FocusedTarget, |_| {});
    let action = resolve_scene_key(SceneKind::Thread, Key::ESC, &c, &mut String::new(), &mut 0);
    assert_ne!(action, Some(InputAction::CloseTab));
}

/// Esc while the inline ↑/↓ pointer sits on a history row
/// cancels the recall (restoring the stashed draft) — the recall state
/// must be escapable by the universal "get me back" chord, and the
/// Esc does not cancel inline history recall (Ctrl-C clears instead).
/// When running/responding, Esc resolves to Interrupt without getting intercepted.
#[test]
fn esc_does_not_cancel_inline_history_recall() {
    let mut c = ctx(Mode::Idle, |_| {});
    c.in_history_recall = true;
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ESC,
            &c,
            &mut String::new(),
            &mut 0
        ),
        None
    );

    // When running/responding, Esc resolves to Interrupt rather than clearing recall
    let mut c = ctx(Mode::Running, |_| {});
    c.in_history_recall = true;
    c.is_responding = true;
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::ESC,
            &c,
            &mut String::new(),
            &mut 0
        ),
        Some(InputAction::Interrupt)
    );
}

/// Every chord the recall hint set advertises resolves in the
/// recall state (hints and dispatch share one semantic origin).
#[test]
fn recall_hints_are_resolvable_in_the_recall_state() {
    for hint in live_chat_hints(HintState::Recall, crate::shell::keymap::Key::TAB) {
        let (resolved, _) = resolves_end_to_end(hint.key, Mode::Idle, "recalled", |c| {
            c.in_history_recall = true;
        });
        assert_ne!(
            resolved,
            InputAction::None,
            "recall hint chord {key:?} did not resolve in the recall state",
            key = hint.key
        );
    }
}

/// The readline-style edge hand-off. On a single-line draft ↑
/// resolves to `HistoryPrev` and ↓ to `HistoryNext` once completion and
/// caret motion have had their chance; a multi-line draft only hands off
/// from its true first/last line.
#[test]
fn arrow_edge_hands_off_to_history_recall() {
    // Single-line draft: both edges hand off immediately.
    let mut input = String::from("hello");
    let mut cursor = 5;
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::UP,
            &ctx(Mode::Idle, |_| {}),
            &mut input,
            &mut cursor
        ),
        Some(InputAction::HistoryPrev)
    );
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::DOWN,
            &ctx(Mode::Idle, |_| {}),
            &mut input,
            &mut cursor
        ),
        Some(InputAction::HistoryNext)
    );

    // Multi-line draft, caret on the middle line: no hand-off.
    let mut input = String::from("one\ntwo\nthree");
    let mut cursor = 5; // on "two"
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::UP,
            &ctx(Mode::Idle, |_| {}),
            &mut input,
            &mut cursor
        ),
        Some(InputAction::None)
    );
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::DOWN,
            &ctx(Mode::Idle, |_| {}),
            &mut input,
            &mut cursor
        ),
        Some(InputAction::None)
    );

    // Caret on the first line of a multi-line draft: ↑ hands off without
    // disturbing the draft; ↓ stays a caret motion.
    let mut input = String::from("one\ntwo");
    let mut cursor = 2;
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::UP,
            &ctx(Mode::Idle, |_| {}),
            &mut input,
            &mut cursor
        ),
        Some(InputAction::HistoryPrev)
    );
    assert_eq!(input, "one\ntwo", "the hand-off must not mutate the draft");
    let mut cursor = 0;
    assert_eq!(
        resolve_chat_surface_key(
            crate::shell::keymap::Key::DOWN,
            &ctx(Mode::Idle, |_| {}),
            &mut input,
            &mut cursor
        ),
        Some(InputAction::None)
    );
    assert_eq!(
        cursor, 4,
        "↓ moved the caret to the next line (column kept)"
    );
}

#[test]
fn central_match_no_longer_owns_chat_enter_on_settings() {
    // Enter on the Settings scene (a full-screen destination)
    // is owned by that scene's own scheme (ConfigActivate), NOT the chat
    // surface's send path — the draft must never be shipped from Settings.
    let mut input = String::from("draft");
    let mut cursor = 5;
    let mut drag = nuo_view::model::selection::SelectionDrag::default();
    let action = route_event_motion(
        Event::Key(KeyEvent {
            code: crossterm::event::KeyCode::Enter,
            modifiers: crossterm::event::KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        &mut crate::shell::composer_grid::CaretMotion::default(),
        crate::shell::input::Dispatch {
            scene: SceneKind::Settings,
            ..Default::default()
        },
        &crate::shell::modal_keys::ModalKeys::default(),
        &crate::shell::sheet::SheetKeys::default(),
        &crate::shell::session::SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::ConfigActivate,
        "Enter on the Settings scene activates its focused item, never ships the draft"
    );
    assert_eq!(input, "draft", "the draft is never sent or mutated");
}
