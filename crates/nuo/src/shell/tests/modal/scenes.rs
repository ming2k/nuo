use super::*;

#[test]
fn switching_picker_view_preserves_query_and_chat_draft_separately() {
    // `[INV-SURFACE-01]`: the picker's query is its own entity state, so it
    // never clobbers (or is clobbered by) the chat composer draft.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "unsent chat".to_string();
    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
        .search = true;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
        .query
        .text = "claude".to_string();

    assert_eq!(app.input, "unsent chat", "composer draft untouched");

    app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
    assert_eq!(
        app.input, "unsent chat",
        "switch never touches the chat composer"
    );

    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
            .query
            .text,
        "claude",
        "picker query is retained independently"
    );
    assert!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
            .search,
        "the search sub-layer is retained too"
    );
    assert!(app.dismiss_surface());
    assert_eq!(app.input, "unsent chat");
}

#[test]
fn sheet_mounting_leaves_the_panel_stack_untouched() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::Tools);
    // A sheet is slot state, not a router layer: mounting it must leave the
    // panel stack untouched, and dismissing it hands the slot straight back.
    app.push_sheet_surface(crate::shell::sheet::SheetKind::Question);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Tools)
    );
    app.dismiss_sheet();
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Tools)
    );
}

#[test]
fn backend_navigation_waits_for_transient_and_drill_in_surfaces() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(app.can_accept_navigation_signal(), "chat is safe");

    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    app.surfaces
        .present_sheet(crate::shell::surfaces::SheetKind::ModelEditor);
    assert!(
        !app.can_accept_navigation_signal(),
        "an editor must not be preempted"
    );
    app.pop_transient_surface();
    assert!(app.can_accept_navigation_signal());

    app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
    app.config_focus = crate::shell::overlays::ConfigFocus::Detail;
    assert!(
        !app.can_accept_navigation_signal(),
        "a parent-owned drill-in must finish or pop first"
    );
}

#[test]
fn explicit_view_close_discards_retained_state_and_payload() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
        .scroll = 17;
    app.close_dialog(crate::shell::surfaces::DialogKind::UsageStats);

    assert!(
        !app.surface_store
            .is_open(crate::shell::surfaces::DialogKind::UsageStats)
    );
    assert!(app.surfaces.active_overlay().is_none());
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
            .scroll,
        0
    );
    assert!(app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats));
}

#[test]
fn switching_away_from_queue_runs_exit_hook() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let sid = "queue-session";
    app.open_dialog(crate::shell::surfaces::DialogKind::Queue);
    app.block_queue(sid);
    app.queue_exit_session = Some((
        sid.to_string(),
        crate::shell::app::QueuePauseOrigin::ModalAutoPause,
    ));

    app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);

    assert!(!app.is_queue_blocked(sid));
    assert!(app.queue_exit_session.is_none());
}

#[test]
fn queue_view_hide_releases_the_auto_block() {
    // Phase 4: the open-time auto-block is released by EVERY hide path
    // (the exit hook in dismiss_active_dialog), not just the Esc arm.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::Queue);
    app.block_queue("sess");
    app.queue_exit_session = Some((
        "sess".to_string(),
        crate::shell::app::QueuePauseOrigin::ModalAutoPause,
    ));
    assert!(app.is_queue_blocked("sess"));

    assert!(app.dismiss_surface());
    assert!(
        !app.is_queue_blocked("sess"),
        "exit hook resumed the outbox"
    );
}

