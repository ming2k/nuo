//! Terminal event normalization and cross-layer routing (M2).
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use nuo_view::model::layout::{LayoutMap, SemanticCursor};
use nuo_view::model::selection::SelectionDrag;

use super::InputAction;
use super::readline::{
    cursor_line_end, cursor_line_start, delete_next_grapheme, delete_previous_grapheme,
    insert_newline, next_grapheme_boundary, next_word_end, normalized_cursor_byte, prev_word_start,
    previous_grapheme_boundary,
};

/// What the committed scene says owns the keyboard (modality
/// is scene-owned, not an app-state mirror) plus the two shared navigation
/// facts every dispatch layer arbitrates against. Built once per event by
/// the caller; the fields it needs are exactly these — nothing else.
#[derive(Debug, Default, Clone)]
pub struct Dispatch {
    /// Active overlay on top of the scene.
    pub overlay: Option<crate::shell::surfaces::OverlaySurface>,
    /// The AI-initiated sheet occupying the composer slot, if any (
    /// §3).
    pub sheet: Option<crate::shell::sheet::SheetKind>,
    /// `true` while the PreAttach interstitial surface owns
    /// the terminal. Mirrors `App::pre_attach.is_some()` so `route_event`
    /// can route keyboard events to PreAttach without inspecting `App`.
    pub pre_attach: bool,
    /// The root scene the user stands in.
    pub scene: crate::shell::surfaces::SceneKind,
    /// User remaps of the global chords (`[keybindings]` config).
    /// Global resolution and the keycap hints both consult it.
    pub key_overrides: crate::shell::keymap::GlobalOverrides,
    /// Which of the chat scene's peer surfaces owns the keyboard
    /// (ADR-0145). One value, so the router cannot be told a contradictory
    /// pair ("an entry is highlighted **and** the composer is live") — the
    /// legacy shape's two booleans could express exactly that.
    ///
    /// `FocusPlane::Transcript` covers both transcript sub-states, browse and
    /// entry highlight: to the router they agree on every arbitration question
    /// (`↑`/`↓` act on the transcript, printables do not reach the draft).
    pub focus: crate::shell::focus::FocusPlane,
    /// A scene component above the application surfaces owns the event. Its
    /// component handler declined the key, so only global chords may run.
    pub scene_blocked: bool,
    /// Whether the notice log viewer is open. The viewer is not an
    /// `OverlaySurface` (its content is client-global, so it is not a
    /// session-scoped dialog), which means the router has to be told about it
    /// explicitly or its keys would fall through to the transcript.
    pub notice_viewer_open: bool,
}

/// Classify one terminal event into the keyboard family resolved by the
/// committed scene. Modal components claim every family; permission sheets
/// claim decision/composer families while transcript navigation falls through.
pub fn event_family(
    event: &Event,
    ui: &crate::shell::ui::ComponentTree,
    focus: crate::shell::focus::FocusPlane,
) -> u64 {
    let transcript_plane = focus == crate::shell::focus::FocusPlane::Transcript;
    use crate::shell::ui::{UiKey, family};

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
            && (key.modifiers.contains(KeyModifiers::CONTROL) || transcript_plane)
        || matches!(key.code, KeyCode::Up | KeyCode::Down)
            && (key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
                || transcript_plane)
    {
        family::TRANSCRIPT
    } else {
        family::COMPOSER
    }
}

/// Whether an interaction sheet is the keyboard foreground.
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
/// and scrolling stay live behind it — a coexisting modal
/// covers it and suspends the pass-through.
fn permission_sheet_foreground(dispatch: &Dispatch) -> bool {
    dispatch.sheet == Some(crate::shell::sheet::SheetKind::Permission) && dispatch.overlay.is_none()
}

/// Whether no overlay is up at all — no modal, no sheet: the chat surface.
fn bare_chat_surface(dispatch: &Dispatch) -> bool {
    dispatch.overlay.is_none() && dispatch.sheet.is_none()
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
fn edits_input_field(
    dispatch: &Dispatch,
    modal_keys: &crate::shell::modal_keys::ModalKeys,
) -> bool {
    if dispatch.overlay.is_some() {
        return crate::shell::modal_keys::modal_claims_composer_line(
            dispatch.overlay,
            dispatch.scene,
            modal_keys,
        );
    }
    if dispatch.focus == crate::shell::focus::FocusPlane::Transcript {
        return false;
    }
    if dispatch.sheet.is_some() && dispatch.overlay.is_none() {
        return dispatch.sheet == Some(crate::shell::sheet::SheetKind::InputInjection);
    }
    crate::shell::modal_keys::modal_claims_composer_line(
        dispatch.overlay,
        dispatch.scene,
        modal_keys,
    )
}

fn scrolls_own_body(
    overlay: Option<crate::shell::surfaces::OverlaySurface>,
    scene: crate::shell::surfaces::SceneKind,
) -> bool {
    if let Some(overlay) = overlay {
        matches!(
            overlay,
            crate::shell::surfaces::OverlaySurface::Dialog(_)
                | crate::shell::surfaces::OverlaySurface::Sheet(
                    crate::shell::surfaces::SheetKind::OAuthPending
                        | crate::shell::surfaces::SheetKind::ProviderPreset
                        | crate::shell::surfaces::SheetKind::CustomProvider,
                )
        )
    } else {
        matches!(
            scene,
            crate::shell::surfaces::SceneKind::Dashboard
                | crate::shell::surfaces::SceneKind::Settings
                | crate::shell::surfaces::SceneKind::CommandLog
        )
    }
}

/// Mutable routing state shared by the per-event-class helpers below.
///
/// `route_event_motion` keeps its public shape — callers own the buffers —
/// and builds this view once, so each helper receives the same routing facts
/// without re-declaring the nine-argument signature.
struct MotionRouter<'a> {
    input: &'a mut String,
    cursor_position: &'a mut usize,
    motion: &'a mut crate::shell::composer_grid::CaretMotion,
    dispatch: Dispatch,
    modal_keys: &'a crate::shell::modal_keys::ModalKeys,
    sheet_keys: &'a crate::shell::sheet::SheetKeys,
    scene_keys: &'a crate::shell::session::SceneKeys,
    drag: &'a mut SelectionDrag,
}

