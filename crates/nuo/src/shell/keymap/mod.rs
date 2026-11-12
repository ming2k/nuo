//! Authoritative Action & Command Registry — Single Source of Truth (SSOT).
//!
//! Every action, shortcut, command palette entry, slash command, and footer
//! hint across the application is declared here as a [`CommandSpec`].
//!
//! ## Core Architectural Principles
//!
//! 1. **Peer focus planes, no modality**: the composer and the transcript are
//!    coequal surfaces of a chat scene (ADR-0145). Printable input belongs to
//!    whichever plane owns the keyboard; it never falls through to a hidden
//!    draft.
//! 2. **Single input owner**: exactly one region/dialog/plane owns focus at any
//!    moment.
//! 3. **Visible, predictable, recoverable focus**: overlays trap focus, closing
//!    restores source, and crossing between peer planes is one symmetric chord.
//! 4. **Single semantic origin**: one action has one semantic source.
//! 5. **Unified derivation**: shortcuts, Footer, and Command Palette are derived from this registry.
//! 6. **Discovery over memorization**: rare actions are found via the `C-x p` Command Palette.
//! 7. **No modal penetration**: overlays strictly isolate input from background views.
//! 8. **Zero silent typing**: a printable key on the transcript plane is inert,
//!    never absorbed into a draft the user is not looking at.
//! 9. **Terminal independence**: core workflows work without Kitty enhanced keyboard protocol.
//! 10. **Zero legacy baggage**: breaking clean from leader chords and modal keymaps.
use std::collections::{HashMap, HashSet};

use crossterm::event::KeyEvent;

use crate::shell::surfaces::{DialogKind, SceneKind};

// Canonical key vocabulary and display formatting

/// Repeated legend tokens — glyph strings that stand for an affordance.
/// Glyph vocabulary for chords. The constants live in the pure substrate so a
/// footer's key token needs no driver; re-exported here because every overlay
/// already imports this module for its hint types.
pub use nuo_ui_primitives::keyvocab;

/// The pure chord model — `Key`, `KeyCode`, `KeyModifiers` and their display
/// tokens — lives in the substrate so bindings carry no terminal-driver
/// dependency ([INV-ARCH-UI-ZERO-IO]).
pub use nuo_ui_primitives::chord::{Key, KeyCode, KeyModifiers, chord_token, display_token};

