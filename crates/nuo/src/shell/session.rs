//! The Conversation scene's self-owned keyboard scheme and resolver
//! (plane-less per, Stage-Scene-Overlay per).
//!
//! The Conversation scene — and its chat sibling, the zoomed TaskInspection —
//! owns the keys that act on its surface across its run states.
//! The keyboard is **plane-less**: there is no composer/transcript
//! mode to enter or leave. Every chord has one meaning; the focused step is a
//! transient selection driven by the `FocusPrevTarget`/`ClearFocusedTarget`
//! verbs (canonical `Alt+↑`/`Alt+↓`), typing always bounces to the draft, and
//! transcript scrolling (PgUp/PgDn/Home/End) is handled unconditionally by the
//! router. Before this module those keys were
//! scattered through the central `input` match as bare `active_modal == None`
//! branches that silently applied to every scene (that mirror is gone with
//! the router's overlay stack is the only foreground authority). They now live here as the
//! surface's own keybinding scheme: an executable resolver,
//! `resolve_chat_surface_key`, whose advertised hints
//! (`live_chat_hints`) share its single semantic origin.
//!
//! ## Layer contract
//!
//! The input router (`crate::shell::input::route_event`) offers the key to this
//! resolver **only** while the chat surface owns the keyboard (no modal, scene
//! is Conversation / TaskInspection). A `Some(action)` means the chat surface
//! consumed the key. A `None` falls through to the shared affordance library
//! (readline editing, caret motion, paste, scrolling) and the modal arms,
//! which stay central until each modal owns its own scheme.
use crate::shell::composer_grid::{CaretMotion, TextGrid, Vertical};
use crate::shell::keymap::{KeyCode, KeyModifiers};

use crate::shell::input::InputAction;
use crate::shell::keymap::LiveHint;

/// The scene schemes' own sub-state: the completion/selection
/// family plus the run-state and navigation facts the Session/Subagent/Side
/// resolvers arbitrate against. Built once per event by the caller; every
/// read below is scene-local, so nothing else leaks in.
#[derive(Debug, Default, Clone)]
pub struct SceneKeys {
    pub is_responding: bool,
    /// Target queue mode for the live composer while a round is running.
    pub composer_send_mode: crate::shell::app::ComposerSendMode,
    /// Which completion menu (slash command vs `@path` mention) is active, or
    /// `None` when no menu is shown. Drives Tab/↑/↓ cycling and the
    /// slash-specific Enter auto-accept. Mirrors [`crate::shell::CompletionKind`].
    pub completion_kind: crate::shell::CompletionKind,
    /// Whether the completion menu is currently hidden behind the Esc/Enter
    /// dismissal latch (`App::completion_dismissed`). Tab consults it to
    /// re-open a dismissed menu: Esc closes, Tab reopens — the toggle's
    /// other half.
    pub completion_dismissed: bool,
    /// Whether the composer still holds text a completion menu could anchor
    /// to — a partial `/command` or an `@mention` under the caret. Together
    /// with [`Self::completion_dismissed`] it decides whether Tab can bring
    /// a dismissed popup back: re-opening makes sense only when the trigger
    /// text survived.
    pub has_trigger_text: bool,
    pub suggestion_count: usize,
    pub suggestion_index: Option<usize>,
    pub has_exact_suggestion: bool,
    /// Whether the inline ↑/↓ recall pointer sits on a history row
    /// (`App::history_index.is_some()`). While true, Esc first cancels the
    /// recall (restoring the stashed draft) before any other Esc arm fires —
    /// the pointer is a transient navigation state and the universal
    /// "get me back" chord must be able to exit it.
    pub in_history_recall: bool,
    /// User remaps of the full-screen scene surface verbs (`session.*` dotted
    /// keys). The scene resolvers consult it; the composer hint row
    /// renders its effective bindings.
    pub surface_overrides: crate::shell::keymap::SurfaceOverrides,
    /// Whether a transcript step/action target holds keyboard focus behind
    /// the scene scheme.
    pub focused_target: bool,
    /// Whether the transcript holds browse focus.
    pub transcript_focused: bool,
    /// The width the composer lays its text out at, i.e. the grid the caret's
    /// ↑/↓ walk (see [`crate::shell::composer_grid::TextGrid`]). `0` before the
    /// first frame has published a composer rect, in which case the grid falls
    /// back to one row per logical line. Geometry belongs to the scene context
    /// because the interaction grammar ("↑ moves the caret one visual row") is
    /// defined against it: a resolver that guessed a width could disagree with
    /// what the panel paints.
    pub composer_text_width: usize,
    /// Whether the subagent the Subagent scene is focused on (the top of the
    /// focus stack) currently has a live round. Scene-local, exactly like the
    /// side view's [`Self::is_responding`]: the Subagent scene owns its own
    /// `Esc` interrupt, and it must NOT read the primary's
    /// `is_responding` (which would advertise an interrupt for an
    /// already-finished child whenever the parent round still runs).
    pub focused_subagent_running: bool,
}

impl SceneKeys {
    /// Whether the composer currently has keyboard focus on the chat surface.
    /// When a transcript step or transcript browse is focused, the composer is inactive.
    pub fn is_composer_active(&self) -> bool {
        !self.focused_target && !self.transcript_focused
    }
}

/// Run states the composer hint row advertises. (HistorySearch is a modal and
/// stays out of the chat scheme until that modal owns its own.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HintState {
    Idle,
    Command,
    Running(crate::shell::app::ComposerSendMode),
    Completion,
    /// The inline ↑/↓ recall pointer sits on a history row. The
    /// draft is stashed; the advertised chord set names the escape hatch
    /// (`Esc draft`) so the state is never a dead end.
    Recall,
    /// A transcript entry holds keyboard focus, so the next keypress acts on
    /// that entry rather than the composer. The row names the verbs that are
    /// live *only* in this state (`y` copy, `Shift-y` diagnostic, `Space`
    /// expand, `Esc` back) — the affordances that are otherwise undiscoverable
    /// because a bare letter pressed with no focus belongs to the composer.
    EntryFocused,
}

/// Short session id prefix (up to 8 characters) for compact display.
pub fn short_session_id(id: &str) -> String {
    let clean = id.trim();
    if clean.len() <= 8 {
        clean.to_string()
    } else {
        clean[..8].to_string()
    }
}