/// Process a crossterm event into a high-level action.
///
/// `input` and `cursor_position` are mutable because some events modify them directly.
#[allow(clippy::too_many_arguments)]
pub fn route_event_motion(
    event: Event,
    input: &mut String,
    cursor_position: &mut usize,
    motion: &mut crate::shell::composer_grid::CaretMotion,
    dispatch: Dispatch,
    modal_keys: &crate::shell::modal_keys::ModalKeys,
    sheet_keys: &crate::shell::sheet::SheetKeys,
    scene_keys: &crate::shell::session::SceneKeys,
    drag: &mut SelectionDrag,
) -> InputAction {
    let mut router = MotionRouter {
        input,
        cursor_position,
        motion,
        dispatch,
        modal_keys,
        sheet_keys,
        scene_keys,
        drag,
    };
    match event {
        Event::Mouse(mouse) => route_mouse_event(&mut router, mouse),
        Event::Key(key) => route_key_event(&mut router, key),
        Event::Paste(text) => route_paste_event(&router, text),
        Event::Resize(cols, rows) => InputAction::TerminalResized { cols, rows },
        _ => InputAction::None,
    }
}

/// Route pointer and wheel events: wheel ticks are forwarded spatially, while
/// button events drive the selection drag lifecycle and hover.
fn route_mouse_event(router: &mut MotionRouter<'_>, mouse: MouseEvent) -> InputAction {
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
        MouseEventKind::ScrollUp => wheel_action(true, x, y),
        MouseEventKind::ScrollDown => wheel_action(false, x, y),
        kind => pointer_action(router, kind, x, y),
    }
}

/// Emit one wheel tick at the exact screen coordinate the terminal reported.
fn wheel_action(up: bool, x: u16, y: u16) -> InputAction {
    InputAction::Wheel { up, x, y }
}

/// Route mouse buttons: left drives the selection drag lifecycle, middle and
/// right emit their dedicated gestures, and movement feeds hover tracking.
fn pointer_action(
    router: &mut MotionRouter<'_>,
    kind: MouseEventKind,
    x: u16,
    y: u16,
) -> InputAction {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            router.drag.start(SemanticCursor::new(0, 0, 0));
            InputAction::SelectionStart { x, y }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if router.drag.active {
                InputAction::SelectionUpdate { x, y }
            } else {
                InputAction::None
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if router.drag.active {
                router.drag.end();
                InputAction::SelectionEnd
            } else {
                InputAction::None
            }
        }
        // Triple-click detection would need a timer; for now we map
        // middle click to "select block" as a quick approximation.
        MouseEventKind::Down(MouseButton::Middle) => InputAction::SelectBlock { x, y },
        MouseEventKind::Down(MouseButton::Right) => InputAction::RightClick { x, y },
        MouseEventKind::Moved => InputAction::Hover { x, y },
        _ => InputAction::None,
    }
}

