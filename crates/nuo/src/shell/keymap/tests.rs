//! Keymap resolution, parsing, hints, and registry lookups.

use super::*;

#[test]
fn global_keys_resolve_correctly() {
    assert_eq!(resolve_global_key(Key::CTRL_L), None);
    // Alt-o is the notice viewer's chord: the fault bubble advertises it,
    // so it must resolve to the verb the bubble names.
    assert_eq!(resolve_global_key(Key::ALT_O), Some(CommandId::OpenNotices));
    assert_eq!(
        resolve_global_key(Key::ALT_0),
        Some(CommandId::NavigateDashboard)
    );
    assert_eq!(
        resolve_global_key(Key::CTRL_SHIFT_P),
        Some(CommandId::CommandPalette)
    );
    assert_eq!(resolve_global_key(Key::ESC), Some(CommandId::CancelOrBack));
    assert_eq!(resolve_global_key(Key::CTRL_C), Some(CommandId::Quit));
    assert_eq!(resolve_global_key(Key::CTRL_Q), Some(CommandId::OpenQueue));
    assert_eq!(
        resolve_global_key(Key::CTRL_SHIFT_C),
        Some(CommandId::CopySelection)
    );
    assert_eq!(
        resolve_global_key(Key::CMD_C),
        Some(CommandId::CopySelection)
    );
    // Ctrl+P and Ctrl+N are session navigation verbs, not global commands.
    assert_eq!(resolve_global_key(Key::CTRL_P), None);
    assert_eq!(resolve_global_key(Key::CTRL_N), None);
}

#[test]
fn every_global_binding_has_a_resolver_handler() {
    // Any Global-scope command that declares a key binding must be
    // resolvable by resolve_global_key. This is the guard against the
    // "hint advertised but no handler" desync — e.g. the former dead
    // Ctrl+O / Ctrl+N / top-level Ctrl+P.
    for cmd in COMMAND_REGISTRY {
        if cmd.scope == Scope::Global && !cmd.bindings.is_empty() {
            assert!(
                cmd.bindings
                    .iter()
                    .any(|&k| resolve_global_key(k) == Some(cmd.id)),
                "global command {:?} binding(s) are not resolvable: {:?}",
                cmd.id,
                cmd.bindings
            );
        }
    }
}

#[test]
fn no_global_binding_is_shared_between_two_commands() {
    // Within the Global layer a single chord must never mean two different
    // commands, or a global hint could resolve to the wrong action.
    // (Contextual commands may deliberately reuse a chord — e.g. `Enter`
    // is both SendPrompt and QueueFollowUp depending on run state — so
    // the invariant is enforced for Global scope only.)
    let mut seen: Vec<(Key, CommandId)> = Vec::new();
    for cmd in COMMAND_REGISTRY {
        if cmd.scope != Scope::Global {
            continue;
        }
        for &k in cmd.bindings {
            if let Some((_, prev)) = seen.iter().find(|(sk, _)| *sk == k) {
                panic!("global binding {k:?} shared by {:?} and {:?}", prev, cmd.id);
            }
            seen.push((k, cmd.id));
        }
    }
}

#[test]
fn non_global_keys_do_not_resolve_as_global() {
    assert_eq!(resolve_global_key(Key::ENTER), None);
    assert_eq!(resolve_global_key(Key::TAB), None);
    assert_eq!(resolve_global_key(Key::CTRL_R), None);
    assert_eq!(resolve_global_key(Key::ctrl('x')), None);
    assert_eq!(resolve_global_key(Key::alt('x')), None);
}

#[test]
fn parse_key_round_trips_chord_specs() {
    // Each spec parses to the exact Key the input layer produces for that
    // keystroke (via Key::from_event), so comparisons hold.
    // Seamlessly supports both '-' and '+' delimiters.
    assert_eq!(parse_key("ctrl+p"), Some(Key::CTRL_P));
    assert_eq!(parse_key("ctrl-p"), Some(Key::CTRL_P));
    assert_eq!(parse_key("ctrl+shift+c"), Some(Key::CTRL_SHIFT_C));
    assert_eq!(parse_key("ctrl-shift-c"), Some(Key::CTRL_SHIFT_C));
    assert_eq!(parse_key("f5"), Some(Key::F5));
    assert_eq!(parse_key("esc"), Some(Key::ESC));
    assert_eq!(
        parse_key("space"),
        Some(Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char(' ')
        })
    );
    assert_eq!(parse_key("shift+tab"), Some(Key::BACKTAB));
    assert_eq!(parse_key("shift-tab"), Some(Key::BACKTAB));
    assert_eq!(parse_key("alt+s"), Some(Key::ALT_S));
    assert_eq!(parse_key("alt-s"), Some(Key::ALT_S));
    assert_eq!(parse_key("ctrl+x"), Some(Key::ctrl('x')));
    assert_eq!(parse_key("ctrl-x"), Some(Key::ctrl('x')));
    assert_eq!(parse_key("nonsense"), None);
    assert_eq!(parse_key(""), None);
}