/// Fold a terminal driver key event into the pure [`Key`].
///
/// The driver type never leaves this boundary: modifiers are normalized here so
/// that a bare uppercase character is a plain key, and a Ctrl chord is
/// lowercased regardless of the terminal's case reporting.
pub fn key_from_event(event: KeyEvent) -> Key {
    let mut code = match event.code {
        crossterm::event::KeyCode::Char(c) => KeyCode::Char(c),
        crossterm::event::KeyCode::Enter => KeyCode::Enter,
        crossterm::event::KeyCode::Esc => KeyCode::Esc,
        crossterm::event::KeyCode::Tab => KeyCode::Tab,
        crossterm::event::KeyCode::BackTab => KeyCode::BackTab,
        crossterm::event::KeyCode::Backspace => KeyCode::Backspace,
        crossterm::event::KeyCode::Delete => KeyCode::Delete,
        crossterm::event::KeyCode::Up => KeyCode::Up,
        crossterm::event::KeyCode::Down => KeyCode::Down,
        crossterm::event::KeyCode::Left => KeyCode::Left,
        crossterm::event::KeyCode::Right => KeyCode::Right,
        crossterm::event::KeyCode::Home => KeyCode::Home,
        crossterm::event::KeyCode::End => KeyCode::End,
        crossterm::event::KeyCode::PageUp => KeyCode::PageUp,
        crossterm::event::KeyCode::PageDown => KeyCode::PageDown,
        crossterm::event::KeyCode::F(n) => KeyCode::F(n),
        _ => KeyCode::Other,
    };
    let mut modifiers = KeyModifiers::NONE;
    for (flag, ours) in [
        (crossterm::event::KeyModifiers::SHIFT, KeyModifiers::SHIFT),
        (
            crossterm::event::KeyModifiers::CONTROL,
            KeyModifiers::CONTROL,
        ),
        (crossterm::event::KeyModifiers::ALT, KeyModifiers::ALT),
        (crossterm::event::KeyModifiers::SUPER, KeyModifiers::SUPER),
    ] {
        if event.modifiers.contains(flag) {
            modifiers.insert(ours);
        }
    }
    // `BackTab` *is* Shift+Tab. Terminals report it two ways — the dedicated
    // CSI Z sequence (which may arrive with or without a SHIFT modifier flag,
    // depending on the emulator and on whether the enhanced-keyboard protocol
    // is active) or as Tab-with-Shift. The pure model has one shape for it, so
    // the shift bit is normalized here rather than re-derived at every
    // comparison site; without this a `Shift-Tab` binding silently misses on
    // terminals that omit the flag.
    if code == KeyCode::BackTab {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if let KeyCode::Char(c) = code {
        if c.is_ascii_uppercase()
            && !modifiers.contains(KeyModifiers::CONTROL)
            && !modifiers.contains(KeyModifiers::ALT)
        {
            modifiers.remove(KeyModifiers::SHIFT);
        } else if modifiers.contains(KeyModifiers::CONTROL) {
            code = KeyCode::Char(c.to_ascii_lowercase());
        }
    }
    Key { modifiers, code }
}

// Command Registry SSOT Specification Types

/// Exhaustive identifier for every executable command in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandId {
    // Global (Hard-Bound Shortcuts)
    CommandPalette,
    CancelOrBack,
    /// Cross the chat scene's peer focus planes (`Shift-Tab`, ADR-0145).
    ToggleFocusPlane,
    InterruptTask,
    Quit,
    KillThread,
    CloseTab,
    NewTab,
    CopySelection,
    CopyFocusedEntry,
    CopyDiagnostic,
    CopyRecentDiagnostics,
    /// Open the notice log viewer: every disclosure this client surfaced,
    /// retained after its bubble faded.
    OpenNotices,
    /// Open the sovereign Help & Keybindings modal dialog (F1 / Alt-h, ADR-0131).
    OpenHelp,

    // Session & Composer
    SendPrompt,
    QueueFollowUp,
    SteerImmediate,
    ToggleSendMode,
    HistorySearch,

    // Surface Navigation
    NavigateSession,
    NavigateDashboard,
    NavigateSettings,
    OpenQueue,
    OpenThreadStats,
    OpenSessionTrace,
    OpenModels,
    OpenConnections,
    OpenActiveConnectionDetail,
    OpenPortShare,
    OpenTools,
    OpenMcp,
    OpenSkills,
    OpenPermissions,
    OpenUsage,
    OpenQuotas,
    OpenTree,
    ForkThread,
    OpenSessions,

    // Management & Actions
    ToggleQueueBlock,
    ClearQueue,
    PermissionsClearAll,
    ProviderAddConnection,
    RedrawScreen,

    // Dialog Actions: Sessions
    SessionOpenSelected,
    SessionDeleteSelected,
    SessionCreateNew,
    SessionOpenInfo,

    // Dialog Actions: Models
    ModelSelect,
    ModelEnterSearch,
    ModelToggleFavorite,
    ModelBlock,
    ModelEditSettings,
    ModelRefresh,

    // Dialog Actions: Connections
    ConnectionOpenDetail,
    ConnectionEnterSearch,
    ConnectionOpenPreset,
    ConnectionOpenCustom,
    ConnectionEdit,
    ConnectionRefresh,
    ConnectionDelete,

    // Dialog Actions: MCP
    McpToggleServer,
    McpReconnectServer,

    // Dialog Actions: Permissions
    PermissionToggleRule,

    // Dialog Actions: Queue
    QueueRecallItem,
    QueueDeleteItem,
    QueueMoveItemUp,
    QueueMoveItemDown,

    // Dialog Actions: Thread Stats
    ThreadStatsOpenTrace,

    // Dialog Actions: Session Trace
    TraceInspectDetail,

    // Dialog Actions: Skills
    SkillsToggleDetail,

    // Dialog Actions: History Search
    HistorySearchInsert,
    HistorySearchDelete,
}

/// Scope context where a command is applicable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Session,
    Composer,
    Dialog(DialogKind),
}