/// The chat surface's live chords for a run state — the single origin for the
/// composer hint row. Every chord returned here is resolvable by
/// [`resolve_chat_surface_key`] in that state (asserted by tests).
///
/// `toggle_mode_key` is the *effective* chord of the `toggle_send_mode` verb
/// the hint advertises exactly the binding that fires, so a user
/// remap is shown (canonical `Tab` when unremapped).
pub(crate) fn live_chat_hints(
    state: HintState,
    toggle_mode_key: crate::shell::keymap::Key,
) -> Vec<LiveHint> {
    use crate::shell::keymap::Key;
    let hints: &[LiveHint] = match state {
        HintState::Idle | HintState::Command | HintState::Recall => {
            &[LiveHint::action(Key::ENTER, "send")]
        }
        HintState::Running(crate::shell::app::ComposerSendMode::Steer) => &[
            LiveHint::nav(toggle_mode_key, "follow-up mode"),
            LiveHint::action(Key::ENTER, "send steer"),
        ],
        HintState::Running(crate::shell::app::ComposerSendMode::FollowUp) => &[
            LiveHint::nav(toggle_mode_key, "steer mode"),
            LiveHint::action(Key::ENTER, "queue follow-up"),
        ],
        HintState::Completion => &[
            LiveHint::nav(Key::ESC, "dismiss"),
            LiveHint::action(Key::TAB, "select"),
            LiveHint::action(Key::ENTER, "select"),
        ],
        // Entry focus: the copy verbs lead (they are the reason a user parks
        // focus here), then the universal toggle, then the way back. Every
        // chord here resolves in `resolve_chat_surface_key`'s focused-target
        // arm (asserted by `focused_entry_hints_are_resolvable`).
        HintState::EntryFocused => &[
            LiveHint::nav(Key::ESC, "back"),
            LiveHint::nav(Key::plain(' '), "expand"),
            LiveHint::action(Key::plain('y'), "copy"),
            LiveHint::action(Key::shift_code(KeyCode::Char('Y')), "diagnostic"),
        ],
    };
    hints.to_vec()
}

/// Resolve a key pressed while the chat surface owns the keyboard. Returns
/// `Some(action)` when the chat surface consumes the key, `None` to fall
/// through to the shared affordance library.
///
/// The surface *verb* chords (history recall, history walk, steer, step-focus
/// enter/clear, focused-step scroll, subagent siblings) resolve override-first
/// (step 9): each verb's canonical chord is replaced by the user's
/// `session.*` binding when one is configured, and the canonical chord goes
/// inactive. The interaction grammar (Enter/Tab/BackTab/Esc/↑/↓/text) is not
/// remappable.
pub(crate) fn resolve_chat_surface_key_motion(
    key: crate::shell::keymap::Key,
    keys: &SceneKeys,
    input: &mut String,
    cursor_position: &mut usize,
    motion: &mut CaretMotion,
) -> Option<InputAction> {
    use crate::shell::keymap::SurfaceVerb;
    let ov = &keys.surface_overrides;
    if keys.is_composer_active() {
        if ov.matches(key, SurfaceVerb::OpenHistory) {
            return Some(InputAction::OpenHistory);
        }
        if ov.matches(key, SurfaceVerb::HistoryPrev) {
            return Some(InputAction::HistoryPrev);
        }
        if ov.matches(key, SurfaceVerb::HistoryNext) {
            return Some(InputAction::HistoryNext);
        }
        if ov.matches(key, SurfaceVerb::ToggleSendMode)
            && keys.is_responding
            && (keys.completion_kind == crate::shell::completion::CompletionKind::None
                || keys.completion_dismissed)
        {
            return Some(InputAction::ToggleSendMode);
        }
    }
    if ov.matches(key, SurfaceVerb::FocusPrevTarget) {
        return Some(InputAction::FocusPrevTarget);
    }
    if ov.matches(key, SurfaceVerb::FocusNextTarget) {
        return Some(InputAction::FocusNextTarget);
    }
    if ov.matches(key, SurfaceVerb::ClearFocusedTarget)
        && (keys.focused_target || keys.transcript_focused)
    {
        return Some(InputAction::ClearFocusedTarget);
    }
    // Home / End scroll the transcript when target or browse focus is active (or if explicitly remapped).
    // When composer is active, bare Home / End fall through to readline line-start/end.
    if ov.is_remapped(SurfaceVerb::ScrollTop) && ov.matches(key, SurfaceVerb::ScrollTop) {
        return Some(InputAction::ScrollTop);
    }
    if ov.is_remapped(SurfaceVerb::ScrollBottom) && ov.matches(key, SurfaceVerb::ScrollBottom) {
        return Some(InputAction::ScrollBottom);
    }
    if keys.focused_target || keys.transcript_focused {
        if ov.matches(key, SurfaceVerb::ScrollTop) {
            return Some(InputAction::ScrollTop);
        }
        if ov.matches(key, SurfaceVerb::ScrollBottom) {
            return Some(InputAction::ScrollBottom);
        }
    }

    match key.code {
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
            resolve_enter(keys, input, cursor_position)
        }
        KeyCode::Tab => resolve_tab(keys),
        KeyCode::Esc => resolve_esc(keys, keys.is_responding, InputAction::Interrupt),
        KeyCode::Up => resolve_up(keys, input, cursor_position, motion),
        KeyCode::Down => resolve_down(keys, input, cursor_position, motion),
        // Only unmodified (or Shift-capitalized) characters are owned as
        // text; every Control/Alt/Super chord is a shared command chord
        // (readline editing, paste, …) handled by the router.
        // When a target is focused, `y`/`c` copy the target's content and
        // `Y`/`C` copy it as a diagnostic record. The two are deliberately
        // different chords rather than one chord with a modifier-read: a plain
        // `y` must stay the "give me the text" verb people already have in
        // muscle memory, while the record is the deliberate "give me something
        // I can paste into a report" verb.
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            if keys.focused_target && (c == 'y' || c == 'c') {
                Some(InputAction::CopyFocusedTarget)
            } else if keys.focused_target && (c == 'Y' || c == 'C') {
                Some(InputAction::CopyFocusedDiagnostic)
            } else if keys.focused_target && c == ' ' {
                // Space is the universal toggle in this TUI's dialogs
                // (MCP servers, permission rules), so it activates the focused
                // entry too. Without this arm Space silently fell into the
                // "focus swallows keys" branch below while the compaction card
                // advertised it.
                Some(InputAction::ActivateFocusedTarget)
            } else if keys.focused_target || keys.transcript_focused {
                Some(InputAction::None)
            } else {
                resolve_printable(keys, c, input, cursor_position)
            }
        }
        _ => None,
    }
}

