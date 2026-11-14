use super::*;

#[test]
fn config_view_reopen_keeps_pane_and_category() {
    // Settings is a full-screen scene whose fields persist
    // natively on `App`: the enter ritual (pane reset + current-scheme
    // positioning) runs on every enter; a reopen keeps the category/pane
    // the user left. Esc's step-backs unwind the pane/dropdown *inside* the
    // scene and stop there; leaving the scene is `close_scene`
    // (`C-x w`, or the scene's own `q`).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Settings
    );

    // Esc's back never leaves the scene, whatever sub-layer is open.
    app.surfaces.config_category = 2;
    app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Detail;
    app.scene_back();
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Settings,
        "Esc steps back inside the scene, it does not leave it"
    );

    // The deliberate exit verb leaves.
    app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Detail;
    app.close_scene();
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );

    // Reopen: the pane/category survived.
    app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
    assert_eq!(
        app.surfaces.config_category, 2,
        "category retained across hide"
    );
    assert_eq!(
        app.surfaces.config_focus,
        crate::shell::overlays::ConfigFocus::Detail,
        "pane retained across hide"
    );
}

/// `[INV-TUI-CLEAN-02]`: Esc is never a scene exit.
/// Sweeps every scene and every Esc-reachable dispatch surface (the router's
/// Esc arm, the shared close-modal handler, the `CancelOrBack` command, and the
/// scene-local back verb) and asserts the current scene survives all of them.
/// Only `close_scene` — the `C-x` namespace's leave verb — may leave a scene.
#[test]
fn esc_never_leaves_a_scene_anywhere_in_the_dispatch() {
    use crate::shell::surfaces::SceneKind;

    for scene in [
        SceneKind::Dashboard,
        SceneKind::Settings,
        SceneKind::Subagent,
    ] {
        let (mut app, _tmp) = app_in_tempdir(&[], &[]);
        app.switch_scene(scene);

        // 1. The shared close-modal handler (Esc over a resolved overlay).
        crate::shell::event_loop::handle_close_modal(&mut app, "s1");
        assert_eq!(
            app.current_scene(),
            scene,
            "the overlay dismiss verb must never leave {scene:?}"
        );

        // 2. The scene-local step back (Esc on the scene's own chrome).
        app.scene_back();
        assert_eq!(
            app.current_scene(),
            scene,
            "the step-back verb must never leave {scene:?}"
        );

        // 3. The shared dismiss verb (its old signature returned `true` for a
        //    scene, which is exactly how Esc used to navigate).
        assert!(
            !app.dismiss_surface(),
            "a dismiss on a bare {scene:?} has nothing to act on"
        );
        assert_eq!(
            app.current_scene(),
            scene,
            "the dismiss verb must never leave {scene:?}"
        );

        // The scene's own exit verb is the one that leaves.
        app.close_scene();
        assert_eq!(
            app.current_scene(),
            SceneKind::Thread,
            "`close_scene` leaves {scene:?}"
        );
    }
}

/// The Thread scene is the home: `close_scene` there is a spent gesture,
/// not a navigation (there is nowhere to go).
#[test]
fn close_scene_at_home_is_a_spent_gesture() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
    assert!(
        !app.close_scene(),
        "leaving the home scene reports no navigation"
    );
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
}

/// `C-x w` dismisses a foreground dialog before touching the scene —
/// the overlay is the visual foreground, so one press spends itself on it and
/// the scene survives. Regression: the chord used to treat any
/// dialog as "the scene", so the second press could land on a demoted view.
#[test]
fn close_scene_spends_itself_on_a_foreground_dialog_first() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::shell::surfaces::SceneKind::Dashboard);
    app.open_dialog(crate::shell::surfaces::DialogKind::Tools);
    assert!(app.active_dialog().is_some());

    // Press 1: the dialog is dismissed, the scene stays.
    app.dismiss_surface();
    assert!(app.active_dialog().is_none(), "the dialog is gone");
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Dashboard,
        "the dismiss stopped at the overlay"
    );

    // Press 2: now the scene itself leaves.
    app.close_scene();
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
}

#[test]
fn config_view_navigation_and_theme_preview() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Settings
    );
    assert_eq!(
        app.surfaces.config_focus,
        crate::shell::overlays::ConfigFocus::Categories
    );
    assert_eq!(app.surfaces.config_category, 0);

    // Cycling down through all 5 categories
    for expected_cat in [1, 2, 3, 4, 0] {
        crate::shell::event_loop::handle_modal_down(&mut app, "s1");
        assert_eq!(app.surfaces.config_category, expected_cat);
    }

    // Switch focus to Detail pane
    app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Detail;
    app.surfaces.config_detail_index = 0;

    let original_surface = app.surfaces.theme.surface();

    // Up/down in detail pane does NOT apply themes before Enter apply
    crate::shell::event_loop::handle_modal_down(&mut app, "s1");
    assert_eq!(app.surfaces.config_detail_index, 1);
    assert_eq!(
        app.surfaces.theme.surface(),
        original_surface,
        "theme must not change on navigation before Enter apply"
    );

    app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Categories;
    assert_eq!(
        app.surfaces.theme.surface(),
        crate::shell::render::Theme::from_color_scheme(
            &app.surfaces.color_scheme,
            &app.surfaces.custom_color_scheme
        )
        .surface()
    );

    // Leaving from Categories is the scene-exit verb — a dismiss on the bare
    // scene is a spent gesture and never navigates.
    assert!(
        !app.dismiss_surface(),
        "a dismiss has nothing to close on a bare scene"
    );
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Settings,
        "a dismiss never leaves the scene"
    );
    app.close_scene();
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
}