/// Route one key press through the modality ladder, in order: release filter,
/// sovereign help, the notice viewer, hard-bound global chords, scene-blocked
/// surfaces, PreAttach, the foreground surface schemes, then the shared
/// fallback key map.
fn route_key_event(router: &mut MotionRouter<'_>, key: KeyEvent) -> InputAction {
    // Ignore key release events: Windows Console / ConPTY and enhanced
    // keyboard protocols send both Press and Release events. Treating
    // Release as an input action causes double typing, immediate Ctrl-C
    // quits, and duplicate hotkey triggers.
    if key.kind == KeyEventKind::Release {
        return InputAction::None;
    }

    let physical_key = crate::shell::keymap::key_from_event(key);

    // F1 / Alt-h opens the sovereign Help dialog (ADR-0131 [INV-HELP-03]).
    // When an overlay is active, F1 flows down to modal dispatch for in-dialog key guidance.
    if (physical_key == crate::shell::keymap::Key::F1
        || physical_key == crate::shell::keymap::Key::ALT_H)
        && router.dispatch.overlay.is_none()
        && !router.dispatch.pre_attach
    {
        return InputAction::OpenHelp;
    }

    // The notice viewer owns its keys while it is open. It is painted
    // above every overlay and is not an `OverlaySurface`, so without
    // this arm its arrow keys would scroll the transcript behind it.
    if router.dispatch.notice_viewer_open {
        return notice_viewer_action(physical_key);
    }

    // Stage 5: Global Hard-Bound Shortcuts
    // Alt+1..9 tab jump (ADR-0040 [INV-KEY-01])
    // Alt+[ / Alt+] symmetric tab navigation history (ADR-0092 [INV-NAV-03])
    // Alt+0 dashboard tab (ADR-0151 [INV-PLANE-04])
    // Alt+w close tab, Alt+t new tab, Alt+o notices, Alt+, settings
    if let Some(action) = hard_bound_global_action(key) {
        return action;
    }

    // Ctrl+Q (queue), Ctrl+C (Interrupt/Quit),
    // CopySelection, and the user-remappable globals.
    if let Some(action) = registered_global_action(physical_key, &router.dispatch) {
        return action;
    }

    if router.dispatch.scene_blocked {
        return InputAction::None;
    }

    // PreAttach interstitial owns the keyboard. The
    // four navigation keys map to dedicated PreAttach actions
    // and everything else is swallowed (no chat composer,
    // modal, or sheet behind the surface). Global chords
    // (Ctrl+C) still resolve above as escape hatches,
    // which is consistent with how SessionsPicker handles
    // them — the operator always has a force-quit path.
    if router.dispatch.pre_attach {
        return pre_attach_action(key.code);
    }

    // Surface Dispatch
    // Each full-screen scene owns the keys for its own focus planes
    // while no modal is up: the Conversation scene's chat scheme (and its
    // TaskInspection siblings) resolves them here, before the modal /
    // global arms below. A key the surface does not own falls through
    // to the shared affordance library and the modal arms.
    if let Some(action) = resolve_surface_action(router, physical_key) {
        return action;
    }

    route_key_fallback(router, key)
}

/// The notice viewer's private key map; it is not an `OverlaySurface`, so the
/// router has to arbitrate its keys explicitly.
fn notice_viewer_action(physical_key: crate::shell::keymap::Key) -> InputAction {
    match physical_key {
        crate::shell::keymap::Key::ESC => InputAction::CloseModal,
        crate::shell::keymap::Key::ALT_O => InputAction::OpenNotices,
        crate::shell::keymap::Key::UP => InputAction::NoticesSelect { forward: false },
        crate::shell::keymap::Key::DOWN => InputAction::NoticesSelect { forward: true },
        crate::shell::keymap::Key::ENTER => InputAction::NoticesToggleDetail,
        crate::shell::keymap::Key::PAGE_UP => InputAction::NoticesPage { forward: false },
        crate::shell::keymap::Key::PAGE_DOWN => InputAction::NoticesPage { forward: true },
        key => {
            // `x` clears the log; `y` copies it. Plain letters, so
            // they are matched by character rather than chord.
            match key.code {
                crate::shell::keymap::KeyCode::Char('x') if key.modifiers.is_empty() => {
                    InputAction::NoticesClear
                }
                crate::shell::keymap::KeyCode::Char('y') if key.modifiers.is_empty() => {
                    InputAction::NoticesCopy
                }
                _ => InputAction::None,
            }
        }
    }
}

/// Hard-bound `Alt` chords that resolve before any surface scheme.
fn hard_bound_global_action(key: KeyEvent) -> Option<InputAction> {
    if key.modifiers != KeyModifiers::ALT {
        return None;
    }
    match key.code {
        KeyCode::Char('[') => Some(InputAction::TabHistoryBack),
        KeyCode::Char(']') => Some(InputAction::TabHistoryForward),
        KeyCode::Char('0') => Some(InputAction::NavigateDashboard),
        KeyCode::Char(c) if ('1'..='9').contains(&c) => {
            let idx = (c as usize) - ('1' as usize);
            Some(InputAction::SelectTab(idx))
        }
        KeyCode::Char('w') | KeyCode::Char('W') => Some(InputAction::CloseTab),
        KeyCode::Char('t') | KeyCode::Char('T') => Some(InputAction::CreateNewSession),
        KeyCode::Char('o') | KeyCode::Char('O') => Some(InputAction::OpenNotices),
        KeyCode::Char(',') => Some(InputAction::NavigateSettings),
        _ => None,
    }
}