/// The Subagent scene's own scheme: owns scene history navigation (`Alt+[` / `Alt+]`,
/// remappable `session.scene_back` / `session.scene_forward` verbs, ADR-0092 [INV-NAV-03]),
/// its **scene-scoped** `Esc` interrupt, and delegates every other key to the shared chat
/// core for step-focus walking.
pub(crate) fn resolve_subagent_key_motion(
    key: crate::shell::keymap::Key,
    keys: &SceneKeys,
    input: &mut String,
    cursor_position: &mut usize,
    motion: &mut CaretMotion,
) -> Option<InputAction> {
    use crate::shell::keymap::SurfaceVerb;
    let ov = &keys.surface_overrides;
    // Scene stack navigation: `Alt+[` / `Alt+]` (SurfaceVerb::SceneHistoryBack / Forward, ADR-0092 [INV-NAV-03])
    if ov.matches(key, SurfaceVerb::SceneHistoryBack) {
        return Some(InputAction::TabHistoryBack);
    }
    if ov.matches(key, SurfaceVerb::SceneHistoryForward) {
        return Some(InputAction::TabHistoryForward);
    }
    // Esc in the Subagent scene interrupts the *viewed child* (scene
    // scope) once a completion and step/browse focus are clear — never the
    // enclosing primary round. It keys off [`SceneKeys::focused_subagent_running`]
    // (this child's own liveness), not the primary `is_responding`, so a
    // finished child under a still-running parent advertises no interrupt and
    // falls through to the shared chat core (which clears step focus and is
    // idle-inert otherwise): `Esc` still never leaves the Scene
    // (`[INV-TUI-CLEAN-02]`). Every other key is the shared chat core —
    // `↑`/`↓` and `Alt+↑`/`Alt+↓` walk steps, `Enter` activates the focused
    // one, `Esc` clears focus, and printables reach the shared editing layer
    // exactly as they always have.
    if key.code == KeyCode::Esc {
        return resolve_esc(
            keys,
            keys.focused_subagent_running,
            InputAction::InterruptSubagent,
        );
    }
    resolve_chat_surface_key_motion(key, keys, input, cursor_position, motion)
}

/// Route a key to the full-screen scene's own scheme. Dashboard and
/// Settings do not own keyboard state yet and return `None`, so the router
/// falls through to the shared affordance library.
pub(crate) fn resolve_scene_key_motion(
    scene: impl Into<crate::shell::surfaces::SceneKind>,
    key: crate::shell::keymap::Key,
    keys: &SceneKeys,
    input: &mut String,
    cursor_position: &mut usize,
    motion: &mut CaretMotion,
) -> Option<InputAction> {
    match scene.into() {
        crate::shell::surfaces::SceneKind::Thread => {
            resolve_chat_surface_key_motion(key, keys, input, cursor_position, motion)
        }
        crate::shell::surfaces::SceneKind::Subagent => {
            resolve_subagent_key_motion(key, keys, input, cursor_position, motion)
        }
        _ => None,
    }
}

/// Enter on the chat surface. Mode-sensitive:
/// a focused step activates, a highlighted completion commits, a unique slash
/// prefix auto-accepts, otherwise the draft is sent — or queued while running.
fn resolve_enter(
    keys: &SceneKeys,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    if keys.focused_target {
        return Some(InputAction::ActivateFocusedTarget);
    }
    if keys.transcript_focused {
        return Some(InputAction::None);
    }
    // Slash-only: Enter on a unique prefix auto-accepts the first suggestion
    // rather than sending `/go` as a (rejected) command. Path mentions skip
    // this so Enter still sends the message.
    if keys.completion_kind == crate::shell::completion::CompletionKind::Slash
        && keys.suggestion_count > 0
        && keys.suggestion_index.is_none()
        && !keys.has_exact_suggestion
    {
        return Some(InputAction::CommitSuggestion("0".to_string()));
    }
    // An explicit highlight (via ↑/↓ or Tab) wins over the exact-match slash
    // fast path below.
    if let Some(i) = keys.suggestion_index
        && keys.completion_kind != crate::shell::completion::CompletionKind::None
    {
        return Some(InputAction::CommitSuggestion(i.to_string()));
    }
    let text = std::mem::take(input);
    *cursor_position = 0;
    // The documented escape hatch: exactly one leading space makes the draft
    // *prose* instead of a command. It has to be consumed, not trimmed off the
    // front of the prompt — both because the user typed it to mean "not a
    // command", and because shipping it leaves an invisible character at the
    // head of what the model sees. Two or more leading spaces are ordinary
    // formatting (an indented code block) and are left alone.
    let text = match text.strip_prefix(' ') {
        Some(rest) if !rest.starts_with(' ') => {
            return if rest.is_empty() {
                None
            } else {
                send_prose(keys, rest.to_string())
            };
        }
        _ => text,
    };
    if text.starts_with('/') {
        // Workspace views (Dashboard, Settings) are not thread-scoped slash commands
        // (ADR-0041 [INV-CMD-01], [INV-ROUTER-01]). Intercept and guide the operator.
        let trimmed = text.trim();
        if trimmed == "/dashboard" || trimmed.starts_with("/dashboard ") {
            return Some(InputAction::WorkspaceGuardrailNotice(
                "💡 Notice: Dashboard is a workspace view. Press 'C-x d' or switch tabs."
                    .to_string(),
            ));
        }
        if trimmed == "/settings"
            || trimmed.starts_with("/settings ")
            || trimmed == "/config"
            || trimmed.starts_with("/config ")
        {
            return Some(InputAction::WorkspaceGuardrailNotice(
                "💡 Notice: Settings is a workspace view. Press 'C-x ,' or switch tabs."
                    .to_string(),
            ));
        }

        // Match on the trimmed text so a slash command typed with a trailing
        // space (e.g. `/models `) still hits the exact-match arm.
        let action = match text.trim() {
            "/threads" | "/sessions" => InputAction::OpenSessions,
            "/models" => InputAction::OpenModels,
            "/connections" => InputAction::OpenConnections,
            "/permissions" => InputAction::OpenPermissions,
            "/tools" => InputAction::OpenTools,
            "/usage" => InputAction::OpenUsage,
            "/quota" | "/quotas" => InputAction::OpenQuotas,
            "/stats" => InputAction::OpenThreadStats,
            "/trace" => InputAction::OpenSessionTrace,
            "/mcp" => InputAction::OpenMcp,
            "/skills" => InputAction::OpenSkills,
            "/port" | "/ports" | "/share" => InputAction::OpenPortPalette,
            "/exit" => InputAction::Quit,
            _ => InputAction::SendSlash(text),
        };
        Some(action)
    } else {
        send_prose(keys, text)
    }
}