/// Category of the command for palette grouping and reference presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandCategory {
    Global,
    Navigate,
    Session,
    Actions,
    Settings,
}

impl CommandCategory {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Navigate => "Navigate",
            Self::Session => "Session",
            Self::Actions => "Actions",
            Self::Settings => "Settings",
        }
    }
}

/// Danger classification for critical/destructive actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DangerLevel {
    Safe,
    Cautious,
    Dangerous,
}

/// Progressive disclosure priority level (L0 to L3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DisclosurePriority {
    /// L0: Maximum of 3 primary actions rendered in the active footer.
    L0Footer,
    /// L1: Local action displayed in a focused region bar.
    L1FocusRegion,
    /// L2: Searchable through the `C-x p` Command Palette.
    L2Palette,
    /// L3: Full contextual reference retained for exhaustive command coverage.
    L3Reference,
}

/// Dynamic availability status for a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable(&'static str),
}

/// Snapshot of application state passed to availability predicates.
///
/// Every field here must be **read** by an availability predicate below —
/// this is deliberately a narrow projection, not a mirror of `App`. Layering
/// questions (which scene, which overlay) are answered by
/// `App::current_scene()` / `App::surfaces` / `App::caret_owner()` at the
/// point of use, so they are not duplicated here.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppContext {
    pub has_overlay: bool,
    pub active_dialog: Option<DialogKind>,
    pub is_responding: bool,
    pub has_selection: bool,
    /// Whether a transcript entry currently holds keyboard focus. Copy and
    /// diagnostic-copy act on the focused entry, so both must be able to say
    /// why they are unavailable instead of silently doing nothing.
    pub has_focused_entry: bool,
    /// Whether the notice log holds anything at all (`app::receipts`). The
    /// viewer and the copy verb both need a non-empty log to act on.
    pub has_notices: bool,
    pub has_running_task: bool,
    pub queue_count: usize,
    /// Whether an ambient session exists (ADR-0035 precondition gating).
    pub has_session: bool,
    /// The active root scene (ADR-0035 precondition gating).
    pub scene: SceneKind,
}

/// Authoritative declaration of a single application command.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub id: CommandId,
    pub label: &'static str,
    pub hint: &'static str,
    pub category: CommandCategory,
    pub scope: Scope,
    pub bindings: &'static [Key],
    pub slash: Option<&'static str>,
    pub availability: fn(&AppContext) -> Availability,
    pub disclosure: DisclosurePriority,
    pub danger: DangerLevel,
    pub description: &'static str,
}

// Availability Predicates

fn avail_always(_: &AppContext) -> Availability {
    Availability::Available
}

/// A session-scoped dialog is available only when an ambient session exists
/// (`[INV-SURFACE-03]`).
fn avail_session(ctx: &AppContext) -> Availability {
    if ctx.has_session {
        Availability::Available
    } else {
        Availability::Unavailable("no active session")
    }
}

/// A scene-scoped dialog (the thread-bound HistorySearch) additionally
/// requires the thread scene (`[INV-SURFACE-03]`).
fn avail_thread(ctx: &AppContext) -> Availability {
    if !ctx.has_session {
        Availability::Unavailable("no active session")
    } else if ctx.scene != SceneKind::Thread {
        Availability::Unavailable("only in the thread scene")
    } else {
        Availability::Available
    }
}

fn avail_running(ctx: &AppContext) -> Availability {
    if ctx.is_responding || ctx.has_running_task {
        Availability::Available
    } else {
        Availability::Unavailable("only while running")
    }
}

fn avail_idle_composer(ctx: &AppContext) -> Availability {
    if ctx.has_overlay {
        Availability::Unavailable("overlay active")
    } else if ctx.is_responding {
        Availability::Unavailable("currently running")
    } else {
        Availability::Available
    }
}

/// Copy verbs act on whatever the keyboard is on: an active selection or a
/// focused entry. Unavailable only when it is neither, and the reason names
/// both ways to aim.
fn avail_copyable(ctx: &AppContext) -> Availability {
    if ctx.has_selection || ctx.has_focused_entry {
        Availability::Available
    } else {
        Availability::Unavailable("nothing selected or focused")
    }
}