#[test]
fn pop_sublayer_steps_back_one_level_at_a_time() {
    // The shared one-step-back (phase 4): Esc's deepest-first chain and the
    // outside-click mirror both route through here.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::SessionTrace);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
        .detail = true;
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
            .detail,
        "drill-in closed"
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::SessionTrace),
        "view stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");

    // Host: preview is the deepest layer (painted over the prompting
    // state), so it pops first; prompting next; then the view itself.
    app.switch_scene(crate::shell::surfaces::SceneKind::Dashboard);
    app.host_preview = Some("transcript".to_string());
    app.host_prompting = true;
    assert!(app.pop_sublayer());
    assert!(app.host_preview.is_none(), "deepest layer (preview) closed");
    assert!(app.host_prompting, "prompting still open beneath");
    assert!(app.pop_sublayer());
    assert!(!app.host_prompting);
    assert!(!app.pop_sublayer());

    // Connections: detail view pops back to connections list
    app.open_dialog(crate::shell::surfaces::DialogKind::Connections);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_detail = true;
    app.connection_detail = Some(Default::default());
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
            .info_detail,
        "connection detail closed"
    );
    assert!(app.connection_detail.is_none());
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Connections),
        "connections modal stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");
}

#[test]
fn pop_sublayer_pops_trace_turn_page_before_round_detail() {
    // Session Trace has three levels (round list -> round detail -> attempt inspector):
    // Esc walks back one level at a time, attempt inspector first.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::SessionTrace);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
        .detail = true;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
        .turn = Some((2, 1));
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
        .turn_cursor = 1;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
        .scroll = 4;
    assert!(app.pop_sublayer());
    assert!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
            .turn
            .is_none(),
        "attempt inspector closed first"
    );
    assert!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
            .detail,
        "round detail stays open"
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
            .scroll,
        0
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
            .turn_cursor,
        1,
        "cursor retained"
    );
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
            .detail,
        "round detail closed next"
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
            .turn_cursor,
        0,
        "cursor reset"
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::SessionTrace),
        "view stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");
}

#[test]
fn dashboard_reopen_keeps_selection_and_log() {
    // The dashboard is a full-screen scene whose dock selection
    // and cockpit log persist natively on `App` across hide. Leaving is the
    // scene-exit verb (`close_scene`) — a dismiss never navigates

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::shell::surfaces::SceneKind::Dashboard);
    app.host_console_log
        .push(crate::shell::overlays::ConsoleLine::Receipt {
            ok: true,
            target: None,
            text: "ok".to_string(),
        });
    app.modal_index = 3;
    app.close_scene();
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );

    app.switch_scene(crate::shell::surfaces::SceneKind::Dashboard);
    assert_eq!(app.modal_index, 3, "dock selection retained");
    assert_eq!(app.host_console_log.len(), 1, "cockpit log retained");
}

#[tokio::test]
async fn closing_dashboard_scene_closes_singleton_tab() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    // 1. Mount initial thread tab and dashboard tab
    app.surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "s1",
            Some("Thread 1"),
        ));
    app.open_tab(crate::shell::surfaces::ClientTab::dashboard());

    assert_eq!(app.surfaces.tabs().len(), 2);
    assert_eq!(app.surfaces.active_tab_index(), 1);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Dashboard
    );

    // 2. Dispatch CloseTab (C-x w)
    let flow = crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseTab,
        "s1",
    )
    .await;

    assert_eq!(flow, crate::shell::event_loop::actions::ActionFlow::Handled);
    assert_eq!(
        app.surfaces.tabs().len(),
        1,
        "dashboard tab must be closed and removed"
    );
    assert_eq!(
        app.surfaces.active_tab_index(),
        0,
        "active tab should switch to remaining thread tab"
    );
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
    assert!(app.surfaces.tabs()[0].is_thread());
}

#[tokio::test]
async fn closing_sole_dashboard_tab_detaches_client() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    // Only dashboard tab mounted
    app.surfaces
        .open_tab(crate::shell::surfaces::ClientTab::dashboard());
    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Dashboard
    );

    let flow = crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseTab,
        "s1",
    )
    .await;

    assert_eq!(
        flow,
        crate::shell::event_loop::actions::ActionFlow::Exit,
        "closing sole tab exits client"
    );
    assert!(app.surfaces.tabs().is_empty());
}
