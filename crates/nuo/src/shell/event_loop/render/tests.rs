use super::project_tab_items;
use super::projection::project_tab_status;
use crate::shell::app::workspaces::UnseenOutcome;
use crate::shell::event_loop::actions::tabs::activate_tab_at_index;
use crate::shell::event_loop::mutations::{AppMutation, ChromeEdit};
use crate::shell::event_loop::runtime::UiRuntime;
use crate::shell::render::{TabBarItem, TabStatus};
use crate::shell::surfaces::{ClientTab, SceneKind};
use crate::shell::tests::new_app_for_relay_tests;

fn monitored_row(id: &str, status: nuo_protocol::ThreadActivity) -> nuo_protocol::MonitoredThread {
    nuo_protocol::MonitoredThread {
        id: id.to_string(),
        title: None,
        overview: String::new(),
        created_at: 100,
        updated_at: 100,
        message_count: 1,
        hosting: nuo_protocol::ThreadHosting::Hosted,
        status,
        round: 1,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: "/tmp/proj".to_string(),
        role: "developer".to_string(),
        parent_id: None,
        origin: nuo_protocol::ThreadOrigin::Trunk,
        digest: None,
    }
}

#[test]
fn non_thread_tabs_always_project_idle() {
    let mut app = new_app_for_relay_tests();
    app.session.running_sessions.insert("t-1".to_string());
    app.human.pending_permission = Some(nuo_tool::PermissionRequest::default());

    assert_eq!(
        project_tab_status(&app, &ClientTab::dashboard(), "t-1"),
        TabStatus::Idle
    );
    assert_eq!(
        project_tab_status(&app, &ClientTab::settings(), "t-1"),
        TabStatus::Idle
    );
}

#[test]
fn scene_projection_produces_titles_and_prioritizes_status_without_stale_host_running() {
    let mut app = new_app_for_relay_tests();
    app.open_tab(ClientTab::thread("t-fg", Some("FG")));
    app.open_tab(ClientTab::thread("t-bg-run", Some("BG Run")));
    app.open_tab(ClientTab::thread("t-bg-attn", Some("BG Attn")));
    app.open_tab(ClientTab::thread("t-bg-done", Some("BG Done")));
    app.open_tab(ClientTab::thread("t-bg-ok", Some("BG Ok")));
    app.open_tab(ClientTab::thread("t-bg-fail", Some("BG Fail")));
    app.open_tab(ClientTab::thread("sess-01a2b3c4", None::<String>));
    app.open_tab(ClientTab::thread("t-host-titled", None::<String>));
    app.open_tab(ClientTab::thread("t-cold-fail", Some("Cold Fail")));
    app.open_tab(ClientTab::dashboard());
    app.open_tab(ClientTab::settings());

    // Foreground thread is running AND awaiting permission → Attention wins.
    app.session.running_sessions.insert("t-fg".to_string());
    app.human.pending_permission = Some(nuo_tool::PermissionRequest::default());

    // Background thread actively running.
    app.session.running_sessions.insert("t-bg-run".to_string());

    // Background thread awaiting user via its resident ThreadInstance phase.
    app.threads
        .register("t-bg-attn", 1, nuo_protocol::ThreadPhase::Ready)
        .set_phase(Some(nuo_view::phase::Phase::AwaitingUser));
    app.session
        .subscribed_threads
        .insert("t-bg-attn".to_string());

    // Subscribed thread that just finished locally (`is_responding() == false`),
    // while `host_sessions` still lags one tick behind with `Running`.
    app.threads
        .register("t-bg-done", 1, nuo_protocol::ThreadPhase::Ready);
    app.session
        .subscribed_threads
        .insert("t-bg-done".to_string());

    // Subscribed background threads with unseen Succeeded / Failed outcomes.
    app.threads
        .register("t-bg-ok", 1, nuo_protocol::ThreadPhase::Ready);
    app.session
        .unseen_outcomes
        .insert("t-bg-ok".to_string(), UnseenOutcome::Succeeded);
    app.threads
        .register("t-bg-fail", 1, nuo_protocol::ThreadPhase::Ready);
    app.session
        .unseen_outcomes
        .insert("t-bg-fail".to_string(), UnseenOutcome::Failed);

    let mut host_titled = monitored_row("t-host-titled", nuo_protocol::ThreadActivity::Idle);
    host_titled.title = Some("Monitor Derived Title".to_string());

    app.session.host_sessions = vec![
        monitored_row("t-bg-done", nuo_protocol::ThreadActivity::Running),
        monitored_row("sess-01a2b3c4", nuo_protocol::ThreadActivity::Running),
        host_titled,
        monitored_row("t-cold-fail", nuo_protocol::ThreadActivity::Failed),
    ];

    let items = project_tab_items(&app, "t-fg");
    assert_eq!(
        items,
        vec![
            TabBarItem::with_status("FG", TabStatus::Attention),
            TabBarItem::with_status("BG Run", TabStatus::Running),
            TabBarItem::with_status("BG Attn", TabStatus::Attention),
            TabBarItem::with_status("BG Done", TabStatus::Idle),
            TabBarItem::with_status("BG Ok", TabStatus::Succeeded),
            TabBarItem::with_status("BG Fail", TabStatus::Failed),
            TabBarItem::with_status("new thread", TabStatus::Running),
            TabBarItem::with_status("Monitor Derived Title", TabStatus::Idle),
            TabBarItem::with_status("Cold Fail", TabStatus::Failed),
            TabBarItem::with_status("dashboard", TabStatus::Idle),
            TabBarItem::with_status("settings", TabStatus::Idle),
        ]
    );
}

