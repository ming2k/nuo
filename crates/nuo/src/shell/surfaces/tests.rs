use super::*;

#[test]
fn scope_matrix_assigns_exactly_one_domain_per_dialog() {
    // `[INV-SURFACE-02]`: the matrix is total and each dialog has exactly
    // one scope.
    assert_eq!(
        DialogKind::HistorySearch.scope(),
        DialogScope::Scene(SceneKind::Thread)
    );
    for id in [
        DialogKind::Switcher,
        DialogKind::Threads,
        DialogKind::Models,
        DialogKind::Connections,
        DialogKind::UsageStats,
        DialogKind::Quotas,
    ] {
        assert_eq!(id.scope(), DialogScope::Global, "{id:?}");
    }
    for id in [
        DialogKind::ThreadStats,
        DialogKind::SessionTrace,
        DialogKind::SessionTree,
        DialogKind::Tools,
        DialogKind::Mcp,
        DialogKind::Skills,
        DialogKind::Permissions,
        DialogKind::Queue,
    ] {
        assert_eq!(id.scope(), DialogScope::Session, "{id:?}");
    }
}

#[test]
fn availability_gates_by_scope() {
    // `[INV-SURFACE-03]`.
    assert!(DialogKind::Models.is_available(SceneKind::Settings, false));
    assert!(!DialogKind::Tools.is_available(SceneKind::Thread, false));
    assert!(DialogKind::Tools.is_available(SceneKind::Settings, true));
    assert!(DialogKind::HistorySearch.is_available(SceneKind::Thread, true));
    assert!(!DialogKind::HistorySearch.is_available(SceneKind::Settings, true));
    assert!(!DialogKind::HistorySearch.is_available(SceneKind::Thread, false));
}

#[test]
fn retention_policies_follow_scope() {
    assert_eq!(
        DialogKind::Switcher.retention_policy(),
        RetentionPolicy::Ephemeral
    );
    assert_eq!(
        DialogKind::Models.retention_policy(),
        RetentionPolicy::Retained
    );
    assert_eq!(
        DialogKind::ThreadStats.retention_policy(),
        RetentionPolicy::SessionScoped
    );
    assert_eq!(
        DialogKind::SessionTrace.retention_policy(),
        RetentionPolicy::SessionScoped
    );
}

#[test]
fn switch_scene_unwinds_scene_bound_dialogs_and_preserves_global() {
    // `[INV-SURFACE-04]` + `[INV-SURFACE-05]`: no blunt `clear()` — a
    // scene switch unwinds only the leaving scene's bound dialogs.
    let mut router = SurfaceRouter::new();
    router.present_dialog(DialogKind::Models);
    router.present_dialog(DialogKind::HistorySearch);
    assert!(router.contains_dialog(DialogKind::HistorySearch));

    router.switch_scene(SceneKind::Settings);
    assert!(
        !router.contains_dialog(DialogKind::HistorySearch),
        "scene-bound dialog unwound on scene exit"
    );
    assert!(
        router.contains_dialog(DialogKind::Models),
        "global dialog survives a scene switch"
    );
}

#[test]
fn unwind_session_removes_session_dialogs_and_preserves_global() {
    let mut router = SurfaceRouter::new();
    router.present_dialog(DialogKind::Models);
    router.present_dialog(DialogKind::ThreadStats);
    router.unwind_session();
    assert!(!router.contains_dialog(DialogKind::ThreadStats));
    assert!(router.contains_dialog(DialogKind::Models));
}

#[test]
fn ephemeral_switcher_resets_on_pop() {
    let mut router = SurfaceRouter::new();
    router.present_dialog(DialogKind::Switcher);
    router.dlg_mut::<SwitcherDialog>().selected = 5;
    router.dlg_mut::<SwitcherDialog>().query.text = "abc".to_string();
    router.pop_overlay();
    // Ephemeral: the entity is dropped on pop; a fresh one is default.
    assert_eq!(router.dlg::<SwitcherDialog>().selected, 0);
    assert!(router.dlg::<SwitcherDialog>().query.is_empty());
}

#[test]
fn pop_runs_dismissal_hook_clearing_embedded_search() {
    let mut router = SurfaceRouter::new();
    router.present_dialog(DialogKind::Models);
    router.dialogs.models.search = true;
    router.dialogs.models.query.text = "gpt".to_string();
    router.pop_overlay();
    assert!(!router.dialogs.models.search);
    assert!(router.dialogs.models.query.is_empty());
}