/// Diagnostic copy needs an entry: a free-floating text selection has no
/// provenance to report (no kind, severity, route or attempt), so a
/// "diagnostic" of it would be a header full of nothing.
/// Recent-fault copy needs at least one retained receipt: with none, the
/// command has nothing to hand over and says so rather than copying an empty
/// block.
fn avail_has_notices(ctx: &AppContext) -> Availability {
    if ctx.has_notices {
        Availability::Available
    } else {
        Availability::Unavailable("no notices retained yet")
    }
}

fn avail_focused_entry(ctx: &AppContext) -> Availability {
    if ctx.has_focused_entry {
        Availability::Available
    } else {
        Availability::Unavailable("no focused entry")
    }
}

fn avail_queue_nonempty(ctx: &AppContext) -> Availability {
    if ctx.queue_count > 0 {
        Availability::Available
    } else {
        Availability::Unavailable("queue is empty")
    }
}

// Static Command Registry Master Table

mod commands;
#[cfg(test)]
mod tests;

pub use self::commands::COMMAND_REGISTRY;

/// Complete set of registered command specs.
pub fn all_commands() -> &'static [CommandSpec] {
    COMMAND_REGISTRY
}

/// Look up a command by its unique identifier.
pub fn find_command(id: CommandId) -> Option<&'static CommandSpec> {
    COMMAND_REGISTRY.iter().find(|cmd| cmd.id == id)
}

/// Look up a command by its slash trigger.
pub fn find_by_slash(slash: &str) -> Option<&'static CommandSpec> {
    let clean = slash.trim().to_ascii_lowercase();
    COMMAND_REGISTRY.iter().find(|cmd| {
        cmd.slash
            .map(|s| s.eq_ignore_ascii_case(&clean))
            .unwrap_or(false)
    })
}

/// Which side of a hint row a chord is advertised on: navigation (left) or the
/// primary action (right).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintSide {
    Nav,
    Action,
}

/// A chord a surface advertises in a run state, with the label a hint row
/// shows. The discovery side of a keybinding scheme: a hint renders
/// exactly the chords a scheme's `live_*_hints` returns, and the tests pin
/// them to the resolver so a hint can never advertise a dead shortcut.
#[derive(Debug, Clone, Copy)]
pub struct LiveHint {
    pub key: Key,
    pub label: &'static str,
    pub side: HintSide,
    pub glyph: Option<&'static str>,
}

impl LiveHint {
    pub const fn nav(key: Key, label: &'static str) -> Self {
        Self {
            key,
            label,
            side: HintSide::Nav,
            glyph: None,
        }
    }

    pub const fn nav_glyph(key: Key, glyph: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            side: HintSide::Nav,
            glyph: Some(glyph),
        }
    }

    pub const fn action(key: Key, label: &'static str) -> Self {
        Self {
            key,
            label,
            side: HintSide::Action,
            glyph: None,
        }
    }

    #[allow(dead_code)]
    pub const fn action_glyph(key: Key, glyph: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            side: HintSide::Action,
            glyph: Some(glyph),
        }
    }

    pub fn display_key(&self) -> &'static str {
        if let Some(glyph) = self.glyph {
            glyph
        } else {
            self.key.display()
        }
    }
}

/// The canonical global chords a user may remap via the `[keybindings]` config.
fn canonical_global_chord(cmd: CommandId) -> Option<Key> {
    match cmd {
        CommandId::CommandPalette => Some(Key::CTRL_SHIFT_P),
        CommandId::Quit => Some(Key::CTRL_C),
        CommandId::CloseTab => Some(Key::ALT_W),
        CommandId::NewTab => Some(Key::ALT_T),
        CommandId::NavigateDashboard => Some(Key::ALT_0),
        CommandId::NavigateSettings => Some(Key::ALT_COMMA),
        CommandId::KillThread => None,
        CommandId::CopySelection => Some(Key::CTRL_SHIFT_C),
        CommandId::OpenThreadStats => None,
        CommandId::OpenSessionTrace => None,
        CommandId::OpenActiveConnectionDetail => None,
        CommandId::CancelOrBack => Some(Key::ESC),
        // The chord exists (so chrome, the Help dialog, and the registry table
        // all read one value), but `GlobalOverrides::from_config` refuses to
        // reassign it: it is the remove half of the `Tab` pair, and a remap
        // would let the pair drift apart (ADR-0145 `[INV-FOCUS-01]`).
        CommandId::ToggleFocusPlane => Some(Key::BACKTAB),
        CommandId::OpenQueue => Some(Key::CTRL_Q),
        CommandId::OpenNotices => Some(Key::ALT_O),
        CommandId::OpenHelp => Some(Key::F1),
        _ => None,
    }
}

