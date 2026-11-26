use crate::shell::client::ClientOutbound;
use crate::shell::event_loop::apply::apply;
use crate::shell::event_loop::{AppMutation, TranscriptEdit, UiRuntime};
use crate::shell::tests::new_app_for_relay_tests;
use tool::Role;
use view::model::document::TranscriptMessage;

/// ADR-0251 / ADR-0253: switching between two resident thread tabs moves whole
/// `ThreadInstance`s in O(1) — preserving messages, scroll, follow_bottom,
/// disclosure state, composer drafts, and model attribution with zero network
/// traffic and zero field-by-field reconciliation.
#[test]
fn switching_between_resident_tabs_preserves_state_without_rebuild() {
    let mut app = new_app_for_relay_tests();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;

    app.session.current_session_id = "thread-a".to_string();
    app.session.live_session_id = "thread-a".to_string();
    app.threads.sync_foreground_id("thread-a");
    app.session
        .subscribed_threads
        .insert("thread-a".to_string());
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "thread-a",
        Some("A"),
    ));
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "thread-b",
        Some("B"),
    ));
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "thread-a",
        Some("A"),
    ));
    app.threads.current_model = "deepseek-v4.1-flash".to_string();
    app.threads
        .messages
        .push(TranscriptMessage::new(Role::User, "Question in thread A"));
    app.threads.scroll = 42;
    app.threads.follow_bottom = false;
    app.composer.input = "Draft for A".to_string();

    // First switch to cold thread-b: mounts B, sends one Subscribe.
    app.surfaces.surfaces.select_tab(1);
    app.activate_thread_workspace("thread-b");
    assert_eq!(app.session.current_session_id, "thread-b");
    assert!(app.threads.messages.is_empty());
    assert_eq!(app.composer.input, "");
    assert!(matches!(
        rx.try_recv(),
        Ok(ClientOutbound::Subscribe {
            target: protocol::ThreadTarget::Attach { id: Some(ref tid) },
            ..
        }) if tid == "thread-b"
    ));

    // Populate thread-b.
    app.threads.current_model = "gemini-4-neon".to_string();
    app.threads
        .messages
        .push(TranscriptMessage::new(Role::User, "Question in thread B"));
    app.threads.scroll = 17;
    app.composer.input = "Draft for B".to_string();

    // Switch back to thread-a: ZERO Subscribe frames, exact state restored!
    app.surfaces.surfaces.select_tab(0);
    app.activate_thread_workspace("thread-a");
    assert!(
        rx.try_recv().is_err(),
        "switching to an already-subscribed resident tab must send zero wire frames"
    );
    assert_eq!(app.session.current_session_id, "thread-a");
    assert_eq!(app.threads.current_model, "deepseek-v4.1-flash");
    assert_eq!(app.threads.messages.len(), 1);
    assert_eq!(app.threads.messages[0].raw, "Question in thread A");
    assert_eq!(app.threads.scroll, 42);
    assert!(!app.threads.follow_bottom);
    assert_eq!(app.composer.input, "Draft for A");

    // Switch back to thread-b: ZERO Subscribe frames, exact state restored!
    app.surfaces.surfaces.select_tab(1);
    app.activate_thread_workspace("thread-b");
    assert!(rx.try_recv().is_err());
    assert_eq!(app.threads.current_model, "gemini-4-neon");
    assert_eq!(app.threads.messages.len(), 1);
    assert_eq!(app.threads.messages[0].raw, "Question in thread B");
    assert_eq!(app.threads.scroll, 17);
    assert_eq!(app.composer.input, "Draft for B");
}

/// ADR-0251 / ADR-0253: background thread streaming mutations land directly in
/// the background `ThreadInstance` through the same unified applier path.
#[test]
fn background_thread_mutations_update_resident_workspace_live() {
    let mut app = new_app_for_relay_tests();
    let runtime = UiRuntime::minimal_for_test();

    app.session.current_session_id = "thread-a".to_string();
    app.session.live_session_id = "thread-a".to_string();
    app.threads.sync_foreground_id("thread-a");
    app.session
        .subscribed_threads
        .insert("thread-a".to_string());
    app.session
        .subscribed_threads
        .insert("thread-b".to_string());
    app.threads
        .messages
        .push(TranscriptMessage::new(Role::User, "A prompt"));

    // Switch to thread-b while thread-a stays resident in background.
    app.activate_thread_workspace("thread-b");
    assert_eq!(app.session.current_session_id, "thread-b");
    assert!(app.threads.messages.is_empty());

    // A live assistant message arrives for background thread-a.
    let bg_changed = apply(
        &mut app,
        &runtime,
        AppMutation::Transcript {
            thread_id: Some("thread-a".to_string()),
            edit: TranscriptEdit::Append {
                class: protocol::EventClass::Dialogue,
                message: TranscriptMessage::new(Role::Assistant, "Live reply for A"),
            },
        },
    );
    assert!(
        !bg_changed,
        "background transcript mutation must not mark foreground dirty"
    );
    assert!(
        app.threads.messages.is_empty(),
        "foreground thread-b must remain untouched"
    );

    // Switching back to thread-a immediately reveals the live reply!
    app.activate_thread_workspace("thread-a");
    assert_eq!(app.threads.messages.len(), 2);
    assert_eq!(app.threads.messages[1].raw, "Live reply for A");
}