#[test]
fn background_round_outcomes_record_unseen_status_and_clear_when_viewed_or_restarted() {
    let mut app = new_app_for_relay_tests();
    let runtime = UiRuntime::minimal_for_test();

    app.open_tab(ClientTab::thread("t-a", Some("Alpha")));
    app.open_tab(ClientTab::thread("t-b", Some("Beta")));
    app.open_tab(ClientTab::thread("t-c", Some("Gamma")));
    // Make t-a the viewed foreground tab.
    app.surfaces.surfaces.select_tab(0);
    app.activate_thread_workspace("t-a");
    app.threads
        .register("t-b", 1, nuo_protocol::ThreadPhase::Ready);
    app.threads
        .register("t-c", 1, nuo_protocol::ThreadPhase::Ready);

    // 1. Background thread t-b completes cleanly → Succeeded (and requests repaint).
    let dirty_b = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("t-b".to_string()),
            edit: ChromeEdit::RoundCompleted(nuo_protocol::RoundSummary::default()),
        },
    );
    assert!(
        dirty_b,
        "background RoundCompleted must trigger TabBar repaint"
    );
    assert_eq!(
        project_tab_items(&app, "t-a")[1].status,
        TabStatus::Succeeded
    );

    // 2. Background thread t-c fails → Failed (and requests repaint).
    let dirty_c = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("t-c".to_string()),
            edit: ChromeEdit::RoundFailed,
        },
    );
    assert!(
        dirty_c,
        "background RoundFailed must trigger TabBar repaint"
    );
    assert_eq!(project_tab_items(&app, "t-a")[2].status, TabStatus::Failed);

    // 3. Foreground viewed thread t-a completes → stays Idle (user is already viewing it).
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("t-a".to_string()),
            edit: ChromeEdit::RoundCompleted(nuo_protocol::RoundSummary::default()),
        },
    );
    assert_eq!(project_tab_items(&app, "t-a")[0].status, TabStatus::Idle);

    // 4. Switching to t-b acknowledges and clears its Succeeded status immediately.
    app.surfaces.surfaces.select_tab(1);
    app.activate_thread_workspace("t-b");
    assert_eq!(project_tab_items(&app, "t-b")[1].status, TabStatus::Idle);

    // 5. Starting a new round on t-c clears its unseen Failed status and shows Running.
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("t-c".to_string()),
            edit: ChromeEdit::RoundLifecycle {
                round_count: 2,
                running: true,
                can_retry: false,
            },
        },
    );
    assert_eq!(project_tab_items(&app, "t-b")[2].status, TabStatus::Running);
    assert!(!app.session.unseen_outcomes.contains_key("t-c"));
}