/// The canonical resolution table (the hard-bound globals + the bar chords:
/// Ctrl+Q queue, Esc dismiss/step-back, Ctrl+C quit,
/// Ctrl+Shift+C copy), ignoring user overrides. Esc resolves to
/// [`CommandId::CancelOrBack`], which never navigates between Scenes.
/// The Command Palette is accessible via `Ctrl-Shift-P` and user remaps.
fn canonical_global_key(key: Key) -> Option<CommandId> {
    if key == Key::F1 || key == Key::ALT_H {
        Some(CommandId::OpenHelp)
    } else if key == Key::BACKTAB {
        // `Shift-Tab` crosses the chat scene's peer focus planes. Not remappable
        // (`canonical_global_chord` returns `None` for the command): it is the
        // remove half of the `Tab` pair, and a remap would break the pairing
        // (ADR-0145 `[INV-FOCUS-01]`).
        Some(CommandId::ToggleFocusPlane)
    } else if key == Key::CTRL_Q {
        Some(CommandId::OpenQueue)
    } else if key == Key::ALT_O {
        Some(CommandId::OpenNotices)
    } else if key == Key::ESC {
        Some(CommandId::CancelOrBack)
    } else if key == Key::CTRL_C {
        Some(CommandId::Quit)
    } else if key == Key::ALT_W {
        Some(CommandId::CloseTab)
    } else if key == Key::ALT_T {
        Some(CommandId::NewTab)
    } else if key == Key::ALT_0 {
        Some(CommandId::NavigateDashboard)
    } else if key == Key::ALT_COMMA {
        Some(CommandId::NavigateSettings)
    } else if key == Key::CTRL_SHIFT_P {
        Some(CommandId::CommandPalette)
    } else if key == Key::CTRL_SHIFT_C || key == Key::CMD_C {
        Some(CommandId::CopySelection)
    } else {
        None
    }
}

/// Map a config table key (snake_case command name) to its [`CommandId`].
pub fn command_id_from_name(name: &str) -> Option<CommandId> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "command_palette" | "command-palette" | "palette" => CommandId::CommandPalette,
        "focus" | "switch_focus" | "switch-focus" | "toggle_focus_plane" | "toggle-focus-plane" => {
            CommandId::ToggleFocusPlane
        }
        "interrupt" | "interrupt_task" | "interrupt-task" => CommandId::InterruptTask,
        "quit" | "quit_nuo" | "quit-nuo" => CommandId::Quit,
        "close_tab" | "close-tab" | "close" => CommandId::CloseTab,
        "new_tab" | "new-tab" | "new_thread" | "new" => CommandId::NewTab,
        "dashboard" | "navigate_dashboard" => CommandId::NavigateDashboard,
        "settings" | "config" | "navigate_settings" => CommandId::NavigateSettings,
        "kill" | "kill_thread" | "kill-thread" | "kill_conversation" | "kill-conversation" => {
            CommandId::KillThread
        }
        "copy" | "copy_selection" | "copy-selection" => CommandId::CopySelection,
        "stats" | "thread_stats" | "thread-stats" | "open_thread_stats" | "open-thread-stats" => {
            CommandId::OpenThreadStats
        }
        "thread_stats_open_trace" => CommandId::ThreadStatsOpenTrace,
        "trace" | "session_trace" | "session-trace" | "open_session_trace"
        | "open-session-trace" | "telemetry" | "open_telemetry" | "open-telemetry" => {
            CommandId::OpenSessionTrace
        }
        "connection"
        | "connection_detail"
        | "active_connection_detail"
        | "active-connection-detail" => CommandId::OpenActiveConnectionDetail,
        "port" | "port_share" | "port-share" | "open_port_share" | "open-port-share" | "share" => {
            CommandId::OpenPortShare
        }
        _ => return None,
    })
}

