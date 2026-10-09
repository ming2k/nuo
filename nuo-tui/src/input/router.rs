//! Terminal event normalization and cross-layer routing (ADR-0197 M2).

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

use crate::model::layout::{LayoutMap, SemanticCursor};
use crate::model::selection::SelectionDrag;

use super::InputAction;
use super::readline::{
    char_index_at_byte, cursor_line_end, cursor_line_start, delete_next_grapheme,
    delete_previous_grapheme, insert_newline, next_grapheme_char_index, next_word_end,
    normalize_cursor_char_index, normalized_cursor_byte, prev_word_start,
    previous_grapheme_char_index,
};

/// What the committed scene says owns the keyboard (ADR-0197 §D2: modality
/// is scene-owned, not an app-state mirror) plus the two shared navigation
/// facts every dispatch layer arbitrates against. Built once per event by
/// the caller; the fields it needs are exactly these — nothing else.
#[derive(Debug, Default, Clone)]
pub struct Dispatch {
    /// Active overlay on top of the scene (ADR-0205).
    pub overlay: Option<crate::surfaces::OverlaySurface>,
    /// The AI-initiated sheet occupying the composer slot, if any (ADR-0173
    /// §3).
    pub sheet: Option<crate::sheet::SheetKind>,
    /// ADR-0175: `true` while the PreAttach interstitial surface owns
    /// the terminal. Mirrors `App::pre_attach.is_some()` so `route_event`
    /// can route keyboard events to PreAttach without inspecting `App`.
    pub pre_attach: bool,
    /// The root scene the user stands in (ADR-0205).
    pub scene: crate::surfaces::SceneKind,
    /// User remaps of the global chords (`[keybindings]` config, ADR-0172).
    /// Global resolution and the keycap hints both consult it.
    pub key_overrides: crate::keymap::GlobalOverrides,
    /// Whether a transcript step/action target currently holds keyboard focus.
    ///
    /// This is the TUI's only navigation state: there is no separate "browse
    /// mode". When `true`, a step is highlighted in the transcript and the
    /// keys that would otherwise edit/scroll instead act on that step — `↑`/`↓`
    /// (and `Ctrl+↑`/`Ctrl+↓`) cycle the focused step, `Enter` activates it,
    /// and `Esc` clears the focus. When `false` every key has its ordinary
    /// input-box meaning. Mirrors `App::focused_target.is_some()`.
    pub focused_target: bool,
    /// Whether the transcript currently holds browse focus (e.g. via mouse click into viewport).
    pub transcript_focused: bool,
    /// A scene component above the application surfaces owns the event. Its
    /// component handler declined the key, so only global chords may run.
    pub scene_blocked: bool,
    /// Whether the `Ctrl+X` scene namespace is armed, awaiting its second
    /// stroke (ADR-0298).
    pub scene_namespace_armed: bool,
}

/// Classify one terminal event into the keyboard family resolved by the
/// committed scene. Modal components claim every family; permission sheets
/// claim decision/composer families while transcript navigation falls through.
pub fn event_family(
    event: &Event,
    ui: &crate::ui::ComponentTree,
    focused_target: bool,
    transcript_focused: bool,
) -> u64 {
    use crate::ui::{UiKey, family};

    let Event::Key(key) = event else {
        return if matches!(event, Event::Paste(_)) {
            family::COMPOSER
        } else {
            u64::MAX
        };
    };
    let sheet_is_foreground = matches!(
        ui.scene().foreground_for(family::SHEET),
        Some(UiKey::Sheet(_))
    );
    if sheet_is_foreground
        && matches!(
            key.code,
            KeyCode::Esc
                | KeyCode::Enter
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Tab
                | KeyCode::BackTab
        )
    {
        return family::SHEET;
    }
    let completion_is_mounted = ui.scene().id(&UiKey::Completion).is_some();
    if completion_is_mounted
        && matches!(
            key.code,
            KeyCode::Esc
                | KeyCode::Enter
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Tab
                | KeyCode::BackTab
        )
    {
        return family::COMPLETION;
    }
    if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
        || matches!(key.code, KeyCode::Home | KeyCode::End)
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || focused_target
                || transcript_focused)
        || matches!(key.code, KeyCode::Up | KeyCode::Down)
            && (key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
                || focused_target
                || transcript_focused)
    {
        family::TRANSCRIPT
    } else {
        family::COMPOSER
    }
}

/// Whether an interaction sheet is the keyboard foreground (ADR-0173 §3).
///
/// A sheet owns its decision keys only while no overlay modal coexists: the
/// modal renders centered above the bottom-slot sheet, so visual order makes
/// the modal the foreground — it takes every non-global key until it closes,
/// and the sheet beneath is inert (its pending decision untouched). Closing
/// the modal hands the keyboard straight back to the sheet, which is why
/// "Esc closes the modal" and "Esc rejects the permission" never fire in one
/// press.
fn sheet_foreground(dispatch: &Dispatch) -> bool {
    dispatch.sheet.is_some() && dispatch.overlay.is_none()
}

/// Whether the permission sheet occupies the composer slot and is the
/// keyboard foreground. The one pass-through surface: transcript navigation
/// and scrolling stay live behind it (ADR-0173 §2) — a coexisting modal
/// covers it and suspends the pass-through.
fn permission_sheet_foreground(dispatch: &Dispatch) -> bool {
    dispatch.sheet == Some(crate::sheet::SheetKind::Permission) && dispatch.overlay.is_none()
}

