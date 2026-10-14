//! Runtime-surface tests: overview, console/relay monitors, server status, activity state, tab surfaces.

use super::*;

#[tokio::test]
async fn transcript_sync_publishes_viewed_session_inside_runtime() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    let (_, viewed_session_id) =
        crate::event_loop::sync::sync_transcripts_and_session(&mut app, &runtime).await;

    assert_eq!(
        runtime.viewed_session_id.lock().await.as_deref(),
        Some(viewed_session_id.as_str())
    );
}

/// Regression for the wiring itself: the event loop feeds the input layer the
/// **unsuppressed** `completion_kind` (the dismissal latch travels as its own
/// `completion_dismissed` flag). Suppressing the kind while the latch is set
/// would make Tab's re-open branch unreachable — `completion_kind` would be
/// `None` exactly when the user pressed Tab after Esc — so this pins the
/// contract end to end through the real mapper.
#[test]
fn tab_after_esc_reopens_through_the_event_loop_context_shape() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();

    // Esc's arm: latch the dismissal, drop the highlight.
    app.suggestion_index = None;
    app.completion_dismissed = true;

    // Build the context exactly as `run_app_loop` does: the candidate list
    // is suppressed (empty) while the latch is set, but the classification
    // is NOT — that distinction is what makes the re-open gesture visible
    // to the input layer.
    let suppress_completions = app.completion_dismissed;
    let completions = if suppress_completions {
        Vec::new()
    } else {
        app.completions()
    };
    let completion_kind = app.completion_kind();
    let has_trigger_text = app.completion_trigger_text_present();
    let mut input = app.input.clone();
    let mut cursor = app.cursor_position;
    let mut drag = crate::model::selection::SelectionDrag::default();
    let action = crate::input::route_event(
        crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        crate::input::Dispatch::default(),
        &crate::modal_keys::ModalKeys::default(),
        &crate::sheet::SheetKeys::default(),
        &crate::session::SceneKeys {
            completion_kind,
            suggestion_count: completions.len(),
            has_exact_suggestion: false,
            suggestion_index: app.suggestion_index,
            completion_dismissed: app.completion_dismissed,
            has_trigger_text,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(
        action,
        crate::input::InputAction::ReopenCompletion,
        "Tab after Esc must re-open the dismissed slash menu"
    );
    // And the ReopenCompletion arm's state change restores a selected menu
    // once the loop's post-dispatch anchor runs.
    app.completion_dismissed = false;
    let completions = app.completions();
    app.anchor_completion_selection(&completions);
    assert_eq!(app.suggestion_index, Some(0));
    assert!(!app.completion_dismissed);
}

#[test]
fn relay_left_arrow_breaks_selection_at_head_then_steps() {
    let mut app = app_with_input_selection("hello world");
    // Hidden caret at the release point: end of "hello world" (char 11).
    // ← must break the selection there and step one left: 10.
    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "← must break selection"
    );
    assert_eq!(
        app.cursor_position, 10,
        "first ← lands one past the release point"
    );
}

