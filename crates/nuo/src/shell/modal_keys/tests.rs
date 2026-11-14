use super::*;
use crate::shell::sheet::SheetKeys;

fn keys(tune: impl FnOnce(&mut ModalKeys)) -> ModalKeys {
    let mut k = ModalKeys::default();
    tune(&mut k);
    k
}

fn sheet_keys(tune: impl FnOnce(&mut SheetKeys)) -> SheetKeys {
    let mut k = SheetKeys::default();
    tune(&mut k);
    k
}

fn key(c: char) -> crate::shell::keymap::Key {
    crate::shell::keymap::Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char(c),
    }
}

/// Test shims: the prompt-editing buffers are only observed by the Host
/// inline prompt; every other surface ignores them.
fn resolve_dialog(
    dialog: DialogKind,
    k: crate::shell::keymap::Key,
    keys: &ModalKeys,
) -> Option<InputAction> {
    resolve_modal_key(
        Some(OverlaySurface::Dialog(dialog)),
        SceneKind::Thread,
        k,
        keys,
        &mut String::new(),
        &mut 0,
    )
}

fn resolve_sheet(
    sheet: SheetKind,
    k: crate::shell::keymap::Key,
    keys: &ModalKeys,
) -> Option<InputAction> {
    resolve_modal_key(
        Some(OverlaySurface::Sheet(sheet)),
        SceneKind::Thread,
        k,
        keys,
        &mut String::new(),
        &mut 0,
    )
}

fn resolve_scene(
    scene: SceneKind,
    k: crate::shell::keymap::Key,
    keys: &ModalKeys,
) -> Option<InputAction> {
    resolve_modal_key(None, scene, k, keys, &mut String::new(), &mut 0)
}

#[test]
fn mcp_owns_space_and_r() {
    let c = keys(|_| {});
    assert_eq!(
        resolve_dialog(DialogKind::Mcp, key(' '), &c),
        Some(InputAction::McpToggle)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Mcp, key('r'), &c),
        Some(InputAction::McpReconnect)
    );
    assert_eq!(resolve_dialog(DialogKind::Mcp, key('z'), &c), None);
}

#[test]
fn queue_owns_delete_and_reorder() {
    let c = keys(|_| {});
    assert_eq!(
        resolve_dialog(DialogKind::Queue, key('D'), &c),
        Some(InputAction::QueueDelete)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Queue, key('K'), &c),
        Some(InputAction::QueueMoveItem { delta: -1 })
    );
    assert_eq!(
        resolve_dialog(DialogKind::Queue, key('J'), &c),
        Some(InputAction::QueueMoveItem { delta: 1 })
    );
}

#[test]
fn picker_search_layer_surrenders_query_chars() {
    // In the search sub-layer every printable char is a query — the modal
    // owns nothing and the shared layer inserts it.
    let c = keys(|k| k.model_searching = true);
    assert_eq!(resolve_dialog(DialogKind::Models, key('/'), &c), None);
    assert_eq!(resolve_dialog(DialogKind::Models, key('*'), &c), None);
    // Browse mode owns the verbs.
    let c = keys(|_| {});
    assert_eq!(
        resolve_dialog(DialogKind::Models, key('*'), &c),
        Some(InputAction::ProviderPickerToggleFavorite)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Models, key('/'), &c),
        Some(InputAction::ModelEnterSearch)
    );
}

#[test]
fn connections_detail_keys_browse_models_and_refresh() {
    let c = keys(|k| k.connection_info_detail = true);
    assert_eq!(resolve_dialog(DialogKind::Connections, key('a'), &c), None);
    assert_eq!(resolve_dialog(DialogKind::Connections, key('c'), &c), None);
    assert_eq!(resolve_dialog(DialogKind::Connections, key('D'), &c), None);
    assert_eq!(
        resolve_dialog(DialogKind::Connections, key('m'), &c),
        Some(InputAction::BrowseConnectionModels)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Connections, key('r'), &c),
        Some(InputAction::RefreshProviderModels)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Connections, key('e'), &c),
        Some(InputAction::OpenModelEditor)
    );
    assert_eq!(
        resolve_dialog(
            DialogKind::Connections,
            crate::shell::keymap::Key::ENTER,
            &c
        ),
        Some(InputAction::BrowseConnectionModels)
    );
}

