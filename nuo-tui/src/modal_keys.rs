//! Per-modal keybinding schemes (ADR-0172).
//!
//! Each modal owns the single-letter *verb* keys that act on its rows and
//! sub-layers — `space`/`r` in the MCP manager, `d`/`n`/`i` in the sessions
//! picker, the dashboard's `a`/`i`/`k`/`s`/`p`/`n` console verbs, and so on —
//! plus its Enter/arrow/Tab verb family, moved here from the router's
//! fallback arms so each surface owns its full key vocabulary. Generic
//! cross-modal affordances (Esc close, readline editing, paste, scrolling)
//! stay in the shared layer of the router.
//!
//! `resolve_modal_key` returns `Some(action)` when the modal consumes the key
//! and `None` to fall through to the shared affordance library / text
//! insertion. It is consulted by `crate::input::route_event` whenever a
//! modal is active, before the shared arms.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::input::readline::{
    char_index_at_byte, delete_next_grapheme, delete_previous_grapheme, next_grapheme_char_index,
    normalized_cursor_byte, previous_grapheme_char_index,
};
use crate::input::{InputAction, OauthCopyTarget};
use crate::keymap::LiveHint;

/// The modal handlers' own sub-state (ADR-0197 M2): which modal sub-layer is
/// live, which field is focused, which pane owns focus. Built once per event
/// by the caller; every read below is modal-local, so nothing else leaks in.
#[derive(Debug, Default, Clone)]
pub struct ModalKeys {
    /// Whether the model picker's search sub-layer is active. Only meaningful
    /// while the foreground modal is `crate::Modal::Models` or
    /// `crate::Modal::Connections`: `false` is browse mode (typing is inert,
    /// `/` enters search, `*`/`e`/`d`/`D` act on the row), `true` borrows the
    /// composer line as the live fuzzy query. Mirrors `App::model_search`.
    pub model_searching: bool,
    /// Whether the history modal's search sub-layer is active. Only meaningful
    /// while the foreground modal is `crate::Modal::HistorySearch`: `false`
    /// is browse mode (typing is inert, `/` enters search), `true` borrows the
    /// composer line as the live fuzzy query. Mirrors `App::history_search`.
    pub history_searching: bool,
    /// Focused text-field index of the provider editor, or `None` when the
    /// modal is closed or an inline selector is focused.
    pub custom_provider_field: Option<u8>,
    /// Focused field of the key editor (`Modal::ModelEditor`): `0` = API key,
    /// `1` = effort selector, `2` = thinking toggle. `None` when that modal is
    /// not open. Drives ←/→ effort cycling (field 1) and Space thinking toggle
    /// (field 2). Mirrors `App::editor_field` while the key editor is open.
    pub editor_field: Option<u8>,
    /// Which pane of the Settings View currently owns focus. Mirrors `App::config_focus`.
    pub config_focus: crate::overlays::ConfigFocus,
    /// While the sessions picker is drilled into its info sub-view (`i`), the
    /// list-only keys (delete `d`, new `n`, info `i`) are inert — the sub-view
    /// is a read-only read-out.
    pub session_info_detail: bool,
    /// While the connections picker is drilled into its detail sub-view (Enter),
    /// the list-only keys (delete `D`, preset `a`, custom `c`) are inert — the sub-view
    /// is a read-only read-out.
    pub connection_info_detail: bool,
    /// Whether the `/host` dashboard's inline prompt is open (`p` prompt or
    /// `n` new session). While true, printable keys edit the prompt text and
    /// Enter submits it. Mirrors `App::host_prompting`.
    pub host_prompting: bool,
    /// Whether the active dialog is displaying its localized key reference sub-view.
    pub dialog_keys: bool,
}

use crate::surfaces::{DialogKind, OverlaySurface, SceneKind, SheetKind};