#[test]
fn key_display_preserves_case_and_uses_hyphen_convention() {
    assert_eq!(Key::CTRL_O.display(), "Ctrl-o");
    assert_eq!(Key::CTRL_N.display(), "Ctrl-n");
    assert_eq!(Key::CTRL_P.display(), "Ctrl-p");
    assert_eq!(Key::CTRL_C.display(), "Ctrl-c");
    assert_eq!(Key::CTRL_SHIFT_C.display(), "Ctrl-Shift-c");
    assert_eq!(Key::ALT_S.display(), "Alt-s");
    assert_eq!(Key::ALT_ENTER.display(), "Alt-Enter");
    assert_eq!(Key::CMD_C.display(), "Cmd-c");
    assert_eq!(Key::ESC.display(), "Esc");
    assert_eq!(Key::ENTER.display(), "Enter");
    assert_eq!(Key::TAB.display(), "Tab");
    assert_eq!(Key::BACKTAB.display(), "Shift-Tab");
    assert_eq!(Key::F5.display(), "F5");
    assert_eq!(
        Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char('o')
        }
        .display(),
        "o"
    );
    assert_eq!(
        Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char('O')
        }
        .display(),
        "O"
    );
    assert_eq!(
        Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char('?')
        }
        .display(),
        "?"
    );
    assert_eq!(
        Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char('/')
        }
        .display(),
        "/"
    );
}

#[test]
fn global_overrides_remap_resolution_and_effective_binding() {
    let mut map = std::collections::HashMap::new();
    map.insert("palette".to_string(), "ctrl+k".to_string());
    map.insert("quit".to_string(), "ctrl+shift+q".to_string());
    map.insert("not_a_command".to_string(), "ctrl+z".to_string());
    let o = GlobalOverrides::from_config(&map);

    // The remapped chord fires; the retired canonical chord stays dead
    // (the palette has no canonical single-stroke chord at all now — ADR-0023).
    assert_eq!(
        resolve_global_key_with(Key::ctrl('k'), &o),
        Some(CommandId::CommandPalette)
    );
    assert_eq!(resolve_global_key_with(Key::CTRL_L, &o), None);
    // The remapped quit fires; canonical Ctrl+C / Ctrl+Q is dead.
    let ctrl_shift_q = Key {
        modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        code: KeyCode::Char('q'),
    };
    assert_eq!(
        resolve_global_key_with(ctrl_shift_q, &o),
        Some(CommandId::Quit)
    );
    assert_eq!(resolve_global_key_with(Key::CTRL_C, &o), None);
    // Ctrl+Q was never touched by this override set, so it still fires.
    assert_eq!(
        resolve_global_key_with(Key::CTRL_Q, &o),
        Some(CommandId::OpenQueue)
    );
    // Commands without canonical chords resolve to None. Alt-o *has* a
    // canonical chord (the notice viewer) and is untouched by this override
    // set, so it still fires.
    assert_eq!(
        resolve_global_key_with(Key::ALT_O, &o),
        Some(CommandId::OpenNotices)
    );
    assert_eq!(resolve_global_key_with(Key::CTRL_L, &o), None);
    // Effective binding follows the override for remapped commands.
    assert_eq!(o.effective_binding(CommandId::Quit), Some(ctrl_shift_q));
    // A command with no canonical chord reports none — chrome renders no
    // keycap for it rather than a placeholder.
    assert_eq!(o.effective_binding(CommandId::InterruptTask), None);
    // Esc / Back is never remappable.
    let mut esc_map = std::collections::HashMap::new();
    esc_map.insert("cancel".to_string(), "ctrl+k".to_string());
    let o2 = GlobalOverrides::from_config(&esc_map);
    assert_eq!(
        o2.effective_binding(CommandId::CancelOrBack),
        Some(Key::ESC)
    );
    assert!(o2.is_empty());
}

