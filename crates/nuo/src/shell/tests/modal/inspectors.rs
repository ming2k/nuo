use super::*;

#[tokio::test]
async fn thread_stats_slash_open_and_unbound_ctrl_o() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();

    let runtime = crate::shell::event_loop::runtime::UiRuntime::minimal_for_test();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenThreadStats,
        "test-session",
    )
    .await;

    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::ThreadStats),
        "OpenThreadStats (/stats) opens ThreadStats dialog"
    );

    // Alt+O opens the notice viewer, which is a global verb.
    assert_eq!(
        crate::shell::keymap::resolve_global_key(crate::shell::keymap::Key::ALT_O),
        Some(crate::shell::keymap::CommandId::OpenNotices)
    );
}

#[tokio::test]
async fn thread_stats_t_affordance_transitions_to_session_trace() {
    // ThreadStats modal binds `t` to transition directly to SessionTrace.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    app.open_dialog(crate::shell::surfaces::DialogKind::ThreadStats);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::ThreadStats)
    );

    let runtime = crate::shell::event_loop::runtime::UiRuntime::minimal_for_test();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ThreadStatsOpenTrace,
        "test-session",
    )
    .await;

    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::SessionTrace),
        "`t` affordance in ThreadStats opens SessionTrace"
    );
}

#[tokio::test]
async fn thread_stats_modal_lifecycle_and_gestalt_layout() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();

    // 1. OpenThreadStats action opens DialogKind::ThreadStats
    let runtime = crate::shell::event_loop::runtime::UiRuntime::minimal_for_test();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenThreadStats,
        "test-session",
    )
    .await;

    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::ThreadStats),
        "OpenThreadStats opens ThreadStats dialog"
    );
    assert_eq!(
        crate::shell::surfaces::DialogKind::ThreadStats.label(),
        "Thread stats",
        "DialogKind::ThreadStats label must be 'Thread stats'"
    );

    // 2. ThreadStats transitions to SessionTrace via 't' chord (ThreadStatsOpenTrace)
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ThreadStatsOpenTrace,
        "test-session",
    )
    .await;

    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::SessionTrace),
        "ThreadStatsOpenTrace affordance opens SessionTrace"
    );

    // 3. Thread Stats overview body utilizes standardized DetailLayout components
    let theme = app.theme;
    let rows = crate::shell::overlays::telemetry::overview::build_overview_body(
        &nuo_protocol::TokenSourceReport::default(),
        &[],
        crate::shell::render::ContextUsageProps {
            snapshot: None,
            window_tokens: Some(128_000),
            draft_content_tokens: 0,
            draft_tokens: 0,
        },
        80,
        &theme,
    );

    // Section 1: Context Window
    assert_eq!(rows[0].text(), "Context Window");
    assert_eq!(
        rows[1].prefix.as_ref().map(|p| p.text.as_str()),
        Some("Used Tokens       ")
    );
    assert_eq!(
        rows[2].prefix.as_ref().map(|p| p.text.as_str()),
        Some("Capacity          ")
    );
    assert_eq!(
        rows[3].prefix.as_ref().map(|p| p.text.as_str()),
        Some("Utilization       ")
    );
    // Utilization progress bar segment exists
    assert!(
        rows[3]
            .to_line()
            .spans
            .iter()
            .any(|s| s.content.contains('['))
    );
    assert!(
        rows[3]
            .to_line()
            .spans
            .iter()
            .any(|s| s.content.contains(']'))
    );

    // Blank separator row before Section 2
    assert_eq!(rows[4].text(), "");

    // Section 2: Thread Token Totals
    assert_eq!(rows[5].text(), "Thread Token Totals");
    assert_eq!(
        rows[6].prefix.as_ref().map(|p| p.text.as_str()),
        Some("Grand Total       ")
    );

    // Section 3: Streaming Performance
    let perf_section = rows.iter().find(|r| r.text() == "Streaming Performance");
    assert!(
        perf_section.is_some(),
        "Streaming Performance section must exist"
    );
}