fn has_body_scroll(overlay: Option<OverlaySurface>, scene: SceneKind) -> bool {
    if let Some(overlay) = overlay {
        matches!(
            overlay,
            OverlaySurface::Dialog(_)
                | OverlaySurface::Sheet(
                    SheetKind::OAuthPending | SheetKind::ProviderPreset | SheetKind::CustomProvider,
                )
        )
    } else {
        matches!(scene, SceneKind::Dashboard | SceneKind::Settings)
    }
}

/// Whether the overlay or scene currently treats the composer line as an editable free-text field.
/// Only the chat-like scenes (`Conversation`/`TaskInspection`/`Aside`) own the composer when no
/// overlay is up; the full-screen `Dashboard`/`Settings` scenes never borrow it.
pub fn modal_claims_composer_line(
    overlay: Option<OverlaySurface>,
    scene: SceneKind,
    keys: &ModalKeys,
) -> bool {
    match overlay {
        None => matches!(
            scene,
            SceneKind::Conversation | SceneKind::TaskInspection | SceneKind::Aside
        ),
        Some(OverlaySurface::Sheet(SheetKind::ModelEditor)) => true,
        Some(OverlaySurface::Dialog(DialogKind::Models | DialogKind::Connections)) => {
            keys.model_searching
        }
        Some(OverlaySurface::Dialog(DialogKind::HistorySearch)) => keys.history_searching,
        Some(OverlaySurface::Sheet(SheetKind::CustomProvider)) => {
            keys.custom_provider_field.is_some()
        }
        _ => false,
    }
}

/// Whether the ModelEditor's toggle fields currently swallow printable characters.
pub fn modal_swallows_printable(overlay: Option<OverlaySurface>, keys: &ModalKeys) -> bool {
    overlay == Some(OverlaySurface::Sheet(SheetKind::ModelEditor))
        && matches!(keys.editor_field, Some(2..=4))
}