#[test]
fn models_detail_keys_open_editor_refresh_favorite_and_activate() {
    let c = keys(|k| k.model_info_detail = true);
    assert_eq!(
        resolve_dialog(DialogKind::Models, key('e'), &c),
        Some(InputAction::OpenModelEditor)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Models, key('r'), &c),
        Some(InputAction::RefreshProviderModels)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Models, key('*'), &c),
        Some(InputAction::ProviderPickerToggleFavorite)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Models, crate::shell::keymap::Key::ENTER, &c),
        Some(InputAction::ProviderPickerActivate)
    );
    // Space, digits, and horizontal arrows are non-mutating in read-only info view
    assert_eq!(resolve_dialog(DialogKind::Models, key(' '), &c), None);
    assert_eq!(resolve_dialog(DialogKind::Models, key('1'), &c), None);
    assert_eq!(
        resolve_dialog(DialogKind::Models, crate::shell::keymap::Key::LEFT, &c),
        None
    );
    assert_eq!(
        resolve_dialog(DialogKind::Models, crate::shell::keymap::Key::RIGHT, &c),
        None
    );
}

#[test]
fn tools_modal_enter_and_space() {
    let list_keys = keys(|_| {});
    assert_eq!(
        resolve_dialog(
            DialogKind::Tools,
            crate::shell::keymap::Key::ENTER,
            &list_keys
        ),
        Some(InputAction::OpenToolDetail)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Tools, key(' '), &list_keys),
        Some(InputAction::SessionActivate)
    );

    let detail_keys = keys(|k| k.tools_info_detail = true);
    assert_eq!(
        resolve_dialog(
            DialogKind::Tools,
            crate::shell::keymap::Key::ENTER,
            &detail_keys
        ),
        None
    );
    assert_eq!(
        resolve_dialog(DialogKind::Tools, key(' '), &detail_keys),
        Some(InputAction::SessionActivate)
    );
}

#[test]
fn dashboard_chars_are_always_actions() {
    let c = keys(|_| {});
    assert_eq!(
        resolve_scene(SceneKind::Dashboard, key('a'), &c),
        Some(InputAction::HostSwitchSelected)
    );
    assert_eq!(
        resolve_scene(SceneKind::Dashboard, key('i'), &c),
        Some(InputAction::HostInterruptSelected)
    );
    assert_eq!(
        resolve_scene(SceneKind::Dashboard, key('q'), &c),
        Some(InputAction::HostPromptSeed('q')),
        "`q` is an ordinary console char — the scene has no `q` exit"
    );
    assert_eq!(
        resolve_scene(SceneKind::Dashboard, key('z'), &c),
        Some(InputAction::HostPromptSeed('z'))
    );
}

#[test]
fn question_space_digit_and_text() {
    use crate::shell::sheet::{SheetKind as LegacySheetKind, resolve_sheet_key};
    let c = sheet_keys(|_| {});
    assert_eq!(
        resolve_sheet_key(LegacySheetKind::Question, key(' '), &c),
        Some(InputAction::QuestionToggle)
    );
    assert_eq!(
        resolve_sheet_key(LegacySheetKind::Question, key('3'), &c),
        Some(InputAction::QuestionSelect(3))
    );
    // With the "Other" field highlighted, space types into it.
    let c = sheet_keys(|k| k.question_other_highlighted = true);
    assert_eq!(
        resolve_sheet_key(LegacySheetKind::Question, key(' '), &c),
        Some(InputAction::QuestionInsertChar(' '))
    );
}

#[test]
fn model_editor_space_and_digits() {
    let c = keys(|k| k.editor_field = Some(2));
    assert_eq!(
        resolve_sheet(SheetKind::ModelEditor, key(' '), &c),
        Some(InputAction::ModelEditorThinkingToggle)
    );
    let c = keys(|k| k.editor_field = Some(1));
    assert_eq!(
        resolve_sheet(SheetKind::ModelEditor, key('5'), &c),
        Some(InputAction::ModelEditorEffortJump { index: 4 })
    );
    // A letter on the API-key field is a query char for the shared layer.
    assert_eq!(resolve_sheet(SheetKind::ModelEditor, key('x'), &c), None);
}