#[tokio::test]
async fn session_trace_slash_and_drill_down_lifecycle() {
    // `[INV-TRACE-01]` / ADR-0037:
    // SessionTrace has hierarchical drill-down without tabs or lateral page hops.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();

    let runtime = crate::shell::event_loop::runtime::UiRuntime::minimal_for_test();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenSessionTrace,
        "test-session",
    )
    .await;

    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::SessionTrace),
        "OpenSessionTrace opens SessionTrace dialog"
    );

    // Initial state: L1 rounds list, no detail, no turn
    assert!(
        !app.surfaces
            .dlg::<crate::shell::surfaces::SessionTraceDialog>()
            .detail
    );
    assert!(
        app.surfaces
            .dlg::<crate::shell::surfaces::SessionTraceDialog>()
            .turn
            .is_none()
    );

    // Enter in L1 drills into L2 (turns list) when rounds exist or directly via action
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
        .detail = true;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
        .turn = Some((1, 1));
    assert!(
        app.surfaces
            .dlg::<crate::shell::surfaces::SessionTraceDialog>()
            .turn
            .is_some()
    );

    // Esc from L3 steps back to L2
    assert!(app.pop_sublayer());
    assert!(
        app.surfaces
            .dlg::<crate::shell::surfaces::SessionTraceDialog>()
            .turn
            .is_none()
    );
    assert!(
        app.surfaces
            .dlg::<crate::shell::surfaces::SessionTraceDialog>()
            .detail
    );

    // Esc from L2 steps back to L1
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg::<crate::shell::surfaces::SessionTraceDialog>()
            .detail
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::SessionTrace)
    );

    // In L1, no sub-layer remains
    assert!(!app.pop_sublayer(), "no sub-layer left in L1");
    assert!(app.dismiss_surface());
    assert_eq!(app.active_dialog(), None);
}

#[tokio::test]
async fn tools_dialog_two_tier_drill_in_and_pop_sublayer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    app.session_context = Some(nuo_protocol::ThreadContextSnapshot {
        model: nuo_protocol::ModelInfo::default(),
        tools: vec![
            nuo_protocol::ToolInfo {
                name: "read_text".to_string(),
                description: "Read file".to_string(),
                enabled: true,
                source: "builtin".to_string(),
                parameters: None,
                risk: Some("read_only".to_string()),
                requires_user: false,
                requires_vision: false,
            },
            nuo_protocol::ToolInfo {
                name: "execute_command".to_string(),
                description: "Run command".to_string(),
                enabled: false,
                source: "builtin".to_string(),
                parameters: None,
                risk: Some("arbitrary_execution".to_string()),
                requires_user: true,
                requires_vision: false,
            },
        ],
        permissions: vec![],
        skills: vec![],
        mcp: vec![],
    });

    app.open_dialog(crate::shell::surfaces::DialogKind::Tools);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Tools),
        "Tools dialog opened"
    );

    // Initial state: Level 1 list, info_detail is false
    assert!(
        !app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .info_detail
    );
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .index,
        0
    );

    let runtime = crate::shell::event_loop::runtime::UiRuntime::minimal_for_test();

    // Select second tool (execute_command)
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::SessionSelect { forward: true },
        "test-session",
    )
    .await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .index,
        1
    );

    // Press Enter -> OpenToolDetail
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenToolDetail,
        "test-session",
    )
    .await;
    assert!(
        app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .info_detail,
        "Drilled into Level 2 tool detail"
    );
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .info_scroll,
        0
    );

    // Scrolling down in Level 2 adjusts info_scroll
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::SessionSelect { forward: true },
        "test-session",
    )
    .await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .info_scroll,
        1,
        "Up/Down in Level 2 scrolls info_scroll without changing tool index"
    );
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .index,
        1
    );

    // Space toggles the tool in Level 2
    let req = app.session_activate_request();
    assert!(req.is_some(), "Space toggles tool being viewed");
    if let Some(nuo_protocol::AgentRequest::ToggleTool { name, enabled }) = req {
        assert_eq!(name, "execute_command");
        assert!(enabled, "execute_command was false, toggle enables it");
    }

    // Esc from Level 2 pops back to Level 1
    assert!(app.pop_sublayer(), "pop_sublayer pops Level 2 detail");
    assert!(
        !app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .info_detail,
        "Returned to Level 1 list"
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Tools),
        "Tools modal remains active"
    );
    assert_eq!(
        app.surfaces
            .dlg::<crate::shell::surfaces::ToolsDialog>()
            .index,
        1,
        "Tool selection preserved"
    );

    // Esc from Level 1 closes modal
    assert!(!app.pop_sublayer(), "No sublayer left in Level 1");
    assert!(app.dismiss_surface());
    assert_eq!(app.active_dialog(), None);
}