/// Registry-backed global chords, including user remaps. Some commands are
/// top-level only; behind an overlay their guard declines and the key falls
/// through to the modal layer.
fn registered_global_action(
    physical_key: crate::shell::keymap::Key,
    dispatch: &Dispatch,
) -> Option<InputAction> {
    let cmd_id =
        crate::shell::keymap::resolve_global_key_with(physical_key, &dispatch.key_overrides)?;
    match cmd_id {
        crate::shell::keymap::CommandId::CommandPalette => Some(InputAction::ViewSwitcherToggle),
        crate::shell::keymap::CommandId::ToggleFocusPlane => {
            // `Shift-Tab` crosses the chat scene's peer focus planes.
            // Top level only: an overlay traps the keyboard, and
            // its own `Shift-Tab` verbs (field walking, tab cycling)
            // are resolved by its scheme well before this arm.
            if dispatch.overlay.is_none()
                && dispatch.sheet.is_none()
                && dispatch.scene == crate::shell::surfaces::SceneKind::Thread
            {
                Some(InputAction::ToggleFocusPlane)
            } else {
                None
            }
        }
        crate::shell::keymap::CommandId::NewTab => Some(InputAction::CreateNewSession),
        crate::shell::keymap::CommandId::NavigateDashboard => Some(InputAction::NavigateDashboard),
        crate::shell::keymap::CommandId::NavigateSettings => Some(InputAction::NavigateSettings),
        crate::shell::keymap::CommandId::OpenThreadStats if dispatch.overlay.is_none() => {
            // Open thread stats (/stats). Top level only:
            // the model bar is session chrome, never visible
            // behind a modal.
            Some(InputAction::OpenThreadStats)
        }
        crate::shell::keymap::CommandId::OpenSessionTrace if dispatch.overlay.is_none() => {
            Some(InputAction::OpenSessionTrace)
        }
        crate::shell::keymap::CommandId::OpenQueue if dispatch.overlay.is_none() => {
            // Ctrl+Q (queue-bar expand keycap, Ctrl row).
            // Top level only: the bar is session chrome and is not
            // visible behind a modal, so behind one the chord falls
            // through to whatever the modal claims.
            Some(InputAction::OpenQueue)
        }
        crate::shell::keymap::CommandId::OpenNotices => {
            // Ctrl+o works from every context, including behind a
            // modal: a fault recorded while a dialog is up is
            // exactly the case the user needs to review, and the
            // viewer paints above the dialog rather than replacing
            // it. A text field that claims printable keys is not
            // affected — this is a Ctrl chord, not a character.
            Some(InputAction::OpenNotices)
        }
        crate::shell::keymap::CommandId::InterruptTask => Some(InputAction::Interrupt),
        crate::shell::keymap::CommandId::Quit => Some(InputAction::CtrlC),
        crate::shell::keymap::CommandId::CloseTab => Some(InputAction::CloseTab),
        crate::shell::keymap::CommandId::CopySelection => Some(InputAction::CopySelection),
        _ => None,
    }
}

/// PreAttach's four navigation keys; everything else cancels the interstitial.
fn pre_attach_action(code: KeyCode) -> InputAction {
    match code {
        KeyCode::Up => InputAction::PreAttachUp,
        KeyCode::Down => InputAction::PreAttachDown,
        KeyCode::Char(' ') => InputAction::PreAttachToggle,
        KeyCode::Enter => InputAction::PreAttachSubmit,
        // Backspace, Delete, unrecognized chars, all Fn keys, all
        // Ctrl+X chords (other than the globals handled above)
        // fall through to Esc semantics.
        _ => InputAction::PreAttachCancel,
    }
}

/// Ask the foreground surface's own scheme before the shared fallback map.
///
/// Modal Verb Dispatch
/// Each modal owns its single-letter verb keys (space/r in the MCP
/// manager, d/n/i in the sessions picker, the dashboard console,
/// …) in its own scheme. A key the modal does not own falls through
/// to the shared affordance library (list nav, readline, paste,
/// scrolling) and text insertion.
/// Sheet Verb Dispatch
/// Each interaction sheet owns its single-key verbs in its own
/// scheme; a key the sheet does not own falls through to the
/// shared affordance library and the sheet arms below. A sheet is
/// the keyboard foreground only while no modal coexists (the
/// arbitration rule): the modal renders above it (visual order),
/// so with both up the modal's own scheme is consulted first and
/// the sheet's verbs are suspended until the modal closes.
fn resolve_surface_action(
    router: &mut MotionRouter<'_>,
    physical_key: crate::shell::keymap::Key,
) -> Option<InputAction> {
    if bare_chat_surface(&router.dispatch)
        && let Some(action) = crate::shell::session::resolve_scene_key_motion(
            router.dispatch.scene,
            physical_key,
            router.scene_keys,
            router.input,
            router.cursor_position,
            router.motion,
        )
    {
        return Some(action);
    }
    if router.dispatch.overlay.is_none()
        && let Some(kind) = router.dispatch.sheet
        && let Some(action) =
            crate::shell::sheet::resolve_sheet_key(kind, physical_key, router.sheet_keys)
    {
        return Some(action);
    }
    if (router.dispatch.overlay.is_some()
        || matches!(
            router.dispatch.scene,
            crate::shell::surfaces::SceneKind::Dashboard
                | crate::shell::surfaces::SceneKind::Settings
        ))
        && let Some(action) = crate::shell::modal_keys::resolve_modal_key(
            router.dispatch.overlay,
            router.dispatch.scene,
            physical_key,
            router.modal_keys,
            router.input,
            router.cursor_position,
        )
    {
        return Some(action);
    }
    None
}

