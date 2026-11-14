//! The chat scenes' self-owned keyboard scheme and resolver.
//!
//! The Conversation scene — and its chat sibling, the zoomed TaskInspection —
//! owns the keys that act on its surface across its run states, and owns them
//! as **two peer focus planes** ([ADR-0145]): the composer (drafting) and the
//! transcript (inspecting). Neither is a mode of the other.
//!
//! - `Shift-Tab` is the sole explicit crossing chord and is an **involution**: the
//!   same press returns, remembering the transcript's highlighted entry
//!   (`[INV-FOCUS-01]`).
//! - `Esc` never crosses planes. It dismisses an overlay, dismisses a
//!   completion, interrupts a running round — and nothing else. The legacy
//!   "Esc gets you back to the composer" arm is deleted, not deprecated.
//! - `Ctrl+P`/`Ctrl+N` walk the transcript's entry highlight; walking off
//!   either end with nothing left to scroll is the *implicit* crossing back.
//! - Printable characters are inserted only while the composer plane owns the
//!   keyboard; on the transcript plane they are inert (never silently dropped
//!   into a hidden draft).
//!
//! Before this module those keys were scattered through the central `input`
//! match as bare `active_modal == None` branches that silently applied to every
//! scene (that mirror is gone with the router's overlay stack is the only
//! foreground authority). They now live here as the surface's own keybinding
//! scheme: an executable resolver, `resolve_chat_surface_key`, whose advertised
//! hints (`live_chat_hints`) share its single semantic origin.
//!
//! [ADR-0145]: ../../../docs/adr/0145-composer-and-transcript-are-peer-focus-planes.md
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
    /// with `Self::completion_dismissed` it decides whether Tab can bring
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
    /// Which peer focus plane of the chat scene owns the keyboard
    /// ([`crate::shell::focus::FocusPlane`], ADR-0145).
    ///
    /// One value replaces the legacy `focused_target`/`transcript_focused`
    /// pair. To the resolver the two transcript sub-states (browse, entry
    /// highlight) are indistinguishable — they agree on every arbitration
    /// question — so the plane is all this bundle needs to carry.
    pub focus: crate::shell::focus::FocusPlane,
    /// The highlighted entry, when the transcript plane holds an entry
    /// highlight. `None` on the composer plane and while browsing.
    pub entry_focus: Option<nuo_view::model::layout::InteractiveTarget>,
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
    /// side view's `Self::is_responding`: the Subagent scene owns its own
    /// `Esc` interrupt, and it must NOT read the primary's
    /// `is_responding` (which would advertise an interrupt for an
    /// already-finished child whenever the parent round still runs).
    pub focused_subagent_running: bool,
}

impl SceneKeys {
    /// Whether the composer plane owns the keyboard on the chat surface.
    pub fn is_composer_active(&self) -> bool {
        self.focus == crate::shell::focus::FocusPlane::Composer
    }

