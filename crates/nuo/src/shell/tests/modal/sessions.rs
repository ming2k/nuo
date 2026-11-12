use super::*;

#[tokio::test]
async fn open_selected_session_preserves_current_running_session_messages_and_resets_scroll() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-running-123";
    app.current_session_id = sid.to_string();
    app.running_sessions.insert(sid.to_string());
    app.messages
        .push(nuo_view::model::document::TranscriptMessage::new(
            nuo_protocol::Role::User,
            "Hello",
        ));
    app.scroll = 42;

    app.sessions_overview = vec![nuo_protocol::ThreadOverview {
        id: sid.to_string(),
        title: Some("Running Task".into()),
        overview: "Running Task".into(),
        created_at: 100,
        updated_at: 200,
        message_count: 1,
        active: true,
        parent_id: None,
        fork_kind: nuo_protocol::ThreadForkKind::Trunk,
        fork_point: None,
        digest: None,
    }];
    app.open_dialog(crate::shell::surfaces::DialogKind::Sessions);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Sessions)
    );

    // Dispatch OpenSelectedSession via dispatch_action_for_test
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenSelectedSession,
        sid,
    )
    .await;

    // Modal dismissed
    assert_eq!(app.active_dialog(), None);
    // Messages must NOT be wiped!
    assert_eq!(app.messages.len(), 1);
    // Scroll reset to 0 (pinned to bottom)
    assert_eq!(app.scroll, 0);
    // Same session does not trigger process quit / switch
    assert_eq!(app.switch_to_target, None);
}

#[tokio::test]
async fn open_selected_session_switches_to_another_thread_via_client_tab_and_runner_attach() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let current_sid = "session-1";
    let target_sid = "session-2";
    app.current_session_id = current_sid.to_string();

    app.sessions_overview = vec![nuo_protocol::ThreadOverview {
        id: target_sid.to_string(),
        title: Some("Target Task".into()),
        overview: "Target Task".into(),
        created_at: 100,
        updated_at: 200,
        message_count: 3,
        active: false,
        parent_id: None,
        fork_kind: nuo_protocol::ThreadForkKind::Trunk,
        fork_point: None,
        digest: None,
    }];
    app.open_dialog(crate::shell::surfaces::DialogKind::Sessions);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Sessions)
    );

    // Dispatch OpenSelectedSession on the target thread
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenSelectedSession,
        current_sid,
    )
    .await;

    // Modal dismissed
    assert_eq!(app.active_dialog(), None);
    // ClientTab opened/focused
    assert!(
        app.surfaces
            .tabs()
            .iter()
            .any(|t| t.thread_id() == Some(target_sid))
    );
    // Runner attach queued
    assert_eq!(app.switch_to_target, Some(target_sid.to_string()));
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn test_subagent_scene_history_navigation_and_stack_unwind() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "conv-1",
        None::<String>,
    ));
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );

    // Enter subagent
    app.enter_subagent("call_child_1".to_string());
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Subagent
    );
    assert!(app.in_subagent_view());

    // Back in scene history via TabHistoryBack (emitted by `Alt+[` or mouse click, ADR-0092)
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::TabHistoryBack,
        "conv-1",
    )
    .await;

    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
    assert!(!app.in_subagent_view());

    // Forward in scene history via TabHistoryForward (emitted by `Alt+]`, ADR-0092)
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::TabHistoryForward,
        "conv-1",
    )
    .await;

    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Subagent
    );
}

#[tokio::test]
async fn test_close_tab_accurately_closes_tab_on_thread_and_subagent() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "conv-1",
        None::<String>,
    ));
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "conv-2",
        None::<String>,
    ));
    assert_eq!(app.surfaces.tabs().len(), 2);
    assert_eq!(app.surfaces.active_tab_index(), 1);

    // Enter subagent in Tab 1
    app.enter_subagent("call_sub_2".to_string());
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Subagent
    );

    // C-x w dispatches CloseTab: closes Tab 1 entirely, does not just pop subagent
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseTab,
        "conv-2",
    )
    .await;

    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(app.surfaces.active_tab_index(), 0);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
    assert_eq!(app.switch_to_target, Some("conv-1".to_string()));

    // Close remaining Tab 0
    let flow = crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseTab,
        "conv-1",
    )
    .await;

    assert_eq!(flow, crate::shell::event_loop::actions::ActionFlow::Exit);
    assert!(app.surfaces.tabs().is_empty());
}

