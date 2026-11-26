use super::*;

mod transcript_scroll_tests {
    use super::*;
    use crate::shell::surfaces::DialogView;

    fn scrollable_app() -> App {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.threads.max_scroll = 100;
        app.threads.scroll = 100;
        app.viewport.view_height = 20;
        app.threads.follow_bottom = true;
        app
    }

    #[test]
    fn wheel_ticks_move_the_transcript_and_rearm_bottom_follow() {
        let mut app = scrollable_app();

        scroll_tick(&mut app, false);
        assert_eq!(app.threads.scroll, 96);
        assert!(!app.threads.follow_bottom);

        scroll_tick(&mut app, true);
        assert_eq!(app.threads.scroll, 100);
        assert!(app.threads.follow_bottom);
    }

    #[test]
    fn page_navigation_uses_the_measured_viewport() {
        let mut app = scrollable_app();

        scroll_transcript_page(&mut app, false);
        assert_eq!(app.threads.scroll, 81);
        assert!(!app.threads.follow_bottom);

        scroll_transcript_page(&mut app, true);
        assert_eq!(app.threads.scroll, 100);
        assert!(app.threads.follow_bottom);
    }

    #[test]
    fn edge_navigation_updates_position_and_follow_mode_together() {
        let mut app = scrollable_app();

        scroll_transcript_to_edge(&mut app, false);
        assert_eq!(app.threads.scroll, 0);
        assert!(!app.threads.follow_bottom);

        scroll_transcript_to_edge(&mut app, true);
        assert_eq!(app.threads.scroll, 100);
        assert!(app.threads.follow_bottom);
    }

    #[test]
    fn wheel_spatial_routing_under_permission_modal() {
        let mut app = scrollable_app();
        app.set_active_sheet_for_test(crate::shell::sheet::SheetKind::Permission);
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport
            .ui
            .mount_permission_sheet(graticule::Rect::new(0, 15, 80, 5));
        app.viewport.ui.commit();
        app.human.permission_show_details = true;
        app.human.permission_max_scroll = 10;
        app.human.permission_scroll = 2;

        let runtime = UiRuntime::minimal_for_test();

        // 1. Wheel over permission sheet (y=16) scrolls details down
        handle_wheel(&mut app, false, 10, 16, &runtime, "");
        assert_eq!(app.human.permission_scroll, 3);
        assert_eq!(app.threads.scroll, 100, "transcript scroll untouched");

        // 2. Wheel over permission sheet (y=16) scrolls details up
        handle_wheel(&mut app, true, 10, 16, &runtime, "");
        assert_eq!(app.human.permission_scroll, 2);
        assert_eq!(app.threads.scroll, 100, "transcript scroll untouched");

        // 3. Wheel above permission sheet (y=5) scrolls transcript
        handle_wheel(&mut app, true, 10, 5, &runtime, "");
        assert_eq!(app.threads.scroll, 96, "transcript scrolled up");
        assert_eq!(
            app.human.permission_scroll, 2,
            "permission scroll untouched"
        );
    }