/// Closing a thread tab drops its resident instance and emits
/// `ClientOutbound::Unsubscribe`.
#[test]
fn closing_thread_workspace_unsubscribes_and_frees_memory() {
    let mut app = new_app_for_relay_tests();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;

    app.session.current_session_id = "thread-a".to_string();
    app.session.live_session_id = "thread-a".to_string();
    app.threads.sync_foreground_id("thread-a");
    app.session
        .subscribed_threads
        .insert("thread-a".to_string());
    app.session
        .subscribed_threads
        .insert("thread-b".to_string());
    app.activate_thread_workspace("thread-b");
    app.activate_thread_workspace("thread-a");
    assert!(app.threads.contains("thread-b"));

    app.close_thread_workspace("thread-b");
    assert!(!app.threads.contains("thread-b"));
    assert!(!app.session.subscribed_threads.contains("thread-b"));
    assert!(matches!(
        rx.try_recv(),
        Ok(ClientOutbound::Unsubscribe { thread_id }) if thread_id == "thread-b"
    ));
}

/// ADR-0253 §5: emptiness predicate agrees with durable authority — command rows
/// do not make a thread non-empty, whereas dialogue or a started round does.
#[test]
fn emptiness_predicate_excludes_command_rows_and_includes_dialogue_and_rounds() {
    let mut inst = super::ThreadInstance::new("t1", 1, protocol::ThreadPhase::Ready);
    assert!(inst.is_dialogue_empty(), "fresh thread is empty");

    // Command rows are not dialogue (ADR-0252 §2, ADR-0253 §5).
    inst.messages
        .push(TranscriptMessage::pending_command("status", ""));
    assert!(
        inst.is_dialogue_empty(),
        "command-only thread must still be empty"
    );

    // Started round makes it non-empty even with zero dialogue rows.
    inst.round_count = 1;
    assert!(!inst.is_dialogue_empty(), "started round is non-empty");
    inst.round_count = 0;

    // Dialogue message makes it non-empty.
    inst.messages
        .push(TranscriptMessage::new(Role::User, "hello"));
    assert!(!inst.is_dialogue_empty(), "dialogue row is non-empty");
}