/// Resolve a key an overlay or scene owns.
pub(crate) fn resolve_modal_key(
    overlay: Option<OverlaySurface>,
    scene: SceneKind,
    key: crate::keymap::Key,
    keys: &ModalKeys,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    if let Some(overlay) = overlay {
        match overlay {
            OverlaySurface::Dialog(DialogKind::HistorySearch) => {
                return resolve_history_search_key(key);
            }
            OverlaySurface::Dialog(DialogKind::Switcher) => return resolve_view_switcher_key(key),
            _ => {}
        }
    } else if scene == SceneKind::Dashboard && keys.host_prompting {
        return Some(resolve_host_prompt_key(key, input, cursor_position));
    }

    if keys.dialog_keys && matches!(overlay, Some(OverlaySurface::Dialog(_))) {
        return Some(resolve_dialog_keys_key(key));
    }

    match key.code {
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
            return Some(if let Some(overlay) = overlay {
                match overlay {
                    OverlaySurface::Dialog(DialogKind::Models) => {
                        InputAction::ProviderPickerActivate
                    }
                    OverlaySurface::Dialog(DialogKind::Connections)
                        if keys.connection_info_detail =>
                    {
                        InputAction::ToggleConnectionModelsExpanded
                    }
                    OverlaySurface::Dialog(DialogKind::Connections) => {
                        InputAction::OpenConnectionDetail
                    }
                    OverlaySurface::Sheet(SheetKind::ModelEditor) => InputAction::SubmitModelEditor,
                    OverlaySurface::Sheet(SheetKind::ProviderPreset) => InputAction::SelectPreset,
                    OverlaySurface::Sheet(SheetKind::OAuthPending) => {
                        InputAction::CopyOauthContent {
                            target: OauthCopyTarget::Selected,
                        }
                    }
                    OverlaySurface::Sheet(SheetKind::CustomProvider) => {
                        InputAction::SubmitCustomProvider
                    }
                    OverlaySurface::Dialog(DialogKind::Sessions) if keys.session_info_detail => {
                        return None;
                    }
                    OverlaySurface::Dialog(DialogKind::Sessions) => {
                        InputAction::OpenSelectedSession
                    }
                    OverlaySurface::Dialog(
                        DialogKind::Tools
                        | DialogKind::Mcp
                        | DialogKind::Permissions
                        | DialogKind::SessionTree
                        | DialogKind::UsageStats,
                    ) => InputAction::CloseModal,
                    OverlaySurface::Dialog(DialogKind::Skills) => InputAction::SkillsToggleDetail,
                    OverlaySurface::Dialog(DialogKind::Queue) => InputAction::RecallQueuedSelected,
                    OverlaySurface::Dialog(DialogKind::Asides) => InputAction::BtwFocusSelected,
                    OverlaySurface::Dialog(DialogKind::Telemetry) => InputAction::TelemetryActivate,
                    _ => return None,
                }
            } else {
                match scene {
                    SceneKind::Dashboard => InputAction::HostPreviewSelected,
                    SceneKind::Settings => InputAction::ConfigActivate,
                    _ => return None,
                }
            });
        }
        KeyCode::Up
            if !(key.modifiers.contains(KeyModifiers::CONTROL)
                && has_body_scroll(overlay, scene)) =>
        {
            return Some(if let Some(overlay) = overlay {
                match overlay {
                    OverlaySurface::Dialog(
                        DialogKind::Models
                        | DialogKind::Connections
                        | DialogKind::Sessions
                        | DialogKind::Permissions
                        | DialogKind::SessionTree
                        | DialogKind::Telemetry,
                    ) => InputAction::ModalUp,
                    OverlaySurface::Dialog(
                        DialogKind::Tools
                        | DialogKind::Mcp
                        | DialogKind::Skills
                        | DialogKind::Queue
                        | DialogKind::Asides,
                    ) => InputAction::SessionSelect { forward: false },
                    OverlaySurface::Sheet(SheetKind::ProviderPreset) => {
                        InputAction::MovePresetChoice { forward: false }
                    }
                    OverlaySurface::Sheet(SheetKind::OAuthPending) => InputAction::ScrollUp,
                    OverlaySurface::Sheet(SheetKind::CustomProvider) => {
                        InputAction::ScrollCustomProvider { forward: false }
                    }
                    OverlaySurface::Dialog(DialogKind::UsageStats) => InputAction::ScrollUp,
                    _ => return None,
                }
            } else {
                match scene {
                    SceneKind::Dashboard | SceneKind::Settings => InputAction::ModalUp,
                    _ => return None,
                }
            });
        }
        KeyCode::Down
            if !(key.modifiers.contains(KeyModifiers::CONTROL)
                && has_body_scroll(overlay, scene)) =>
        {
            return Some(if let Some(overlay) = overlay {
                match overlay {
                    OverlaySurface::Dialog(
                        DialogKind::Models
                        | DialogKind::Connections
                        | DialogKind::Sessions
                        | DialogKind::Permissions
                        | DialogKind::SessionTree
                        | DialogKind::Telemetry,
                    ) => InputAction::ModalDown,
                    OverlaySurface::Dialog(
                        DialogKind::Tools
                        | DialogKind::Mcp
                        | DialogKind::Skills
                        | DialogKind::Queue
                        | DialogKind::Asides,
                    ) => InputAction::SessionSelect { forward: true },
                    OverlaySurface::Sheet(SheetKind::ProviderPreset) => {
                        InputAction::MovePresetChoice { forward: true }
                    }
                    OverlaySurface::Sheet(SheetKind::OAuthPending) => InputAction::ScrollDown,
                    OverlaySurface::Sheet(SheetKind::CustomProvider) => {
                        InputAction::ScrollCustomProvider { forward: true }
                    }
                    OverlaySurface::Dialog(DialogKind::UsageStats) => InputAction::ScrollDown,
                    _ => return None,
                }
            } else {
                match scene {
                    SceneKind::Dashboard | SceneKind::Settings => InputAction::ModalDown,
                    _ => return None,
                }
            });
        }
        KeyCode::Left => {
            if let Some(overlay) = overlay {
                match overlay {
                    OverlaySurface::Dialog(DialogKind::Telemetry) => {
                        return Some(InputAction::TelemetryPrevTab);
                    }
                    OverlaySurface::Sheet(SheetKind::ModelEditor)
                        if keys.editor_field == Some(1) =>
                    {
                        return Some(InputAction::ModelEditorEffortCycle { delta: -1 });
                    }
                    OverlaySurface::Sheet(SheetKind::CustomProvider)
                        if keys.custom_provider_field.is_none() =>
                    {
                        return Some(InputAction::CycleCustomProviderChoice { forward: false });
                    }
                    _ => {}
                }
            } else if scene == SceneKind::Settings
                && keys.config_focus == crate::overlays::ConfigFocus::Detail
            {
                return Some(InputAction::ConfigSegmentPrev);
            }
        }
        KeyCode::Right => {
            if let Some(overlay) = overlay {
                match overlay {
                    OverlaySurface::Dialog(DialogKind::Telemetry) => {
                        return Some(InputAction::TelemetryNextTab);
                    }
                    OverlaySurface::Sheet(SheetKind::ModelEditor)
                        if keys.editor_field == Some(1) =>
                    {
                        return Some(InputAction::ModelEditorEffortCycle { delta: 1 });
                    }
                    OverlaySurface::Sheet(SheetKind::CustomProvider)
                        if keys.custom_provider_field.is_none() =>
                    {
                        return Some(InputAction::CycleCustomProviderChoice { forward: true });
                    }
                    _ => {}
                }
            } else if scene == SceneKind::Settings
                && keys.config_focus == crate::overlays::ConfigFocus::Detail
            {
                return Some(InputAction::ConfigSegmentNext);
            }
        }
        KeyCode::Tab => {
            return Some(if let Some(overlay) = overlay {
                match overlay {
                    OverlaySurface::Sheet(SheetKind::ModelEditor) => {
                        InputAction::ModelEditorNextField
                    }
                    OverlaySurface::Sheet(SheetKind::CustomProvider) => {
                        InputAction::CustomProviderNextField
                    }
                    OverlaySurface::Dialog(DialogKind::Telemetry) => InputAction::TelemetryNextTab,
                    OverlaySurface::Sheet(SheetKind::OAuthPending) => {
                        InputAction::CycleOauthSelection
                    }
                    OverlaySurface::Dialog(DialogKind::Sessions) => {
                        InputAction::ToggleSessionTimelineExpand
                    }
                    _ => return None,
                }
            } else if scene == SceneKind::Dashboard {
                InputAction::HostFocusToggle
            } else {
                return None;
            });
        }
        KeyCode::BackTab => {
            let overlay = overlay?;
            return match overlay {
                OverlaySurface::Sheet(SheetKind::CustomProvider) => {
                    Some(InputAction::CustomProviderPrevField)
                }
                OverlaySurface::Dialog(DialogKind::Telemetry) => {
                    Some(InputAction::TelemetryPrevTab)
                }
                OverlaySurface::Sheet(SheetKind::OAuthPending) => {
                    Some(InputAction::CycleOauthSelection)
                }
                _ => None,
            };
        }
        _ => {}
    }

    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }

    if let Some(overlay) = overlay {
        if c == '?'
            && matches!(overlay, OverlaySurface::Dialog(_))
            && !modal_claims_composer_line(Some(overlay), scene, keys)
        {
            return Some(InputAction::ToggleDialogKeys);
        }
        match overlay {
            OverlaySurface::Dialog(DialogKind::Tools) if c == ' ' => {
                Some(InputAction::SessionActivate)
            }
            OverlaySurface::Dialog(DialogKind::Mcp) => match c {
                ' ' => Some(InputAction::McpToggle),
                'r' => Some(InputAction::McpReconnect),
                _ => None,
            },
            OverlaySurface::Sheet(SheetKind::OAuthPending) => match c {
                'c' => Some(InputAction::CopyOauthContent {
                    target: OauthCopyTarget::UserCode,
                }),
                'u' => Some(InputAction::CopyOauthContent {
                    target: OauthCopyTarget::Url,
                }),
                ' ' | 'y' => Some(InputAction::CopyOauthContent {
                    target: OauthCopyTarget::Selected,
                }),
                _ => None,
            },
            OverlaySurface::Sheet(SheetKind::ProviderPreset) => match c {
                'b' => Some(InputAction::SelectPresetWithOauthMethod {
                    method: nuo_wire::LoginMethod::Browser,
                }),
                'd' => Some(InputAction::SelectPresetWithOauthMethod {
                    method: nuo_wire::LoginMethod::Device,
                }),
                _ => None,
            },
            OverlaySurface::Dialog(DialogKind::Permissions) => match c {
                ' ' => Some(InputAction::PermissionsActivate),
                'c' => Some(InputAction::PermissionsClearAll),
                _ => None,
            },
            OverlaySurface::Dialog(DialogKind::Telemetry) => match c {
                '1' => Some(InputAction::TelemetrySetTab(
                    crate::overlays::telemetry::TelemetryTab::Overview,
                )),
                '2' => Some(InputAction::TelemetrySetTab(
                    crate::overlays::telemetry::TelemetryTab::Activity,
                )),
                '[' | 'h' => Some(InputAction::TelemetryPrevTab),
                ']' | 'l' => Some(InputAction::TelemetryNextTab),
                _ => None,
            },
            OverlaySurface::Dialog(DialogKind::Models) => resolve_picker_key(c, true, keys),
            OverlaySurface::Dialog(DialogKind::Connections) if keys.connection_info_detail => {
                match c {
                    ' ' | 'm' | 'M' => Some(InputAction::ToggleConnectionModelsExpanded),
                    'r' | 'R' => Some(InputAction::RefreshProviderModels),
                    'e' => Some(InputAction::OpenModelEditor),
                    _ => None,
                }
            }
            OverlaySurface::Dialog(DialogKind::Connections) => resolve_picker_key(c, false, keys),
            OverlaySurface::Dialog(DialogKind::Sessions) if !keys.session_info_detail => match c {
                'd' => Some(InputAction::DeleteSelectedSession),
                'n' | 'N' => Some(InputAction::CreateNewSession),
                'i' => Some(InputAction::OpenSessionInfo),
                _ => None,
            },
            OverlaySurface::Dialog(DialogKind::Queue) => match c {
                'D' => Some(InputAction::QueueDelete),
                'K' => Some(InputAction::QueueMoveItem { delta: -1 }),
                'J' => Some(InputAction::QueueMoveItem { delta: 1 }),
                _ => None,
            },
            OverlaySurface::Dialog(DialogKind::Asides) if c == 'D' => {
                Some(InputAction::BtwCloseSelected)
            }
            OverlaySurface::Sheet(SheetKind::ModelEditor) => resolve_model_editor_key(c, keys),
            _ => None,
        }
    } else {
        match scene {
            SceneKind::Settings => resolve_config_key(c, keys),
            SceneKind::Dashboard => resolve_host_key(c),
            _ => None,
        }
    }
}