    #[test]
    fn wheel_spatial_routing_under_overlay_modal_isolates_backdrop() {
        let mut app = scrollable_app();
        app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::UsageStats,
            )),
            graticule::Rect::new(10, 5, 60, 10),
        );
        app.viewport.ui.commit();
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
            .scroll = 5;

        let runtime = UiRuntime::minimal_for_test();

        // 1. Wheel on backdrop (x=2, y=2) outside modal_rect: absorbed, neither modal nor transcript scrolls
        handle_wheel(&mut app, false, 2, 2, &runtime, "");
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .scroll,
            5,
            "modal scroll untouched on backdrop"
        );
        assert_eq!(
            app.threads.scroll, 100,
            "transcript scroll untouched on backdrop"
        );

        // 2. Wheel inside modal (x=20, y=8): scrolls modal body
        handle_wheel(&mut app, false, 20, 8, &runtime, "");
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .scroll,
            6,
            "modal scrolled inside modal_rect"
        );
        assert_eq!(app.threads.scroll, 100, "transcript scroll untouched");
    }

    #[test]
    fn wheel_spatial_routing_under_dialog_sub_views_scrolls_info_detail() {
        let mut app = scrollable_app();
        app.open_dialog(crate::shell::surfaces::DialogKind::Threads);
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Threads,
            )),
            graticule::Rect::new(10, 5, 60, 10),
        );
        app.viewport.ui.commit();

        let threads = app
            .surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>();
        threads.scroll = 2;
        threads.info_detail = true;
        threads.info_scroll = 0;

        let runtime = UiRuntime::minimal_for_test();

        // Wheel inside Threads dialog in info_detail mode:
        // Must increment info_scroll and leave main list scroll untouched! [INV-SURFACE-08]
        handle_wheel(&mut app, false, 20, 8, &runtime, "");
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .info_scroll,
            1,
            "info_detail scrolled down"
        );
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .scroll,
            2,
            "main list scroll untouched"
        );

        handle_wheel(&mut app, true, 20, 8, &runtime, "");
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .info_scroll,
            0,
            "info_detail scrolled back up"
        );
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .scroll,
            2,
            "main list scroll untouched"
        );
    }

    #[test]
    fn wheel_spatial_routing_under_dialog_keys_scrolls_keys_sublayer() {
        let mut app = scrollable_app();
        app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::UsageStats,
            )),
            graticule::Rect::new(10, 5, 60, 10),
        );
        app.viewport.ui.commit();

        let stats = app
            .surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>();
        stats.set_keys_open(true);
        *stats.keys_scroll_mut() = 0;

        let runtime = UiRuntime::minimal_for_test();

        // Wheel inside UsageStats while dialog_keys is open:
        // Must increment keys_scroll on the dialog entity, not the body scroll!
        handle_wheel(&mut app, false, 20, 8, &runtime, "");
        assert_eq!(
            *app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .keys_scroll_mut(),
            1,
            "dialog keys sublayer scrolled down"
        );
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .scroll,
            0,
            "main body scroll untouched"
        );
    }

    #[test]
    fn wheel_over_tab_bar_cycles_tabs() {
        let mut app = scrollable_app();
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                None::<String>,
            ));
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s2",
                None::<String>,
            ));
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces.surfaces.select_tab(0);
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 0);

        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            graticule::Rect::new(0, 0, 80, 1),
        );
        app.viewport.ui.commit();

        let runtime = UiRuntime::minimal_for_test();

        // 1. Wheel down over TabBar cycles to next tab (tab 1)
        handle_wheel(&mut app, false, 20, 0, &runtime, "s1");
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 1);

        // 2. Wheel up over TabBar cycles to previous tab (tab 0)
        handle_wheel(&mut app, true, 20, 0, &runtime, "s1");
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 0);

        // 3. Wheel up at tab 0 wraps around to tab 2
        handle_wheel(&mut app, true, 20, 0, &runtime, "s1");
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 2);
    }

    #[tokio::test]
    async fn completion_menu_mouse_wheel_and_click() {
        let mut app = scrollable_app();
        app.composer.input = "/m".to_string();
        app.composer.cursor_position = 2;
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport
            .ui
            .mount_completion(graticule::Rect::new(0, 8, 30, 2));
        app.viewport
            .ui
            .mount_completion_item(0, graticule::Rect::new(0, 8, 30, 1));
        app.viewport
            .ui
            .mount_completion_item(1, graticule::Rect::new(0, 9, 30, 1));
        app.viewport.ui.commit();

        let runtime = UiRuntime::minimal_for_test();

        // Wheel over menu cycles suggestions
        handle_wheel(&mut app, false, 5, 8, &runtime, "");
        assert!(app.composer.suggestion_index.is_some());

        // Click on completion item 0 accepts completion
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 8).await;
        assert!(app.composer.completion_dismissed);
        assert_eq!(app.composer.suggestion_index, None);
    }

    #[tokio::test]
    async fn mouse_click_on_tab_bar_switches_active_tab() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        // Mount tabs
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::settings());
        app.surfaces.surfaces.select_tab(0);
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 0);

        // Mount TabBarItem(1) (Dashboard tab) at (14, 0)
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(1),
            graticule::Rect::new(14, 0, 14, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(2),
            graticule::Rect::new(30, 0, 13, 1),
        );
        app.viewport.ui.commit();

        // Click on Tab 1
        mouse::handle_selection_start(&mut app, &runtime, "s1", 16, 0).await;
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 1);
        assert_eq!(
            app.surfaces.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Dashboard
        );

        // Click on Tab 2
        mouse::handle_selection_start(&mut app, &runtime, "s1", 32, 0).await;
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 2);
        assert_eq!(
            app.surfaces.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Settings
        );
    }

    #[tokio::test]
    async fn mouse_click_on_tab_bar_background_is_inert_and_preserves_composer_focus() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        assert!(
            app.threads.focus.is_composer(),
            "composer active by default"
        );

        // Mount TabBar across full width (0..80) and TabBarItem(0) at (0..14)
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            graticule::Rect::new(0, 0, 80, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(0),
            graticule::Rect::new(0, 0, 14, 1),
        );
        app.viewport.ui.commit();

        // Click outside tabs on the TabBar background (col 50)
        mouse::handle_selection_start(&mut app, &runtime, "s1", 50, 0).await;
        // ADR-0091 [INV-TAB-11]: TabBar background clicks are inert container gestures
        // and must NOT leak side-effects into active scene context.
        assert!(
            app.threads.focus.is_composer(),
            "clicking TabBar background must not blur composer"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_active_thread_tab_is_idempotent_and_preserves_composer_focus() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces.surfaces.select_tab(0);
        assert!(
            app.threads.focus.is_composer(),
            "composer active by default"
        );

        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            graticule::Rect::new(0, 0, 80, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(0),
            graticule::Rect::new(0, 0, 14, 1),
        );
        app.viewport.ui.commit();

        // Click on already-active Thread tab (col 5)
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 0).await;
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 0);
        // ADR-0091 [INV-TAB-11]: Re-selecting the already active tab is an idempotent
        // container gesture that preserves active context.
        assert!(
            app.threads.focus.is_composer(),
            "clicking active tab must not blur composer"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_scene_history_back_unwinds_subagent_to_thread() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.open_tab(crate::shell::surfaces::ClientTab::thread(
            "conv-1",
            None::<String>,
        ));
        app.enter_subagent("call_sub_1".to_string());
        assert_eq!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Subagent
        );
        assert!(app.in_subagent_view());

        // Mount SceneBar at row 1 and SceneHistoryBack at (50..58, 1)
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::SceneBar,
            graticule::Rect::new(0, 1, 80, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::SceneHistoryBack,
            graticule::Rect::new(50, 1, 8, 1),
        );
        app.viewport.ui.commit();

        // Click on the SceneHistoryBack button (col 52, row 1)
        mouse::handle_selection_start(&mut app, &runtime, "conv-1", 52, 1).await;
        assert_eq!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Thread
        );
        assert!(!app.in_subagent_view());
    }

    #[tokio::test]
    async fn mouse_click_on_scene_breadcrumb_jumps_to_ancestor_depth() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.open_tab(crate::shell::surfaces::ClientTab::thread(
            "conv-1",
            None::<String>,
        ));
        app.enter_subagent("call_sub_1".to_string());
        assert_eq!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Subagent
        );
        assert!(app.in_subagent_view());

        // Mount SceneBreadcrumb(0) (thread ancestor) at (60..66, 1)
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::SceneBreadcrumb(0),
            graticule::Rect::new(60, 1, 6, 1),
        );
        app.viewport.ui.commit();

        // Click on the thread ancestor crumb (col 62, row 1)
        mouse::handle_selection_start(&mut app, &runtime, "conv-1", 62, 1).await;
        assert_eq!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Thread
        );
        assert!(!app.in_subagent_view());
    }

    #[tokio::test]
    async fn tab_switching_preserves_and_restores_sovereign_tab_context() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces.surfaces.select_tab(0);

        // Customize Thread 1 context: browse mode, scrolled up, draft input text
        app.threads
            .focus
            .enter_transcript(crate::shell::focus::TranscriptFocus::Browse);
        app.threads.scroll = 42;
        app.composer.input = "draft question for thread 1".to_string();
        app.composer.cursor_position = 5;

        // Switch to Dashboard (Tab 1)
        activate_tab_at_index(&mut app, 1, &runtime, "s1");
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 1);
        assert_eq!(
            app.surfaces.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Dashboard
        );

        // Switch back to Thread 1 (Tab 0)
        activate_tab_at_index(&mut app, 0, &runtime, "s1");
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 0);
        assert_eq!(
            app.surfaces.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Thread
        );

        // ADR-0091 [INV-TAB-12]: Restores sovereign tab context exactly as left
        assert!(
            app.threads.focus.is_transcript(),
            "browse focus mode must be preserved across tab switch"
        );
        assert_eq!(app.threads.scroll, 42, "scroll offset must be preserved");
        assert_eq!(
            app.composer.input, "draft question for thread 1",
            "draft text must be preserved"
        );
        assert_eq!(
            app.composer.cursor_position, 5,
            "caret position must be preserved"
        );
    }

    #[tokio::test]
    async fn mouse_click_outside_composer_on_unmounted_zone_is_inert() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        assert!(
            app.threads.focus.is_composer(),
            "composer active by default"
        );

        // Click in outer unmounted zone / dead zone
        mouse::handle_selection_start(&mut app, &runtime, "s1", 79, 10).await;
        // ADR-0091 [INV-TAB-11]: Unmounted zones outside the active transcript content rect
        // are inert and must not leak focus mutations into the composer.
        assert!(
            app.threads.focus.is_composer(),
            "clicking outside composer on unmounted zone must be inert"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_dialog_entry_selects_row() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.session.sessions_overview = (0..5)
            .map(|i| crate::shell::tests::overview_row(&format!("s{i}")))
            .collect();
        app.open_dialog(crate::shell::surfaces::DialogKind::Threads);
        let threads = app
            .surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>();
        threads.index = 0;
        threads.scroll = 0;

        // Modal bounding box: y=5..20 (height=15).
        // Body starts at y = 5 + 3 = 8.
        // Row 0 is at y=8, Row 1 is at y=9, Row 2 is at y=10.
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Threads,
            )),
            graticule::Rect::new(10, 5, 60, 15),
        );
        app.viewport.ui.commit();

        // 1. Click on Row 1 (y = 9) inside modal: selects row 1
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 9).await;
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            1,
            "clicking row 1 sets index to 1"
        );
        assert!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .follow
        );

        // 2. Click on Row 2 (y = 10) inside modal: selects row 2
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 10).await;
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            2,
            "clicking row 2 sets index to 2"
        );

        // 3. Sub-view parity [INV-SURFACE-08]: in info_detail mode, clicking inside list area does not corrupt index
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .info_detail = true;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 8).await;
        assert_eq!(
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            2,
            "info_detail mode protects list index"
        );
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .info_detail = false;

        // 4. Re-clicking an already-selected thread row for a different session switches to target
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 10).await;
        assert_eq!(app.surfaces.switch_to_target, Some("s2".to_string()));

        // 5. Re-clicking the already-selected row for the current session dismisses the dialog
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .index = 1;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 9).await;
        assert!(
            app.active_dialog().is_none(),
            "re-clicking current session dismisses dialog"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_config_dropdown_selects_and_outside_dismisses() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
        app.surfaces.websearch_config = Some(protocol::WebSearchConfigView {
            revision: 42,
            provider: protocol::WebSearchProvider::Exa,
            reader: protocol::WebReaderProvider::Disabled,
            timeout_secs: 20,
            searxng_url: None,
            search_credential: protocol::WebCredentialStatus::Stored,
            reader_credential: protocol::WebCredentialStatus::NotRequired,
            capabilities: protocol::web_provider_capabilities(),
        });

        // 1. Open dropdown
        let dropdown = view::views::settings::build_websearch_provider_dropdown(
            "exa",
            app.surfaces.websearch_config.as_ref(),
        );
        let anchor = ui_primitives::components::dropdown::DropdownAnchor::center_screen();
        app.surfaces.config_dropdown = Some((dropdown, anchor.clone()));
        assert!(app.surfaces.config_dropdown.is_some());

        // Mount dropdown popup at (20, 5, 40, 10)
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::ConfigDropdown,
            graticule::Rect::new(20, 5, 40, 10),
        );
        app.viewport.ui.commit();

        // Click outside at (5, 2): dismisses dropdown
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 2).await;
        assert!(
            app.surfaces.config_dropdown.is_none(),
            "clicking outside dismisses dropdown"
        );

        // 2. Open dropdown again and click item 1 (Tavily) at (25, 8)
        let dropdown = view::views::settings::build_websearch_provider_dropdown(
            "exa",
            app.surfaces.websearch_config.as_ref(),
        );
        app.surfaces.config_dropdown = Some((dropdown, anchor));
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::ConfigDropdown,
            graticule::Rect::new(20, 5, 40, 10),
        );
        app.viewport.ui.commit();

        // Items start at y = 5 + 2 = 7. Row 0 (Exa) at y=7, Row 1 (Tavily) at y=8.
        mouse::handle_selection_start(&mut app, &runtime, "s1", 25, 8).await;
        assert!(
            app.surfaces.config_dropdown.is_none(),
            "selecting item closes dropdown"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_settings_category_and_row_toggle() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
        app.surfaces.config_category = 0; // Appearance
        app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Categories;

        // Mount Category 1 (Components) at (2, 4, 18, 2) and Detail Row 0 at (24, 4, 40, 2)
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::SettingsCategory(1),
            graticule::Rect::new(2, 4, 18, 2),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::SettingsRow(0),
            graticule::Rect::new(24, 4, 40, 2),
        );
        app.viewport.ui.commit();

        // 1. Click Category 1: switches to Components category
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 4).await;
        assert_eq!(
            app.surfaces.config_category, 1,
            "switched to Components category"
        );
        assert_eq!(
            app.surfaces.config_focus,
            crate::shell::overlays::ConfigFocus::Categories
        );

        // 2. Click Detail Row 0 (Components row 0 = Reasoning Traces): toggles reasoning expanded!
        let initial_reasoning = app.viewport.reasoning_default_expanded;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 30, 4).await;
        assert_eq!(
            app.surfaces.config_focus,
            crate::shell::overlays::ConfigFocus::Detail
        );
        assert_eq!(app.surfaces.config_detail_index, 0);
        assert_eq!(
            app.viewport.reasoning_default_expanded, !initial_reasoning,
            "clicking components row toggles setting"
        );

        // 3. Click Detail Row 0 again: toggles back
        mouse::handle_selection_start(&mut app, &runtime, "s1", 30, 4).await;
        assert_eq!(
            app.viewport.reasoning_default_expanded, initial_reasoning,
            "clicking components row again toggles setting back"
        );
    }

    #[tokio::test]
    async fn mouse_click_outside_dialog_dismisses_dialog() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.open_dialog(crate::shell::surfaces::DialogKind::Tools);
        assert!(app.active_dialog().is_some());
        app.surfaces.click_outside_dismiss = true;

        // Modal mounted at (10, 5, 60, 15)
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Tools,
            )),
            graticule::Rect::new(10, 5, 60, 15),
        );
        app.viewport.ui.commit();

        // Click outside at (2, 2) dismisses the dialog
        mouse::handle_selection_start(&mut app, &runtime, "s1", 2, 2).await;
        assert!(
            app.active_dialog().is_none(),
            "dialog dismissed on backdrop click"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_overlay_modal_does_not_set_transcript_focused() {
        let mut app = scrollable_app();
        let runtime = UiRuntime::minimal_for_test();
        let antigravity = crate::PROVIDER_PRESETS
            .iter()
            .find(|p| p.id == "google-antigravity")
            .expect("antigravity preset exists");

        app.open_preset_chooser();
        app.begin_oauth_add(antigravity, protocol::LoginMethod::Browser);
        assert!(app.threads.focus.is_composer());

        // Mount the OAuth overlay area in UI so pointer target falls within overlay
        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Sheet(
                crate::shell::surfaces::SheetKind::OAuthPending,
            )),
            graticule::Rect::new(10, 5, 60, 14),
        );
        app.viewport.ui.commit();

        // Mouse click at an arbitrary coordinate inside the overlay where no text cursor exists
        mouse::handle_selection_start(&mut app, &runtime, "s1", 12, 6).await;

        assert!(
            app.threads.focus.is_composer(),
            "Click on overlay modal must NOT set transcript_focused to true"
        );
    }
}