#[test]
fn stack_entry_owns_the_open_dialog_entity() {
    // `[INV-SURFACE-01]` / ADR-0035 §4: the overlay stack owns the live
    // dialog entity while it is open; the retained registry holds only a
    // placeholder until the dialog is retired.
    let mut router = SurfaceRouter::new();
    router.present_dialog(DialogKind::Models);
    router.dlg_mut::<ModelsDialog>().scroll = 7;
    assert_eq!(router.dlg::<ModelsDialog>().scroll, 7);
    assert_eq!(
        router.dialogs.models.scroll, 0,
        "the registry slot is a placeholder while the stack owns the entity"
    );
    router.pop_overlay();
    assert_eq!(
        router.dialogs.models.scroll, 7,
        "popping returns the retained entity to the registry"
    );
}

#[test]
fn take_sheets_drains_sheet_overlays_preserving_dialogs() {
    let mut router = SurfaceRouter::new();
    router.present_dialog(DialogKind::Models);
    router.present_sheet(SheetKind::CustomProvider);
    let sheets = router.take_sheets();
    assert_eq!(sheets, vec![SheetKind::CustomProvider]);
    assert!(!router.contains_sheet(SheetKind::CustomProvider));
    assert!(router.contains_dialog(DialogKind::Models));
}

#[test]
fn dialog_view_contract_reports_identity_scope_and_geometry() {
    // The `DialogView` contract (ADR-0035 §1) exposes identity, domain,
    // and the modal geometry.
    let mut d = Dialogs::default();
    let tools = d.view_mut(DialogKind::Tools);
    assert_eq!(tools.kind(), DialogKind::Tools);
    assert_eq!(tools.scope(), DialogScope::Session);
    assert!(tools.layout_spec().width_percent > 0);
    assert_eq!(
        d.view_mut(DialogKind::HistorySearch).scope(),
        DialogScope::Scene(SceneKind::Thread)
    );
}

#[test]
fn session_switch_archives_and_reinstates_session_scoped_state() {
    // `[INV-SURFACE-05]` §5: session-scoped state is archived per session
    // and reinstated on return; global state is never touched.
    let mut d = Dialogs::default();
    d.switch_session("a");
    d.thread_stats.scroll = 11;
    d.models.scroll = 3;
    d.switch_session("b");
    assert_eq!(d.thread_stats.scroll, 0, "fresh session starts clean");
    assert_eq!(d.models.scroll, 3, "global state untouched");
    d.thread_stats.scroll = 22;
    d.switch_session("a");
    assert_eq!(d.thread_stats.scroll, 11, "session A reinstated");
    d.switch_session("b");
    assert_eq!(d.thread_stats.scroll, 22, "session B reinstated");
}

#[test]
fn dialog_entities_are_independent() {
    let mut d = Dialogs::default();
    d.tools.scroll = 7;
    d.mcp.scroll = 9;
    assert_eq!(d.tools.scroll, 7);
    assert_eq!(d.mcp.scroll, 9);
    d.reset(DialogKind::Tools);
    assert_eq!(d.tools.scroll, 0);
    assert_eq!(d.mcp.scroll, 9, "no aliasing between entities");
}