/// The history modal (Ctrl+R) owns its family across key codes: `Esc` closes
/// (restoring the stashed draft), `Enter`/`Tab` insert the focused entry into
/// the composer and close, `↑`/`↓` walk the list. While the search sub-layer
/// is active, printable keys and Backspace edit the borrowed composer line via
/// the shared editing layer (`edits_input_field`).
pub(crate) fn resolve_history_search_key(key: crate::keymap::Key) -> Option<InputAction> {
    match key.code {
        KeyCode::Esc => Some(InputAction::CloseModal),
        KeyCode::Delete if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(InputAction::HistoryDeleteSelected)
        }
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(InputAction::HistoryInsert)
        }
        KeyCode::Tab => Some(InputAction::HistoryInsert),
        KeyCode::Up
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(InputAction::ModalUp)
        }
        KeyCode::Down
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(InputAction::ModalDown)
        }
        _ => None,
    }
}

/// The command palette (`C-x p`, ADR-0301) owns its filter family: every
/// printable key types into the palette's own query (never the composer),
/// `Backspace` trims the query, `Delete` drops the selected entry, and `Enter`
/// executes the highlighted command. List walking (↑/↓) and Esc-close stay in
/// the shared affordance layer — they are cross-modal verbs.
fn resolve_view_switcher_key(key: crate::keymap::Key) -> Option<InputAction> {
    match key.code {
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            Some(InputAction::ViewSwitcherFilter { ch: c })
        }
        KeyCode::Backspace => Some(InputAction::ViewSwitcherBackspace),
        KeyCode::Delete => Some(InputAction::ViewCloseSelected),
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(InputAction::ViewSwitchActivate)
        }
        _ => None,
    }
}