/// Parse a `[keybindings]` chord spec like `"ctrl-shift-p"`, `"ctrl+p"`, `"f1"`, or
/// `"alt-enter"` into the exact [`Key`] the input layer produces for that
/// keystroke (normalized through [`key_from_event`]), so a config chord and a
/// pressed key compare equal.
pub fn parse_key(spec: &str) -> Option<Key> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut cmd = false;
    let mut code = None;

    let parts: Vec<&str> = spec.split(['+', '-']).collect();
    for part in parts {
        let p = part.trim().to_ascii_lowercase();
        if p.is_empty() {
            continue;
        }
        match p.as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            "super" | "cmd" | "meta" | "command" => cmd = true,
            "esc" | "escape" => code = Some(KeyCode::Esc),
            "enter" | "return" => code = Some(KeyCode::Enter),
            "tab" => code = Some(KeyCode::Tab),
            "space" => code = Some(KeyCode::Char(' ')),
            "backspace" => code = Some(KeyCode::Backspace),
            "delete" | "del" => code = Some(KeyCode::Delete),
            "home" => code = Some(KeyCode::Home),
            "end" => code = Some(KeyCode::End),
            "pageup" | "pgup" => code = Some(KeyCode::PageUp),
            "pagedown" | "pgdn" => code = Some(KeyCode::PageDown),
            "up" => code = Some(KeyCode::Up),
            "down" => code = Some(KeyCode::Down),
            "left" => code = Some(KeyCode::Left),
            "right" => code = Some(KeyCode::Right),
            _ => {
                if let Some(n) = p.strip_prefix('f').and_then(|s| s.parse::<u8>().ok())
                    && (1..=12).contains(&n)
                {
                    code = Some(KeyCode::F(n));
                } else if let Some(ch) = p.chars().next()
                    && p.chars().count() == 1
                {
                    code = Some(KeyCode::Char(ch));
                } else {
                    return None;
                }
            }
        }
    }

    let mut modifiers = KeyModifiers::NONE;
    if ctrl {
        modifiers |= KeyModifiers::CONTROL;
    }
    if alt {
        modifiers |= KeyModifiers::ALT;
    }
    if shift {
        modifiers |= KeyModifiers::SHIFT;
    }
    if cmd {
        modifiers |= KeyModifiers::SUPER;
    }
    // crossterm reports Shift+Tab as the dedicated BackTab code, not
    // Tab-with-Shift; mirror that so comparisons with real keystrokes hold.
    let code = if code == Some(KeyCode::Tab) && shift {
        Some(KeyCode::BackTab)
    } else {
        code
    };
    Some(Key {
        modifiers,
        code: code?,
    })
}

/// User remaps of the global chords (`[keybindings]` config).
///
/// A remapped command's canonical chord becomes inactive; the assigned chord
/// triggers the command. Resolution is override-first, then canonical
/// (minus the remapped commands), so the two never double-fire.
#[derive(Debug, Clone, Default)]
pub struct GlobalOverrides {
    assigned: HashMap<Key, CommandId>,
    remapped: HashSet<CommandId>,
}

impl GlobalOverrides {
    /// Build from a `[keybindings]` table (`command-id → chord spec`).
    /// Unknown command ids and unparseable chords are skipped; `Esc`/Back and
    /// `Shift-Tab`/SwitchFocus are deliberately not remappable (the universal escape
    /// hatch, and the remove half of the `Tab` grammar pair).
    pub fn from_config(map: &HashMap<String, String>) -> Self {
        let mut o = Self::default();
        for (id, spec) in map {
            if let (Some(cmd), Some(key)) = (command_id_from_name(id), parse_key(spec))
                && !matches!(cmd, CommandId::CancelOrBack | CommandId::ToggleFocusPlane)
            {
                o.assigned.insert(key, cmd);
                o.remapped.insert(cmd);
            }
        }
        o
    }

    /// Whether any global chord is remapped.
    pub fn is_empty(&self) -> bool {
        self.assigned.is_empty()
    }