    /// Whether the transcript plane owns the keyboard (browse or entry).
    pub fn is_transcript_active(&self) -> bool {
        self.focus == crate::shell::focus::FocusPlane::Transcript
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
    /// that entry rather than the composer.
    ///
    /// The advertised verbs are decided by *what kind of entry* is focused, not
    /// assumed: a collapsible step expands in place, while a subagent task has
    /// no inline body to reveal — activation drills into its scene. The row
    /// used to promise `Space expand` for every focused entry, which was simply
    /// false for a subagent and sent the user looking for a disclosure that the
    /// renderer never draws.
    EntryFocused {
        /// Whether the focused entry is a subagent task (drill-in instead of
        /// disclose).
        subagent: bool,
    },
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
    // Every row on the composer plane names the chord that crosses to the
    // transcript, because that crossing is the plane's only exit and the row is
    // where a user learns it (ADR-0145). `Shift-Tab` is grammar rather than a
    // remappable verb, so the keycap is the literal `Key::BACKTAB`.
    let hints: &[LiveHint] = match state {
        HintState::Idle | HintState::Command | HintState::Recall => &[
            LiveHint::nav(Key::BACKTAB, "transcript"),
            LiveHint::action(Key::ENTER, "send"),
        ],
        HintState::Running(crate::shell::app::ComposerSendMode::Steer) => &[
            LiveHint::nav(toggle_mode_key, "follow-up mode"),
            LiveHint::nav(Key::BACKTAB, "transcript"),
            LiveHint::action(Key::ENTER, "send steer"),
        ],
        HintState::Running(crate::shell::app::ComposerSendMode::FollowUp) => &[
            LiveHint::nav(toggle_mode_key, "steer mode"),
            LiveHint::nav(Key::BACKTAB, "transcript"),
            LiveHint::action(Key::ENTER, "queue follow-up"),
        ],
        HintState::Completion => &[
            LiveHint::nav(Key::ESC, "dismiss"),
            LiveHint::action(Key::TAB, "select"),
            LiveHint::action(Key::ENTER, "select"),
        ],
        // Entry focus: the copy verbs lead (they are the reason a user parks
        // attention here), then the universal toggle, then the way back — and
        // the way back is `Shift-Tab`, never `Esc`: `Esc` does not cross planes.
        // Every chord here resolves in `resolve_chat_surface_key`'s entry arm
        // (asserted by `focused_entry_hints_are_resolvable`).
        HintState::EntryFocused { subagent: false } => &[
            LiveHint::nav(Key::BACKTAB, "composer"),
            LiveHint::nav(Key::plain(' '), "expand"),
            LiveHint::action(Key::plain('v'), "view full log"),
            // `y` on a folded command copies *the rows the fold hides*, not the
            // whole raw payload: the hint row and the fold block's own sentence
            // must describe the same verb, or the user learns one behaviour from
            // the chrome and gets another from the key.
            LiveHint::action(Key::plain('y'), "copy"),
            LiveHint::action(Key::shift_code(KeyCode::Char('Y')), "diagnostic"),
        ],
        // A subagent task has no inline body: activation enters its scene. The
        // row must say `open`, not `expand`.
        HintState::EntryFocused { subagent: true } => &[
            LiveHint::nav(Key::BACKTAB, "composer"),
            LiveHint::nav(Key::plain(' '), "open subagent"),
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
    // Home / End scroll the transcript when the transcript plane owns the
    // keyboard (or if explicitly remapped). On the composer plane, bare
    // Home / End fall through to readline line-start/end.
    if ov.is_remapped(SurfaceVerb::ScrollTop) && ov.matches(key, SurfaceVerb::ScrollTop) {
        return Some(InputAction::ScrollTop);
    }
    if ov.is_remapped(SurfaceVerb::ScrollBottom) && ov.matches(key, SurfaceVerb::ScrollBottom) {
        return Some(InputAction::ScrollBottom);
    }
    if keys.is_transcript_active() {
        if ov.matches(key, SurfaceVerb::ScrollTop) {
            return Some(InputAction::ScrollTop);
        }
        if ov.matches(key, SurfaceVerb::ScrollBottom) {
            return Some(InputAction::ScrollBottom);
        }
    }

    match key.code {
        // Shift-Tab is *not* resolved here: the crossing is a Global-scope chord, so
        // the router's global arm claims it before scene dispatch (ADR-0145
        // `[INV-FOCUS-01]`). Resolving it in both places would give one gesture
        // two producers that could disagree about overlays and sheets.
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
        // When a target is focused, `v` reads its whole log and `y` copies it,
        // `Y` copies it as a diagnostic record. One spelling per verb: the fold
        // block writes its row as a sentence naming these two keys, so the
        // resolver may not honour a second, unadvertised alias (`o`, `c`) — a
        // key that works but is never printed is a key nobody can learn, and it
        // costs a collision (`o` sits one Shift away from `Alt-o` notices, `c`
        // one modifier away from `Ctrl-c`).
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            let entry_focused = keys.entry_focus.is_some();
            if entry_focused && c == 'y' {
                Some(InputAction::CopyFocusedTarget)
            } else if entry_focused && c == 'Y' {
                Some(InputAction::CopyFocusedDiagnostic)
            } else if entry_focused && c == ' ' {
                // Space is the universal toggle in this TUI's dialogs
                // (MCP servers, permission rules), so it activates the focused
                // entry too. Without this arm Space silently fell into the
                // "transcript swallows keys" branch below while the compaction
                // card advertised it.
                Some(InputAction::ActivateFocusedTarget)
            } else if entry_focused && c == 'v' {
                Some(InputAction::ViewFocusedTargetOutput)
            } else if keys.is_transcript_active() {
                // The transcript plane owns the keyboard: a printable key is
                // inert. It must never be inserted into a draft the user is
                // not looking at (ADR-0145 `[INV-FOCUS-02]`).
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
    // Esc in the Subagent scene interrupts the *viewed child* (scene scope)
    // once an open completion is dismissed — never the enclosing primary round.
    // It keys off [`SceneKeys::focused_subagent_running`] (this child's own
    // liveness), not the primary `is_responding`, so a finished child under a
    // still-running parent advertises no interrupt and `Esc` is simply spent:
    // `Esc` still never leaves the Scene (`[INV-TUI-CLEAN-02]`). Every other
    // key is the shared chat core — `Shift-Tab` crosses planes, `Ctrl+P`/`Ctrl+N` and
    // `↑`/`↓` walk the highlighted entry, `Enter` activates it, and printables
    // reach the shared editing layer from the composer plane. `Esc` never
    // crosses planes (ADR-0145).
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
        // The log scene has no composer and no caret, so the motion handle is
        // not part of its contract.
        crate::shell::surfaces::SceneKind::CommandLog => resolve_command_log_key_motion(key, keys),
        _ => None,
    }
}

/// The CommandLog scene's own scheme: it is a **reading** surface, so it owns
/// only reading verbs and delegates nothing to the chat core.
///
/// That is deliberate. The log used to be a dialog, which meant the shared
/// modal layer answered its keys by guessing which dialog was up; as a scene it
/// has exactly four facts to serve — scroll, scope, copy, leave — and stating
/// them here keeps them out of the chat scheme that would otherwise have to know
/// a log exists.
pub(crate) fn resolve_command_log_key_motion(
    key: crate::shell::keymap::Key,
    keys: &SceneKeys,
) -> Option<InputAction> {
    use crate::shell::keymap::SurfaceVerb;
    let ov = &keys.surface_overrides;
    // Scene-stack navigation stays available: the log is one push on the tab's
    // stack, so the standard back/forward verbs work exactly as they do from any
    // other scene.
    if ov.matches(key, SurfaceVerb::SceneHistoryBack) {
        return Some(InputAction::TabHistoryBack);
    }
    if ov.matches(key, SurfaceVerb::SceneHistoryForward) {
        return Some(InputAction::TabHistoryForward);
    }
    match key.code {
        // Leaving is the universal escape hatch, and at this depth it means "back
        // to the transcript" — never "quit" (`[INV-TUI-CLEAN-02]`). `q` is the
        // same verb: a full-page reading surface is the one place the pager
        // gesture belongs, and the log replaced a pager.
        KeyCode::Esc | KeyCode::Char('q') => Some(InputAction::SceneBack),
        KeyCode::Up | KeyCode::Char('k') => Some(InputAction::ScrollUp),
        KeyCode::Down | KeyCode::Char('j') => Some(InputAction::ScrollDown),
        KeyCode::PageUp => Some(InputAction::ScrollPageUp),
        KeyCode::PageDown | KeyCode::Char(' ') => Some(InputAction::ScrollPageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(InputAction::ScrollTop),
        KeyCode::End | KeyCode::Char('G') => Some(InputAction::ScrollBottom),
        KeyCode::Char('y') => Some(InputAction::CommandLogCopy),
        KeyCode::Char('h') => Some(InputAction::CommandLogToggleScope),
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
    if keys.entry_focus.is_some() {
        return Some(InputAction::ActivateFocusedTarget);
    }
    if keys.is_transcript_active() {
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
        let trimmed = text.trim();
        let cmd = if let Some((cmd, _)) = trimmed.split_once(' ') {
            cmd
        } else {
            trimmed
        };

        // Match on the trimmed command name so client workspace routes and slash commands
        // typed with or without arguments (e.g. `/dashboard`, `/settings`, `/models`, `/models gpt-4o`)
        // directly dispatch their respective client actions without dead guardrails.
        let action = match cmd {
            "/dashboard" => InputAction::NavigateDashboard,
            "/settings" | "/config" => InputAction::NavigateSettings,
            "/new" | "/thread" => InputAction::CreateNewSession,
            "/close" => InputAction::CloseTab,
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
            "/notices" | "/logs" => InputAction::OpenNotices,
            "/port" | "/ports" | "/share" => InputAction::OpenPortPalette,
            "/help" => InputAction::OpenHelp,
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

/// `Esc` on the chat scenes, in one priority order: dismiss an open completion,
/// then interrupt a running round, then nothing. Inline history recall is
/// preserved across `Esc` (so edits are not lost and the interrupt is not
/// intercepted; `Ctrl+C` clears the input).
///
/// `responding` and `interrupt` are the caller's scene-scoped liveness and
/// interrupt target: the Conversation scene passes its own `is_responding` and
/// [`InputAction::Interrupt`]; the Subagent scene passes the *viewed child's*
/// liveness and [`InputAction::InterruptSubagent`], so a finished child never
/// emits the primary interrupt (scene scope).
///
/// **`Esc` never crosses focus planes** ([ADR-0145]). The legacy arm
/// "`Esc` while a transcript entry is focused → clear the focus" is deleted:
/// it gave one chord two unrelated jobs (dismiss and *navigate*), which is
/// exactly why the remove direction had no symmetric twin. `Esc` dismisses a
/// completion, or interrupts a running round, or is spent — the plane stays
/// where the user put it and `Shift-Tab` is the way back.
///
/// [ADR-0145]: ../../../docs/adr/0145-composer-and-transcript-are-peer-focus-planes.md
fn resolve_esc(keys: &SceneKeys, responding: bool, interrupt: InputAction) -> Option<InputAction> {
    // One completion arm, not two: the deleted focus arm sat between them, and
    // its removal exposed that the second arm's extra `suggestion_count > 0`
    // guard was already unreachable behind the first.
    if keys.completion_kind != crate::shell::completion::CompletionKind::None
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
/// - On the transcript plane with an entry highlighted, walk to the previous
///   entry; while browsing, scroll the transcript up.
/// - On the composer plane, walk completion suggestions, move the caret one **visual row**
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
    if keys.entry_focus.is_some() {
        Some(InputAction::FocusPrevTarget)
    } else if keys.is_transcript_active() {
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
/// - On the transcript plane with an entry highlighted, walk to the next
///   entry; while browsing, scroll the transcript down.
/// - On the composer plane, walk completion suggestions, move the caret one **visual row**
///   down (keeping its display column), or at the bottom row hand off to newer
///   history recall / the stashed draft.
fn resolve_down(
    keys: &SceneKeys,
    input: &str,
    cursor_position: &mut usize,
    motion: &mut CaretMotion,
) -> Option<InputAction> {
    if keys.entry_focus.is_some() {
        Some(InputAction::FocusNextTarget)
    } else if keys.is_transcript_active() {
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

/// A printable character on the chat surface.
///
/// The caller has already established that the composer plane owns the
/// keyboard, so the character belongs to the draft: returning `None` hands it
/// to the shared readline editing layer. The transcript plane is inert (see the
/// `Char` arm in [`resolve_chat_surface_key_motion`]): a keystroke aimed at the
/// transcript must never land in a hidden draft
/// ([ADR-0145](../../../docs/adr/0145-composer-and-transcript-are-peer-focus-planes.md)
/// `[INV-FOCUS-02]`). Subagent sibling navigation (`[`/`]`) is owned by the
/// Subagent scene's resolver.
fn resolve_printable(
    _keys: &SceneKeys,
    _c: char,
    _input: &mut String,
    _cursor_position: &mut usize,
) -> Option<InputAction> {
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
mod tests;