/// The history modal's hint row (single origin for the composer's history
/// hint): every chord advertised here is handled by
/// [`resolve_history_search_key`].
const HISTORY_HINTS: &[LiveHint] = &[
    LiveHint::nav(crate::keymap::Key::ESC, "close"),
    LiveHint::nav_glyph(
        crate::keymap::Key::UP,
        crate::keymap::keyvocab::ARROWS_UD,
        "navigate",
    ),
    LiveHint::nav(crate::keymap::Key::SHIFT_DELETE, "delete"),
    LiveHint::action(crate::keymap::Key::TAB, "insert"),
    LiveHint::action(crate::keymap::Key::ENTER, "insert"),
];

pub(crate) fn live_history_hints() -> &'static [LiveHint] {
    HISTORY_HINTS
}

/// Question sheet: `space` toggles the selection (unless the free-text
/// "Other" row is highlighted), `1..9` picks an option, anything else types
/// into the focused field.
/// Settings scene: `space` activates the row; in the Detail pane `1`/`h` and
/// `2`/`l` step segments. The scene has **no exit verb of its own**: leaving is
/// the `Ctrl+X` namespace alone (ADR-0298), so no printable letter is
/// overloaded here.
fn resolve_config_key(c: char, keys: &ModalKeys) -> Option<InputAction> {
    if c == ' ' {
        return Some(InputAction::ConfigActivate);
    }
    if keys.config_focus == crate::overlays::ConfigFocus::Detail {
        if c == '1' || c == 'h' {
            return Some(InputAction::ConfigSegmentPrev);
        }
        if c == '2' || c == 'l' {
            return Some(InputAction::ConfigSegmentNext);
        }
    }
    None
}