/// Whether no overlay is up at all — no modal, no sheet: the chat surface.
fn bare_chat_surface(dispatch: &Dispatch) -> bool {
    dispatch.overlay.is_none() && dispatch.sheet.is_none()
}

/// Whether clicks, drags and hover reach the live transcript: on the bare
/// chat surface, or behind the foreground permission sheet. A coexisting
/// modal owns the screen above the sheet, so the pass-through suspends.
fn transcript_interactive(dispatch: &Dispatch) -> bool {
    bare_chat_surface(dispatch) || permission_sheet_foreground(dispatch)
}

/// Whether the foreground surface (sheet or modal) pages its own body on the
/// scroll keys.
fn foreground_scrolls_own_body(dispatch: &Dispatch) -> bool {
    if sheet_foreground(dispatch) {
        return dispatch
            .sheet
            .is_some_and(|kind| kind.keyboard_claims().body_scroll);
    }
    scrolls_own_body(dispatch.overlay, dispatch.scene)
}

/// Whether the composer line is being edited on the foreground surface.
fn edits_input_field(dispatch: &Dispatch, modal_keys: &crate::modal_keys::ModalKeys) -> bool {
    if dispatch.focused_target || dispatch.transcript_focused {
        return false;
    }
    if dispatch.sheet.is_some() && dispatch.overlay.is_none() {
        return dispatch.sheet == Some(crate::sheet::SheetKind::InputInjection);
    }
    crate::modal_keys::modal_claims_composer_line(dispatch.overlay, dispatch.scene, modal_keys)
}

fn scrolls_own_body(
    overlay: Option<crate::surfaces::OverlaySurface>,
    scene: crate::surfaces::SceneKind,
) -> bool {
    if let Some(overlay) = overlay {
        matches!(
            overlay,
            crate::surfaces::OverlaySurface::Dialog(_)
                | crate::surfaces::OverlaySurface::Sheet(
                    crate::surfaces::SheetKind::OAuthPending
                        | crate::surfaces::SheetKind::ProviderPreset
                        | crate::surfaces::SheetKind::CustomProvider,
                )
        )
    } else {
        matches!(
            scene,
            crate::surfaces::SceneKind::Dashboard | crate::surfaces::SceneKind::Settings
        )
    }
}