#[test]
fn foreground_thread_finishing_or_failing_while_on_dashboard_records_unseen_and_clears_on_return() {
    let mut app = new_app_for_relay_tests();
    let runtime = UiRuntime::minimal_for_test();

    app.open_tab(ClientTab::thread("t-fg", Some("Main")));
    app.activate_thread_workspace("t-fg");
    // User switches to Dashboard; `t-fg` remains the resident foreground thread
    // in `app.threads`, but is NO LONGER the viewed tab.
    app.open_tab(ClientTab::dashboard());
    assert_eq!(app.surfaces.surfaces.active_tab_index(), 1);

    // 1. Round completes while user is on Dashboard → Succeeded.
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::ChromeEdit {
            thread_id: Some("t-fg".to_string()),
            edit: ChromeEdit::RoundCompleted(nuo_protocol::RoundSummary::default()),
        },
    );
    assert_eq!(
        project_tab_items(&app, "t-fg")[0].status,
        TabStatus::Succeeded
    );

    // 2. Clicking back to tab 0 (`id == viewed_session_id`) clears the unseen outcome.
    activate_tab_at_index(&mut app, 0, &runtime, "t-fg");
    assert_eq!(project_tab_items(&app, "t-fg")[0].status, TabStatus::Idle);

    // 3. Switch to Dashboard again and fail via ThreadPhase::Failed → Failed.
    activate_tab_at_index(&mut app, 1, &runtime, "t-fg");
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::SessionPhase {
            thread_id: Some("t-fg".to_string()),
            phase: nuo_protocol::ThreadPhase::Failed {
                code: None,
                message: "boom".to_string(),
            },
        },
    );
    assert_eq!(project_tab_items(&app, "t-fg")[0].status, TabStatus::Failed);

    // 4. Returning to tab 0 clears Failed.
    activate_tab_at_index(&mut app, 0, &runtime, "t-fg");
    assert_eq!(project_tab_items(&app, "t-fg")[0].status, TabStatus::Idle);
}

#[test]
fn cold_unsubscribed_background_tab_records_succeeded_and_failed_from_host_sessions() {
    let mut app = new_app_for_relay_tests();
    let runtime = UiRuntime::minimal_for_test();

    app.open_tab(ClientTab::thread("t-fg", Some("FG")));
    app.open_tab(ClientTab::thread("t-cold-ok", Some("Cold Ok")));
    app.open_tab(ClientTab::thread("t-cold-err", Some("Cold Err")));
    app.surfaces.surfaces.select_tab(0);
    app.activate_thread_workspace("t-fg");

    // Snapshot 1: both cold tabs are Running on the server.
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::HostSessions(vec![
            monitored_row("t-cold-ok", nuo_protocol::ThreadActivity::Running),
            monitored_row("t-cold-err", nuo_protocol::ThreadActivity::Running),
        ]),
    );
    let items1 = project_tab_items(&app, "t-fg");
    assert_eq!(items1[1].status, TabStatus::Running);
    assert_eq!(items1[2].status, TabStatus::Running);

    // Snapshot 2: one completes (`Idle`), one fails (`Failed`).
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::HostSessions(vec![
            monitored_row("t-cold-ok", nuo_protocol::ThreadActivity::Idle),
            monitored_row("t-cold-err", nuo_protocol::ThreadActivity::Failed),
        ]),
    );
    let items2 = project_tab_items(&app, "t-fg");
    assert_eq!(items2[1].status, TabStatus::Succeeded);
    assert_eq!(items2[2].status, TabStatus::Failed);
}

#[test]
fn subagent_scene_projects_focused_subagent_description_as_tab_title() {
    let mut app = new_app_for_relay_tests();
    app.open_tab(ClientTab::thread("t-fg", Some("Parent Thread")));
    app.activate_thread_workspace("t-fg");

    let subagent_msg = nuo_view::model::document::TranscriptMessage::tool_step(
        "call-sub-1",
        "spawn_agent",
        r#"{"description":"Audit SQL indexes","role":"reviewer"}"#,
    );
    app.threads.fg_mut().messages.push(subagent_msg);
    app.threads
        .fg_mut()
        .focus_stack
        .push(crate::shell::app::ZoomFrame {
            call_id: "call-sub-1".to_string(),
            saved_scroll: crate::shell::app::ScrollSnapshot {
                offset: 0,
                follow_bottom: true,
            },
        });
    app.surfaces.surfaces.switch_scene(SceneKind::Subagent);

    let items = project_tab_items(&app, "t-fg");
    assert_eq!(items[0].title, "Audit SQL indexes");
}

