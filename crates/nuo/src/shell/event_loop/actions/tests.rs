use super::*;

mod transcript_scroll_tests {
    use super::*;
    use crate::shell::surfaces::DialogView;

    fn scrollable_app() -> App {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.max_scroll = 100;
        app.scroll = 100;
        app.view_height = 20;
        app.follow_bottom = true;
        app
    }

    #[test]
    fn wheel_ticks_move_the_transcript_and_rearm_bottom_follow() {
        let mut app = scrollable_app();

        scroll_tick(&mut app, false);
        assert_eq!(app.scroll, 96);
        assert!(!app.follow_bottom);

        scroll_tick(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn page_navigation_uses_the_measured_viewport() {
        let mut app = scrollable_app();

        scroll_transcript_page(&mut app, false);
        assert_eq!(app.scroll, 81);
        assert!(!app.follow_bottom);

        scroll_transcript_page(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn edge_navigation_updates_position_and_follow_mode_together() {
        let mut app = scrollable_app();

        scroll_transcript_to_edge(&mut app, false);
        assert_eq!(app.scroll, 0);
        assert!(!app.follow_bottom);

        scroll_transcript_to_edge(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn wheel_spatial_routing_under_permission_modal() {
        let mut app = scrollable_app();
        app.set_active_sheet_for_test(crate::shell::sheet::SheetKind::Permission);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui
            .mount_permission_sheet(nuotc::Rect::new(0, 15, 80, 5));
        app.ui.commit();
        app.permission_show_details = true;
        app.permission_max_scroll = 10;
        app.permission_scroll = 2;

        let runtime = UiRuntime::minimal_for_test();

        // 1. Wheel over permission sheet (y=16) scrolls details down
        handle_wheel(&mut app, false, 10, 16, &runtime, "");
        assert_eq!(app.permission_scroll, 3);
        assert_eq!(app.scroll, 100, "transcript scroll untouched");

        // 2. Wheel over permission sheet (y=16) scrolls details up
        handle_wheel(&mut app, true, 10, 16, &runtime, "");
        assert_eq!(app.permission_scroll, 2);
        assert_eq!(app.scroll, 100, "transcript scroll untouched");

        // 3. Wheel above permission sheet (y=5) scrolls transcript
        handle_wheel(&mut app, true, 10, 5, &runtime, "");
        assert_eq!(app.scroll, 96, "transcript scrolled up");
        assert_eq!(app.permission_scroll, 2, "permission scroll untouched");
    }

    #[test]
    fn wheel_spatial_routing_under_overlay_modal_isolates_backdrop() {
        let mut app = scrollable_app();
        app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::UsageStats,
            )),
            nuotc::Rect::new(10, 5, 60, 10),
        );
        app.ui.commit();
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
            .scroll = 5;

        let runtime = UiRuntime::minimal_for_test();

        // 1. Wheel on backdrop (x=2, y=2) outside modal_rect: absorbed, neither modal nor transcript scrolls
        handle_wheel(&mut app, false, 2, 2, &runtime, "");
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .scroll,
            5,
            "modal scroll untouched on backdrop"
        );
        assert_eq!(app.scroll, 100, "transcript scroll untouched on backdrop");

        // 2. Wheel inside modal (x=20, y=8): scrolls modal body
        handle_wheel(&mut app, false, 20, 8, &runtime, "");
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .scroll,
            6,
            "modal scrolled inside modal_rect"
        );
        assert_eq!(app.scroll, 100, "transcript scroll untouched");
    }

    #[test]
    fn wheel_spatial_routing_under_dialog_sub_views_scrolls_info_detail() {
        let mut app = scrollable_app();
        app.open_dialog(crate::shell::surfaces::DialogKind::Threads);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Threads,
            )),
            nuotc::Rect::new(10, 5, 60, 10),
        );
        app.ui.commit();

        let threads = app
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
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .info_scroll,
            1,
            "info_detail scrolled down"
        );
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .scroll,
            2,
            "main list scroll untouched"
        );

        handle_wheel(&mut app, true, 20, 8, &runtime, "");
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .info_scroll,
            0,
            "info_detail scrolled back up"
        );
        assert_eq!(
            app.surfaces
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
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::UsageStats,
            )),
            nuotc::Rect::new(10, 5, 60, 10),
        );
        app.ui.commit();

        let stats = app
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
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .keys_scroll_mut(),
            1,
            "dialog keys sublayer scrolled down"
        );
        assert_eq!(
            app.surfaces
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
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                None::<String>,
            ));
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s2",
                None::<String>,
            ));
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces.select_tab(0);
        assert_eq!(app.surfaces.active_tab_index(), 0);

        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            nuotc::Rect::new(0, 0, 80, 1),
        );
        app.ui.commit();

        let runtime = UiRuntime::minimal_for_test();

        // 1. Wheel down over TabBar cycles to next tab (tab 1)
        handle_wheel(&mut app, false, 20, 0, &runtime, "s1");
        assert_eq!(app.surfaces.active_tab_index(), 1);

        // 2. Wheel up over TabBar cycles to previous tab (tab 0)
        handle_wheel(&mut app, true, 20, 0, &runtime, "s1");
        assert_eq!(app.surfaces.active_tab_index(), 0);

        // 3. Wheel up at tab 0 wraps around to tab 2
        handle_wheel(&mut app, true, 20, 0, &runtime, "s1");
        assert_eq!(app.surfaces.active_tab_index(), 2);
    }

    #[tokio::test]
    async fn completion_menu_mouse_wheel_and_click() {
        let mut app = scrollable_app();
        app.input = "/m".to_string();
        app.cursor_position = 2;
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount_completion(nuotc::Rect::new(0, 8, 30, 2));
        app.ui
            .mount_completion_item(0, nuotc::Rect::new(0, 8, 30, 1));
        app.ui
            .mount_completion_item(1, nuotc::Rect::new(0, 9, 30, 1));
        app.ui.commit();

        let runtime = UiRuntime::minimal_for_test();

        // Wheel over menu cycles suggestions
        handle_wheel(&mut app, false, 5, 8, &runtime, "");
        assert!(app.suggestion_index.is_some());

        // Click on completion item 0 accepts completion
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 8).await;
        assert!(app.completion_dismissed);
        assert_eq!(app.suggestion_index, None);
    }

    #[tokio::test]
    async fn mouse_click_on_tab_bar_switches_active_tab() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        // Mount tabs
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::settings());
        app.surfaces.select_tab(0);
        assert_eq!(app.surfaces.active_tab_index(), 0);

        // Mount TabBarItem(1) (Dashboard tab) at (14, 0)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(1),
            nuotc::Rect::new(14, 0, 14, 1),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(2),
            nuotc::Rect::new(30, 0, 13, 1),
        );
        app.ui.commit();

        // Click on Tab 1
        mouse::handle_selection_start(&mut app, &runtime, "s1", 16, 0).await;
        assert_eq!(app.surfaces.active_tab_index(), 1);
        assert_eq!(
            app.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Dashboard
        );

        // Click on Tab 2
        mouse::handle_selection_start(&mut app, &runtime, "s1", 32, 0).await;
        assert_eq!(app.surfaces.active_tab_index(), 2);
        assert_eq!(
            app.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Settings
        );
    }

    #[tokio::test]
    async fn mouse_click_on_tab_bar_background_is_inert_and_preserves_composer_focus() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        assert!(app.focus.is_composer(), "composer active by default");

        // Mount TabBar across full width (0..80) and TabBarItem(0) at (0..14)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            nuotc::Rect::new(0, 0, 80, 1),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(0),
            nuotc::Rect::new(0, 0, 14, 1),
        );
        app.ui.commit();

        // Click outside tabs on the TabBar background (col 50)
        mouse::handle_selection_start(&mut app, &runtime, "s1", 50, 0).await;
        // ADR-0091 [INV-TAB-11]: TabBar background clicks are inert container gestures
        // and must NOT leak side-effects into active scene context.
        assert!(
            app.focus.is_composer(),
            "clicking TabBar background must not blur composer"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_active_thread_tab_is_idempotent_and_preserves_composer_focus() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces.select_tab(0);
        assert!(app.focus.is_composer(), "composer active by default");

        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            nuotc::Rect::new(0, 0, 80, 1),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(0),
            nuotc::Rect::new(0, 0, 14, 1),
        );
        app.ui.commit();

        // Click on already-active Thread tab (col 5)
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 0).await;
        assert_eq!(app.surfaces.active_tab_index(), 0);
        // ADR-0091 [INV-TAB-11]: Re-selecting the already active tab is an idempotent
        // container gesture that preserves active context.
        assert!(
            app.focus.is_composer(),
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
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::SceneBar,
            nuotc::Rect::new(0, 1, 80, 1),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::SceneHistoryBack,
            nuotc::Rect::new(50, 1, 8, 1),
        );
        app.ui.commit();

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
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::SceneBreadcrumb(0),
            nuotc::Rect::new(60, 1, 6, 1),
        );
        app.ui.commit();

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
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces.select_tab(0);

        // Customize Thread 1 context: browse mode, scrolled up, draft input text
        app.focus
            .enter_transcript(crate::shell::focus::TranscriptFocus::Browse);
        app.scroll = 42;
        app.input = "draft question for thread 1".to_string();
        app.cursor_position = 5;

        // Switch to Dashboard (Tab 1)
        activate_tab_at_index(&mut app, 1, &runtime, "s1");
        assert_eq!(app.surfaces.active_tab_index(), 1);
        assert_eq!(
            app.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Dashboard
        );

        // Switch back to Thread 1 (Tab 0)
        activate_tab_at_index(&mut app, 0, &runtime, "s1");
        assert_eq!(app.surfaces.active_tab_index(), 0);
        assert_eq!(
            app.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Thread
        );

        // ADR-0091 [INV-TAB-12]: Restores sovereign tab context exactly as left
        assert!(
            app.focus.is_transcript(),
            "browse focus mode must be preserved across tab switch"
        );
        assert_eq!(app.scroll, 42, "scroll offset must be preserved");
        assert_eq!(
            app.input, "draft question for thread 1",
            "draft text must be preserved"
        );
        assert_eq!(app.cursor_position, 5, "caret position must be preserved");
    }

    #[tokio::test]
    async fn mouse_click_outside_composer_on_unmounted_zone_is_inert() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        assert!(app.focus.is_composer(), "composer active by default");

        // Click in outer unmounted zone / dead zone
        mouse::handle_selection_start(&mut app, &runtime, "s1", 79, 10).await;
        // ADR-0091 [INV-TAB-11]: Unmounted zones outside the active transcript content rect
        // are inert and must not leak focus mutations into the composer.
        assert!(
            app.focus.is_composer(),
            "clicking outside composer on unmounted zone must be inert"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_dialog_entry_selects_row() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.sessions_overview = (0..5)
            .map(|i| crate::shell::tests::overview_row(&format!("s{i}")))
            .collect();
        app.open_dialog(crate::shell::surfaces::DialogKind::Threads);
        let threads = app
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>();
        threads.index = 0;
        threads.scroll = 0;

        // Modal bounding box: y=5..20 (height=15).
        // Body starts at y = 5 + 3 = 8.
        // Row 0 is at y=8, Row 1 is at y=9, Row 2 is at y=10.
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Threads,
            )),
            nuotc::Rect::new(10, 5, 60, 15),
        );
        app.ui.commit();

        // 1. Click on Row 1 (y = 9) inside modal: selects row 1
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 9).await;
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            1,
            "clicking row 1 sets index to 1"
        );
        assert!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .follow
        );

        // 2. Click on Row 2 (y = 10) inside modal: selects row 2
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 10).await;
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            2,
            "clicking row 2 sets index to 2"
        );

        // 3. Sub-view parity [INV-SURFACE-08]: in info_detail mode, clicking inside list area does not corrupt index
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .info_detail = true;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 8).await;
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            2,
            "info_detail mode protects list index"
        );
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .info_detail = false;

        // 4. Re-clicking an already-selected thread row for a different session switches to target
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 10).await;
        assert_eq!(app.switch_to_target, Some("s2".to_string()));

        // 5. Re-clicking the already-selected row for the current session dismisses the dialog
        app.surfaces
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
        app.websearch_config = Some(nuo_protocol::WebSearchConfigView {
            revision: 42,
            provider: nuo_protocol::WebSearchProvider::Exa,
            reader: nuo_protocol::WebReaderProvider::Disabled,
            timeout_secs: 20,
            searxng_url: None,
            search_credential: nuo_protocol::WebCredentialStatus::Stored,
            reader_credential: nuo_protocol::WebCredentialStatus::NotRequired,
            capabilities: nuo_protocol::web_provider_capabilities(),
        });

        // 1. Open dropdown
        let dropdown = crate::shell::views::settings::build_websearch_provider_dropdown(
            "exa",
            app.websearch_config.as_ref(),
        );
        let anchor = nuo_ui_primitives::components::dropdown::DropdownAnchor::center_screen();
        app.config_dropdown = Some((dropdown, anchor.clone()));
        assert!(app.config_dropdown.is_some());

        // Mount dropdown popup at (20, 5, 40, 10)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::ConfigDropdown,
            nuotc::Rect::new(20, 5, 40, 10),
        );
        app.ui.commit();

        // Click outside at (5, 2): dismisses dropdown
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 2).await;
        assert!(
            app.config_dropdown.is_none(),
            "clicking outside dismisses dropdown"
        );

        // 2. Open dropdown again and click item 1 (Tavily) at (25, 8)
        let dropdown = crate::shell::views::settings::build_websearch_provider_dropdown(
            "exa",
            app.websearch_config.as_ref(),
        );
        app.config_dropdown = Some((dropdown, anchor));
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::ConfigDropdown,
            nuotc::Rect::new(20, 5, 40, 10),
        );
        app.ui.commit();

        // Items start at y = 5 + 2 = 7. Row 0 (Exa) at y=7, Row 1 (Tavily) at y=8.
        mouse::handle_selection_start(&mut app, &runtime, "s1", 25, 8).await;
        assert!(
            app.config_dropdown.is_none(),
            "selecting item closes dropdown"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_settings_category_and_row_toggle() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
        app.config_category = 0; // Appearance
        app.config_focus = crate::shell::overlays::ConfigFocus::Categories;

        // Mount Category 1 (Components) at (2, 4, 18, 2) and Detail Row 0 at (24, 4, 40, 2)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::SettingsCategory(1),
            nuotc::Rect::new(2, 4, 18, 2),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::SettingsRow(0),
            nuotc::Rect::new(24, 4, 40, 2),
        );
        app.ui.commit();

        // 1. Click Category 1: switches to Components category
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 4).await;
        assert_eq!(app.config_category, 1, "switched to Components category");
        assert_eq!(
            app.config_focus,
            crate::shell::overlays::ConfigFocus::Categories
        );

        // 2. Click Detail Row 0 (Components row 0 = Reasoning Traces): toggles reasoning expanded!
        let initial_reasoning = app.reasoning_default_expanded;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 30, 4).await;
        assert_eq!(
            app.config_focus,
            crate::shell::overlays::ConfigFocus::Detail
        );
        assert_eq!(app.config_detail_index, 0);
        assert_eq!(
            app.reasoning_default_expanded, !initial_reasoning,
            "clicking components row toggles setting"
        );

        // 3. Click Detail Row 0 again: toggles back
        mouse::handle_selection_start(&mut app, &runtime, "s1", 30, 4).await;
        assert_eq!(
            app.reasoning_default_expanded, initial_reasoning,
            "clicking components row again toggles setting back"
        );
    }

    #[tokio::test]
    async fn mouse_click_outside_dialog_dismisses_dialog() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.open_dialog(crate::shell::surfaces::DialogKind::Tools);
        assert!(app.active_dialog().is_some());
        app.click_outside_dismiss = true;

        // Modal mounted at (10, 5, 60, 15)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Tools,
            )),
            nuotc::Rect::new(10, 5, 60, 15),
        );
        app.ui.commit();

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
        app.begin_oauth_add(antigravity, nuo_protocol::LoginMethod::Browser);
        assert!(app.focus.is_composer());

        // Mount the OAuth overlay area in UI so pointer target falls within overlay
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Sheet(
                crate::shell::surfaces::SheetKind::OAuthPending,
            )),
            nuotc::Rect::new(10, 5, 60, 14),
        );
        app.ui.commit();

        // Mouse click at an arbitrary coordinate inside the overlay where no text cursor exists
        mouse::handle_selection_start(&mut app, &runtime, "s1", 12, 6).await;

        assert!(
            app.focus.is_composer(),
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
        app.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        assert!(enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::SessionTree,
            &runtime,
            "s1"
        ));
        assert!(matches!(rx.try_recv(), Ok(AgentRequest::QueryThreadTree)));
        app.dismiss_surface();
        assert!(!enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::SessionTree,
            &runtime,
            "s1"
        ));
        assert!(matches!(rx.try_recv(), Ok(AgentRequest::QueryThreadTree)));
    }

    #[test]
    fn sessions_and_skills_have_complete_query_paths() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::Threads,
            &runtime,
            "s1",
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentRequest::QueryThreadsOverview)
        ));
        enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::Skills,
            &runtime,
            "s1",
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentRequest::QueryThreadContext)
        ));
    }
}