#[test]
fn relay_right_arrow_clamps_at_buffer_end() {
    let mut app = app_with_input_selection("abc");
    app.cursor_position = 3; // released at the end
    relay_probe(&mut app, crossterm::event::KeyCode::Right);
    assert_eq!(app.cursor_position, 3, "→ past the end clamps");
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn relay_up_and_down_restore_hidden_caret() {
    // The hidden caret's position for a whole-input selection is defined as
    // the head edge (the buffer end) — ↑/↓ restore the caret there and
    // consume the press, rather than leaving the stale pre-selection
    // position in place.
    let mut app = app_with_input_selection("hello");
    app.cursor_position = 1; // stale visible caret from before the drag
    relay_probe(&mut app, crossterm::event::KeyCode::Up);
    assert_eq!(
        app.cursor_position, 5,
        "↑ must restore the caret at the head edge, not the stale position"
    );
    assert_eq!(app.selection, SelectionState::None);

    // ↓ behaves identically: adopt the head edge and consume the press. The
    // press itself does not walk lines or history — that resumes from the
    // restored position on the next key.
    let mut app = app_with_input_selection("hello");
    app.cursor_position = 1;
    relay_probe(&mut app, crossterm::event::KeyCode::Down);
    assert_eq!(app.cursor_position, 5);
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn relay_backspace_and_delete_replace_selection() {
    for code in [
        crossterm::event::KeyCode::Backspace,
        crossterm::event::KeyCode::Delete,
    ] {
        let mut app = app_with_input_selection("keep this");
        app.cursor_position = 1; // stale visible caret
        let action = relay_probe(&mut app, code);
        assert!(
            matches!(action, Some(crate::input::InputAction::Backspace)),
            "delete-family must return Backspace's post-edit signal"
        );
        assert_eq!(app.input, "", "the whole selection goes in one stroke");
        assert_eq!(app.cursor_position, 0);
        assert_eq!(app.selection, SelectionState::None);
    }
}

#[test]
fn relay_ignores_keys_without_selection_or_outside_family() {
    // No selection: the probe must miss so ordinary input handling runs.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hi".to_string();
    assert!(relay_probe(&mut app, crossterm::event::KeyCode::Left).is_none());

    // With a selection, an uninvolved key (e.g. `x`) must NOT be swallowed:
    // typing over a selection is out of scope for the relay (the TUI has no
    // replace-selection-on-type), so the key keeps its normal meaning.
    let mut app = app_with_input_selection("hi");
    assert!(relay_probe(&mut app, crossterm::event::KeyCode::Char('x')).is_none());
    assert!(
        app.has_input_selection(),
        "an uninvolved key must leave the selection intact"
    );
}

#[tokio::test]
async fn console_bare_text_prompts_the_selection() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    console_dispatch(&mut app, "fix the flaky test", false).await;
    match &app.host_console_log[..] {
        [
            crate::overlays::ConsoleLine::Dispatch {
                targets, action, ..
            },
        ] => {
            assert_eq!(targets, &[1], "bare text routes to the selection (#1)");
            assert_eq!(*action, "prompt");
        }
        other => panic!("expected one dispatch line, got {other:?}"),
    }
}

#[tokio::test]
async fn console_bare_text_from_n_creates_instead() {
    // The `n`-opened prompt's default role is create: an explicit address
    // overrides it, but plain text must not silently prompt another
    // session.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    console_dispatch(&mut app, "refactor the retry loop", true).await;
    match &app.host_console_log[..] {
        [
            crate::overlays::ConsoleLine::Dispatch {
                targets, action, ..
            },
        ] => {
            assert!(targets.is_empty(), "create targets nobody");
            assert_eq!(*action, "new session");
        }
        other => panic!("expected one dispatch line, got {other:?}"),
    }
}

#[tokio::test]
async fn console_unknown_address_is_a_notice_not_a_dispatch() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    console_dispatch(&mut app, "@9 do the thing", false).await;
    match &app.host_console_log[..] {
        [crate::overlays::ConsoleLine::Notice(text)] => {
            assert!(text.contains("#9"), "notice names the address: {text}");
        }
        other => panic!("expected one notice, got {other:?}"),
    }
}

#[tokio::test]
async fn console_verb_without_selection_is_a_notice() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    app.host_sessions.clear();
    console_dispatch(&mut app, "/interrupt", false).await;
    match &app.host_console_log[..] {
        [crate::overlays::ConsoleLine::Notice(text)] => {
            assert!(text.contains("no session"), "notice explains: {text}");
        }
        other => panic!("expected one notice, got {other:?}"),
    }
}