/// Regression: switching to a finished/idle tab while another resident tab is
/// actively running its ReAct loop must never resurrect the idle tab's
/// activity bar, elapsed timer, turn counter, retry clause, background tasks,
/// or harness posture — each `ThreadInstance` is the sole owner of its chrome.
#[test]
fn background_thread_activity_never_leaks_onto_finished_foreground_tab() {
    use crate::shell::event_loop::mutations::ChromeEdit;
    use std::sync::atomic::Ordering;

    let mut app = new_app_for_relay_tests();
    let runtime = UiRuntime::minimal_for_test();

    app.session.current_session_id = "thread-done".to_string();
    app.session.live_session_id = "thread-done".to_string();
    app.threads.sync_foreground_id("thread-done");
    app.session
        .subscribed_threads
        .insert("thread-done".to_string());
    app.session
        .subscribed_threads
        .insert("thread-react".to_string());

    // Thread A finished its task and is idle.
    app.threads.loop_status = protocol::LoopStatus::Idle;
    app.threads.phase = None;
    app.threads.round_started_at = None;
    app.threads.current_turn = 0;

    // Open/mount Thread B and switch back to finished Thread A.
    app.activate_thread_workspace("thread-react");
    app.activate_thread_workspace("thread-done");
    assert_eq!(app.session.current_session_id, "thread-done");
    assert!(!app.viewed_chrome().responding);
    assert!(app.viewed_chrome().phase.is_none());

    // Thread B starts a round in background: Idle -> Running flips TabBar dirty once.
    let started_dirty = apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("thread-react".to_string()),
            edit: ChromeEdit::RoundLifecycle {
                round_count: 1,
                running: true,
                can_retry: false,
            },
        },
    );
    assert!(
        started_dirty,
        "background Idle->Running must request a TabBar repaint"
    );
    assert!(app.session.running_sessions.contains("thread-react"));
    assert!(!app.session.running_sessions.contains("thread-done"));
    assert!(runtime.is_responding.load(Ordering::SeqCst));

    // Subsequent in-flight ReAct updates on Thread B stay silent for foreground
    // and never touch finished Thread A.
    let retry = crate::shell::app::ProviderRetryState::scheduled(
        2,
        5,
        std::time::Instant::now() + std::time::Duration::from_secs(4),
        "HTTP 529 Overloaded",
    );
    for mutation in [
        AppMutation::ChromeEdit {
            thread_id: Some("thread-react".to_string()),
            edit: ChromeEdit::TurnStarted { round: 1, turn: 3 },
        },
        AppMutation::ChromeEdit {
            thread_id: Some("thread-react".to_string()),
            edit: ChromeEdit::TransportSetback(retry),
        },
        AppMutation::BackgroundTaskStarted {
            thread_id: Some("thread-react".to_string()),
            id: "job-b".to_string(),
            label: "cargo build".to_string(),
            started_at_ms: 1000,
        },
        AppMutation::HarnessUnattended {
            thread_id: Some("thread-react".to_string()),
            enabled: true,
        },
        AppMutation::HarnessConfined {
            thread_id: Some("thread-react".to_string()),
            confined: false,
        },
    ] {
        let dirty = apply(&mut app, &runtime, mutation);
        assert!(
            !dirty,
            "steady-state background ReAct updates must not dirty foreground thread"
        );
    }

    // Finished foreground Thread A stays completely idle and untouched.
    let done_chrome = app.viewed_chrome();
    assert!(
        done_chrome.phase.is_none(),
        "idle foreground tab must not inherit background phase"
    );
    assert!(
        !done_chrome.responding,
        "idle foreground tab must not become responding"
    );
    assert!(
        done_chrome.round_started_at.is_none(),
        "idle foreground tab must not inherit background timer"
    );
    assert_eq!(done_chrome.current_turn, 0);
    assert!(done_chrome.transport_setback.is_none());
    assert!(!app.has_visible_background_tasks());
    assert!(!app.threads.unattended);
    assert!(app.threads.confined);
    assert!(
        app.has_live_transport_setback(),
        "background setback still keeps countdown animation alive"
    );

    // Background Thread B advances from AwaitingModel to Tool work:
    // its own retry clause retires, while Thread A remains idle.
    apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("thread-react".to_string()),
            edit: ChromeEdit::PhaseOnly(Some(view::phase::Phase::Tool(
                view::phase::ToolVerb::Running,
            ))),
        },
    );
    assert!(app.viewed_chrome().phase.is_none());
    assert!(!app.has_live_transport_setback());

    // Switching to Thread B immediately shows its live ReAct state & background task.
    app.activate_thread_workspace("thread-react");
    let react_chrome = app.viewed_chrome();
    assert_eq!(
        react_chrome.phase,
        Some(view::phase::Phase::Tool(view::phase::ToolVerb::Running))
    );
    assert!(react_chrome.responding);
    assert!(react_chrome.round_started_at.is_some());
    assert_eq!(react_chrome.current_turn, 3);
    assert!(react_chrome.transport_setback.is_none());
    assert!(app.has_visible_background_tasks());
    assert!(app.threads.unattended);
    assert!(!app.threads.confined);

    // ClearPermissions must never wipe Thread B's active Tool phase while
    // clearing AwaitingUser on another thread.
    apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("thread-done".to_string()),
            edit: ChromeEdit::ActivityFolded(view::phase::Phase::AwaitingUser),
        },
    );
    apply(&mut app, &runtime, AppMutation::ClearPermissions);
    assert_eq!(
        app.viewed_chrome().phase,
        Some(view::phase::Phase::Tool(view::phase::ToolVerb::Running)),
        "ClearPermissions must not clear non-AwaitingUser phases"
    );
    assert_eq!(
        app.threads.get("thread-done").and_then(|t| t.phase.clone()),
        None,
        "ClearPermissions clears AwaitingUser on parked threads"
    );

    // Ending Thread B retires its live surface and clears running_sessions + runtime flag.
    apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("thread-react".to_string()),
            edit: ChromeEdit::RoundEnded,
        },
    );
    assert!(!app.viewed_chrome().responding);
    assert!(!app.session.running_sessions.contains("thread-react"));
    assert!(!runtime.is_responding.load(Ordering::SeqCst));
}

/// Closing the active foreground thread tab must drop it rather than parking
/// a ghost instance into `background` when the next tab activates.
#[test]
fn closing_foreground_thread_workspace_drops_it_without_leaking_to_background() {
    let mut app = new_app_for_relay_tests();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;

    app.session.current_session_id = "thread-a".to_string();
    app.session.live_session_id = "thread-a".to_string();
    app.threads.sync_foreground_id("thread-a");
    app.session
        .subscribed_threads
        .insert("thread-a".to_string());
    app.session
        .subscribed_threads
        .insert("thread-b".to_string());
    app.activate_thread_workspace("thread-b");
    app.activate_thread_workspace("thread-a");
    app.session.running_sessions.insert("thread-a".to_string());

    // Close foreground thread-a, then activate thread-b (same order as handle_close_tab_at).
    app.close_thread_workspace("thread-a");
    app.activate_thread_workspace("thread-b");

    assert_eq!(app.session.current_session_id, "thread-b");
    assert!(
        !app.threads.contains("thread-a"),
        "closed foreground thread must not leak into background"
    );
    assert!(!app.session.running_sessions.contains("thread-a"));
}