/// The shared fallback map, grouped by key class so each family stays
/// reviewable on its own.
fn route_key_fallback(router: &mut MotionRouter<'_>, key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Esc => escape_action(router),
        KeyCode::Char(_)
        | KeyCode::Enter
        | KeyCode::Tab
        | KeyCode::BackTab
        | KeyCode::Backspace
        | KeyCode::Delete
        | KeyCode::Left
        | KeyCode::Right => editing_action(router, key),
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Home
        | KeyCode::End => navigation_action(router, key),
        _ => InputAction::None,
    }
}

/// `Esc` arbitration: overlay verbs first, then the foreground sheet, then
/// scene-owned sub-layers.
fn escape_action(router: &MotionRouter<'_>) -> InputAction {
    if let Some(overlay) = router.dispatch.overlay {
        match overlay {
            crate::shell::surfaces::OverlaySurface::Sheet(
                crate::shell::surfaces::SheetKind::ProviderPreset,
            ) => InputAction::CancelPresetChooser,
            crate::shell::surfaces::OverlaySurface::Sheet(
                crate::shell::surfaces::SheetKind::OAuthPending,
            ) => InputAction::CancelOauthPending,
            crate::shell::surfaces::OverlaySurface::Sheet(
                crate::shell::surfaces::SheetKind::CustomProvider,
            ) => InputAction::CancelCustomProvider,
            crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Models
                | crate::shell::surfaces::DialogKind::Connections,
            ) if router.modal_keys.model_searching => InputAction::ModelExitSearch,
            _ => InputAction::CloseModal,
        }
    } else if permission_sheet_foreground(&router.dispatch) {
        // The permission sheet is the keyboard foreground, so
        // `Esc` is its reject gesture — never a focus gesture.
        // It used to demote a highlighted transcript entry
        // first, which meant one `Esc` could be spent on the
        // transcript behind the sheet while the pending
        // decision sat unanswered (ADR-0145). Stepping back out
        // of the "Always allow" sub-choice is still a step back
        // *inside* the sheet, so it stays.
        if router.sheet_keys.permission_confirm_always {
            InputAction::PermissionBack
        } else {
            InputAction::PermissionReject
        }
    } else if router.dispatch.sheet == Some(crate::shell::sheet::SheetKind::Question) {
        InputAction::QuestionCancel
    } else if router.dispatch.sheet == Some(crate::shell::sheet::SheetKind::InputInjection) {
        InputAction::InputCancel
    } else if router.dispatch.scene == crate::shell::surfaces::SceneKind::Settings
        || router.dispatch.scene == crate::shell::surfaces::SceneKind::Dashboard
    {
        // Esc steps back one *scene-owned* sub-layer (a
        // dropdown, a drill-in pane, the dashboard's preview or
        // inline prompt) and nothing else. It never leaves the
        // scene (`[INV-TUI-CLEAN-02]`): the scene's
        // own exit is `C-x w` (ADR-0043, ADR-0077).
        InputAction::SceneBack
    } else {
        InputAction::None
    }
}