#[tokio::test]
async fn test_close_tab_switches_to_mru_thread_and_arms_reconnect() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.current_session_id = "conv-1".to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "conv-1",
        Some("Thread 1"),
    ));
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "conv-2",
        Some("Thread 2"),
    ));
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "conv-3",
        Some("Thread 3"),
    ));
    assert_eq!(app.surfaces.tabs().len(), 3);
    assert_eq!(app.surfaces.active_tab_index(), 2);

    // Visit conv-1, then conv-2
    app.surfaces.select_tab(0);
    app.surfaces.select_tab(1);
    assert_eq!(app.surfaces.active_tab_index(), 1);

    // Close Tab 1 (conv-2). MRU history: conv-3 -> conv-1 -> conv-2.
    // Closing conv-2 must jump back to conv-1, NOT conv-3!
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseTab,
        "conv-2",
    )
    .await;

    assert_eq!(app.surfaces.tabs().len(), 2);
    assert_eq!(app.surfaces.active_tab_index(), 0);
    assert_eq!(app.surfaces.tabs()[0].thread_id(), Some("conv-1"));
    assert_eq!(app.switch_to_target, Some("conv-1".to_string()));
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn test_close_tab_from_dashboard_and_settings_updates_scene_and_renders() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.current_session_id = "s1".to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "s1",
        Some("Thread 1"),
    ));
    app.open_tab(crate::shell::surfaces::ClientTab::settings());
    app.open_tab(crate::shell::surfaces::ClientTab::dashboard());

    assert_eq!(app.surfaces.tabs().len(), 3);
    assert_eq!(app.surfaces.active_tab_index(), 2);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Dashboard
    );

    // Close active Tab 2 (Dashboard). MRU previous is Settings (Tab 1).
    let flow = crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseTab,
        "s1",
    )
    .await;

    assert_eq!(flow, crate::shell::event_loop::actions::ActionFlow::Handled);
    assert_eq!(app.surfaces.tabs().len(), 2);
    assert_eq!(app.surfaces.active_tab_index(), 1);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Settings
    );
    assert_eq!(
        app.config_focus,
        crate::shell::overlays::ConfigFocus::Categories
    );

    // Close active Tab 1 (Settings). MRU previous is Thread 1 (Tab 0).
    let flow = crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseTab,
        "s1",
    )
    .await;

    assert_eq!(flow, crate::shell::event_loop::actions::ActionFlow::Handled);
    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(app.surfaces.active_tab_index(), 0);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
    assert!(app.focus.is_composer());
}

#[tokio::test]
async fn open_selected_session_overwrites_empty_tab_and_discards_empty_thread() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let empty_sid = "empty-session-1";
    let target_sid = "target-session-2";
    app.current_session_id = empty_sid.to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        empty_sid,
        None::<String>,
    ));
    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(app.surfaces.tabs()[0].thread_id(), Some(empty_sid));
    assert!(app.is_current_thread_empty());

    app.sessions_overview = vec![nuo_protocol::ThreadOverview {
        id: target_sid.to_string(),
        title: Some("Target Thread".into()),
        overview: "Target Thread".into(),
        created_at: 100,
        updated_at: 200,
        message_count: 5,
        active: false,
        parent_id: None,
        fork_kind: nuo_protocol::ThreadForkKind::Trunk,
        fork_point: None,
        digest: None,
    }];
    app.open_dialog(crate::shell::surfaces::DialogKind::Sessions);

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenSelectedSession,
        empty_sid,
    )
    .await;

    // Must NOT create a second tab! Tab count must remain 1, overwritten in-place!
    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(app.surfaces.tabs()[0].thread_id(), Some(target_sid));
    assert_eq!(app.switch_to_target, Some(target_sid.to_string()));

    // Must have sent DeleteSession intent for the abandoned empty thread
    let req = rx.try_recv().expect("DeleteSession sent for empty thread");
    match req {
        nuo_protocol::AgentRequest::DeleteThread { id } => {
            assert_eq!(id, empty_sid);
        }
        other => panic!("expected DeleteSession, got {other:?}"),
    }
}