#[tokio::test]
async fn console_help_lists_the_grammar() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    console_dispatch(&mut app, "/help", false).await;
    let text: Vec<String> = app
        .host_console_log
        .iter()
        .filter_map(|l| match l {
            crate::overlays::ConsoleLine::Notice(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    let joined = text.join("\n");
    for verb in ["/interrupt", "/suspend", "/kill", "/new", "@3 text"] {
        assert!(joined.contains(verb), "help must mention {verb}: {joined}");
    }
}

#[tokio::test]
async fn console_kill_key_arms_then_confirms() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    crate::event_loop::host_test_shims::kill(&mut app, &runtime);
    // First press: armed, with a notice naming the target.
    assert!(app.host_kill_confirm.is_some(), "first k arms");
    assert!(matches!(
        app.host_console_log.last(),
        Some(crate::overlays::ConsoleLine::Notice(t)) if t.contains("#1")
    ));
    // Second press: confirmed — the arm clears and a kill dispatch logs.
    crate::event_loop::host_test_shims::kill(&mut app, &runtime);
    assert!(app.host_kill_confirm.is_none(), "second k fires");
    assert!(matches!(
        app.host_console_log.last(),
        Some(crate::overlays::ConsoleLine::Dispatch { action, .. }) if *action == "kill"
    ));
}

#[tokio::test]
async fn console_kill_arm_cancels_on_selection_move() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    crate::event_loop::host_test_shims::kill(&mut app, &runtime);
    assert!(app.host_kill_confirm.is_some());
    // Moving the dock selection (the ModalUp path) cancels the arm.
    crate::event_loop::host_test_shims::kill_cancel(&mut app);
    assert!(app.host_kill_confirm.is_none());
    // A `k` after the cancel arms afresh rather than firing.
    crate::event_loop::host_test_shims::kill(&mut app, &runtime);
    assert!(app.host_kill_confirm.is_some(), "re-arm, not fire");
    assert_eq!(app.host_console_log.len(), 2, "no kill dispatched yet");
}

#[test]
fn websearch_provider_dropdown_builds_and_selects() {
    let ws = nuo_wire::WebSearchConfigView {
        revision: 0,
        provider: nuo_wire::WebSearchProvider::Exa,
        reader: nuo_wire::WebReaderProvider::Disabled,
        timeout_secs: 20,
        searxng_url: None,
        search_credential: nuo_wire::WebCredentialStatus::Stored,
        reader_credential: nuo_wire::WebCredentialStatus::NotRequired,
        capabilities: nuo_wire::web_provider_capabilities(),
    };
    let dropdown = crate::overlays::build_websearch_provider_dropdown("tavily", Some(&ws));
    assert_eq!(dropdown.context.as_deref(), Some("websearch_provider"));
    assert_eq!(
        dropdown.selected_payload().map(|s| s.as_str()),
        Some("tavily")
    );
    // The six compiled search providers plus the `disabled` entry.
    assert_eq!(dropdown.items.len(), 7);
    assert_eq!(
        dropdown
            .items
            .iter()
            .find(|item| item.id == "exa")
            .and_then(|item| item.indicator),
        Some(crate::components::dropdown::DropdownIndicator::Ready)
    );
    assert_eq!(
        dropdown
            .items
            .iter()
            .find(|item| item.id == "tavily")
            .and_then(|item| item.indicator),
        None,
        "the server reports readiness only for the active provider"
    );
}

#[test]
fn websearch_reader_dropdown_builds_and_selects() {
    let mut ws = nuo_wire::WebSearchConfigView {
        revision: 0,
        provider: nuo_wire::WebSearchProvider::Exa,
        reader: nuo_wire::WebReaderProvider::Jina,
        timeout_secs: 20,
        searxng_url: None,
        search_credential: nuo_wire::WebCredentialStatus::Stored,
        reader_credential: nuo_wire::WebCredentialStatus::Stored,
        capabilities: nuo_wire::web_provider_capabilities(),
    };
    let dropdown = crate::overlays::build_websearch_reader_dropdown("jina", Some(&ws));
    assert_eq!(dropdown.context.as_deref(), Some("websearch_reader"));
    assert_eq!(
        dropdown.selected_payload().map(|s| s.as_str()),
        Some("jina")
    );
    // The single compiled reader plus the `disabled` entry.
    assert_eq!(dropdown.items.len(), 2);

    // A snapshot that advertises no reader capability offers only `disabled`.
    ws.capabilities
        .retain(|capability| capability.axis == nuo_wire::WebProviderAxis::Search);
    let empty_dropdown = crate::overlays::build_websearch_reader_dropdown("disabled", Some(&ws));
    assert_eq!(empty_dropdown.items.len(), 1);
}