/// Chord-family editing: inert Control chords, newline insertion, and the
/// readline motion / kill commands. Plain text mutation lives in
/// [`text_input_action`]; the arm order here matches the original map so a
/// guarded arm keeps falling through to the printable catch-all.
fn editing_action(router: &mut MotionRouter<'_>, key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Ctrl+R (history search) is a chat-surface chord resolved
            // by the Conversation scene's scheme; no other
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
            if router.dispatch.overlay
                == Some(crate::shell::surfaces::OverlaySurface::Dialog(
                    crate::shell::surfaces::DialogKind::Queue,
                ))
            {
                InputAction::QueueToggleBlock
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
        KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => InputAction::None,
        // Ctrl+L is inert: the palette's canonical chord is the `C-x`
        // scene namespace's switcher verb (`C-x p`, ADR-0023), so `Ctrl-L` no longer maps to any command. Swallow
        // it explicitly (like the inert Ctrl-H above) so the generic
        // printable arm never inserts a literal `l`. A user who prefers
        // `Ctrl-L` can still remap `palette` to it in `[keybindings]`,
        // in which case it resolves through the Stage-5 globals above
        // and never reaches this arm.
        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => InputAction::None,
        // Ctrl+M is a declared global binding (registry →
        // OpenModels). In a raw terminal Ctrl+M is byte-identical
        // to Enter, so the registry arm only fires under the Kitty
        // protocol; without it Ctrl+M arrives as Enter and leaves
        // input behavior untouched — no regression. It only reaches
        // this arm inside a modal, where it is a no-op.
        KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => InputAction::None,
        // Alt+Enter / Ctrl+J: insert a literal newline so the input
        // box supports multi-line drafting. Plain Enter sends the
        // message, so these are the only multi-line entry paths.
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
            if edits_input_field(&router.dispatch, router.modal_keys) {
                insert_newline(
                    router.input,
                    router.cursor_position,
                    router.dispatch.overlay.is_none(),
                );
            }
            InputAction::None
        }
        KeyCode::Enter => {
            // Sheet submits and modal activation verbs
            // are owned by the sheet / modal schemes,
            // resolved above. Alt+Enter stays the multi-line newline
            // chord (arm above). What remains here is inert: the
            // chat surface's Enter (activate focused step / commit
            // completion / send / queue / slash) is resolved by the
            // Session scene's scheme before this match,
            // and the surfaces that punt (e.g. the pickers' read-only
            // info sub-views) are no-ops too.
            InputAction::None
        }
        KeyCode::Tab => {
            // Modal Tab-focus verbs and the HistorySearch
            // insert are owned by the modal schemes above; the chat
            // surface's Tab (commit / reopen a completion) by the
            // Session scene's scheme. Inert here.
            InputAction::None
        }
        KeyCode::BackTab => {
            // Modal BackTab verbs and the sheets'
            // reverse-walk are owned by the surface schemes above;
            // the chat surface's BackTab by the Session scene's
            // scheme. Inert here.
            InputAction::None
        }
        // Ctrl+J: alias for Alt+Enter — insert a literal newline.
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if edits_input_field(&router.dispatch, router.modal_keys) {
                insert_newline(
                    router.input,
                    router.cursor_position,
                    router.dispatch.overlay.is_none(),
                );
            }
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
            if edits_input_field(&router.dispatch, router.modal_keys) {
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
            if edits_input_field(&router.dispatch, router.modal_keys) && *router.cursor_position > 0
            {
                *router.cursor_position =
                    previous_grapheme_boundary(router.input, *router.cursor_position);
            }
            InputAction::None
        }
        // Ctrl+A: move the caret to the start of the current line
        // (readline convention). Works wherever free text is being
        // edited — the main prompt in Compose zone and the free-text
        // modals. Outside those (Browse zone, read-only modals) it is
        // a no-op so it never inserts a literal 'a' or scrolls.
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if edits_input_field(&router.dispatch, router.modal_keys) {
                cursor_line_start(router.input, router.cursor_position);
            }
            InputAction::None
        }
        // Ctrl+E: move the caret to the end of the current line.
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if edits_input_field(&router.dispatch, router.modal_keys) {
                cursor_line_end(router.input, router.cursor_position);
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
            if edits_input_field(&router.dispatch, router.modal_keys) {
                let start = prev_word_start(router.input, *router.cursor_position);
                if start < *router.cursor_position {
                    router
                        .input
                        .replace_range(start..*router.cursor_position, "");
                    *router.cursor_position = start;
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
            if edits_input_field(&router.dispatch, router.modal_keys) {
                let mut start = *router.cursor_position;
                cursor_line_start(router.input, &mut start);
                if start < *router.cursor_position {
                    router
                        .input
                        .replace_range(start..*router.cursor_position, "");
                    *router.cursor_position = start;
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
            if edits_input_field(&router.dispatch, router.modal_keys)
                && *router.cursor_position < router.input.len()
            {
                let mut end = *router.cursor_position;
                cursor_line_end(router.input, &mut end);
                if end == *router.cursor_position {
                    end = (*router.cursor_position + 1).min(router.input.len());
                }
                router.input.replace_range(*router.cursor_position..end, "");
                return InputAction::Backspace;
            }
            InputAction::None
        }
        // Alt+B: jump back one word (readline `backward-word`).
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
            if edits_input_field(&router.dispatch, router.modal_keys) {
                *router.cursor_position = prev_word_start(router.input, *router.cursor_position);
            }
            InputAction::None
        }
        // Alt+F: jump forward one word (readline `forward-word`).
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
            if edits_input_field(&router.dispatch, router.modal_keys) {
                *router.cursor_position = next_word_end(router.input, *router.cursor_position);
            }
            InputAction::None
        }
        // Alt+D: delete the next whitespace-delimited word (readline
        // `kill-word`). Symmetric counterpart to Ctrl+W.
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
            if edits_input_field(&router.dispatch, router.modal_keys) {
                let end = next_word_end(router.input, *router.cursor_position);
                if end > *router.cursor_position {
                    router.input.replace_range(*router.cursor_position..end, "");
                    return InputAction::Backspace;
                }
            }
            InputAction::None
        }
        KeyCode::Char(_)
        | KeyCode::Backspace
        | KeyCode::Delete
        | KeyCode::Left
        | KeyCode::Right => text_input_action(router, key),
        _ => InputAction::None,
    }
}