/// Submit `text` as a prompt: steered into the live round, queued behind it, or
/// sent as a fresh turn — the composer's send mode decides, not this function.
///
/// Extracted so the two ways a draft becomes prose (an ordinary non-slash
/// draft, and a `/`-prefixed draft escaped with a leading space) cannot pick
/// different send modes.
fn send_prose(keys: &SceneKeys, text: String) -> Option<InputAction> {
    if text.is_empty() {
        return None;
    }
    if keys.is_responding {
        match keys.composer_send_mode {
            crate::shell::app::ComposerSendMode::Steer => Some(InputAction::SteerImmediate(text)),
            crate::shell::app::ComposerSendMode::FollowUp => Some(InputAction::QueueFollowUp(text)),
        }
    } else {
        Some(InputAction::SendChat(text))
    }
}

/// Tab on the chat surface: commit a live completion, re-open a dismissed
/// menu, or toggle between steer and follow-up queue mode while running.
fn resolve_tab(keys: &SceneKeys) -> Option<InputAction> {
    if !keys.is_composer_active() {
        return None;
    }
    if keys.completion_kind != crate::shell::completion::CompletionKind::None
        && keys.suggestion_count > 0
        && !keys.has_exact_suggestion
        && !keys.completion_dismissed
    {
        let idx = keys.suggestion_index.unwrap_or(0);
        Some(InputAction::CommitSuggestion(idx.to_string()))
    } else if keys.completion_kind != crate::shell::completion::CompletionKind::None
        && keys.completion_dismissed
        && keys.has_trigger_text
        && !keys.is_responding
    {
        Some(InputAction::ReopenCompletion)
    } else if keys.is_responding
        && !keys
            .surface_overrides
            .is_remapped(crate::shell::keymap::SurfaceVerb::ToggleSendMode)
    {
        Some(InputAction::ToggleSendMode)
    } else {
        None
    }
}

/// Esc on the Conversation scene. Priority order mirrors the pre- central
/// arm: dismiss an open completion first, then clear step focus, then interrupt
/// a running round. Inline history recall is preserved across Esc (so edits
/// are not lost and interrupt is not intercepted; Ctrl-c clears the input).
///
/// `responding` and `interrupt` are the caller's scene-scoped liveness and
/// interrupt target: the Conversation scene passes its own `is_responding` and
/// [`InputAction::Interrupt`]; the Subagent scene passes the *viewed child's*
/// liveness and [`InputAction::InterruptSubagent`], so a finished child never
/// emits the primary interrupt (scene scope). The earlier priority
/// arms (dismiss completion, clear focus) are identical in both cases.
fn resolve_esc(keys: &SceneKeys, responding: bool, interrupt: InputAction) -> Option<InputAction> {
    if keys.completion_kind != crate::shell::completion::CompletionKind::None
        && !keys.completion_dismissed
    {
        Some(InputAction::CloseCompletion)
    } else if keys.focused_target || keys.transcript_focused {
        Some(InputAction::ClearFocusedTarget)
    } else if keys.completion_kind != crate::shell::completion::CompletionKind::None
        && keys.suggestion_count > 0
        && !keys.completion_dismissed
    {
        Some(InputAction::CloseCompletion)
    } else if responding {
        Some(interrupt)
    } else {
        None
    }
}

/// The grid this scene's caret motions walk: the composer's wrapped rows at
/// the width the caller published. `0` means "no frame has measured the panel
/// yet" — the grid then falls back to one row per logical line rather than
/// guessing a width.
fn composer_grid(keys: &SceneKeys, input: &str) -> TextGrid {
    match keys.composer_text_width {
        0 => TextGrid::unwrapped(input),
        width => TextGrid::new(input, width),
    }
}

/// ↑ on the chat surface:
/// - When a transcript step is focused, walk focus to the previous step.
/// - When transcript browse focus is active, scroll the transcript up.
/// - Otherwise, walk completion suggestions, move the caret one **visual row**
///   up (keeping its display column), or at the top row hand off to inline
///   history recall.
///
/// Visual rows, not `\n`-separated lines: the caret must stop on every row the
/// panel paints — including the empty rows a Ctrl-J leaves behind — and must
/// never skip the continuation rows of a soft-wrapped line.
fn resolve_up(
    keys: &SceneKeys,
    input: &str,
    cursor_position: &mut usize,
    motion: &mut CaretMotion,
) -> Option<InputAction> {
    if keys.focused_target {
        Some(InputAction::FocusPrevTarget)
    } else if keys.transcript_focused {
        Some(InputAction::ScrollUp)
    } else if keys.completion_kind != crate::shell::completion::CompletionKind::None
        && keys.suggestion_count > 0
        && !keys.has_exact_suggestion
    {
        Some(InputAction::SuggestPrev)
    } else if let Some(target) =
        composer_grid(keys, input).shift_row(*cursor_position, Vertical::Up, motion)
    {
        *cursor_position = target;
        Some(InputAction::None)
    } else {
        // Top row: readline-style edge hand-off to inline history recall.
        Some(InputAction::HistoryPrev)
    }
}

/// ↓ on the chat surface:
/// - When a transcript step is focused, walk focus to the next step.
/// - When transcript browse focus is active, scroll the transcript down.
/// - Otherwise, walk completion suggestions, move the caret one **visual row**
///   down (keeping its display column), or at the bottom row hand off to newer
///   history recall / the stashed draft.
fn resolve_down(
    keys: &SceneKeys,
    input: &str,
    cursor_position: &mut usize,
    motion: &mut CaretMotion,
) -> Option<InputAction> {
    if keys.focused_target {
        Some(InputAction::FocusNextTarget)
    } else if keys.transcript_focused {
        Some(InputAction::ScrollDown)
    } else if keys.completion_kind != crate::shell::completion::CompletionKind::None
        && keys.suggestion_count > 0
        && !keys.has_exact_suggestion
    {
        Some(InputAction::SuggestNext)
    } else if let Some(target) =
        composer_grid(keys, input).shift_row(*cursor_position, Vertical::Down, motion)
    {
        *cursor_position = target;
        Some(InputAction::None)
    } else {
        // Last line: walk history forward (or restore the stashed draft once
        // the newest entry is passed).
        Some(InputAction::HistoryNext)
    }
}

