use super::*;

#[test]
fn models_dialog_arrow_navigation_skips_unusable_models() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let info = |model: &str, usable: bool| nuo_protocol::ProviderModelInfo {
        model: model.to_string(),
        name: None,
        protocol: String::new(),
        effort: None,
        thinking: None,
        effort_levels: Vec::new(),
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: if usable {
            Some(nuo_protocol::Availability::usable())
        } else {
            Some(nuo_protocol::Availability::locked(Some(
                "Requires subscription or account".to_string(),
            )))
        },
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    };
    app.surfaces.provider_picker = nuo_protocol::ProviderPickerSnapshot {
        default_id: "p1".into(),
        rows: vec![nuo_protocol::ProviderPickerRow {
            id: "p1".into(),
            name: "P1".into(),
            model: "m1".into(),
            models: vec!["m1".into(), "m2".into(), "m3".into()],
            model_info: vec![
                info("m1", true),
                info("m2", false), // locked
                info("m3", true),
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    app.session.current_provider = "p1".to_string();
    app.session.current_model = "m1".to_string();

    assert!(app.open_dialog(crate::shell::surfaces::DialogKind::Models));
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .index,
        0,
        "initially points at first usable model"
    );

    // ModalDown: should skip index 1 (locked) and jump directly to index 2
    if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
        let action = crate::shell::input::InputAction::ModalDown;
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .index,
        2,
        "arrow down must skip locked model index 1 and land on index 2"
    );

    // ModalDown again: wraps around to index 0, skipping locked index 1
    if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
        let action = crate::shell::input::InputAction::ModalDown;
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .index,
        0,
        "arrow down wrap must skip locked model and land on index 0"
    );

    // ModalUp: wraps backward to index 2, skipping locked index 1
    if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
        let action = crate::shell::input::InputAction::ModalUp;
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .index,
        2,
        "arrow up wrap must skip locked model and land on index 2"
    );

    // ModalUp again: moves to index 0, skipping locked index 1
    if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
        let action = crate::shell::input::InputAction::ModalUp;
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .index,
        0,
        "arrow up must skip locked model and land on index 0"
    );
}

#[test]
fn models_dialog_click_on_unusable_model_is_ignored() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let info = |model: &str, usable: bool| nuo_protocol::ProviderModelInfo {
        model: model.to_string(),
        name: None,
        protocol: String::new(),
        effort: None,
        thinking: None,
        effort_levels: Vec::new(),
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: if usable {
            Some(nuo_protocol::Availability::usable())
        } else {
            Some(nuo_protocol::Availability::locked(Some(
                "Requires subscription".to_string(),
            )))
        },
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    };
    app.surfaces.provider_picker = nuo_protocol::ProviderPickerSnapshot {
        default_id: "p1".into(),
        rows: vec![nuo_protocol::ProviderPickerRow {
            id: "p1".into(),
            name: "P1".into(),
            model: "m1".into(),
            models: vec!["m1".into(), "m2".into()],
            model_info: vec![
                info("m1", true),
                info("m2", false), // locked at index 1
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    app.session.current_provider = "p1".to_string();
    app.session.current_model = "m1".to_string();

    assert!(app.open_dialog(crate::shell::surfaces::DialogKind::Models));
    app.transcript.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
    app.transcript.ui.mount(
        crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::Models,
        )),
        nuotc::Rect::new(10, 5, 60, 15),
    );
    app.transcript.ui.commit();

    let models_dlg = app
        .surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
    models_dlg.index = 0;
    models_dlg.scroll = 0;

    // Body starts at y = 5 + 3 = 8.
    // Line 0 is section header "All models", Line 1 is model row 0 (usable), Line 2 is model row 1 (locked).
    if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
        // Click on Line 2 (locked row m2) at y = 10
        let action = crate::shell::input::InputAction::SelectionStart { x: 20, y: 10 };
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.surfaces.put_active_view(ent);
    }
    // Index must NOT have changed to 1
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .index,
        0,
        "clicking on an unusable model must NOT change selection cursor"
    );
}