#[test]
fn untitled_thread_tabs_use_human_labels_without_uuid_fragments() {
    // ADR-0268 `[INV-HEAD-12]`: untitled thread tabs project `new thread` (and
    // `new thread 2`, `new thread 3`, … only when multiple untitled tabs are
    // open simultaneously), never random UUID hex tails.
    let mut app = new_app_for_relay_tests();
    app.open_tab(ClientTab::thread(
        "0196a1b2-0000-7000-8000-deadbeef7db7",
        None::<String>,
    ));
    app.open_tab(ClientTab::thread(
        "0196a1b2-0000-7000-8000-cafebabe1234",
        Some("Fix auth"),
    ));
    app.open_tab(ClientTab::thread(
        "0196a1b2-0000-7000-8000-feedface5678",
        None::<String>,
    ));
    app.open_tab(ClientTab::dashboard());
    app.open_tab(ClientTab::thread(
        "0196a1b2-0000-7000-8000-1122334499aa",
        None::<String>,
    ));

    let items = project_tab_items(&app, "0196a1b2-0000-7000-8000-deadbeef7db7");
    let titles: Vec<&str> = items.iter().map(|i| i.title.as_ref()).collect();
    assert_eq!(
        titles,
        vec![
            "new thread",
            "Fix auth",
            "new thread 2",
            "dashboard",
            "new thread 3",
        ]
    );
    for t in &titles {
        assert!(
            !t.contains("thread#") && !t.contains("7db7") && !t.contains("5678"),
            "tab titles must not leak UUID hex tails: {titles:?}"
        );
    }

    // Once the first untitled thread receives an authentic title, remaining
    // untitled tabs renumber cleanly so the first untitled one is `new thread`.
    app.update_tab_title(
        "0196a1b2-0000-7000-8000-deadbeef7db7",
        "Investigate startup race",
    );
    let updated: Vec<String> = project_tab_items(&app, "0196a1b2-0000-7000-8000-deadbeef7db7")
        .into_iter()
        .map(|i| i.title.into_owned())
        .collect();
    assert_eq!(
        updated,
        vec![
            "Investigate startup race",
            "Fix auth",
            "new thread",
            "dashboard",
            "new thread 2",
        ]
    );

    // Command Palette / `[▾]` Tab Overview shares the exact same UUID-free projection.
    let palette_ctx = crate::shell::palette::PaletteContext::capture(&app);
    let palette_labels: Vec<&str> = palette_ctx.tabs.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(
        palette_labels,
        vec![
            "Investigate startup race",
            "Fix auth",
            "new thread",
            "Dashboard",
            "new thread 2",
        ]
    );
}

#[test]
fn subagent_scene_renders_role_token_in_brand_bold_end_to_end() {
    let mut app = new_app_for_relay_tests();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;
    app.open_tab(ClientTab::thread("t-fg", Some("Parent Thread")));
    app.activate_thread_workspace("t-fg");

    let mut subagent_msg = nuo_view::model::document::TranscriptMessage::tool_step(
        "call-sub-1",
        "spawn_agent",
        r#"{"description":"Audit SQL indexes","role":"reviewer"}"#,
    );
    subagent_msg.push_subagent_event(
        &nuo_tool::SubagentEvent::Started {
            profile: "reviewer".to_string(),
        },
        &app.session.tui_config,
    );
    app.threads.fg_mut().messages.push(subagent_msg);
    app.threads
        .fg_mut()
        .focus_stack
        .push(crate::shell::app::ZoomFrame {
            call_id: "call-sub-1".to_string(),
            saved_scroll: crate::shell::app::ScrollSnapshot {
                offset: 0,
                follow_bottom: true,
            },
        });
    app.surfaces.surfaces.switch_scene(SceneKind::Subagent);

    let mut terminal = graticule::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "t-fg");
    });
    let row1_cells = &terminal.buffer().cells()[80..160];
    let row1: String = row1_cells.iter().map(|c| c.symbol.as_str()).collect();
    assert!(
        row1.contains("REVIEWER  Audit SQL indexes") && !row1.contains("[REVIEWER]"),
        "chromatic Subagent SceneBar renders bare role token: {row1:?}"
    );
    let role_col = row1.find("REVIEWER").unwrap();
    assert_eq!(
        row1_cells[role_col].style.fg,
        app.surfaces.theme.brand(),
        "Subagent role token must paint in brand tone"
    );
    assert!(
        row1_cells[role_col]
            .style
            .add
            .contains(graticule::Modifier::BOLD),
        "Subagent role token must be bold"
    );
}