/// Dialog key reference sub-view input routing.
fn resolve_dialog_keys_key(key: crate::keymap::Key) -> InputAction {
    match key.code {
        KeyCode::Esc => InputAction::ToggleDialogKeys,
        KeyCode::Char('?')
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            InputAction::ToggleDialogKeys
        }
        KeyCode::Up => InputAction::DialogKeysScroll { delta: -1 },
        KeyCode::Down => InputAction::DialogKeysScroll { delta: 1 },
        KeyCode::PageUp => InputAction::DialogKeysScroll { delta: -5 },
        KeyCode::PageDown => InputAction::DialogKeysScroll { delta: 5 },
        KeyCode::Home => InputAction::DialogKeysScroll { delta: -100 },
        _ => InputAction::None,
    }
}

/// Models / Connections picker browse-mode verbs. While the search sub-layer
/// is active every char is a query and the modal owns nothing here.
fn resolve_picker_key(c: char, is_models: bool, keys: &ModalKeys) -> Option<InputAction> {
    if keys.model_searching {
        return None;
    }
    if c == '/' {
        // Browse mode: `/` opens the search sub-layer rather than inserting
        // a literal slash — mirrors the history modal.
        return Some(InputAction::ModelEnterSearch);
    }
    if is_models && c == '*' {
        // Models browse mode only: star the highlighted MODEL as a favorite.
        return Some(InputAction::ProviderPickerToggleFavorite);
    }
    if is_models && c == 'x' {
        // Models browse mode: 'x' blocks/intercepts the highlighted model from the connection pipe (ADR-0203 §10).
        return Some(InputAction::ProviderPickerBlockModel);
    }
    if !is_models && c == 'a' {
        // Connections browse mode: `a` opens the curated preset branch.
        return Some(InputAction::OpenPresetChooser);
    }
    if !is_models && c == 'c' {
        // Custom connections are a sibling of the preset branch.
        return Some(InputAction::OpenCustomConnection);
    }
    if c == 'e' {
        // Connections: edit the highlighted provider. Models: edit the
        // highlighted model's per-model settings.
        return Some(InputAction::OpenModelEditor);
    }
    if c == 'r' || c == 'R' {
        return Some(InputAction::RefreshProviderModels);
    }
    if !is_models && c == 'D' {
        // Connections browse mode: `Shift+D` deletes the highlighted custom
        // provider (ignored for built-ins by the handler).
        return Some(InputAction::DeleteProvider);
    }
    None
}