/// Sheet-mount focus consistency (ADR-0173 §3 × ADR-0174): an agent-driven
/// sheet is a context switch — the transcript-focus states (`focused_target`
/// and `transcript_focused`) parked at every mount site must include browse
/// focus, not just the step target. Without the browse-focus half, this
/// sequence dims the composer forever after the sheet closes: user clicks the
/// transcript (browse focus) → question sheet mounts (only `focused_target`
/// cleared) → user answers → sheet unmounts → composer renders its inactive
/// palette against the stale `transcript_focused`.
#[tokio::test]
async fn sheet_mount_parks_browse_focus_not_just_step_target() {
    use nuo_wire::{UserQuestion, UserQuestionOption, UserQuestionRequest};

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    // Browse focus is live: the user clicked into the transcript a moment ago.
    app.transcript_focused = true;

    // The agent's question arrives; the per-frame sync mounts the sheet.
    app.pending_questions.push_back(UserQuestionRequest {
        id: "q1".into(),
        questions: vec![UserQuestion {
            header: Some("Style".into()),
            question: "Which error handling crate?".into(),
            options: vec![
                UserQuestionOption {
                    label: "anyhow".into(),
                    description: None,
                },
                UserQuestionOption {
                    label: "eyre".into(),
                    description: None,
                },
            ],
            multi_select: false,
        }],
        origin: None,
    });
    crate::event_loop::sync::sync_request_surfaces(&mut app, &runtime);

    assert_eq!(
        app.active_sheet(),
        Some(crate::sheet::SheetKind::Question),
        "question sheet mounted"
    );
    assert!(
        !app.transcript_focused,
        "browse focus must be parked at sheet mount, alongside the step target"
    );
    assert!(app.focused_target.is_none());

    // The user answers; the Closed effect unmounts the sheet. The composer
    // slot is handed back clean — no stale focus dims it.
    app.question = None;
    app.dismiss_sheet();

    assert_eq!(
        app.caret_owner(),
        crate::CaretOwner::Composer,
        "composer must own the caret again after the sheet closes"
    );
}

/// The permission sheet's pass-through (ADR-0173 §2) stays intact: browse
/// focus legitimately re-armed *behind* the sheet while it is up (a click
/// into the transcript during the decision) is not stolen back by the
/// per-frame sync — the park happens at mount only.
#[tokio::test]
async fn permission_sheet_does_not_steal_focus_rearmed_behind_it() {
    use nuo_wire::PermissionRequest;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    app.pending_permissions.push_back(PermissionRequest {
        id: "p1".into(),
        tool: "bash".into(),
        label: "Run tests".into(),
        description: String::new(),
        arguments: "{}".into(),
        scope: String::new(),
        elevation: false,
        one_off: false,
        origin: None,
        hazard: None,
        submission: None,
    });
    crate::event_loop::sync::sync_request_surfaces(&mut app, &runtime);
    assert_eq!(
        app.active_sheet(),
        Some(crate::sheet::SheetKind::Permission)
    );
    assert!(!app.transcript_focused, "parked at mount");

    // The user clicks the transcript behind the pass-through sheet — the
    // mouse path legitimately re-arms browse focus (mouse.rs).
    app.transcript_focused = true;

    // A later sync frame must not clear it again (no re-mount occurs —
    // the sheet is already up).
    crate::event_loop::sync::sync_request_surfaces(&mut app, &runtime);
    assert!(
        app.transcript_focused,
        "browse focus re-armed behind the pass-through sheet must survive"
    );
}

#[tokio::test]
async fn terminal_resize_action_clears_height_cache_and_marks_scroll_settle() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    // Populate a height cache entry
    app.layout_height_cache.set(1, 10);
    assert_eq!(app.layout_height_cache.get(1), Some(10));

    // When scrolled into history (!follow_bottom)
    app.follow_bottom = false;
    app.scroll_settle_pending = false;

    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::TerminalResized {
            cols: 120,
            rows: 40,
        },
        "session-1",
    )
    .await;

    // Height cache must be cleared for re-layout
    assert_eq!(app.layout_height_cache.get(1), None);
    // Scroll settle must be pending to re-clamp scroll offset
    assert!(app.scroll_settle_pending);
}
