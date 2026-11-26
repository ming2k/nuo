use super::{project_tab_status, project_tab_statuses};
use crate::shell::render::TabStatus;
use crate::shell::surfaces::ClientTab;
use crate::shell::tests::new_app_for_relay_tests;

fn monitored_row(id: &str, status: protocol::ThreadActivity) -> protocol::MonitoredThread {
    protocol::MonitoredThread {
        id: id.to_string(),
        title: None,
        overview: String::new(),
        created_at: 100,
        updated_at: 100,
        message_count: 1,
        hosting: protocol::ThreadHosting::Hosted,
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
        origin: protocol::ThreadOrigin::Trunk,
        digest: None,
    }
}

#[test]
fn non_thread_tabs_always_project_idle() {
    let mut app = new_app_for_relay_tests();
    app.session.running_sessions.insert("t-1".to_string());
    app.human.pending_permission = Some(tool::PermissionRequest::default());

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
fn thread_status_projection_prioritizes_attention_over_running_and_avoids_stale_host_running() {
    let mut app = new_app_for_relay_tests();
    app.open_tab(ClientTab::thread("t-fg", Some("FG")));
    app.open_tab(ClientTab::thread("t-bg-run", Some("BG Run")));
    app.open_tab(ClientTab::thread("t-bg-attn", Some("BG Attn")));
    app.open_tab(ClientTab::thread("t-bg-done", Some("BG Done")));
    app.open_tab(ClientTab::thread("t-cold-run", Some("Cold Run")));
    app.open_tab(ClientTab::settings());

    // Foreground thread is running AND awaiting permission → Attention wins.
    app.session.running_sessions.insert("t-fg".to_string());
    app.human.pending_permission = Some(tool::PermissionRequest::default());

    // Background thread actively running via session_chrome.
    app.session.running_sessions.insert("t-bg-run".to_string());

    // Background thread awaiting user via session_chrome phase.
    app.session.session_chrome.insert(
        "t-bg-attn".to_string(),
        crate::shell::app::SessionChrome {
            phase: Some(view::phase::Phase::AwaitingUser),
            responding: true,
            ..Default::default()
        },
    );

    // Subscribed thread that just finished locally (`responding: false`),
    // while `host_sessions` still lags one tick behind with `Running`.
    app.session.session_chrome.insert(
        "t-bg-done".to_string(),
        crate::shell::app::SessionChrome {
            responding: false,
            ..Default::default()
        },
    );
    app.session.host_sessions = vec![
        monitored_row("t-bg-done", protocol::ThreadActivity::Running),
        monitored_row("t-cold-run", protocol::ThreadActivity::Running),
    ];

    let statuses = project_tab_statuses(&app, "t-fg");
    assert_eq!(
        statuses,
        vec![
            TabStatus::Attention, // t-fg
            TabStatus::Running,   // t-bg-run
            TabStatus::Attention, // t-bg-attn
            TabStatus::Idle,      // t-bg-done (local chrome beats lagging host row)
            TabStatus::Running,   // t-cold-run (cold tab falls back to host row)
            TabStatus::Idle,      // settings
        ]
    );
}