/// Dashboard (Host) console verbs. Every printable key is an action here —
/// never literal input — with `a` attach, `i` interrupt, `k` kill, `s`
/// suspend, `p`/`n` opening the inline prompt / new-session field, and any
/// other char seeding the console composer ("typing is opening"). The scene has
/// **no exit verb of its own**: leaving is the `Ctrl+X` namespace alone
/// (ADR-0298), so `q` types like every other unclaimed letter.
fn resolve_host_key(c: char) -> Option<InputAction> {
    match c {
        'a' => Some(InputAction::HostSwitchSelected),
        'i' => Some(InputAction::HostInterruptSelected),
        'k' => Some(InputAction::HostKillSelected),
        's' => Some(InputAction::HostSuspendSelected),
        'p' => Some(InputAction::HostPromptOpen),
        'n' => Some(InputAction::HostNewSession),
        _ => Some(InputAction::HostPromptSeed(c)),
    }
}

/// The dashboard's inline prompt (moved verbatim from the router's
/// inline-prompt stage): printable keys and Backspace edit the borrowed
/// prompt line, Delete forward-deletes (no chip handling — the dashboard
/// prompt never stages attachments), ←/→ move the caret, Enter submits, Esc
/// cancels the prompt (the scene-local step back), and every other key is
/// swallowed so the prompt owns the keyboard.
fn resolve_host_prompt_key(
    key: crate::keymap::Key,
    input: &mut String,
    cursor_position: &mut usize,
) -> InputAction {
    match key.code {
        KeyCode::Char(c) => {
            let byte_pos = normalized_cursor_byte(input, *cursor_position);
            *cursor_position = char_index_at_byte(input, byte_pos);
            input.insert(byte_pos, c);
            *cursor_position += 1;
            InputAction::InsertChar(c)
        }
        KeyCode::Backspace => {
            delete_previous_grapheme(input, cursor_position);
            InputAction::None
        }
        KeyCode::Delete => {
            delete_next_grapheme(input, cursor_position);
            InputAction::None
        }
        KeyCode::Left => {
            *cursor_position = previous_grapheme_char_index(input, *cursor_position);
            InputAction::None
        }
        KeyCode::Right => {
            *cursor_position = next_grapheme_char_index(input, *cursor_position);
            InputAction::None
        }
        KeyCode::Enter => InputAction::HostPromptSubmit,
        // Esc cancels the prompt — a scene-local step back, never a scene exit
        // (ADR-0298 §2).
        KeyCode::Esc => InputAction::SceneBack,
        _ => InputAction::None,
    }
}