#[test]
fn models_dialog_reset_picker_nav_lands_on_first_usable_model() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let info = |model: &str, usable: bool| nuo_protocol::ProviderModelInfo {
        model: model.to_string(),
        name: None,
        protocol: String::new(),
        effort: None,
        thinking: None,
        effort_levels: Vec::new(),
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: if usable {
            Some(nuo_protocol::Availability::usable())
        } else {
            Some(nuo_protocol::Availability::locked(None))
        },
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    };
    app.surfaces.provider_picker = nuo_protocol::ProviderPickerSnapshot {
        default_id: "p1".into(),
        rows: vec![nuo_protocol::ProviderPickerRow {
            id: "p1".into(),
            name: "P1".into(),
            model: "m1".into(),
            models: vec!["m0".into(), "m1".into()],
            model_info: vec![
                info("m0", false), // locked at index 0
                info("m1", true),  // usable at index 1
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    assert!(app.open_dialog(crate::shell::surfaces::DialogKind::Models));
    app.reset_picker_nav();
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .index,
        1,
        "reset_picker_nav must land on first usable model (index 1), not locked index 0"
    );
}

#[test]
fn models_dialog_two_tier_drill_in_and_pop_sublayer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let info = |model: &str, usable: bool| nuo_protocol::ProviderModelInfo {
        model: model.to_string(),
        name: Some(format!("Model {model}")),
        protocol: String::new(),
        effort: Some("medium".to_string()),
        thinking: Some(true),
        effort_levels: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: if usable {
            Some(nuo_protocol::Availability::usable())
        } else {
            Some(nuo_protocol::Availability::locked(Some(
                "Requires subscription or account entitlement".to_string(),
            )))
        },
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    };
    app.surfaces.provider_picker = nuo_protocol::ProviderPickerSnapshot {
        default_id: "p1".into(),
        rows: vec![nuo_protocol::ProviderPickerRow {
            id: "p1".into(),
            name: "P1".into(),
            model: "m1".into(),
            models: vec!["m1".into(), "m2".into()],
            model_info: vec![
                info("m1", true),  // usable at index 0
                info("m2", false), // locked at index 1
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };

    assert!(app.open_dialog(crate::shell::surfaces::DialogKind::Models));
    assert!(
        !app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .info_detail
    );

    // 1. Drill into usable model m1
    crate::shell::event_loop::actions::handle_open_model_detail(&mut app);
    assert!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .info_detail
    );

    // 2. Open model editor sheet via `e` from detail view (clean separation: inspector is read-only)
    crate::shell::event_loop::actions::handle_open_model_editor(&mut app);
    assert!(
        app.surfaces
            .surfaces
            .contains_sheet(crate::shell::surfaces::SheetKind::ModelEditor)
    );
    assert_eq!(app.provider_editor.editor_effort, "medium");
    assert!(app.provider_editor.editor_thinking);

    // Adjust effort live inside the editor
    let levels: &[String] = &app.provider_editor.editor_effort_levels;
    let cur = levels
        .iter()
        .position(|l| l == &app.provider_editor.editor_effort)
        .unwrap() as isize;
    let next = ((cur + 1).rem_euclid(levels.len() as isize)) as usize;
    app.provider_editor.editor_effort = levels[next].clone();
    assert_eq!(app.provider_editor.editor_effort, "high");

    // Pop the editor sheet; returns cleanly to the underlying detail sub-view
    app.pop_transient_surface();
    assert!(
        !app.surfaces
            .surfaces
            .contains_sheet(crate::shell::surfaces::SheetKind::ModelEditor)
    );
    assert!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .info_detail
    );

    // 3. Esc pops detail sublayer back to Level 1 list
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .info_detail
    );

    // 4. Enter on locked model opens detail view rather than being a silent dead end
    let models_dlg = app
        .surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
    models_dlg.index = 1; // point at m2 (locked)
    let rows = app.models_flat_filtered();
    let picked = rows.get(1);
    assert!(picked.is_some_and(|r| !r.usable));
    crate::shell::event_loop::actions::handle_open_model_detail(&mut app);
    assert!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .info_detail
    );

    // Pop back to list
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .info_detail
    );
}

#[test]
fn browse_connection_models_transitions_to_models_dialog() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces.connection_detail = Some(nuo_protocol::ConnectionDetail {
        name: "google-antigravity".to_string(),
        provider: "google".to_string(),
        provider_label: "Google Antigravity".to_string(),
        models: vec!["gemini-1.5-pro".to_string()],
        ..Default::default()
    });

    assert!(app.open_dialog(crate::shell::surfaces::DialogKind::Connections));
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_detail = true;

    // Simulate BrowseConnectionModels action
    let filter = app
        .surfaces
        .connection_detail
        .as_ref()
        .map(|d| d.provider_label.clone())
        .unwrap();
    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    let models_dlg = app
        .surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
    models_dlg.info_detail = false;
    models_dlg.search = true;
    models_dlg.query.text = filter.clone();
    models_dlg.query.cursor = filter.len();

    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Models)
    );
    assert_eq!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .query
            .text,
        "Google Antigravity"
    );
    assert!(
        app.surfaces
            .surfaces
            .dlg::<crate::shell::surfaces::ModelsDialog>()
            .search
    );
}

#[tokio::test]
async fn models_dialog_query_edit_reanchors_cursor_to_top_match() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(app.open_dialog(crate::shell::surfaces::DialogKind::Models));
    let models_dlg = app
        .surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
    models_dlg.search = true;
    models_dlg.index = 4;
    models_dlg.scroll = 2;

    // Simulate typing a character 'g' via process_event_for_test
    let event = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('g'),
        crossterm::event::KeyModifiers::NONE,
    ));
    crate::shell::event_loop::process_event_for_test(&mut app, &event, "").await;

    let dlg = app
        .surfaces
        .surfaces
        .dlg::<crate::shell::surfaces::ModelsDialog>();
    assert_eq!(
        dlg.index, 0,
        "query edit must re-anchor cursor to top match"
    );
    assert_eq!(dlg.scroll, 0, "query edit must reset scroll to top");
    assert!(dlg.follow, "query edit must arm follow mode");
}

// ---------------------------------------------------------------------------
// ADR-0123 — attach refusal typing, stale-tab retirement, identity-addressed
// session rebasing.
// ---------------------------------------------------------------------------