#[test]
fn config_view_monochrome_profile_navigation_and_item_counts() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces.profile = nuotc::TerminalProfile::dec_vt100_monochrome();
    app.surfaces.theme = crate::shell::render::Theme::monochrome();

    app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Settings
    );
    assert_eq!(
        app.surfaces.config_focus,
        crate::shell::overlays::ConfigFocus::Categories
    );
    assert_eq!(app.surfaces.config_category, 0);

    // Categories are always all 5, cleanly available on all profiles
    for expected_cat in [1, 2, 3, 4, 0] {
        crate::shell::event_loop::handle_modal_down(&mut app, "s1");
        assert_eq!(app.surfaces.config_category, expected_cat);
    }

    // On monochrome, Appearance detail item count is 1 (Monochrome Hardware Mode)
    app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Detail;
    app.surfaces.config_detail_index = 0;
    // Cycling down on single-item detail pane remains at index 0
    crate::shell::event_loop::handle_modal_down(&mut app, "s1");
    assert_eq!(app.surfaces.config_detail_index, 0);
}

#[tokio::test]
async fn config_view_components_toggle_and_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = nuo_host::paths::Dirs {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_host::paths::TestDirsGuard::install(dirs);

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
    // Navigate to category 1 (Components)
    app.surfaces.config_category = 1;
    app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Detail;

    // Rows are addressed through the panel's own ordering so this test cannot
    // silently drift when a component is added (ADR-0020).
    use crate::shell::views::settings::components::{ComponentRowId, item_count, row_for_index};
    let index_of = |pred: &dyn Fn(ComponentRowId) -> bool| {
        (0..item_count())
            .find(|i| row_for_index(*i).is_some_and(pred))
            .expect("row must exist in the Components panel")
    };
    let reasoning_row = index_of(&|row| row == ComponentRowId::Reasoning);
    let command_row = index_of(&|row| matches!(row, ComponentRowId::Tool(c) if c.id == "command"));
    let diff_row = index_of(&|row| matches!(row, ComponentRowId::Tool(c) if c.id == "diff"));
    let search_row = index_of(&|row| matches!(row, ComponentRowId::Tool(c) if c.id == "search"));
    let auto_scroll_row = index_of(&|row| row == ComponentRowId::AutoScroll);

    app.surfaces.config_detail_index = reasoning_row;
    assert!(!crate::shell::config::reasoning_default_expanded(
        &app.session.tui_config
    ));
    assert!(!app.transcript.reasoning_default_expanded);

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(crate::shell::config::reasoning_default_expanded(
        &app.session.tui_config
    ));
    assert!(app.transcript.reasoning_default_expanded);

    // The command component is collapsed by default, so activating expands it — and
    // the choice must land on every shell spelling, not just the canonical one.
    app.surfaces.config_detail_index = command_row;
    assert!(!crate::shell::config::tool_default_expanded(
        &app.session.tui_config,
        "execute_command"
    ));
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(crate::shell::config::tool_default_expanded(
        &app.session.tui_config,
        "execute_command"
    ));

    // The Diffs row covers edit_text and write_file together.
    app.surfaces.config_detail_index = diff_row;
    assert!(crate::shell::config::tool_default_expanded(
        &app.session.tui_config,
        "edit_text"
    ));
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(!crate::shell::config::tool_default_expanded(
        &app.session.tui_config,
        "edit_text"
    ));
    assert!(!crate::shell::config::tool_default_expanded(
        &app.session.tui_config,
        "write_file"
    ));

    // An untouched row keeps its declared default — toggling one component
    // must never move another.
    assert!(!crate::shell::config::tool_default_expanded(
        &app.session.tui_config,
        "search_text"
    ));
    app.surfaces.config_detail_index = search_row;
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(crate::shell::config::tool_default_expanded(
        &app.session.tui_config,
        "search_text"
    ));
    assert!(!crate::shell::config::tool_default_expanded(
        &app.session.tui_config,
        "edit_text"
    ));

    app.surfaces.config_detail_index = auto_scroll_row;
    assert!(!app.transcript.expand_auto_scroll);
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(app.transcript.expand_auto_scroll);
    assert!(app.session.tui_config.expand_auto_scroll);

    // Verify config file on disk has the persisted settings
    let loaded = crate::shell::config::TuiConfig::load();
    assert!(crate::shell::config::reasoning_default_expanded(&loaded));
    assert!(crate::shell::config::tool_default_expanded(
        &loaded,
        "execute_command"
    ));
    assert!(!crate::shell::config::tool_default_expanded(
        &loaded, "bash"
    ));
    assert!(!crate::shell::config::tool_default_expanded(
        &loaded,
        "edit_text"
    ));
    assert!(crate::shell::config::tool_default_expanded(
        &loaded,
        "search_text"
    ));
    assert!(loaded.expand_auto_scroll);
}