    /// The chord that should *display* for a command: the user's override
    /// when one is configured, else the canonical chord — and `None` when the
    /// command has no chord at all.
    ///
    /// Chrome must render the binding that actually fires, and nothing when
    /// none does: a keycap is a promise, so there is no fallback
    /// value here. Returning a placeholder chord for an unbound command is
    /// exactly how the queue bar came to advertise a `Ctrl-q` that resolved to
    /// nothing.
    pub fn effective_binding(&self, cmd: CommandId) -> Option<Key> {
        if let Some((key, _)) = self.assigned.iter().find(|(_, c)| **c == cmd) {
            return Some(*key);
        }
        canonical_global_chord(cmd)
    }
}

/// Resolve a key against the canonical global chords plus any user overrides.
pub fn resolve_global_key_with(key: Key, overrides: &GlobalOverrides) -> Option<CommandId> {
    if let Some(cmd) = overrides.assigned.get(&key) {
        return Some(*cmd);
    }
    canonical_global_key(key).filter(|cmd| !overrides.remapped.contains(cmd))
}

/// A user-remappable **surface verb** — a single-purpose chord the full-screen
/// views own (step 9). Verbs are the *shortcut* layer of a surface's
/// scheme: each maps one chord to one semantic action, possibly gated by the
/// surface's run state. The interaction grammar (Enter send/queue/activate/
/// commit, Tab act-inside-the-composer, Shift-Tab cross-to-the-transcript, Esc
/// dismiss/interrupt, ↑/↓ walk, printable text) is deliberately **not**
/// remappable — it is the surface's language, not a shortcut, mirroring how
/// `Esc`/Back/`Shift-Tab` are never remappable in the global layer (ADR-0145).
///
/// A verb also never *navigates between planes*: there is no `clear_focus` or
/// `toggle_focus` member, because the crossing is grammar. The three verbs that
/// remain close to focus (`FocusPrev/NextTarget`, `ScrollTop/Bottom`) act
/// *inside* the transcript plane and are guarded by the plane being active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceVerb {
    /// Open the Ctrl+R history recall modal.
    OpenHistory,
    /// Toggle between steer and follow-up queue mode while running (`Tab`).
    ToggleSendMode,
    /// Previous / next prompt-history recall (`Alt+P` / `Alt+N`).
    HistoryPrev,
    HistoryNext,
    /// Enter or step backward through transcript step focus (`Ctrl+P`).
    FocusPrevTarget,
    /// Step forward through transcript step focus (`Ctrl+N`).
    FocusNextTarget,
    /// Jump the focused step's scroll to the thread edges (`Home`/`End`).
    ScrollTop,
    ScrollBottom,
    /// Subagent/scene navigation back (`Alt+[`).
    SceneHistoryBack,
    /// Subagent/scene navigation forward (`Alt+]`).
    SceneHistoryForward,
}

impl SurfaceVerb {
    /// The canonical chord, used for dispatch when unremapped and for hints
    /// when the user has not overridden it.
    pub(crate) fn canonical(self) -> Key {
        match self {
            SurfaceVerb::OpenHistory => Key::CTRL_R,
            SurfaceVerb::ToggleSendMode => Key::TAB,
            SurfaceVerb::HistoryPrev => Key::ALT_P,
            SurfaceVerb::HistoryNext => Key::ALT_N,
            SurfaceVerb::FocusPrevTarget => Key::CTRL_P,
            SurfaceVerb::FocusNextTarget => Key::CTRL_N,
            SurfaceVerb::ScrollTop => Key::HOME,
            SurfaceVerb::ScrollBottom => Key::END,
            SurfaceVerb::SceneHistoryBack => Key::ALT_BRACKET_LEFT,
            SurfaceVerb::SceneHistoryForward => Key::ALT_BRACKET_RIGHT,
        }
    }