#[test]
fn non_printable_and_unowned_modals_fall_through() {
    let c = keys(|_| {});
    let esc = crate::shell::keymap::Key::ESC;
    assert_eq!(resolve_dialog(DialogKind::Mcp, esc, &c), None);
    let c = keys(|_| {});
    assert_eq!(
        resolve_dialog(DialogKind::HistorySearch, key('q'), &c),
        None
    );
    // InputInjection is a pure text surface: every key edits via the
    // shared layer, so the sheet scheme owns nothing.
    use crate::shell::sheet::{SheetKind as LegacySheetKind, resolve_sheet_key};
    let c = sheet_keys(|_| {});
    assert_eq!(
        resolve_sheet_key(LegacySheetKind::InputInjection, key('q'), &c),
        None
    );
}

#[test]
fn history_modal_owns_insert_and_close_family() {
    use crate::shell::keymap::Key;
    let c = keys(|_| {});
    assert_eq!(
        resolve_dialog(DialogKind::HistorySearch, Key::ESC, &c),
        Some(InputAction::CloseModal)
    );
    assert_eq!(
        resolve_dialog(DialogKind::HistorySearch, Key::ENTER, &c),
        Some(InputAction::HistoryInsert)
    );
    assert_eq!(
        resolve_dialog(DialogKind::HistorySearch, Key::TAB, &c),
        Some(InputAction::HistoryInsert)
    );
    assert_eq!(
        resolve_dialog(DialogKind::HistorySearch, Key::UP, &c),
        Some(InputAction::ModalUp)
    );
    assert_eq!(
        resolve_dialog(DialogKind::HistorySearch, Key::DOWN, &c),
        Some(InputAction::ModalDown)
    );
    // Query chars are not history verbs — they edit via the shared layer.
    assert_eq!(
        resolve_dialog(DialogKind::HistorySearch, key('q'), &c),
        None
    );
}

#[test]
fn history_hints_are_all_resolvable() {
    let c = keys(|_| {});
    for h in live_history_hints() {
        assert!(
            resolve_dialog(DialogKind::HistorySearch, h.key, &c).is_some(),
            "advertised history chord {h:?} is not handled"
        );
    }
}

#[test]
fn palette_owns_filter_and_delete_family() {
    use crate::shell::keymap::Key;
    let c = keys(|_| {});
    assert_eq!(
        resolve_dialog(DialogKind::Switcher, key('q'), &c),
        Some(InputAction::ViewSwitcherFilter { ch: 'q' })
    );
    let backspace = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Backspace,
    };
    assert_eq!(
        resolve_dialog(DialogKind::Switcher, backspace, &c),
        Some(InputAction::ViewSwitcherBackspace)
    );
    let delete = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Delete,
    };
    assert_eq!(
        resolve_dialog(DialogKind::Switcher, delete, &c),
        Some(InputAction::ViewCloseSelected)
    );
    assert_eq!(
        resolve_dialog(DialogKind::Switcher, Key::ENTER, &c),
        Some(InputAction::ViewSwitchActivate)
    );
    // ↑/↓ list walking and Esc-close stay in the shared affordance layer.
    assert_eq!(resolve_dialog(DialogKind::Switcher, Key::UP, &c), None);
    assert_eq!(resolve_dialog(DialogKind::Switcher, Key::ESC, &c), None);
}

#[test]
fn brackets_navigate_scene_history_in_dashboard_and_settings() {
    use crate::shell::keymap::Key;
    let c = keys(|_| {});
    let bracket_left = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char('['),
    };
    let bracket_right = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char(']'),
    };

    // Un-modified bracket keys do not hijack scene history (ADR-0092 [INV-NAV-03]):
    // In Dashboard, printable chars seed host prompt:
    assert_eq!(
        resolve_scene(SceneKind::Dashboard, bracket_left, &c),
        Some(InputAction::HostPromptSeed('['))
    );
    assert_eq!(
        resolve_scene(SceneKind::Dashboard, bracket_right, &c),
        Some(InputAction::HostPromptSeed(']'))
    );
    // In Settings, bracket keys fall through:
    assert_eq!(resolve_scene(SceneKind::Settings, bracket_left, &c), None);
    assert_eq!(resolve_scene(SceneKind::Settings, bracket_right, &c), None);
}
