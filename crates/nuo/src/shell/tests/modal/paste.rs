use super::*;

#[test]
fn modal_paste_splices_text_inline_stripping_newlines() {
    // Pasting into a free-text modal field (here the provider editor's
    // API-key field) splices the text at the cursor and collapses newlines
    // so a copied multi-line block pastes as one continuous single line,
    // matching the single-line semantics the modal already enforces. No
    // chip is inserted and no attachment is staged.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::shell::surfaces::SheetKind::ModelEditor);
    app.editor_field = 0;
    app.input = "sk-".to_string();
    app.cursor_position = app.input.chars().count();

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::shell::clipboard::ClipboardRead::Text("abc\ndef\n".to_string()),
    );

    assert_eq!(app.input, "sk-abcdef");
    assert_eq!(app.cursor_position, "sk-abcdef".chars().count());
    assert!(
        app.pending_text_pastes.is_empty(),
        "no chip staging in modals"
    );
    assert!(
        !app.input.contains("Pasted text"),
        "no chip label in modals"
    );
}

#[test]
fn modal_paste_inserts_at_cursor_not_at_end() {
    // The splice honors the cursor position, so a paste in the middle of
    // an existing field inserts between the surrounding characters rather
    // than appending.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::shell::surfaces::SheetKind::ModelEditor);
    app.editor_field = 1;
    app.input = "gpt-4omini".to_string();
    app.cursor_position = "gpt-4o".chars().count();

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::shell::clipboard::ClipboardRead::Text("turbo".to_string()),
    );

    assert_eq!(app.input, "gpt-4oturbomini");
    assert_eq!(
        app.cursor_position,
        "gpt-4oturbo".chars().count(),
        "cursor lands just past the inserted text"
    );
}

#[test]
fn modal_paste_applies_to_provider_picker_and_history_search() {
    // The inline paste path is shared by every free-text modal that borrows
    // the input line, so the model picker filter and the history search
    // query paste the same way as the editor.
    for dialog in [
        crate::shell::surfaces::DialogKind::Models,
        crate::shell::surfaces::DialogKind::HistorySearch,
    ] {
        let (mut app, _tmp) = app_in_tempdir(&[], &[]);
        app.open_dialog(dialog);
        app.input = String::new();
        app.cursor_position = 0;

        clipboard_ops::apply_clipboard_paste(
            &mut app,
            crate::shell::clipboard::ClipboardRead::Text("query".to_string()),
        );

        assert_eq!(
            app.input, "query",
            "paste should inline into free-text modal"
        );
        assert_eq!(app.cursor_position, "query".chars().count());
        assert!(app.pending_text_pastes.is_empty());
    }
}

#[test]
fn modal_paste_drops_image_with_failure_toast() {
    // An image paste has nowhere to go in a single-line modal field, so it
    // is dropped with a failure toast rather than silently lost or staged
    // as an attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::shell::surfaces::SheetKind::ModelEditor);
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::shell::clipboard::ClipboardRead::Image {
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime: "image/png".to_string(),
        },
    );

    assert!(app.input.is_empty(), "image paste must not insert text");
    assert!(
        app.pending_images.is_empty(),
        "no attachment staging in modals"
    );
    assert!(
        app.copy_toast_failed,
        "image paste in a modal should toast a failure"
    );
    assert!(app.copy_toast_until.is_some());
}

#[test]
fn picker_caret_owner_exists_only_in_search_mode() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for dialog in [
        crate::shell::surfaces::DialogKind::Models,
        crate::shell::surfaces::DialogKind::Connections,
    ] {
        app.open_dialog(dialog);
        app.set_picker_search(false);
        assert_eq!(
            app.caret_owner(),
            CaretOwner::None,
            "{dialog:?} browse mode has no editable field"
        );
        app.set_picker_search(true);
        assert_eq!(
            app.caret_owner(),
            CaretOwner::Overlay,
            "{dialog:?} search mode owns the visible query field"
        );
    }
}

#[test]
fn overlay_caret_owner_is_arbitrated_by_the_layer_stack() {
    // Caret ownership is never declared statically by a surface type. It is
    // arbitrated once per frame by `App::caret_owner()` from the mounted layer
    // stack, so every state-dependent case — a picker that is only
    // editable while its search row is open, the Question sheet's "Other"
    // field, the HistorySearch panel that borrows the live composer line —
    // resolves through the same function instead of a per-surface copy. See
    // `caret_owner_question_owns_caret_only_on_other` and
    // `picker_caret_owner_exists_only_in_search_mode` for those cases.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::shell::surfaces::SheetKind::CustomProvider);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::Overlay,
        "CustomProvider owns the caret"
    );
    app.surfaces.dismiss_all_overlays();
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
}