    /// The `[keybindings.session]` config name (a `[keybindings]` top-level
    /// name would collide with a global command id, so surface verbs live in
    /// the nested `session` table).
    pub(crate) fn name(self) -> &'static str {
        match self {
            SurfaceVerb::OpenHistory => "open_history",
            SurfaceVerb::ToggleSendMode => "toggle_send_mode",
            SurfaceVerb::HistoryPrev => "history_prev",
            SurfaceVerb::HistoryNext => "history_next",
            SurfaceVerb::FocusPrevTarget => "focus_prev",
            SurfaceVerb::FocusNextTarget => "focus_next",
            SurfaceVerb::ScrollTop => "scroll_top",
            SurfaceVerb::ScrollBottom => "scroll_bottom",
            SurfaceVerb::SceneHistoryBack => "scene_back",
            SurfaceVerb::SceneHistoryForward => "scene_forward",
        }
    }

    fn from_name(name: &str) -> Option<SurfaceVerb> {
        SurfaceVerb::ALL.into_iter().find(|v| v.name() == name)
    }

    /// All verbs — `from_name` and the consistency test iterate this so a new
    /// verb must ship a config name, a canonical chord, and a resolvable
    /// handling path.
    pub const ALL: [SurfaceVerb; 10] = [
        SurfaceVerb::OpenHistory,
        SurfaceVerb::ToggleSendMode,
        SurfaceVerb::HistoryPrev,
        SurfaceVerb::HistoryNext,
        SurfaceVerb::FocusPrevTarget,
        SurfaceVerb::FocusNextTarget,
        SurfaceVerb::ScrollTop,
        SurfaceVerb::ScrollBottom,
        SurfaceVerb::SceneHistoryBack,
        SurfaceVerb::SceneHistoryForward,
    ];
}

/// User remaps of the surface verbs (`[keybindings.session]` config table —
/// Step 9).
///
/// Mirrors [`GlobalOverrides`]: a remapped verb's canonical chord goes inactive
/// and the assigned chord triggers the verb. Resolution is override-first.
#[derive(Debug, Clone, Default)]
pub struct SurfaceOverrides {
    assigned: HashMap<Key, SurfaceVerb>,
    remapped: HashSet<SurfaceVerb>,
}

impl SurfaceOverrides {
    /// Build from the `[keybindings.session]` table (verb → chord spec).
    /// Unknown verbs and unparseable chords are skipped; `Esc` / `BackTab`
    /// are deliberately not assignable (the universal escape hatch, mirroring
    /// the global layer).
    pub fn from_config(map: &HashMap<String, String>) -> Self {
        let mut o = Self::default();
        for (id, spec) in map {
            if let (Some(verb), Some(key)) = (SurfaceVerb::from_name(id), parse_key(spec))
                && !matches!(key.code, KeyCode::Esc | KeyCode::BackTab)
            {
                o.assigned.insert(key, verb);
                o.remapped.insert(verb);
            }
        }
        o
    }

    /// Whether any surface verb is remapped.
    pub fn is_empty(&self) -> bool {
        self.assigned.is_empty()
    }

    /// Whether a specific surface verb has been explicitly remapped in configuration.
    pub fn is_remapped(&self, verb: SurfaceVerb) -> bool {
        self.remapped.contains(&verb)
    }

    /// The chord that should *display* and *dispatch* for a verb (override,
    /// else canonical), so hints keep showing the binding that actually fires.
    pub fn effective_binding(&self, verb: SurfaceVerb) -> Key {
        if let Some((key, _)) = self.assigned.iter().find(|(_, v)| **v == verb) {
            return *key;
        }
        verb.canonical()
    }

    /// Whether `key` is a verb's effective chord. Resolvers gate on this so
    /// remapping just re-points the chord; the verb's run-state guard runs
    /// unchanged.
    pub fn matches(&self, key: Key, verb: SurfaceVerb) -> bool {
        key == self.effective_binding(verb)
    }
}

/// Resolve one of the canonical global bindings.
///
/// Returns `Some(CommandId)` only when the key matches a designated global binding.
pub fn resolve_global_key(key: Key) -> Option<CommandId> {
    resolve_global_key_with(key, &GlobalOverrides::default())
}

/// Derive command palette entries with current availability flags.
pub fn commands_for_palette(ctx: &AppContext) -> Vec<(&'static CommandSpec, Availability)> {
    COMMAND_REGISTRY
        .iter()
        .filter(|cmd| cmd.scope == Scope::Global && cmd.disclosure == DisclosurePriority::L2Palette)
        .map(|cmd| (cmd, (cmd.availability)(ctx)))
        .collect()
}

// Tests