/// Text-plane editing: printable insertion, atomic attachment deletion, and
/// single-character / word caret movement.
fn text_input_action(router: &mut MotionRouter<'_>, key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Char(c) => {
            // The command palette's filter is owned by its scheme
            // (modal_keys::resolve_view_switcher_key);
            // the modal verb keys too. Only shared text insertion
            // remains here: editing surfaces (chat composer, borrowed
            // one-line modal filters, the key editor's API-key field)
            // insert the character, everything else is inert.
            // `edits_input_field` resolves the foreground: a
            // coexisting modal outranks the sheet (the injection
            // sheet's borrowed line is inert until the modal closes),
            // and the question sheet's printable verbs are resolved
            // by its own scheme above — never reaching here.
            if edits_input_field(&router.dispatch, router.modal_keys)
                && !crate::shell::modal_keys::modal_swallows_printable(
                    router.dispatch.overlay,
                    router.modal_keys,
                )
            {
                // The key editor's thinking field (2) is a toggle, not
                // a text field — don't let printable chars mutate the
                // borrowed input line while it's focused.
                let byte_pos = normalized_cursor_byte(router.input, *router.cursor_position);
                router.input.insert(byte_pos, c);
                *router.cursor_position = byte_pos + c.len_utf8();
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
            // (modal_keys) and the question sheet's
            // "Other"-field backspace by the sheet scheme
            // (sheet.rs).
            if edits_input_field(&router.dispatch, router.modal_keys) && *router.cursor_position > 0
            {
                // Alt+Backspace / Ctrl+Backspace delete the previous
                // whitespace-delimited word in one stroke, matching
                // readline's `backward-kill-word`. Plain Backspace
                // keeps the chip-aware atomic delete below so pasted
                // attachment placeholders vanish in a single tap.
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    let start = prev_word_start(router.input, *router.cursor_position);
                    if start < *router.cursor_position {
                        router
                            .input
                            .replace_range(start..*router.cursor_position, "");
                        *router.cursor_position = start;
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
                if let Some((start, end)) = nuo_view::composer_attachments::chip_range_for_backspace(
                    router.input,
                    *router.cursor_position,
                ) {
                    router.input.replace_range(start..end, "");
                    *router.cursor_position = start;
                    return InputAction::Backspace;
                }
                delete_previous_grapheme(router.input, router.cursor_position);
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
            // (modal_keys).
            if edits_input_field(&router.dispatch, router.modal_keys)
                && *router.cursor_position < router.input.len()
            {
                if let Some((start, end)) = nuo_view::composer_attachments::chip_range_for_delete(
                    router.input,
                    *router.cursor_position,
                ) {
                    router.input.replace_range(start..end, "");
                    return InputAction::DeleteForward;
                }
                if delete_next_grapheme(router.input, router.cursor_position) {
                    return InputAction::DeleteForward;
                }
            }
            InputAction::None
        }
        KeyCode::Left => {
            // Modal segment/cycle verbs (telemetry tabs, settings
            // segments, the effort ladder, the provider-editor
            // choice row) are owned by the modal scheme (modal_keys,
            // ), resolved above.
            // In provider-editor text fields, ←/→ retain ordinary
            // caret movement.
            if edits_input_field(&router.dispatch, router.modal_keys) && *router.cursor_position > 0
            {
                // Ctrl+Left (and Alt+Left on terminals that translate
                // it) jumps back one whitespace-delimited word,
                // matching readline's `backward-word`.
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    *router.cursor_position =
                        prev_word_start(router.input, *router.cursor_position);
                } else {
                    *router.cursor_position =
                        previous_grapheme_boundary(router.input, *router.cursor_position);
                }
            }
            InputAction::None
        }
        KeyCode::Right => {
            // Modal segment/cycle verbs are owned by the modal
            // scheme (modal_keys), resolved above.
            if edits_input_field(&router.dispatch, router.modal_keys)
                && *router.cursor_position < router.input.len()
            {
                // Ctrl+Right (and Alt+Right) jump forward one word.
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    *router.cursor_position = next_word_end(router.input, *router.cursor_position);
                } else {
                    *router.cursor_position =
                        next_grapheme_boundary(router.input, *router.cursor_position);
                }
            }
            InputAction::None
        }
        _ => InputAction::None,
    }
}