/// Process a crossterm event into a high-level action.
///
/// `input` and `cursor_position` are mutable because some events modify them directly.
#[allow(clippy::too_many_arguments)]
pub fn route_event(
    event: Event,
    input: &mut String,
    cursor_position: &mut usize,
    dispatch: Dispatch,
    modal_keys: &crate::modal_keys::ModalKeys,
    sheet_keys: &crate::sheet::SheetKeys,
    scene_keys: &crate::session::SceneKeys,
    drag: &mut SelectionDrag,
) -> InputAction {
    match event {
        Event::Mouse(mouse) => {
            let x = mouse.column;
            let y = mouse.row;
            match mouse.kind {
                // The wheel is spatially routed by the event loop's Wheel
                // handler: modal bodies still take it while a modal owns the
                // surface, and otherwise a tick inside the composer panel
                // scrolls the input's own viewport, falling back to the
                // transcript everywhere else. The question modal's body
                // scroll stays decoupled from the ↑/↓ highlight so wheeling
                // browses a long option list without moving the selection
                // cursor.
                MouseEventKind::ScrollUp => InputAction::Wheel { up: true, x, y },
                MouseEventKind::ScrollDown => InputAction::Wheel { up: false, x, y },
                MouseEventKind::Down(MouseButton::Left) => {
                    // The permission sheet replaces the composer but leaves the
                    // transcript above fully interactive, so a click there can
                    // still toggle steps, drag-select text, follow links, etc.
                    // The sheet itself has no click targets (its buttons are
                    // keyboard-driven) and covers only the composer/hint slot,
                    // which has no registered transcript region, so a press
                    // landing on it resolves to nothing and stays inert.
                    if transcript_interactive(&dispatch) {
                        drag.start(SemanticCursor::new(0, 0, 0));
                        InputAction::SelectionStart { x, y }
                    } else if dispatch.sheet == Some(crate::sheet::SheetKind::Question)
                        || dispatch.overlay.is_some()
                        || dispatch.scene == crate::surfaces::SceneKind::Dashboard
                        || dispatch.scene == crate::surfaces::SceneKind::Settings
                    {
                        InputAction::SelectionStart { x, y }
                    } else {
                        InputAction::None
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if drag.active
                        && (transcript_interactive(&dispatch)
                            || dispatch.overlay
                                == Some(crate::surfaces::OverlaySurface::Sheet(
                                    crate::surfaces::SheetKind::OAuthPending,
                                )))
                    {
                        InputAction::SelectionUpdate { x, y }
                    } else if drag.active {
                        // A drag armed inside a selectable modal document
                        // (SelectionStart resolved to a MODAL_DOC region)
                        // keeps updating while the button is held, even under
                        // modals that otherwise swallow mouse events.
                        InputAction::SelectionUpdate { x, y }
                    } else {
                        InputAction::None
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if drag.active {
                        drag.end();
                        InputAction::SelectionEnd
                    } else {
                        InputAction::None
                    }
                }
                // Triple-click detection would need a timer; for now we map
                // middle click to "select block" as a quick approximation.
                MouseEventKind::Down(MouseButton::Middle) => {
                    if transcript_interactive(&dispatch) {
                        InputAction::SelectBlock { x, y }
                    } else {
                        InputAction::None
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    // Right-click opens detail/feedback for interactive
                    // transcript elements. Allowed during a permission prompt
                    // because the transcript stays interactive.
                    if transcript_interactive(&dispatch) {
                        InputAction::RightClick { x, y }
                    } else {
                        InputAction::None
                    }
                } // Mouse motion (reported because `EnableMouseCapture` requests
                // mode 1003 "all motion"). Forwarded on the main view and
                // during a permission prompt so hover affordances keep working
                // on the still-interactive transcript; blocked behind other
                // overlay modals.
                MouseEventKind::Moved => {
                    if transcript_interactive(&dispatch) {
                        InputAction::Hover { x, y }
                    } else {
                        InputAction::None
                    }
                }
                _ => InputAction::None,
            }
        }
        Event::Key(key) => {
            // Ignore key release events: Windows Console / ConPTY and enhanced
            // keyboard protocols send both Press and Release events. Treating
            // Release as an input action causes double typing, immediate Ctrl-C
            // quits, and duplicate hotkey triggers.
            if key.kind == KeyEventKind::Release {
                return InputAction::None;
            }

            let physical_key = crate::keymap::Key::from_event(key);

            // The `Ctrl+X` scene namespace's second stroke (ADR-0298 §1). The
            // verb table is `keymap::scene_namespace` — the same table the
            // which-key card renders, so dispatch and advertisement cannot
            // drift (ADR-0238). A second stroke is typed without looking, so
            // case is folded before resolution; `Esc` and `C-g` cancel, and
            // anything unrecognized cancels rather than falling through (a
            // half-typed chord must never fire a global).
            if dispatch.scene_namespace_armed {
                use crate::keymap::scene_namespace::{SceneVerb, opens};
                // `C-x C-x` re-arms rather than cancelling: the namespace's own
                // opening stroke is idempotent, matching how a repeated prefix
                // behaves in every leader-chord editor.
                if opens(physical_key) {
                    return InputAction::SetSceneNamespaceArmed(true);
                }
                return match SceneVerb::from_second_stroke(physical_key) {
                    Some(SceneVerb::Leave) => InputAction::CloseScene,
                    Some(SceneVerb::Switcher) => InputAction::ViewSwitcherToggle,
                    Some(SceneVerb::Threads) => InputAction::OpenSessions,
                    Some(SceneVerb::Dashboard) => InputAction::NavigateDashboard,
                    Some(SceneVerb::Settings) => InputAction::NavigateSettings,
                    // `C-x C-c` is the namespace's quit spelling; a bare `c`
                    // carries no meaning here (it is not in the verb's strokes).
                    Some(SceneVerb::Quit) => InputAction::CtrlC,
                    // `Esc` / `C-g` / anything unrecognized cancels. A
                    // half-typed chord must never fall through to a global.
                    None => InputAction::CancelSceneNamespace,
                };
            }

            // Open the `Ctrl+X` scene namespace.
            if crate::keymap::scene_namespace::opens(physical_key) {
                return InputAction::SetSceneNamespaceArmed(true);
            }

            // Stage 5: Global Hard-Bound Shortcuts
            // Alt+1..9 tab jump (ADR-0040 [INV-KEY-01])
            // Alt+Left / Alt+Right tab navigation history (ADR-0040 [INV-KEY-02], ADR-0042)
            if key.modifiers == KeyModifiers::ALT {
                match key.code {
                    KeyCode::Left => return InputAction::TabHistoryBack,
                    KeyCode::Right => return InputAction::TabHistoryForward,
                    KeyCode::Char(c) if ('1'..='9').contains(&c) => {
                        let idx = (c as usize) - ('1' as usize);
                        return InputAction::SelectTab(idx);
                    }
                    _ => {}
                }
            }

            // Ctrl+Q (queue), Ctrl+C (Interrupt/Quit),
            // CopySelection, and the user-remappable globals. The Command
            // Palette is deliberately absent from the canonical table: its
            // canonical entry is the `C-x` scene namespace's switcher verb
            // (`C-x p`, ADR-0023), resolved above — before any
            // scene or modal arm — so `C-x p` opens it from every context and
            // the old `Ctrl-L`-behind-a-modal prohibition is gone.
            if let Some(cmd_id) =
                crate::keymap::resolve_global_key_with(physical_key, &dispatch.key_overrides)
            {
                match cmd_id {
                    crate::keymap::CommandId::CommandPalette => {
                        // Only reachable through a user remap of the palette
                        // command (the canonical chord was retired, ADR-0023).
                        // Mirror the namespace exactly: open at the top level,
                        // close while already open, and — with no modal
                        // prohibition any more — switch to it from behind
                        // another modal too.
                        return InputAction::ViewSwitcherToggle;
                    }
                    crate::keymap::CommandId::OpenSessionStats if dispatch.overlay.is_none() => {
                        // Open session stats (/stats). Top level only:
                        // the model bar is session chrome, never visible
                        // behind a modal.
                        return InputAction::OpenSessionStats;
                    }
                    crate::keymap::CommandId::OpenSessionTrace if dispatch.overlay.is_none() => {
                        return InputAction::OpenSessionTrace;
                    }
                    crate::keymap::CommandId::OpenQueue if dispatch.overlay.is_none() => {
                        // Ctrl+Q (queue-bar expand keycap, ADR-0126's Ctrl row).
                        // Top level only: the bar is session chrome and is not
                        // visible behind a modal, so behind one the chord falls
                        // through to whatever the modal claims.
                        return InputAction::OpenQueue;
                    }
                    crate::keymap::CommandId::InterruptTask => return InputAction::Interrupt,
                    crate::keymap::CommandId::Quit => return InputAction::CtrlC,
                    crate::keymap::CommandId::CloseTab => return InputAction::CloseTab,
                    crate::keymap::CommandId::CopySelection => return InputAction::CopySelection,
                    _ => {}
                }
            }

            if dispatch.scene_blocked {
                return InputAction::None;
            }

            // ADR-0175: PreAttach interstitial owns the keyboard. The
            // four navigation keys map to dedicated PreAttach actions
            // and everything else is swallowed (no chat composer,
            // modal, or sheet behind the surface). Global chords
            // (Ctrl+C) still resolve above as escape hatches,
            // which is consistent with how SessionsPicker handles
            // them — the operator always has a force-quit path.
            if dispatch.pre_attach {
                return match key.code {
                    KeyCode::Up => InputAction::PreAttachUp,
                    KeyCode::Down => InputAction::PreAttachDown,
                    KeyCode::Char(' ') => InputAction::PreAttachToggle,
                    KeyCode::Enter => InputAction::PreAttachSubmit,
                    // Backspace, Delete, unrecognized chars, all Fn keys, all
                    // Ctrl+X chords (other than the globals handled above)
                    // fall through to Esc semantics.
                    _ => InputAction::PreAttachCancel,
                };
            }

            // Surface Dispatch (ADR-0172 / ADR-0205)
            // Each full-screen scene owns the keys for its own focus planes
            // while no modal is up: the Conversation scene's chat scheme (and its
            // TaskInspection / Aside siblings) resolves them here, before the modal /
            // global arms below. A key the surface does not own falls through
            // to the shared affordance library and the modal arms.
            if bare_chat_surface(&dispatch)
                && let Some(action) = crate::session::resolve_scene_key(
                    dispatch.scene,
                    physical_key,
                    scene_keys,
                    input,
                    cursor_position,
                )
            {
                return action;
            }

            // Modal Verb Dispatch (ADR-0172)
            // Each modal owns its single-letter verb keys (space/r in the MCP
            // manager, d/n/i in the sessions picker, the dashboard console,
            // …) in its own scheme. A key the modal does not own falls through
            // to the shared affordance library (list nav, readline, paste,
            // scrolling) and text insertion.
            // Sheet Verb Dispatch (ADR-0173 §3)
            // Each interaction sheet owns its single-key verbs in its own
            // scheme; a key the sheet does not own falls through to the
            // shared affordance library and the sheet arms below. A sheet is
            // the keyboard foreground only while no modal coexists (the
            // arbitration rule): the modal renders above it (visual order),
            // so with both up the modal's own scheme is consulted first and
            // the sheet's verbs are suspended until the modal closes.
            if dispatch.overlay.is_none()
                && let Some(kind) = dispatch.sheet
                && let Some(action) =
                    crate::sheet::resolve_sheet_key(kind, physical_key, sheet_keys)
            {
                return action;
            }
            if (dispatch.overlay.is_some()
                || matches!(
                    dispatch.scene,
                    crate::surfaces::SceneKind::Dashboard | crate::surfaces::SceneKind::Settings
                ))
                && let Some(action) = crate::modal_keys::resolve_modal_key(
                    dispatch.overlay,
                    dispatch.scene,
                    physical_key,
                    modal_keys,
                    input,
                    cursor_position,
                )
            {
                return action;
            }

            match key.code {
                KeyCode::Esc => {
                    if let Some(overlay) = dispatch.overlay {
                        match overlay {
                            crate::surfaces::OverlaySurface::Sheet(
                                crate::surfaces::SheetKind::ProviderPreset,
                            ) => InputAction::CancelPresetChooser,
                            crate::surfaces::OverlaySurface::Sheet(
                                crate::surfaces::SheetKind::OAuthPending,
                            ) => InputAction::CancelOauthPending,
                            crate::surfaces::OverlaySurface::Sheet(
                                crate::surfaces::SheetKind::CustomProvider,
                            ) => InputAction::CancelCustomProvider,
                            crate::surfaces::OverlaySurface::Dialog(
                                crate::surfaces::DialogKind::Models
                                | crate::surfaces::DialogKind::Connections,
                            ) if modal_keys.model_searching => InputAction::ModelExitSearch,
                            _ => InputAction::CloseModal,
                        }
                    } else if permission_sheet_foreground(&dispatch) {
                        if sheet_keys.permission_confirm_always {
                            InputAction::PermissionBack
                        } else if dispatch.focused_target {
                            InputAction::ClearFocusedTarget
                        } else {
                            InputAction::PermissionReject
                        }
                    } else if dispatch.sheet == Some(crate::sheet::SheetKind::Question) {
                        InputAction::QuestionCancel
                    } else if dispatch.sheet == Some(crate::sheet::SheetKind::InputInjection) {
                        InputAction::InputCancel
                    } else if dispatch.scene == crate::surfaces::SceneKind::Settings
                        || dispatch.scene == crate::surfaces::SceneKind::Dashboard
                    {
                        // Esc steps back one *scene-owned* sub-layer (a
                        // dropdown, a drill-in pane, the dashboard's preview or
                        // inline prompt) and nothing else. It never leaves the
                        // scene (ADR-0205 `[INV-TUI-CLEAN-02]`): the scene's
                        // own exit is `C-x w`/`C-x k` (ADR-0298 §1).
                        InputAction::SceneBack
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+R (history search) is a chat-surface chord resolved
                    // by the Conversation scene's scheme (ADR-0172 / ADR-0205); no other
                    // surface claims it.
                    InputAction::None
                }
                // Ctrl+P toggles the queue block inside the Queue modal so the
                // user can resume without closing the list. At the top level
                // Ctrl+P is the chat surface's `focus_prev` step-walk verb
                // (resolved by the Conversation scene before this match), so
                // this arm only ever fires while the Queue modal is active.
                // Inside any other modal it is a no-op.
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if dispatch.overlay
                        == Some(crate::surfaces::OverlaySurface::Dialog(
                            crate::surfaces::DialogKind::Queue,
                        ))
                    {
                        InputAction::QueueToggleBlock
                    } else {
                        InputAction::None
                    }
                }

                // F5 is a declared global binding (registry → OpenBtwList).
                // Inside the asides modal itself it re-queries the list (a
                // refresh) rather than toggling the modal closed; inside any
                // other modal it is a no-op.
                KeyCode::F(5) => {
                    if dispatch.overlay
                        == Some(crate::surfaces::OverlaySurface::Dialog(
                            crate::surfaces::DialogKind::Asides,
                        ))
                    {
                        InputAction::OpenBtwList
                    } else {
                        InputAction::None
                    }
                }
                // Ctrl+H is inert: it has no chord in the registry. Under the
                // Kitty enhanced-keyboard protocol it arrives as a distinct
                // control event; in a raw terminal it is byte-identical to
                // Backspace (0x08) and lands in the `Backspace` arm instead.
                // Explicitly swallow it here so the printable-char arm below
                // never inserts a literal `h`.
                KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    InputAction::None
                }
                // Ctrl+L is inert: the palette's canonical chord is the `C-x`
                // scene namespace's switcher verb (`C-x p`, ADR-0023), so `Ctrl-L` no longer maps to any command. Swallow
                // it explicitly (like the inert Ctrl-H above) so the generic
                // printable arm never inserts a literal `l`. A user who prefers
                // `Ctrl-L` can still remap `palette` to it in `[keybindings]`,
                // in which case it resolves through the Stage-5 globals above
                // and never reaches this arm.
                KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    InputAction::None
                }
                // Ctrl+M is a declared global binding (registry →
                // OpenModels). In a raw terminal Ctrl+M is byte-identical
                // to Enter, so the registry arm only fires under the Kitty
                // protocol; without it Ctrl+M arrives as Enter and leaves
                // input behavior untouched — no regression. It only reaches
                // this arm inside a modal, where it is a no-op.
                KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    InputAction::None
                }
                // Alt+Enter / Ctrl+J: insert a literal newline so the input
                // box supports multi-line drafting. Plain Enter sends the
                // message, so these are the only multi-line entry paths.
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                    insert_newline(input, cursor_position, dispatch.overlay.is_none());
                    InputAction::None
                }
                KeyCode::Enter => {
                    // Sheet submits (ADR-0173 §3) and modal activation verbs
                    // (ADR-0172) are owned by the sheet / modal schemes,
                    // resolved above. Alt+Enter stays the multi-line newline
                    // chord (arm above). What remains here is inert: the
                    // chat surface's Enter (activate focused step / commit
                    // completion / send / queue / slash) is resolved by the
                    // Session scene's scheme (ADR-0172 / ADR-0205) before this match,
                    // and the surfaces that punt (e.g. the pickers' read-only
                    // info sub-views) are no-ops too.
                    InputAction::None
                }
                KeyCode::Tab => {
                    // Modal Tab-focus verbs (ADR-0172) and the HistorySearch
                    // insert are owned by the modal schemes above; the chat
                    // surface's Tab (commit / reopen a completion) by the
                    // Session scene's scheme (ADR-0173 / ADR-0205). Inert here.
                    InputAction::None
                }
                KeyCode::BackTab => {
                    // Modal BackTab verbs (ADR-0172) and the sheets'
                    // reverse-walk are owned by the surface schemes above;
                    // the chat surface's BackTab by the Session scene's
                    // scheme (ADR-0172 / ADR-0205). Inert here.
                    InputAction::None
                }
                // Ctrl+J: alias for Alt+Enter — insert a literal newline.
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    insert_newline(input, cursor_position, dispatch.overlay.is_none());
                    InputAction::None
                }
                // Ctrl+V: paste from the system clipboard. Active on the
                // main prompt and in the free-text modals (provider editor,
                // provider picker filter, history search) which borrow the
                // input line as a single-line field. The app loop reads the
                // clipboard asynchronously and either attaches an image,
                // inserts the text at the cursor (main prompt), or splices it
                // inline into the modal field (modals).
                KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        InputAction::Paste
                    } else {
                        InputAction::None
                    }
                }
                // Ctrl+B: move the caret back one character (readline
                // `backward-char`). Mirrors Left and sits alongside the
                // Ctrl+A / Ctrl+E line-motion family. Active wherever free text
                // is edited; a no-op elsewhere so it never inserts a literal
                // 'b' or scrolls.
                KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&dispatch, modal_keys) && *cursor_position > 0 {
                        *cursor_position = previous_grapheme_char_index(input, *cursor_position);
                    }
                    InputAction::None
                }
                // Ctrl+A: move the caret to the start of the current line
                // (readline convention). Works wherever free text is being
                // edited — the main prompt in Compose zone and the free-text
                // modals. Outside those (Browse zone, read-only modals) it is
                // a no-op so it never inserts a literal 'a' or scrolls.
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        cursor_line_start(input, cursor_position);
                    }
                    InputAction::None
                }
                // Ctrl+E: move the caret to the end of the current line.
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        cursor_line_end(input, cursor_position);
                    }
                    InputAction::None
                }
                // Ctrl+W: delete the previous whitespace-delimited word
                // (readline `unix-word-rubout`). Skips trailing whitespace
                // then removes the contiguous run of non-whitespace before
                // the caret, crossing newline boundaries.
                // No-op outside free-text surfaces so it never closes a
                // modal or inserts a literal 'w'.
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        let start = prev_word_start(input, *cursor_position);
                        if start < *cursor_position {
                            let start_byte = input
                                .char_indices()
                                .nth(start)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            let end_byte = input
                                .char_indices()
                                .nth(*cursor_position)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            input.replace_range(start_byte..end_byte, "");
                            *cursor_position = start;
                            return InputAction::Backspace;
                        }
                    }
                    InputAction::None
                }
                // Ctrl+U: delete from the caret to the start of the current
                // logical line (readline `unix-line-discard`). Multi-line
                // drafts only lose the current line; Ctrl+C still clears the
                // whole buffer when the user wants a full wipe.
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        let mut start = *cursor_position;
                        cursor_line_start(input, &mut start);
                        if start < *cursor_position {
                            let start_byte = input
                                .char_indices()
                                .nth(start)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            let end_byte = input
                                .char_indices()
                                .nth(*cursor_position)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            input.replace_range(start_byte..end_byte, "");
                            *cursor_position = start;
                            return InputAction::Backspace;
                        }
                    }
                    InputAction::None
                }
                // Ctrl+K: delete from the caret to the end of the current
                // logical line (readline `kill-line`). If already at the end
                // of the line (before a newline), deletes the newline to join
                // the next line.
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        let char_count = input.chars().count();
                        if *cursor_position < char_count {
                            let mut end = *cursor_position;
                            cursor_line_end(input, &mut end);
                            if end == *cursor_position {
                                end = *cursor_position + 1;
                            }
                            let start_byte = input
                                .char_indices()
                                .nth(*cursor_position)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            let end_byte = input
                                .char_indices()
                                .nth(end)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            input.replace_range(start_byte..end_byte, "");
                            return InputAction::Backspace;
                        }
                    }
                    InputAction::None
                }
                // Alt+B: jump back one word (readline `backward-word`).
                KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        *cursor_position = normalize_cursor_char_index(
                            input,
                            prev_word_start(input, *cursor_position),
                        );
                    }
                    InputAction::None
                }
                // Alt+F: jump forward one word (readline `forward-word`).
                KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        *cursor_position = normalize_cursor_char_index(
                            input,
                            next_word_end(input, *cursor_position),
                        );
                    }
                    InputAction::None
                }
                // Alt+D: delete the next whitespace-delimited word (readline
                // `kill-word`). Symmetric counterpart to Ctrl+W.
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
                    if edits_input_field(&dispatch, modal_keys) {
                        let end = next_word_end(input, *cursor_position);
                        if end > *cursor_position {
                            let start_byte = input
                                .char_indices()
                                .nth(*cursor_position)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            let end_byte = input
                                .char_indices()
                                .nth(end)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            input.replace_range(start_byte..end_byte, "");
                            return InputAction::Backspace;
                        }
                    }
                    InputAction::None
                }
                // Alt+S / Alt+P / Alt+N are chat-surface chords (steer now /
                // previous / next prompt history), resolved by the Conversation
                // scene's scheme (ADR-0172 / ADR-0205) before this match.
                KeyCode::Char(c) => {
                    // The command palette's filter is owned by its scheme
                    // (modal_keys::resolve_view_switcher_key, ADR-0172);
                    // the modal verb keys too. Only shared text insertion
                    // remains here: editing surfaces (chat composer, borrowed
                    // one-line modal filters, the key editor's API-key field)
                    // insert the character, everything else is inert.
                    // `edits_input_field` resolves the foreground: a
                    // coexisting modal outranks the sheet (the injection
                    // sheet's borrowed line is inert until the modal closes),
                    // and the question sheet's printable verbs are resolved
                    // by its own scheme above — never reaching here.
                    if edits_input_field(&dispatch, modal_keys)
                        && !crate::modal_keys::modal_swallows_printable(
                            dispatch.overlay,
                            modal_keys,
                        )
                    {
                        // The key editor's thinking field (2) is a toggle, not
                        // a text field — don't let printable chars mutate the
                        // borrowed input line while it's focused.
                        let byte_pos = normalized_cursor_byte(input, *cursor_position);
                        *cursor_position = char_index_at_byte(input, byte_pos);
                        input.insert(byte_pos, c);
                        *cursor_position += 1;
                        // Return InsertChar so the event loop can reset the
                        // completion-dismissal latch and suggestion highlight.
                        // The input mutation already happened above; the event
                        // loop's InsertChar handler treats the char as a signal
                        // only (it does not re-insert).
                        InputAction::InsertChar(c)
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::Backspace => {
                    // The palette's query backspace is owned by its scheme
                    // (modal_keys, ADR-0172) and the question sheet's
                    // "Other"-field backspace by the sheet scheme
                    // (sheet.rs, ADR-0173 §3).
                    if edits_input_field(&dispatch, modal_keys) && *cursor_position > 0 {
                        // Alt+Backspace / Ctrl+Backspace delete the previous
                        // whitespace-delimited word in one stroke, matching
                        // readline's `backward-kill-word`. Plain Backspace
                        // keeps the chip-aware atomic delete below so pasted
                        // attachment placeholders vanish in a single tap.
                        if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            let start = prev_word_start(input, *cursor_position);
                            if start < *cursor_position {
                                let start_byte = input
                                    .char_indices()
                                    .nth(start)
                                    .map(|(i, _)| i)
                                    .unwrap_or(input.len());
                                let end_byte = input
                                    .char_indices()
                                    .nth(*cursor_position)
                                    .map(|(i, _)| i)
                                    .unwrap_or(input.len());
                                input.replace_range(start_byte..end_byte, "");
                                *cursor_position = start;
                                return InputAction::Backspace;
                            }
                        }
                        // Chip-aware atomic delete: when the cursor sits
                        // immediately after an attachment placeholder (and
                        // optionally one trailing space the paste path
                        // inserts), one Backspace removes the whole chip in
                        // a single keystroke — mirroring codex / claude-code
                        // / opencode. The event loop runs the reconcile pass
                        // on the returned `Backspace` action, which drops
                        // the orphaned entry from `pending_images` /
                        // `pending_text_pastes` and relabels survivors.
                        let byte_cursor = input
                            .char_indices()
                            .map(|(i, _)| i)
                            .nth(*cursor_position)
                            .unwrap_or(input.len());
                        if let Some((start, end)) =
                            crate::composer_attachments::chip_range_for_backspace(
                                input,
                                byte_cursor,
                            )
                        {
                            let removed_chars = input[start..end].chars().count();
                            input.replace_range(start..end, "");
                            *cursor_position -= removed_chars;
                            return InputAction::Backspace;
                        }
                        delete_previous_grapheme(input, cursor_position);
                        // Return Backspace so the event loop resets the
                        // completion-dismissal latch and suggestion highlight,
                        // matching InsertChar above.
                        InputAction::Backspace
                    } else {
                        InputAction::None
                    }
                }
                // `Del` key: forward delete — remove the character *after*
                // the caret. Gated like Backspace on the same free-text
                // surfaces (`edits_input_field`), so it never disturbs a
                // read-only modal. Chip-aware: a Delete landing on the `[` of
                // an attachment chip removes the whole chip in one
                // keystroke, mirroring the chip-aware Backspace. The caret
                // does not move (forward delete only shortens the text).
                KeyCode::Delete => {
                    // The palette's delete-selected is owned by its scheme
                    // (modal_keys, ADR-0172).
                    if edits_input_field(&dispatch, modal_keys)
                        && *cursor_position < input.chars().count()
                    {
                        let byte_cursor = input
                            .char_indices()
                            .map(|(i, _)| i)
                            .nth(*cursor_position)
                            .unwrap_or(input.len());
                        if let Some((start, end)) =
                            crate::composer_attachments::chip_range_for_delete(input, byte_cursor)
                        {
                            input.replace_range(start..end, "");
                            return InputAction::DeleteForward;
                        }
                        if delete_next_grapheme(input, cursor_position) {
                            return InputAction::DeleteForward;
                        }
                    }
                    InputAction::None
                }
                KeyCode::Left => {
                    // Modal segment/cycle verbs (telemetry tabs, settings
                    // segments, the effort ladder, the provider-editor
                    // choice row) are owned by the modal scheme (modal_keys,
                    // ADR-0172), resolved above.
                    // In provider-editor text fields, ←/→ retain ordinary
                    // caret movement.
                    if edits_input_field(&dispatch, modal_keys) && *cursor_position > 0 {
                        // Ctrl+Left (and Alt+Left on terminals that translate
                        // it) jumps back one whitespace-delimited word,
                        // matching readline's `backward-word`.
                        if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            *cursor_position = normalize_cursor_char_index(
                                input,
                                prev_word_start(input, *cursor_position),
                            );
                        } else {
                            *cursor_position =
                                previous_grapheme_char_index(input, *cursor_position);
                        }
                    }
                    InputAction::None
                }
                KeyCode::Right => {
                    // Modal segment/cycle verbs are owned by the modal
                    // scheme (modal_keys, ADR-0172), resolved above.
                    if edits_input_field(&dispatch, modal_keys)
                        && *cursor_position < input.chars().count()
                    {
                        // Ctrl+Right (and Alt+Right) jump forward one word.
                        if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            *cursor_position = normalize_cursor_char_index(
                                input,
                                next_word_end(input, *cursor_position),
                            );
                        } else {
                            *cursor_position = next_grapheme_char_index(input, *cursor_position);
                        }
                    }
                    InputAction::None
                }
                // Alt+↑ / Alt+↓ (transcript step focus switching) are
                // chat-surface chords, resolved by the Conversation scene's scheme
                // (ADR-0172 / ADR-0205) before this match.
                // Ctrl+↑ / Ctrl+↓ inside a modal scroll the modal body by one
                // page — the same gesture a pager or editor binds to a
                // half-page jump. Mirrors PageUp / PageDown so users have both
                // the dedicated keys and the chord (useful on keyboards without
                // Page keys). Routed through the shared `Scroll*` actions.
                KeyCode::Up
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && scrolls_own_body(dispatch.overlay, dispatch.scene) =>
                {
                    InputAction::ScrollPageUp
                }
                KeyCode::Down
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && scrolls_own_body(dispatch.overlay, dispatch.scene) =>
                {
                    InputAction::ScrollPageDown
                }
                KeyCode::Up => {
                    // Sheet ↑ (ADR-0173 §3) and the modal list-walk verbs
                    // (ADR-0172) are owned by the surface schemes, resolved
                    // above. The palette's ↑/↓ stay here: its scheme
                    // resolves the arrows to None (they are shared list-walk
                    // affordances), so the router keeps them.
                    if dispatch.overlay
                        == Some(crate::surfaces::OverlaySurface::Dialog(
                            crate::surfaces::DialogKind::Switcher,
                        ))
                    {
                        InputAction::ModalUp
                    } else {
                        // Chat-surface ↑ (walk focused steps / completion
                        // suggestions / multi-line caret) is resolved by the
                        // Conversation scene's scheme (ADR-0172 / ADR-0205); everything else
                        // the schemes punted on is inert.
                        InputAction::None
                    }
                }
                KeyCode::Down => {
                    // Sheet ↓ and the modal list-walk verbs are owned by the
                    // surface schemes, resolved above; the palette's ↓ stays
                    // here for the same reason as ↑.
                    if dispatch.overlay
                        == Some(crate::surfaces::OverlaySurface::Dialog(
                            crate::surfaces::DialogKind::Switcher,
                        ))
                    {
                        InputAction::ModalDown
                    } else {
                        // Chat-surface ↓ is resolved by the Conversation scene's
                        // scheme (ADR-0172 / ADR-0205).
                        InputAction::None
                    }
                }
                // PageUp / PageDown: Scroll transcript or modal body by one viewport page.
                KeyCode::PageUp => {
                    // Transcript paging on the bare chat surface and behind
                    // the foreground permission sheet; a body-scrolling
                    // sheet or modal pages itself (claims, ADR-0173 §2).
                    if bare_chat_surface(&dispatch)
                        || permission_sheet_foreground(&dispatch)
                        || foreground_scrolls_own_body(&dispatch)
                    {
                        InputAction::ScrollPageUp
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::PageDown => {
                    // Transcript paging on the bare chat surface and behind
                    // the foreground permission sheet; a body-scrolling
                    // sheet or modal pages itself (claims, ADR-0173 §2).
                    if bare_chat_surface(&dispatch)
                        || permission_sheet_foreground(&dispatch)
                        || foreground_scrolls_own_body(&dispatch)
                    {
                        InputAction::ScrollPageDown
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::Home
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && (bare_chat_surface(&dispatch)
                            || permission_sheet_foreground(&dispatch)
                            || foreground_scrolls_own_body(&dispatch)) =>
                {
                    InputAction::ScrollTop
                }
                KeyCode::End
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && (bare_chat_surface(&dispatch)
                            || permission_sheet_foreground(&dispatch)
                            || foreground_scrolls_own_body(&dispatch)) =>
                {
                    InputAction::ScrollBottom
                }
                // Bare Home / End:
                // - When a target or browse focus is active (or in permission sheet): scroll transcript to top / bottom.
                // - When composer or modal input field is active: move caret to line start / line end (readline convention).
                KeyCode::Home => {
                    if permission_sheet_foreground(&dispatch)
                        || dispatch.focused_target
                        || dispatch.transcript_focused
                    {
                        InputAction::ScrollTop
                    } else if edits_input_field(&dispatch, modal_keys) {
                        cursor_line_start(input, cursor_position);
                        InputAction::None
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::End => {
                    if permission_sheet_foreground(&dispatch)
                        || dispatch.focused_target
                        || dispatch.transcript_focused
                    {
                        InputAction::ScrollBottom
                    } else if edits_input_field(&dispatch, modal_keys) {
                        cursor_line_end(input, cursor_position);
                        InputAction::None
                    } else {
                        InputAction::None
                    }
                }
                _ => InputAction::None,
            }
        }
        Event::Paste(text) => {
            // Terminal-level bracketed paste. Route the payload through the
            // same chip-or-inline logic as Ctrl+V on the main prompt, and
            // splice it inline into the focused field in the free-text
            // modals (provider editor, provider picker filter, history
            // search).
            if dispatch
                .sheet
                .is_some_and(|kind| crate::sheet::sheet_owns_bracketed_paste(kind, sheet_keys))
            {
                // The "Other" field owns its own buffer; route the bracketed
                // payload into it via the event loop's paste apply.
                InputAction::BracketedPaste(text)
            } else if edits_input_field(&dispatch, modal_keys) {
                InputAction::BracketedPaste(text)
            } else {
                InputAction::None
            }
        }
        Event::Resize(cols, rows) => {
            // The event loop does the real work (redraw + re-arm mouse capture)
            // off this signal; here we surface the exact geometry the terminal
            // signaled rather than leaving it to the catch-all `None`.
            InputAction::TerminalResized { cols, rows }
        }
        _ => InputAction::None,
    }
}

/// Resolve a screen coordinate to the block it belongs to.
pub fn resolve_block(layout_map: &LayoutMap, x: u16, y: u16) -> Option<(usize, usize)> {
    if let Some(r) = layout_map.region_at(x, y) {
        return Some((r.message_idx, r.block_idx));
    }
    if let Some(rect) = layout_map.composer_rect()
        && rect.x <= x
        && x < rect.x + rect.width
        && rect.y <= y
        && y < rect.y + rect.height
    {
        return Some((crate::model::layout::INPUT_MSG_IDX, 0));
    }
    None
}