#[cfg(test)]
mod view_entry_tests {
    use super::*;

    #[test]
    fn every_show_refreshes_remote_view_data() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        assert!(enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::SessionTree,
            &runtime,
            "s1"
        ));
        assert!(matches!(
            rx.try_recv().ok().and_then(|o| o.into_request()),
            Some(AgentRequest::QueryThreadTree)
        ));
        app.dismiss_surface();
        assert!(!enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::SessionTree,
            &runtime,
            "s1"
        ));
        assert!(matches!(
            rx.try_recv().ok().and_then(|o| o.into_request()),
            Some(AgentRequest::QueryThreadTree)
        ));
    }

    #[test]
    fn the_context_plan_dialog_queries_the_plan_on_every_open() {
        // ADR-0220 §3: the plan is live process state, so opening the inspector
        // re-queries instead of reusing whatever the last open installed.
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::ContextPlan,
            &runtime,
            "s1",
        );
        assert!(matches!(
            rx.try_recv().ok().and_then(|o| o.into_request()),
            Some(AgentRequest::QueryContextPlan)
        ));
    }

    #[test]
    fn sessions_and_skills_have_complete_query_paths() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::Threads,
            &runtime,
            "s1",
        );
        assert!(matches!(
            rx.try_recv().ok().and_then(|o| o.into_request()),
            Some(AgentRequest::QueryThreadsOverview)
        ));
        enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::Skills,
            &runtime,
            "s1",
        );
        assert!(matches!(
            rx.try_recv().ok().and_then(|o| o.into_request()),
            Some(AgentRequest::QueryThreadContext)
        ));
    }
}

