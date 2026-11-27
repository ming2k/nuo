use super::*;

#[test]
fn picker_connections_count_matches_provider_rows_no_add_row() {
    // Adding a connection is a footer shortcut (`a`) now, not a synthetic list
    // row, so `picker_row_count()` for Connections equals the provider count
    // exactly (no +1).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::Connections);
    // Seed a few snapshot rows so providers_filtered() renders the full list
    // (the picker is snapshot-driven).
    let row = |id: &str| nuo_protocol::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        provider: String::new(),
        headers: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.surfaces.provider_picker = nuo_protocol::ProviderPickerSnapshot {
        default_id: "kimi-code".to_string(),
        rows: vec![row("kimi-code"), row("openai"), row("anthropic")],
    };
    let providers = app.providers_filtered().len();
    assert!(providers > 0, "snapshot seeds the full provider list");
    assert_eq!(app.picker_row_count(), providers);
}

/// Confirming the overlay dispatches exactly one `DeleteConnection` request and
/// tears the overlay down, so a stray second confirm cannot re-delete.
#[test]
fn confirm_provider_delete_dispatches_once_and_clears() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // Simulate the overlay already being open (staged from a prior Shift+D).
    app.surfaces.pending_provider_delete = Some("doomed".to_string());

    let req = app
        .confirm_provider_delete()
        .expect("confirm dispatches when an id is staged");
    assert!(
        matches!(req, AgentRequest::DeleteConnection { ref name } if name == "doomed"),
        "confirm dispatches a DeleteConnection request for the staged name"
    );
    // Overlay torn down: no staged id remains.
    assert!(
        app.surfaces.pending_provider_delete.is_none(),
        "confirm clears the staged id"
    );
    // A second confirm is a no-op (nothing left to delete).
    assert!(
        app.confirm_provider_delete().is_none(),
        "second confirm is inert after the overlay closes"
    );
}

/// Cancelling the overlay drops the staged id and resets focus to the safe
/// default (Cancel), so reopening the overlay later starts fresh.
#[test]
fn cancel_provider_delete_clears_and_resets_focus() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces.pending_provider_delete = Some("doomed".to_string());
    app.surfaces.provider_delete_focus = crate::shell::ProviderDeleteChoice::Delete;

    app.cancel_provider_delete();

    assert!(
        app.surfaces.pending_provider_delete.is_none(),
        "cancel clears the staged id"
    );
    assert_eq!(
        app.surfaces.provider_delete_focus,
        crate::shell::ProviderDeleteChoice::Cancel,
        "cancel resets focus to the safe default"
    );
}