/// Key editor (ModelEditor): `space` cycles the non-text fields (thinking
/// toggle / capability overrides), a digit on the effort field jumps to that
/// ladder rung; everything else edits the borrowed input line (shared layer).
fn resolve_model_editor_key(c: char, keys: &ModalKeys) -> Option<InputAction> {
    if c == ' ' && matches!(keys.editor_field, Some(2..=4)) {
        Some(match keys.editor_field {
            Some(3) => InputAction::ModelEditorVisionCycle,
            Some(4) => InputAction::ModelEditorToolCycle,
            _ => InputAction::ModelEditorThinkingToggle,
        })
    } else if c.is_ascii_digit() && c != '0' && keys.editor_field == Some(1) {
        // A digit on the effort field jumps straight to that ladder rung
        // (`1` = shallowest … `7` = deepest) instead of inserting into the
        // borrowed input line. `0` is not a tier.
        let index = c as usize - '1' as usize;
        Some(InputAction::ModelEditorEffortJump { index })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::SheetKeys;

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

    fn key(c: char) -> crate::keymap::Key {
        crate::keymap::Key {
            modifiers: crossterm::event::KeyModifiers::NONE,
            code: KeyCode::Char(c),
        }
    }

    /// Test shims: the prompt-editing buffers are only observed by the Host
    /// inline prompt; every other surface ignores them.
    fn resolve_dialog(
        dialog: DialogKind,
        k: crate::keymap::Key,
        keys: &ModalKeys,
    ) -> Option<InputAction> {
        resolve_modal_key(
            Some(OverlaySurface::Dialog(dialog)),
            SceneKind::Conversation,
            k,
            keys,
            &mut String::new(),
            &mut 0,
        )
    }

    fn resolve_sheet(
        sheet: SheetKind,
        k: crate::keymap::Key,
        keys: &ModalKeys,
    ) -> Option<InputAction> {
        resolve_modal_key(
            Some(OverlaySurface::Sheet(sheet)),
            SceneKind::Conversation,
            k,
            keys,
            &mut String::new(),
            &mut 0,
        )
    }

    fn resolve_scene(
        scene: SceneKind,
        k: crate::keymap::Key,
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
    fn connections_detail_keys_toggle_expansion_and_refresh() {
        let c = keys(|k| k.connection_info_detail = true);
        assert_eq!(resolve_dialog(DialogKind::Connections, key('a'), &c), None);
        assert_eq!(resolve_dialog(DialogKind::Connections, key('c'), &c), None);
        assert_eq!(resolve_dialog(DialogKind::Connections, key('D'), &c), None);
        assert_eq!(
            resolve_dialog(DialogKind::Connections, key(' '), &c),
            Some(InputAction::ToggleConnectionModelsExpanded)
        );
        assert_eq!(
            resolve_dialog(DialogKind::Connections, key('m'), &c),
            Some(InputAction::ToggleConnectionModelsExpanded)
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
            resolve_dialog(DialogKind::Connections, crate::keymap::Key::ENTER, &c),
            Some(InputAction::ToggleConnectionModelsExpanded)
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
        use crate::sheet::{SheetKind as LegacySheetKind, resolve_sheet_key};
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
        let esc = crate::keymap::Key::ESC;
        assert_eq!(resolve_dialog(DialogKind::Mcp, esc, &c), None);
        let c = keys(|_| {});
        assert_eq!(
            resolve_dialog(DialogKind::HistorySearch, key('q'), &c),
            None
        );
        // InputInjection is a pure text surface: every key edits via the
        // shared layer, so the sheet scheme owns nothing.
        use crate::sheet::{SheetKind as LegacySheetKind, resolve_sheet_key};
        let c = sheet_keys(|_| {});
        assert_eq!(
            resolve_sheet_key(LegacySheetKind::InputInjection, key('q'), &c),
            None
        );
    }

    #[test]
    fn history_modal_owns_insert_and_close_family() {
        use crate::keymap::Key;
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
        use crate::keymap::Key;
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
}