mod tab_lifecycle_tests {
    use super::*;

    fn app_with_tabs() -> App {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::settings());
        app.surfaces.surfaces.select_tab(0);
        app
    }

    /// ADR-0235 `[INV-TAB-18]`: the close mark closes the pill the pointer
    /// names, even when that pill is not the active one.
    #[tokio::test]
    async fn close_mark_closes_the_named_tab_not_the_active_one() {
        let mut app = app_with_tabs();
        let runtime = UiRuntime::minimal_for_test();
        assert_eq!(app.surfaces.surfaces.active_tab_index(), 0);

        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            graticule::Rect::new(0, 0, 80, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(1),
            graticule::Rect::new(14, 0, 14, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarClose(1),
            graticule::Rect::new(26, 0, 1, 1),
        );
        app.viewport.ui.commit();

        mouse::handle_selection_start(&mut app, &runtime, "s1", 26, 0).await;

        let kinds: Vec<_> = app
            .surfaces
            .surfaces
            .tabs()
            .iter()
            .map(|tab| tab.kind.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                crate::shell::surfaces::TabKind::Thread("s1".to_string()),
                crate::shell::surfaces::TabKind::Settings,
            ],
            "the dashboard pill was pointed at, so the dashboard tab closed"
        );
        assert_eq!(
            app.surfaces.surfaces.active_tab_index(),
            0,
            "closing an inactive tab leaves the selection where it was"
        );
    }

    /// ADR-0235 `[INV-TAB-17]`: the add affordance arms the same new-tab
    /// resolution the keyboard uses.
    #[tokio::test]
    async fn add_affordance_arms_a_new_tab() {
        let mut app = app_with_tabs();
        let runtime = UiRuntime::minimal_for_test();
        // A non-empty thread: a fresh tab means detaching into a new attach.
        app.threads
            .messages
            .push(view::model::document::TranscriptMessage::new(
                tool::Role::User,
                "Existing message",
            ));
        assert!(!app.is_current_thread_empty());

        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarAdd,
            graticule::Rect::new(40, 0, 3, 1),
        );
        app.viewport.ui.commit();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 41, 0).await;

        assert!(
            !app.link
                .should_quit
                .load(std::sync::atomic::Ordering::SeqCst),
            "ADR-0251: creating a new tab subscribes over the live multiplexed connection without quitting"
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::shell::client::ClientOutbound::Subscribe {
                target: protocol::ThreadTarget::New { .. },
                ..
            })
        ));
    }

    /// The add affordance recycles an empty thread in place instead of leaving
    /// it behind — one verb, one meaning, whichever surface fired it.
    #[tokio::test]
    async fn add_affordance_recycles_an_empty_thread_in_place() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;
        let runtime = UiRuntime::minimal_for_test();
        let sid = "empty-session-1";
        app.session.current_session_id = sid.to_string();
        app.open_tab(crate::shell::surfaces::ClientTab::thread(
            sid,
            None::<String>,
        ));
        assert!(app.is_current_thread_empty());

        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarAdd,
            graticule::Rect::new(40, 0, 3, 1),
        );
        app.viewport.ui.commit();

        mouse::handle_selection_start(&mut app, &runtime, sid, 41, 0).await;

        assert!(
            !app.link
                .should_quit
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        match rx.try_recv().ok().and_then(|o| o.into_request()) {
            Some(protocol::AgentRequest::SlashCommand(command)) => assert_eq!(command, "/new"),
            other => panic!("expected `/new` for an empty thread, got {other:?}"),
        }
    }

    /// ADR-0235 `[INV-TAB-16]`: hover resolves through the committed scene, and
    /// stepping from a pill onto its own `✕` keeps the reveal clock running.
    #[tokio::test]
    async fn hover_tracks_the_pill_then_its_close_mark_as_one_visit() {
        use view::view_header::TabBarHover;
        let mut app = app_with_tabs();
        let runtime = UiRuntime::minimal_for_test();

        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            graticule::Rect::new(0, 0, 80, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(0),
            graticule::Rect::new(1, 0, 16, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarClose(0),
            graticule::Rect::new(14, 0, 1, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarAdd,
            graticule::Rect::new(40, 0, 3, 1),
        );
        app.viewport.ui.commit();

        mouse::handle_hover(&mut app, &runtime, 5, 0).await;
        assert_eq!(app.surfaces.tab_hover, Some(TabBarHover::Tab(0)));
        let first = app.surfaces.tab_hover_since.expect("the visit has a clock");

        mouse::handle_hover(&mut app, &runtime, 14, 0).await;
        assert_eq!(app.surfaces.tab_hover, Some(TabBarHover::Close(0)));
        assert_eq!(
            app.surfaces.tab_hover_since,
            Some(first),
            "the close mark belongs to the pill, so the reveal does not rewind"
        );

        mouse::handle_hover(&mut app, &runtime, 41, 0).await;
        assert_eq!(app.surfaces.tab_hover, Some(TabBarHover::Add));

        mouse::handle_hover(&mut app, &runtime, 70, 0).await;
        assert_eq!(app.surfaces.tab_hover, Some(TabBarHover::Rail));

        mouse::handle_hover(&mut app, &runtime, 5, 10).await;
        assert_eq!(app.surfaces.tab_hover, None, "leaving the row clears it");
        assert!(app.surfaces.tab_hover_since.is_none());
    }

    /// ADR-0245 `[INV-TAB-20]`: hovering the elastic TabBar help portal lights
    /// up `TabBarHover::Help`, and clicking it toggles `DialogKind::Help`
    /// identically to `F1` / `Alt-h`.
    #[tokio::test]
    async fn clicking_tabbar_help_toggles_help_dialog() {
        use crate::shell::surfaces::DialogKind;
        use view::view_header::TabBarHover;

        let mut app = app_with_tabs();
        let runtime = UiRuntime::minimal_for_test();

        app.viewport.ui.begin(graticule::Rect::new(0, 0, 80, 24));
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            graticule::Rect::new(0, 0, 80, 1),
        );
        app.viewport.ui.mount(
            crate::shell::ui::UiKey::TabBarHelp,
            graticule::Rect::new(72, 0, 7, 1),
        );
        app.viewport.ui.commit();

        mouse::handle_hover(&mut app, &runtime, 74, 0).await;
        assert_eq!(app.surfaces.tab_hover, Some(TabBarHover::Help));

        mouse::handle_selection_start(&mut app, &runtime, "sess-1", 74, 0).await;
        assert_eq!(
            app.active_dialog(),
            Some(DialogKind::Help),
            "first click on TabBarHelp opens the Help dialog"
        );

        mouse::handle_selection_start(&mut app, &runtime, "sess-1", 74, 0).await;
        assert_eq!(
            app.active_dialog(),
            None,
            "second click on TabBarHelp dismisses the Help dialog"
        );
    }
}