#[test]
fn client_tab_mounting_switching_and_non_destructive_closing() {
    let mut router = SurfaceRouter::new();
    assert!(router.tabs().is_empty());

    // 1. Mount thread tab (ADR-0039 [INV-TAB-02])
    let idx0 = router.open_tab(ClientTab::thread("conv-1", Some("Refactor DB")));
    assert_eq!(idx0, 0);
    assert_eq!(router.active_tab_index(), 0);
    assert_eq!(router.active_scene(), SceneKind::Thread);
    assert_eq!(router.tabs().len(), 1);

    // 2. Mount second thread tab
    let idx1 = router.open_tab(ClientTab::thread("conv-2", Some("Quick Query")));
    assert_eq!(idx1, 1);
    assert_eq!(router.active_tab_index(), 1);
    assert_eq!(router.active_scene(), SceneKind::Thread);

    // 3. Mount dashboard tab
    let idx2 = router.open_tab(ClientTab::dashboard());
    assert_eq!(idx2, 2);
    assert_eq!(router.active_tab_index(), 2);
    assert_eq!(router.active_scene(), SceneKind::Dashboard);

    // 4. Re-opening existing dashboard focuses it rather than duplicating
    let idx2_reopen = router.open_tab(ClientTab::dashboard());
    assert_eq!(idx2_reopen, 2);
    assert_eq!(router.tabs().len(), 3);

    // 5. Tab navigation
    router.select_tab(0);
    assert_eq!(router.active_tab_index(), 0);
    assert_eq!(router.active_scene(), SceneKind::Thread);

    router.next_tab();
    assert_eq!(router.active_tab_index(), 1);

    router.prev_tab();
    assert_eq!(router.active_tab_index(), 0);

    // 6. Non-destructive close / Detach (ADR-0039 [INV-TAB-03])
    let closed = router.close_tab(0);
    assert_eq!(
        closed,
        Some(ClientTab::thread("conv-1", Some("Refactor DB")))
    );
    assert_eq!(router.tabs().len(), 2);
    assert_eq!(router.active_tab_index(), 0);
    assert_eq!(
        router.tabs()[0],
        ClientTab::thread("conv-2", Some("Quick Query"))
    );

    // 7. Autonomous Tab History Stack (ADR-0040 [INV-HEAD-02], ADR-0042)
    // Push Subagent on Tab 0 (conv-2)
    router.tabs[0].push_scene(SceneKind::Subagent);
    assert_eq!(router.tabs[0].current_scene(), SceneKind::Subagent);
    assert!(router.tabs[0].can_back());
    assert_eq!(
        router.tabs[0].breadcrumbs(),
        &[SceneKind::Thread, SceneKind::Subagent]
    );

    // Under [INV-SCENE-02], cross-container scenes (Settings, Dashboard) are strictly
    // rejected when pushed onto a Thread tab's intra-tab scene stack.
    router.tabs[0].push_scene(SceneKind::Settings);
    assert_eq!(
        router.tabs[0].current_scene(),
        SceneKind::Subagent,
        "cross-container Settings push onto Thread tab must be rejected"
    );
    assert_eq!(
        router.tabs[0].breadcrumbs(),
        &[SceneKind::Thread, SceneKind::Subagent]
    );

    router.tabs[0].push_scene(SceneKind::Dashboard);
    assert_eq!(
        router.tabs[0].current_scene(),
        SceneKind::Subagent,
        "cross-container Dashboard push onto Thread tab must be rejected"
    );

    // History back on Tab 0
    assert_eq!(router.tabs[0].back(), Some(SceneKind::Thread));
    assert_eq!(router.tabs[0].current_scene(), SceneKind::Thread);

    // History forward on Tab 0
    assert_eq!(router.tabs[0].forward(), Some(SceneKind::Subagent));
    assert_eq!(router.tabs[0].current_scene(), SceneKind::Subagent);

    // Verify Tab 1 (Dashboard) is completely isolated from Tab 0's history
    assert_eq!(router.tabs[1].current_scene(), SceneKind::Dashboard);
    assert!(!router.tabs[1].can_back());

    // 8. switch_scene and back_scene sync with active tab (ADR-0042, ADR-0075)
    // Tab 0 is active and currently at Subagent
    router.select_tab(0);
    assert_eq!(router.active_scene(), SceneKind::Subagent);
    assert_eq!(router.back_scene(), SceneKind::Thread);
    assert_eq!(router.active_scene(), SceneKind::Thread);

    // switch_scene to Dashboard activates Tab 1 under [INV-TAB-10]
    router.switch_scene(SceneKind::Dashboard);
    assert_eq!(router.active_tab_index(), 1);
    assert_eq!(router.active_scene(), SceneKind::Dashboard);
    assert_eq!(
        router.tabs()[0].current_scene(),
        SceneKind::Thread,
        "Tab 0 history must not be polluted by Dashboard switch"
    );
}

#[test]
fn test_tab_title_lifecycle_and_placeholder_update() {
    let mut router = SurfaceRouter::new();
    let idx = router.open_tab(ClientTab::thread("conv-42", None::<String>));
    assert_eq!(idx, 0);

    let tab = router.tabs().first().unwrap();
    assert_eq!(tab.label(), "thread");
    assert_eq!(tab.display_title(), None);

    // Update with authentic title (with surrounding spaces to verify trimming)
    assert!(router.update_tab_title("conv-42", "  Refactor Auth Engine  "));
    let tab = router.tabs().first().unwrap();
    assert_eq!(tab.label(), "Refactor Auth Engine");
    assert_eq!(tab.display_title(), Some("Refactor Auth Engine"));

    // Update with whitespace clears to None
    assert!(router.update_tab_title("conv-42", "   "));
    let tab = router.tabs().first().unwrap();
    assert_eq!(tab.label(), "thread");
    assert_eq!(tab.display_title(), None);
}