/// Chrome's keycaps and the dispatcher read one table: every chord a
/// `Global` command advertises in the registry resolves back to that exact
/// command. This is the guard that would have caught the queue bar's
/// hardcoded `Ctrl-q`.
#[test]
fn every_advertised_global_binding_resolves_to_its_command() {
    for cmd in COMMAND_REGISTRY {
        if cmd.scope != Scope::Global {
            continue;
        }
        for &key in cmd.bindings {
            assert_eq!(
                resolve_global_key(key),
                Some(cmd.id),
                "registry advertises {key:?} for {:?}, but dispatch resolves it elsewhere",
                cmd.id
            );
        }
        // The display path agrees with the dispatch path.
        let display = GlobalOverrides::default().effective_binding(cmd.id);
        if !cmd.bindings.is_empty() {
            assert_eq!(
                display,
                Some(cmd.bindings[0]),
                "{:?} advertises {:?} but would render {display:?}",
                cmd.id,
                cmd.bindings[0]
            );
        }
    }
}

#[test]
fn command_id_from_name_accepts_snake_and_kebab() {
    assert_eq!(
        command_id_from_name("interrupt"),
        Some(CommandId::InterruptTask)
    );
    assert_eq!(
        command_id_from_name("command-palette"),
        Some(CommandId::CommandPalette)
    );
    assert_eq!(
        command_id_from_name("telemetry"),
        Some(CommandId::OpenSessionTrace)
    );
    assert_eq!(command_id_from_name("nope"), None);
}

#[test]
fn surface_overrides_remap_effective_binding_and_matches() {
    let mut map = std::collections::HashMap::new();
    map.insert("open_history".to_string(), "ctrl+shift+r".to_string());
    map.insert("toggle_send_mode".to_string(), "ctrl+t".to_string());
    map.insert("not_a_verb".to_string(), "ctrl+z".to_string());
    map.insert("interrupt".to_string(), "ctrl+x".to_string()); // global, ignored here
    map.insert("history_prev".to_string(), "bogus!!".to_string()); // bad chord, skipped
    let o = SurfaceOverrides::from_config(&map);

    assert_eq!(
        o.effective_binding(SurfaceVerb::OpenHistory),
        Key::CTRL_SHIFT_R
    );
    assert_eq!(
        o.effective_binding(SurfaceVerb::ToggleSendMode),
        Key::CTRL_T
    );
    // Unremapped verbs keep their canonical chord.
    assert_eq!(o.effective_binding(SurfaceVerb::HistoryNext), Key::ALT_N);
    assert_eq!(o.effective_binding(SurfaceVerb::ScrollTop), Key::HOME);
    // matches() follows the effective binding; the canonical chord is dead.
    assert!(o.matches(Key::CTRL_SHIFT_R, SurfaceVerb::OpenHistory));
    assert!(!o.matches(Key::CTRL_R, SurfaceVerb::OpenHistory));
    assert!(o.matches(Key::CTRL_T, SurfaceVerb::ToggleSendMode));
    assert!(!o.matches(Key::TAB, SurfaceVerb::ToggleSendMode));
    assert!(o.matches(Key::ALT_N, SurfaceVerb::HistoryNext));
    assert!(!o.is_empty());
}

#[test]
fn every_surface_verb_has_a_name_and_a_canonical_chord() {
    for verb in SurfaceVerb::ALL {
        assert!(!verb.name().is_empty(), "{verb:?} has no config name");
        assert!(
            SurfaceVerb::from_name(verb.name()) == Some(verb),
            "{verb:?} name does not round-trip"
        );
        let _ = verb.canonical();
        // A remapped verb must keep resolving through its own chord.
        let mut map = std::collections::HashMap::new();
        map.insert(verb.name().to_string(), "ctrl+shift+p".to_string());
        let o = SurfaceOverrides::from_config(&map);
        assert!(
            o.matches(Key::CTRL_SHIFT_P, verb),
            "{verb:?} override did not land on its effective chord"
        );
    }
}

#[test]
fn all_commands_have_valid_labels_and_descriptions() {
    for cmd in COMMAND_REGISTRY {
        assert!(
            !cmd.label.is_empty(),
            "command {:?} has empty label",
            cmd.id
        );
        assert!(
            !cmd.description.is_empty(),
            "command {:?} has empty description",
            cmd.id
        );
    }
}

#[test]
fn find_by_slash_resolves_all_slash_triggers() {
    assert_eq!(
        find_by_slash("/models").map(|c| c.id),
        Some(CommandId::OpenModels)
    );
    assert_eq!(
        find_by_slash("/tools").map(|c| c.id),
        Some(CommandId::OpenTools)
    );
    assert_eq!(
        find_by_slash("/stats").map(|c| c.id),
        Some(CommandId::OpenThreadStats)
    );
    // ADR-0151 [INV-PLANE-01]: Workspace operations carry no slash trigger.
    assert!(find_by_slash("/settings").is_none());
    assert!(find_by_slash("/dashboard").is_none());
    assert!(find_by_slash("/close").is_none());
    assert!(find_by_slash("/commands").is_none());
}