/// A printable character on the chat surface. When transcript or a step target is focused,
/// characters do not bounce to the composer: focus returns to the composer only explicitly
/// via `Esc` or a mouse click. Subagent sibling navigation (`[`/`]`) is owned by the Subagent
/// scene's resolver.
fn resolve_printable(
    keys: &SceneKeys,
    _c: char,
    _input: &mut String,
    _cursor_position: &mut usize,
) -> Option<InputAction> {
    if keys.focused_target || keys.transcript_focused {
        return Some(InputAction::None);
    }
    None
}

/// Resolve a scene-scoped key with a stateless caret memory.
///
/// The thread surface passes its persistent [`CaretMotion`] through
/// [`resolve_scene_key_motion`]; callers that hold no composer (tests, one-shot
/// dispatch) use this.
#[allow(dead_code)] // consumed by the interaction tests
pub(crate) fn resolve_scene_key(
    scene: impl Into<crate::shell::surfaces::SceneKind>,
    key: crate::shell::keymap::Key,
    keys: &SceneKeys,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    let mut motion = CaretMotion::default();
    resolve_scene_key_motion(scene, key, keys, input, cursor_position, &mut motion)
}

/// Stateless [`resolve_chat_surface_key_motion`].
#[allow(dead_code)] // consumed by the interaction tests
pub(crate) fn resolve_chat_surface_key(
    key: crate::shell::keymap::Key,
    keys: &SceneKeys,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    let mut motion = CaretMotion::default();
    resolve_chat_surface_key_motion(key, keys, input, cursor_position, &mut motion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::input::route_event_motion;
    use crate::shell::surfaces::SceneKind;
    use crossterm::event::{Event, KeyEvent, KeyEventKind, KeyEventState};

    /// Run states used to build a context for resolver tests. Test-local so
    /// the shipped module carries no dead types (the resolver is
    /// the scheme; the mode table is a test concern).
    #[derive(Clone, Copy, Debug)]
    enum Mode {
        Idle,
        Running,
        FocusedTarget,
        Completion,
        Subagent,
    }

    /// A mode-appropriate chat-surface scene-keys bundle for direct resolver
    /// tests.
    fn ctx(mode: Mode, tune: impl FnOnce(&mut SceneKeys)) -> SceneKeys {
        let mut c = SceneKeys::default();
        match mode {
            Mode::Idle => {}
            Mode::Running => c.is_responding = true,
            Mode::FocusedTarget => c.focused_target = true,
            Mode::Completion => {
                c.completion_kind = crate::shell::completion::CompletionKind::Slash;
                c.suggestion_count = 3;
                c.suggestion_index = Some(1);
            }
            Mode::Subagent => {}
        }
        tune(&mut c);
        c
    }

    /// The scene a Mode stands in (surface dispatch keys off the explicit
    /// scene).
    fn scene_of(mode: Mode) -> SceneKind {
        match mode {
            Mode::Subagent => SceneKind::Subagent,
            _ => SceneKind::Thread,
        }
    }

    /// Route a chord through the real `route_event` pipeline to confirm the
    /// resolver is reached for the chat surface (layer wiring).
    use crate::shell::keymap::Key;

    fn process(
        scene: SceneKind,
        code: KeyCode,
        modifiers: KeyModifiers,
        mode: Mode,
    ) -> InputAction {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = nuo_view::model::selection::SelectionDrag::default();
        let keys = ctx(mode, |_| {});
        let dispatch = crate::shell::input::Dispatch {
            scene,
            ..Default::default()
        };
        crate::shell::input::route_event_motion(
            Event::Key(crate::shell::input::keystroke::event_from_chord(Key {
                code,
                modifiers,
            })),
            &mut input,
            &mut cursor,
            &mut crate::shell::composer_grid::CaretMotion::default(),
            dispatch,
            &crate::shell::modal_keys::ModalKeys::default(),
            &crate::shell::sheet::SheetKeys::default(),
            &keys,
            &mut drag,
        )
    }

    #[test]
    fn every_owned_chord_resolves_in_its_mode() {
        // The chat surface's owned chords, paired with the run state they act
        // in. Each must resolve (an owned chord is never dead).
        // Esc is an *interrupt* on every scene, so its owned entries are the
        // running states — a scene exit is not an Esc arm.
        let owned: &[(crate::shell::keymap::Key, Mode)] = &[
            (crate::shell::keymap::Key::ENTER, Mode::Idle),
            (crate::shell::keymap::Key::ENTER, Mode::Running),
            (crate::shell::keymap::Key::ENTER, Mode::FocusedTarget),
            (crate::shell::keymap::Key::ESC, Mode::Running),
            (crate::shell::keymap::Key::ESC, Mode::FocusedTarget),
            (crate::shell::keymap::Key::TAB, Mode::Running),
            (crate::shell::keymap::Key::CTRL_R, Mode::Idle),
            (crate::shell::keymap::Key::ALT_P, Mode::Idle),
            (crate::shell::keymap::Key::ALT_N, Mode::Idle),
            (crate::shell::keymap::Key::CTRL_P, Mode::Idle),
            (crate::shell::keymap::Key::CTRL_N, Mode::FocusedTarget),
        ];
        for (key, mode) in owned {
            let c = ctx(*mode, |_| {});
            let mut input = String::from("hi");
            let mut cursor = input.chars().count();
            let resolved = resolve_scene_key(scene_of(*mode), *key, &c, &mut input, &mut cursor);
            assert!(
                resolved.is_some(),
                "owned chord {key:?} did not resolve in {mode:?}"
            );
        }

        // The subagent scene owns Esc only while its *viewed* round is
        // interruptible; idle, it is inert (it has no Esc exit). It reads its
        // own scene-local liveness — the viewed child's
        // `focused_subagent_running` — never the primary's `is_responding`.
        let (mode, expected) = (Mode::Subagent, InputAction::InterruptSubagent);
        {
            let mut c = ctx(mode, |c| {
                c.is_responding = true;
                c.focused_subagent_running = true;
            });
            let mut input = String::new();
            let mut cursor = 0;
            let resolved = resolve_scene_key(
                scene_of(mode),
                crate::shell::keymap::Key::ESC,
                &c,
                &mut input,
                &mut cursor,
            );
            assert_eq!(
                resolved,
                Some(expected),
                "a running {mode:?} scene owns Esc as its *scene-scoped* interrupt"
            );
            c.is_responding = false;
            c.focused_subagent_running = false;
            assert!(
                resolve_scene_key(
                    scene_of(mode),
                    crate::shell::keymap::Key::ESC,
                    &c,
                    &mut input,
                    &mut cursor
                )
                .is_none(),
                "an idle {mode:?} scene has no Esc arm at all"
            );
        }
    }

    #[test]
    fn global_and_modal_keys_are_not_session_owned() {
        // Readline/editing and navigation chords are shared, not owned by the
        // chat surface — the resolver must leave them for the router.
        for key in [
            crate::shell::keymap::Key::CTRL_A,
            crate::shell::keymap::Key::CTRL_G,
            crate::shell::keymap::Key::CTRL_W,
            crate::shell::keymap::Key::CTRL_L,
            crate::shell::keymap::Key::PAGE_UP,
            crate::shell::keymap::Key::PAGE_DOWN,
        ] {
            let c = ctx(Mode::Idle, |_| {});
            let mut input = String::from("x");
            let mut cursor = 1;
            assert!(
                resolve_chat_surface_key(key, &c, &mut input, &mut cursor).is_none(),
                "{key:?} must not be owned by the chat surface"
            );
        }

        // Ctrl+P and Ctrl+N are canonical focus navigation verbs owned by the chat surface.
        let c = ctx(Mode::Idle, |_| {});
        let mut input = String::from("x");
        let mut cursor = 1;
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::CTRL_P,
                &c,
                &mut input,
                &mut cursor
            ),
            Some(InputAction::FocusPrevTarget)
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::CTRL_N,
                &c,
                &mut input,
                &mut cursor
            ),
            Some(InputAction::FocusNextTarget)
        );
    }

    #[test]
    fn enter_multi_mode_reactions() {
        // Idle + text → send.
        let action = {
            let mut input = String::from("hello");
            let mut cursor = 5;
            resolve_chat_surface_key(
                crate::shell::keymap::Key::ENTER,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor,
            )
        };
        assert_eq!(action, Some(InputAction::SendChat("hello".into())));

        // Running + text in Steer mode (default) → steer immediate.
        let action = {
            let mut input = String::from("next");
            let mut cursor = 4;
            resolve_chat_surface_key(
                crate::shell::keymap::Key::ENTER,
                &ctx(Mode::Running, |_| {}),
                &mut input,
                &mut cursor,
            )
        };
        assert_eq!(action, Some(InputAction::SteerImmediate("next".into())));

        // Running + text in FollowUp mode → queue follow-up.
        let action = {
            let mut input = String::from("next");
            let mut cursor = 4;
            resolve_chat_surface_key(
                crate::shell::keymap::Key::ENTER,
                &ctx(Mode::Running, |c| {
                    c.composer_send_mode = crate::shell::app::ComposerSendMode::FollowUp;
                }),
                &mut input,
                &mut cursor,
            )
        };
        assert_eq!(action, Some(InputAction::QueueFollowUp("next".into())));

        // Focused step → activate, ignoring the draft.
        let action = {
            let mut input = String::from("draft");
            let mut cursor = 5;
            resolve_chat_surface_key(
                crate::shell::keymap::Key::ENTER,
                &ctx(Mode::FocusedTarget, |_| {}),
                &mut input,
                &mut cursor,
            )
        };
        assert_eq!(action, Some(InputAction::ActivateFocusedTarget));

        // Highlighted completion → commit the highlighted item.
        let action = resolve_chat_surface_key(
            crate::shell::keymap::Key::ENTER,
            &ctx(Mode::Completion, |_| {}),
            &mut String::new(),
            &mut 0,
        );
        assert_eq!(action, Some(InputAction::CommitSuggestion("1".into())));
    }

    #[test]
    fn focus_navigation_and_interrupt_react_to_mode() {
        // Esc interrupts only while running.
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::ESC,
                &ctx(Mode::Running, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::Interrupt)
        );
        // Esc clears focus while a step is focused, even when running.
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::ESC,
                &ctx(Mode::FocusedTarget, |c| c.is_responding = true),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::ClearFocusedTarget)
        );
        // Tab without completion is inert (completion-only).
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::TAB,
                &ctx(Mode::Idle, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            None
        );
        // While a step is focused, bare ↑ steps to the previous target.
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::UP,
                &ctx(Mode::FocusedTarget, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::FocusPrevTarget)
        );
        // While a step is focused, bare ↓ steps to the next target.
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::DOWN,
                &ctx(Mode::FocusedTarget, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::FocusNextTarget)
        );
        // While transcript browse focus is active, bare ↑/↓ scroll the transcript.
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::UP,
                &ctx(Mode::Idle, |c| c.transcript_focused = true),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::ScrollUp)
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::DOWN,
                &ctx(Mode::Idle, |c| c.transcript_focused = true),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::ScrollDown)
        );
        // While idle in composer, bare ↑ hands off to inline history recall.
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::UP,
                &ctx(Mode::Idle, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::HistoryPrev)
        );
        // Tab toggles send mode only while running.
        assert!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::TAB,
                &ctx(Mode::Idle, |_| {}),
                &mut String::from("steer"),
                &mut 5,
            )
            .is_none()
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::TAB,
                &ctx(Mode::Running, |_| {}),
                &mut String::from("steer"),
                &mut 5,
            ),
            Some(InputAction::ToggleSendMode)
        );
    }

    #[test]
    fn typed_character_in_transcript_is_inert_and_does_not_bounce() {
        let mut input = String::from("");
        let mut cursor = 0;
        let action = resolve_chat_surface_key(
            crate::shell::keymap::Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('x'),
            },
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut input,
            &mut cursor,
        );
        assert_eq!(action, Some(InputAction::None));
        assert_eq!(input, "", "typed char must not pollute the composer");
    }

    #[test]
    fn copy_keys_act_on_focused_target() {
        let mut input = String::from("");
        let mut cursor = 0;
        // 'y' copies focused target content without bouncing
        let action = resolve_chat_surface_key(
            crate::shell::keymap::Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('y'),
            },
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut input,
            &mut cursor,
        );
        assert_eq!(action, Some(InputAction::CopyFocusedTarget));
        assert_eq!(input, "", "yank key must not pollute composer input");

        // 'c' also copies focused target
        let action = resolve_chat_surface_key(
            crate::shell::keymap::Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('c'),
            },
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut input,
            &mut cursor,
        );
        assert_eq!(action, Some(InputAction::CopyFocusedTarget));
        assert_eq!(input, "", "copy key must not pollute composer input");
    }

    /// `Shift-y` / `Shift-C` are the diagnostic-copy chord, and they are the
    /// *only* difference from `y`/`c`: the lowercase verb keeps its historical
    /// "give me the text" meaning, so muscle memory is never silently upgraded
    /// into a different payload.
    #[test]
    fn shift_copy_keys_emit_a_diagnostic_for_the_focused_target() {
        for code in [KeyCode::Char('Y'), KeyCode::Char('C')] {
            let mut input = String::from("");
            let mut cursor = 0;
            let action = resolve_chat_surface_key(
                crate::shell::keymap::Key {
                    modifiers: KeyModifiers::SHIFT,
                    code,
                },
                &ctx(Mode::FocusedTarget, |_| {}),
                &mut input,
                &mut cursor,
            );
            assert_eq!(
                action,
                Some(InputAction::CopyFocusedDiagnostic),
                "{code:?} must request a diagnostic"
            );
            assert_eq!(input, "", "copy keys must never pollute the composer");
        }
    }

    /// Hints and dispatch share one origin: every chord the entry-focus row
    /// advertises must actually resolve to an action while an entry is focused.
    /// Without this, the newly-discoverable copy verbs could advertise a dead
    /// key the moment the resolver changed.
    #[test]
    fn focused_entry_hints_are_resolvable() {
        for hint in live_chat_hints(HintState::EntryFocused, crate::shell::keymap::Key::TAB) {
            let mut input = String::new();
            let mut cursor = 0;
            let resolved = resolve_chat_surface_key(
                hint.key,
                &ctx(Mode::FocusedTarget, |_| {}),
                &mut input,
                &mut cursor,
            );
            assert!(
                resolved.is_some(),
                "entry-focus hint {key:?} did not resolve while an entry is focused",
                key = hint.key
            );
            assert_eq!(
                input,
                "",
                "hint chord {key:?} leaked into the composer",
                key = hint.key
            );
        }
    }

    #[test]
    fn router_offers_chat_keys_only_on_chat_surfaces() {
        use crate::shell::input::InputAction;
        // Tab on the Thread scene without a completion is inert (
        // the chord belongs to completion, not to plane switching).
        assert_eq!(
            process(
                SceneKind::Thread,
                KeyCode::Tab,
                KeyModifiers::NONE,
                Mode::Idle
            ),
            InputAction::None
        );
        // On the Settings scene the resolver is never consulted: Tab is inert.
        assert_eq!(
            process(
                SceneKind::Settings,
                KeyCode::Tab,
                KeyModifiers::NONE,
                Mode::Idle
            ),
            InputAction::None
        );
        // A subagent step's Enter still activates through the shared path.
        assert_eq!(
            process(
                SceneKind::Subagent,
                KeyCode::Enter,
                KeyModifiers::NONE,
                Mode::FocusedTarget
            ),
            InputAction::ActivateFocusedTarget
        );
    }

    /// Read the current key-handling region's Enter behavior as a canary:
    /// after the extraction the central match must no longer send the draft.
    #[test]
    fn advertised_hints_are_resolvable_in_their_state() {
        // Every chord `live_chat_hints` advertises for a run state must be
        // consumable by the resolver in that state (hints and
        // dispatch share a semantic origin).
        use crate::shell::keymap::Key;

        // Idle.
        let c = ctx(Mode::Idle, |_| {});
        let mut input = String::from("hi");
        assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut input, &mut 2).is_some());

        // Running.
        let c = ctx(Mode::Running, |_| {});
        let mut input = String::from("next");
        assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut input, &mut 4).is_some());
        let mut input = String::from("steer");
        assert!(resolve_chat_surface_key(Key::TAB, &c, &mut input, &mut 5).is_some());

        // Completion.
        let c = ctx(Mode::Completion, |_| {});
        assert!(resolve_chat_surface_key(Key::ESC, &c, &mut String::new(), &mut 0).is_some());
        assert!(resolve_chat_surface_key(Key::TAB, &c, &mut String::new(), &mut 0).is_some());
        assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut String::new(), &mut 0).is_some());
    }

    #[test]
    fn surface_overrides_remap_verbs_and_kill_canonical_chords() {
        use crate::shell::keymap::{Key, SurfaceOverrides};

        let mut map = std::collections::HashMap::new();
        map.insert("open_history".to_string(), "ctrl+shift+r".to_string());
        map.insert("toggle_send_mode".to_string(), "ctrl+t".to_string());
        map.insert("scene_back".to_string(), "alt+b".to_string());
        let ov = SurfaceOverrides::from_config(&map);
        let c = ctx(Mode::Idle, |c| c.surface_overrides = ov.clone());

        // The assigned chord fires; the canonical chord is dead.
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::CTRL_SHIFT_R,
                &c,
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::OpenHistory)
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::CTRL_R,
                &c,
                &mut String::new(),
                &mut 0
            ),
            None,
            "the canonical Ctrl+R must go inactive once open_history is remapped"
        );

        // ToggleSendMode's guard still applies on the remapped chord: fires only while
        // running.
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::CTRL_T,
                &ctx(Mode::Running, |c| c.surface_overrides = ov.clone()),
                &mut String::from("steer"),
                &mut 5
            ),
            Some(InputAction::ToggleSendMode)
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::CTRL_T,
                &c,
                &mut String::from("steer"),
                &mut 5
            ),
            None,
            "toggle_send_mode on a remapped chord must still require a running round"
        );

        // Subagent scene back follows the remapped chord, same guards.
        let rc = ctx(Mode::Subagent, |c| c.surface_overrides = ov);
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                Key::alt('b'),
                &rc,
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::TabHistoryBack)
        );
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                Key::ALT_BRACKET_LEFT,
                &rc,
                &mut String::new(),
                &mut 0
            ),
            None,
            "the canonical Alt+[ must go inactive once scene_back is remapped"
        );
    }

    #[test]
    fn subagent_and_side_never_exit_on_esc_and_keep_their_navigation() {
        use crate::shell::keymap::Key;

        // Subagent: Esc clears step focus, and otherwise resolves to nothing —
        // it never exits the scene ([INV-TUI-CLEAN-02]). Alt+[ / Alt+] is the canonical navigation.
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                Key::ESC,
                &ctx(Mode::Subagent, |_| {}),
                &mut String::new(),
                &mut 0
            ),
            None,
            "an idle zoom has no Esc arm; leaving is the `C-x` namespace"
        );
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                Key::ALT_BRACKET_LEFT,
                &ctx(Mode::Subagent, |_| {}),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::TabHistoryBack),
            "`Alt+[` in subagent view steps back to parent thread (ADR-0092)"
        );
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                Key::ALT_BRACKET_RIGHT,
                &ctx(Mode::Subagent, |_| {}),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::TabHistoryForward),
            "`Alt+]` in subagent view steps forward in scene history (ADR-0092)"
        );
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                Key::ESC,
                &ctx(Mode::Subagent, |c| c.focused_target = true),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::ClearFocusedTarget),
            "subagent Esc clears the focused step"
        );
        // Scene scope (the regression this guards): a Subagent-scene Esc must
        // NEVER resolve to the primary `Interrupt`. When the viewed child has
        // finished but the *primary* round still runs, the zoom's Esc is inert —
        // it must not penetrate the scene boundary and stop the outer round.
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                Key::ESC,
                &ctx(Mode::Subagent, |c| {
                    c.is_responding = true; // the *primary* is running
                    c.focused_subagent_running = false; // the viewed child finished
                }),
                &mut String::new(),
                &mut 0
            ),
            None,
            "a finished viewed child under a running primary owns no Esc: the \
             primary interrupt must not leak through the Subagent scene"
        );
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                Key::ESC,
                &ctx(Mode::Subagent, |c| {
                    c.is_responding = true;
                    c.focused_subagent_running = true;
                }),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::InterruptSubagent),
            "a running viewed child owns Esc as the scene-scoped interrupt"
        );
        // Raw `[` and `q` no longer trigger scene navigation (ADR-0092 [INV-NAV-03])
        let bracket = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char('['),
        };
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                bracket,
                &ctx(Mode::Subagent, |_| {}),
                &mut String::new(),
                &mut 0
            ),
            None,
            "raw `[` falls through to normal input"
        );
        let q_key = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char('q'),
        };
        assert_eq!(
            resolve_scene_key(
                SceneKind::Subagent,
                q_key,
                &ctx(Mode::Subagent, |_| {}),
                &mut String::new(),
                &mut 0
            ),
            None,
            "raw `q` falls through to normal input"
        );

        // The Thread scene never emits the subagent exits.
        let c = ctx(Mode::FocusedTarget, |_| {});
        let action = resolve_scene_key(SceneKind::Thread, Key::ESC, &c, &mut String::new(), &mut 0);
        assert_ne!(action, Some(InputAction::CloseTab));
    }

    /// Esc while the inline ↑/↓ pointer sits on a history row
    /// cancels the recall (restoring the stashed draft) — the recall state
    /// must be escapable by the universal "get me back" chord, and the
    /// Esc does not cancel inline history recall (Ctrl-C clears instead).
    /// When running/responding, Esc resolves to Interrupt without getting intercepted.
    #[test]
    fn esc_does_not_cancel_inline_history_recall() {
        let mut c = ctx(Mode::Idle, |_| {});
        c.in_history_recall = true;
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::ESC,
                &c,
                &mut String::new(),
                &mut 0
            ),
            None
        );

        // When running/responding, Esc resolves to Interrupt rather than clearing recall
        let mut c = ctx(Mode::Running, |_| {});
        c.in_history_recall = true;
        c.is_responding = true;
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::ESC,
                &c,
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::Interrupt)
        );
    }

    /// Every chord the recall hint set advertises resolves in the
    /// recall state (hints and dispatch share one semantic origin).
    #[test]
    fn recall_hints_are_resolvable_in_the_recall_state() {
        for hint in live_chat_hints(HintState::Recall, crate::shell::keymap::Key::TAB) {
            let mut c = ctx(Mode::Idle, |_| {});
            c.in_history_recall = true;
            let mut input = String::from("recalled");
            let mut cursor = input.chars().count();
            let resolved = resolve_chat_surface_key(hint.key, &c, &mut input, &mut cursor);
            assert!(
                resolved.is_some(),
                "recall hint chord {key:?} did not resolve in the recall state",
                key = hint.key
            );
        }
    }

    /// The readline-style edge hand-off. On a single-line draft ↑
    /// resolves to `HistoryPrev` and ↓ to `HistoryNext` once completion and
    /// caret motion have had their chance; a multi-line draft only hands off
    /// from its true first/last line.
    #[test]
    fn arrow_edge_hands_off_to_history_recall() {
        // Single-line draft: both edges hand off immediately.
        let mut input = String::from("hello");
        let mut cursor = 5;
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::UP,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::HistoryPrev)
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::DOWN,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::HistoryNext)
        );

        // Multi-line draft, caret on the middle line: no hand-off.
        let mut input = String::from("one\ntwo\nthree");
        let mut cursor = 5; // on "two"
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::UP,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::None)
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::DOWN,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::None)
        );

        // Caret on the first line of a multi-line draft: ↑ hands off without
        // disturbing the draft; ↓ stays a caret motion.
        let mut input = String::from("one\ntwo");
        let mut cursor = 2;
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::UP,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::HistoryPrev)
        );
        assert_eq!(input, "one\ntwo", "the hand-off must not mutate the draft");
        let mut cursor = 0;
        assert_eq!(
            resolve_chat_surface_key(
                crate::shell::keymap::Key::DOWN,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::None)
        );
        assert_eq!(
            cursor, 4,
            "↓ moved the caret to the next line (column kept)"
        );
    }

    #[test]
    fn central_match_no_longer_owns_chat_enter_on_settings() {
        // Enter on the Settings scene (a full-screen destination)
        // is owned by that scene's own scheme (ConfigActivate), NOT the chat
        // surface's send path — the draft must never be shipped from Settings.
        let mut input = String::from("draft");
        let mut cursor = 5;
        let mut drag = nuo_view::model::selection::SelectionDrag::default();
        let action = route_event_motion(
            Event::Key(KeyEvent {
                code: crossterm::event::KeyCode::Enter,
                modifiers: crossterm::event::KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            &mut input,
            &mut cursor,
            &mut crate::shell::composer_grid::CaretMotion::default(),
            crate::shell::input::Dispatch {
                scene: SceneKind::Settings,
                ..Default::default()
            },
            &crate::shell::modal_keys::ModalKeys::default(),
            &crate::shell::sheet::SheetKeys::default(),
            &crate::shell::session::SceneKeys::default(),
            &mut drag,
        );
        assert_eq!(
            action,
            InputAction::ConfigActivate,
            "Enter on the Settings scene activates its focused item, never ships the draft"
        );
        assert_eq!(input, "draft", "the draft is never sent or mutated");
    }
}