#[tokio::test]
async fn open_selected_session_appends_new_tab_when_current_tab_has_messages() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let active_sid = "active-session-1";
    let target_sid = "target-session-2";
    app.current_session_id = active_sid.to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        active_sid,
        Some("Active Thread"),
    ));
    // Add a message to make it non-empty
    app.messages
        .push(nuo_view::model::document::TranscriptMessage::new(
            nuo_protocol::Role::User,
            "Hello",
        ));
    assert!(!app.is_current_thread_empty());

    app.sessions_overview = vec![nuo_protocol::ThreadOverview {
        id: target_sid.to_string(),
        title: Some("Target Thread".into()),
        overview: "Target Thread".into(),
        created_at: 100,
        updated_at: 200,
        message_count: 5,
        active: false,
        parent_id: None,
        fork_kind: nuo_protocol::ThreadForkKind::Trunk,
        fork_point: None,
        digest: None,
    }];
    app.open_dialog(crate::shell::surfaces::DialogKind::Sessions);

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenSelectedSession,
        active_sid,
    )
    .await;

    // Must append a new tab (total 2 tabs)!
    assert_eq!(app.surfaces.tabs().len(), 2);
    assert_eq!(app.surfaces.tabs()[0].thread_id(), Some(active_sid));
    assert_eq!(app.surfaces.tabs()[1].thread_id(), Some(target_sid));
    assert_eq!(app.switch_to_target, Some(target_sid.to_string()));

    // Must NOT have sent DeleteSession!
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn create_new_session_resets_in_place_when_empty_and_switches_to_new_when_non_empty() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let empty_sid = "empty-session-1";
    app.current_session_id = empty_sid.to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        empty_sid,
        None::<String>,
    ));
    assert!(app.is_current_thread_empty());

    // 1. In empty tab: pressing 'N' should send /new to reset in-place
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CreateNewSession,
        empty_sid,
    )
    .await;

    assert!(!app.switch_to_new);
    let req = rx.try_recv().expect("/new sent for empty thread");
    match req {
        nuo_protocol::AgentRequest::SlashCommand(cmd) => assert_eq!(cmd, "/new"),
        other => panic!("expected /new slash command, got {other:?}"),
    }

    // 2. In non-empty tab: pressing 'N' should arm switch_to_new
    app.messages
        .push(nuo_view::model::document::TranscriptMessage::new(
            nuo_protocol::Role::User,
            "Existing message",
        ));
    assert!(!app.is_current_thread_empty());

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CreateNewSession,
        empty_sid,
    )
    .await;

    assert!(app.switch_to_new);
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn slash_new_command_resets_in_place_when_empty_and_switches_to_new_tab_when_non_empty() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-test-1";
    app.current_session_id = sid.to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        sid,
        None::<String>,
    ));
    let session = crate::shell::SessionSource::Remote {
        session_id: sid.to_string(),
    };
    assert!(app.is_current_thread_empty());

    // 1. In empty tab: "/new" should send /new to reset in-place
    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/new".to_string()).await;

    assert!(!app.switch_to_new);
    let req = rx.try_recv().expect("/new sent for empty thread");
    match req {
        nuo_protocol::AgentRequest::SlashCommand(cmd) => assert_eq!(cmd, "/new"),
        other => panic!("expected /new slash command, got {other:?}"),
    }

    // 2. In non-empty tab: "/new" must NOT send /new over wire, but arm switch_to_new and preserve conversation
    app.messages.clear();
    app.messages
        .push(nuo_view::model::document::TranscriptMessage::new(
            nuo_protocol::Role::User,
            "Crucial conversation",
        ));
    assert!(!app.is_current_thread_empty());

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/new".to_string()).await;

    assert!(app.switch_to_new);
    assert_eq!(app.switch_to_new_prompt, None);
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
    // Verify no destructive in-place reset command was sent
    assert!(rx.try_recv().is_err());
    // Verify existing messages are preserved
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].raw, "Crucial conversation");
}

#[tokio::test]
async fn slash_new_with_prompt_arms_switch_to_new_prompt_when_non_empty() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-test-2";
    app.current_session_id = sid.to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        sid,
        None::<String>,
    ));
    let session = crate::shell::SessionSource::Remote {
        session_id: sid.to_string(),
    };

    app.messages
        .push(nuo_view::model::document::TranscriptMessage::new(
            nuo_protocol::Role::User,
            "Prior turn",
        ));
    assert!(!app.is_current_thread_empty());

    super::event_loop::handle_send_slash(
        &mut app,
        &runtime,
        &session,
        "/new optimize memory allocations".to_string(),
    )
    .await;

    assert!(app.switch_to_new);
    assert_eq!(
        app.switch_to_new_prompt,
        Some("optimize memory allocations".to_string())
    );
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn slash_session_new_behaves_identically_to_slash_new() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-canonical-test";
    app.current_session_id = sid.to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        sid,
        None::<String>,
    ));
    let session = crate::shell::SessionSource::Remote {
        session_id: sid.to_string(),
    };

    app.messages
        .push(nuo_view::model::document::TranscriptMessage::new(
            nuo_protocol::Role::User,
            "Active dialogue",
        ));
    assert!(!app.is_current_thread_empty());

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/session new".to_string())
        .await;

    assert!(
        app.switch_to_new,
        "/session new must arm switch_to_new on non-empty thread"
    );
    assert!(
        app.should_quit.load(std::sync::atomic::Ordering::SeqCst),
        "/session new must request runner quit to mount new tab"
    );
    assert!(
        rx.try_recv().is_err(),
        "/session new must not send destructive in-place reset"
    );
}

#[tokio::test]
async fn slash_new_with_round_count_greater_than_zero_opens_new_tab() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-rounds-test";
    app.current_session_id = sid.to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        sid,
        None::<String>,
    ));
    let session = crate::shell::SessionSource::Remote {
        session_id: sid.to_string(),
    };

    app.messages.clear();
    app.round_count = 1;
    assert!(!app.is_current_thread_empty());

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/new".to_string()).await;

    assert!(app.switch_to_new);
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
}