#[test]
fn test_inv_tab_10_and_inv_scene_02_discipline() {
    let mut router = SurfaceRouter::new();

    // Start with a thread tab
    let idx0 = router.open_tab(ClientTab::thread("conv-main", Some("Main Thread")));
    assert_eq!(idx0, 0);
    assert_eq!(router.active_scene(), SceneKind::Thread);
    assert_eq!(router.tabs()[0].breadcrumbs(), &[SceneKind::Thread]);

    // [INV-SCENE-02]: Thread tab rejects cross-container scenes
    let thread_tab = &mut router.tabs[0];
    thread_tab.push_scene(SceneKind::Dashboard);
    assert_eq!(thread_tab.current_scene(), SceneKind::Thread);
    assert_eq!(thread_tab.breadcrumbs(), &[SceneKind::Thread]);

    thread_tab.push_scene(SceneKind::Settings);
    assert_eq!(thread_tab.current_scene(), SceneKind::Thread);
    assert_eq!(thread_tab.breadcrumbs(), &[SceneKind::Thread]);

    // Thread tab accepts intra-tab drill-in (Subagent)
    thread_tab.push_scene(SceneKind::Subagent);
    assert_eq!(thread_tab.current_scene(), SceneKind::Subagent);
    assert_eq!(
        thread_tab.breadcrumbs(),
        &[SceneKind::Thread, SceneKind::Subagent]
    );

    // [INV-TAB-10]: switch_scene to Dashboard allocates/focuses Dashboard tab container
    router.switch_scene(SceneKind::Dashboard);
    assert_eq!(router.tabs().len(), 2);
    assert_eq!(router.active_tab_index(), 1);
    assert_eq!(router.active_scene(), SceneKind::Dashboard);

    // Tab 0 history remains strictly intact and unpolluted
    assert_eq!(
        router.tabs()[0].breadcrumbs(),
        &[SceneKind::Thread, SceneKind::Subagent],
        "Tab 0 scene stack must be completely unaffected by opening Dashboard tab"
    );

    // Tab 1 (Dashboard) owns its own isolated history
    assert_eq!(router.tabs()[1].kind, TabKind::Dashboard);
    assert_eq!(router.tabs()[1].breadcrumbs(), &[SceneKind::Dashboard]);
    assert!(!router.tabs()[1].can_back());

    // [INV-SCENE-02]: Dashboard tab rejects thread scenes
    let dash_tab = &mut router.tabs[1];
    dash_tab.push_scene(SceneKind::Thread);
    assert_eq!(dash_tab.current_scene(), SceneKind::Dashboard);
    dash_tab.push_scene(SceneKind::Subagent);
    assert_eq!(dash_tab.current_scene(), SceneKind::Dashboard);

    // Back navigation on Dashboard tab stays at root Dashboard scene
    assert_eq!(router.back_scene(), SceneKind::Dashboard);
    assert_eq!(router.active_scene(), SceneKind::Dashboard);

    // Switch back to Tab 0: restores Subagent scene depth
    router.select_tab(0);
    assert_eq!(router.active_scene(), SceneKind::Subagent);
    assert_eq!(router.back_scene(), SceneKind::Thread);
    assert_eq!(router.active_scene(), SceneKind::Thread);
}

#[test]
fn test_tab_mru_history_lifecycle_and_fallback() {
    let mut router = SurfaceRouter::new();
    // 1. Open 3 tabs: Tab 0 (Thread A), Tab 1 (Thread B), Tab 2 (Dashboard)
    router.open_tab(ClientTab::thread("conv-a", Some("Thread A")));
    router.open_tab(ClientTab::thread("conv-b", Some("Thread B")));
    router.open_tab(ClientTab::dashboard());
    assert_eq!(router.tabs().len(), 3);
    assert_eq!(router.active_tab_index(), 2);

    // 2. Visit Tab 0 (Thread A), then Tab 1 (Thread B)
    router.select_tab(0);
    assert_eq!(router.active_tab_index(), 0);
    router.select_tab(1);
    assert_eq!(router.active_tab_index(), 1);

    // History order: Dashboard -> Thread A -> Thread B
    // 3. Close active Tab 1 (Thread B) -> should jump back to Thread A (Tab 0)
    let closed = router.close_tab(1);
    assert_eq!(closed.unwrap().kind, TabKind::Thread("conv-b".to_string()));
    assert_eq!(router.tabs().len(), 2);
    assert_eq!(router.active_tab_index(), 0);
    assert_eq!(
        router.active_tab().unwrap().kind,
        TabKind::Thread("conv-a".to_string())
    );

    // 4. Now active is Thread A. Close active Tab 0 (Thread A) -> should jump to Dashboard
    let closed = router.close_tab(0);
    assert_eq!(closed.unwrap().kind, TabKind::Thread("conv-a".to_string()));
    assert_eq!(router.tabs().len(), 1);
    assert_eq!(router.active_tab_index(), 0);
    assert_eq!(router.active_tab().unwrap().kind, TabKind::Dashboard);

    // 5. Close remaining Dashboard tab -> empty
    let closed = router.close_tab(0);
    assert_eq!(closed.unwrap().kind, TabKind::Dashboard);
    assert!(router.tabs().is_empty());
    assert_eq!(router.active_tab_index(), 0);
}