/// Navigation keys: modal body paging, transcript paging, and Home/End's
/// dual role (scroll the transcript or move the caret to a line edge).
fn navigation_action(router: &mut MotionRouter<'_>, key: KeyEvent) -> InputAction {
    match key.code {
        // The entry walk (`Ctrl+P` / `Ctrl+N`, and bare ↑/↓ once an
        // entry is highlighted) is a chat-surface chord, resolved by
        // the Conversation scene's scheme before this match.
        // Ctrl+↑ / Ctrl+↓ inside a modal scroll the modal body by one
        // page — the same gesture a pager or editor binds to a
        // half-page jump. Mirrors PageUp / PageDown so users have both
        // the dedicated keys and the chord (useful on keyboards without
        // Page keys). Routed through the shared `Scroll*` actions.
        KeyCode::Up
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && scrolls_own_body(router.dispatch.overlay, router.dispatch.scene) =>
        {
            InputAction::ScrollPageUp
        }
        KeyCode::Down
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && scrolls_own_body(router.dispatch.overlay, router.dispatch.scene) =>
        {
            InputAction::ScrollPageDown
        }
        KeyCode::Up => {
            // Sheet ↑ and the modal list-walk verbs
            // are owned by the surface schemes, resolved
            // above. The palette's ↑/↓ stay here: its scheme
            // resolves the arrows to None (they are shared list-walk
            // affordances), so the router keeps them.
            if router.dispatch.overlay
                == Some(crate::shell::surfaces::OverlaySurface::Dialog(
                    crate::shell::surfaces::DialogKind::Switcher,
                ))
            {
                InputAction::ModalUp
            } else {
                // Chat-surface ↑ (walk focused steps / completion
                // suggestions / multi-line caret) is resolved by the
                // Conversation scene's scheme; everything else
                // the schemes punted on is inert.
                InputAction::None
            }
        }
        KeyCode::Down => {
            // Sheet ↓ and the modal list-walk verbs are owned by the
            // surface schemes, resolved above; the palette's ↓ stays
            // here for the same reason as ↑.
            if router.dispatch.overlay
                == Some(crate::shell::surfaces::OverlaySurface::Dialog(
                    crate::shell::surfaces::DialogKind::Switcher,
                ))
            {
                InputAction::ModalDown
            } else {
                // Chat-surface ↓ is resolved by the Conversation scene's
                // scheme.
                InputAction::None
            }
        }
        // PageUp / PageDown: Scroll transcript or modal body by one viewport page.
        KeyCode::PageUp => {
            // Transcript paging on the bare chat surface and behind
            // the foreground permission sheet; a body-scrolling
            // sheet or modal pages itself (claims).
            if bare_chat_surface(&router.dispatch)
                || permission_sheet_foreground(&router.dispatch)
                || foreground_scrolls_own_body(&router.dispatch)
            {
                InputAction::ScrollPageUp
            } else {
                InputAction::None
            }
        }
        KeyCode::PageDown => {
            // Transcript paging on the bare chat surface and behind
            // the foreground permission sheet; a body-scrolling
            // sheet or modal pages itself (claims).
            if bare_chat_surface(&router.dispatch)
                || permission_sheet_foreground(&router.dispatch)
                || foreground_scrolls_own_body(&router.dispatch)
            {
                InputAction::ScrollPageDown
            } else {
                InputAction::None
            }
        }
        KeyCode::Home
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && (bare_chat_surface(&router.dispatch)
                    || permission_sheet_foreground(&router.dispatch)
                    || foreground_scrolls_own_body(&router.dispatch)) =>
        {
            InputAction::ScrollTop
        }
        KeyCode::End
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && (bare_chat_surface(&router.dispatch)
                    || permission_sheet_foreground(&router.dispatch)
                    || foreground_scrolls_own_body(&router.dispatch)) =>
        {
            InputAction::ScrollBottom
        }
        // Bare Home / End:
        // - When a target or browse focus is active (or in permission sheet): scroll transcript to top / bottom.
        // - When composer or modal input field is active: move caret to line start / line end (readline convention).
        KeyCode::Home => {
            if permission_sheet_foreground(&router.dispatch)
                || router.dispatch.focus == crate::shell::focus::FocusPlane::Transcript
            {
                InputAction::ScrollTop
            } else if edits_input_field(&router.dispatch, router.modal_keys) {
                cursor_line_start(router.input, router.cursor_position);
                InputAction::None
            } else {
                InputAction::None
            }
        }
        KeyCode::End => {
            if permission_sheet_foreground(&router.dispatch)
                || router.dispatch.focus == crate::shell::focus::FocusPlane::Transcript
            {
                InputAction::ScrollBottom
            } else if edits_input_field(&router.dispatch, router.modal_keys) {
                cursor_line_end(router.input, router.cursor_position);
                InputAction::None
            } else {
                InputAction::None
            }
        }
        _ => InputAction::None,
    }
}

/// Bracketed paste: the question sheet's "Other" field owns its own buffer,
/// otherwise the shared composer-line logic decides whether to accept it.
fn route_paste_event(router: &MotionRouter<'_>, text: String) -> InputAction {
    // Terminal-level bracketed paste. Route the payload through the
    // same chip-or-inline logic as Ctrl+V on the main prompt, and
    // splice it inline into the focused field in the free-text
    // modals (provider editor, provider picker filter, history
    // search).
    if router.dispatch.sheet.is_some_and(|kind| {
        crate::shell::sheet::sheet_owns_bracketed_paste(kind, router.sheet_keys)
    }) {
        // The "Other" field owns its own buffer; route the bracketed
        // payload into it via the event loop's paste apply.
        InputAction::BracketedPaste(text)
    } else if edits_input_field(&router.dispatch, router.modal_keys) {
        InputAction::BracketedPaste(text)
    } else {
        InputAction::None
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
        return Some((nuo_view::model::layout::INPUT_MSG_IDX, 0));
    }
    None
}

/// Route one event with a caller-owned caret-motion memory.
///
/// The routed key resolution reaches the scene resolvers through
/// `crate::shell::session::resolve_scene_key`, which owns the caret's vertical
/// memory today; this entry point exists so the thread surface can pass its
/// persistent [`crate::shell::composer_grid::CaretMotion`] without threading a
/// second resolution path, and so modal fields (which have no memory) share one
/// signature.
#[allow(clippy::too_many_arguments)]
pub fn route_event(
    event: Event,
    input: &mut String,
    cursor_position: &mut usize,
    dispatch: Dispatch,
    modal_keys: &crate::shell::modal_keys::ModalKeys,
    sheet_keys: &crate::shell::sheet::SheetKeys,
    scene_keys: &crate::shell::session::SceneKeys,
    drag: &mut SelectionDrag,
) -> InputAction {
    let mut motion = crate::shell::composer_grid::CaretMotion::default();
    route_event_motion(
        event,
        input,
        cursor_position,
        &mut motion,
        dispatch,
        modal_keys,
        sheet_keys,
        scene_keys,
        drag,
    )
}