/// `[INV-TAB-13]` — a session rebase must be addressed by the session being
/// **left**, never by "the active tab".
///
/// This is the root-cause lock for the ghost-tab class. The reproduction is the
/// real race: an empty thread is discarded (fire-and-forget `DeleteSession`)
/// while the tab has *already* been replaced by the session the user picked. The
/// server's reply names the discarded session; the replacement tab must be left
/// alone.
///
/// Under the previous positional design this test fails: the replacement tab's
/// id is overwritten with the reaper-bound replacement id, which leaves a tab
/// whose session does not exist — the click that produced a fatal attach.
#[test]
fn conversation_cleared_does_not_rebase_a_replacement_tab() {
    let mut app = crate::shell::tests::new_app_for_relay_tests();
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    let discarded = "discarded-empty-session";
    let picked = "picked-session";
    let minted = "minted-replacement";

    // Step 1: the empty thread the user was on.
    app.surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            discarded,
            None::<String>,
        ));
    // Step 2: the picker swapped it synchronously (before the delete reply).
    app.surfaces.replace_tab(
        0,
        crate::shell::surfaces::ClientTab::thread(picked, Some("Picked")),
    );
    app.surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "other",
            Some("Other"),
        ));
    app.surfaces.select_tab(1); // "other" is active: positional rebase would hit this

    // Step 3: the delete reply lands, naming the session being left.
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::LiveSession {
            previous_id: discarded.to_string(),
            next_id: minted.to_string(),
        },
    );

    // No tab represented `discarded` any more, so nothing may be rebased.
    assert_eq!(
        app.surfaces.tabs()[0].thread_id(),
        Some(picked),
        "the replacement tab must keep its identity"
    );
    assert_eq!(
        app.surfaces.tabs()[1].thread_id(),
        Some("other"),
        "an unrelated active tab must not be rebased by position"
    );
    assert!(
        !app.surfaces
            .tabs()
            .iter()
            .any(|t| t.thread_id() == Some(minted)),
        "no tab may be manufactured for a session that was never attached"
    );
    // The lookup fact still follows the server: the live session *did* move.
    assert_eq!(app.live_session_id, minted);
}

/// `[INV-TAB-13]` — when the tab *does* represent the session being left, the
/// rebase lands on exactly that tab and clears only its title.
#[test]
fn conversation_cleared_rebases_the_named_tab_only() {
    let mut app = crate::shell::tests::new_app_for_relay_tests();
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    app.surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "left-behind",
            Some("Old Title"),
        ));
    app.surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "bystander",
            Some("Keep Me"),
        ));

    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::LiveSession {
            previous_id: "left-behind".to_string(),
            next_id: "fresh".to_string(),
        },
    );

    assert_eq!(app.surfaces.tabs()[0].thread_id(), Some("fresh"));
    assert_eq!(
        app.surfaces.tabs()[0].title,
        None,
        "the rebased tab has no authentic title yet"
    );
    assert_eq!(app.surfaces.tabs()[1].thread_id(), Some("bystander"));
    assert_eq!(
        app.surfaces.tabs()[1].title.as_deref(),
        Some("Keep Me"),
        "an untouched tab's title must never be collateral damage"
    );
}

/// `[INV-TAB-14]` — a tab is retired only on server-attested `session_gone`.
///
/// Suspension and idle teardown publish `SessionRemoved` while leaving the
/// transcript durable, so liveness must never be inferred from a broadcast.
/// This test pins the *absence* of that inference at the only place that may
/// retire: the attach loop's retirement helper, which is keyed on the refusal
/// code.
#[test]
fn stale_tab_retirement_is_keyed_on_the_gone_id_only() {
    let mut tabs = Some(vec![
        crate::shell::surfaces::ClientTab::thread("keep-a", Some("A")),
        crate::shell::surfaces::ClientTab::thread("gone", Some("Gone")),
        crate::shell::surfaces::ClientTab::thread("keep-b", Some("B")),
    ]);

    let retired = super::runner::retire_preserved_tab(&mut tabs, "gone");
    assert!(retired);
    let remaining: Vec<_> = tabs
        .unwrap()
        .iter()
        .filter_map(|t| t.thread_id().map(str::to_string))
        .collect();
    assert_eq!(remaining, vec!["keep-a".to_string(), "keep-b".to_string()]);

    // Idempotent: a second refusal for the same id is not an error.
    let mut again = Some(vec![crate::shell::surfaces::ClientTab::thread(
        "keep-a",
        None::<String>,
    )]);
    assert!(!super::runner::retire_preserved_tab(&mut again, "gone"));
    assert_eq!(again.unwrap().len(), 1);
}
