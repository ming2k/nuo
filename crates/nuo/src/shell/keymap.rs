//! Authoritative Action & Command Registry — Single Source of Truth (SSOT).
//!
//! Every action, shortcut, command palette entry, slash command, and footer
//! hint across the application is declared here as a [`CommandSpec`].
//!
//! ## Core Architectural Principles
//!
//! 1. **Composer-first, no input modality**: typing always flows to Composer.
//! 2. **Single input owner**: only one region/dialog owns focus at any moment.
//! 3. **Visible, predictable, recoverable focus**: overlays trap focus, closing restores source.
//! 4. **Single semantic origin**: one action has one semantic source.
//! 5. **Unified derivation**: shortcuts, Footer, and Command Palette are derived from this registry.
//! 6. **Discovery over memorization**: rare actions are found via the `C-x p` Command Palette.
//! 7. **No modal penetration**: overlays strictly isolate input from background views.
//! 8. **Zero loss of printable characters**: typing in transcript bounces back to composer.
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
    InterruptTask,
    Quit,
    KillThread,
    CloseTab,
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

pub static COMMAND_REGISTRY: &[CommandSpec] = &[
    // Canonical Global Bindings
    CommandSpec {
        id: CommandId::CommandPalette,
        label: "Command Palette",
        hint: "/commands",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/commands"),
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Open unified command palette and surface switcher",
    },
    CommandSpec {
        id: CommandId::CancelOrBack,
        label: "Back / Cancel",
        hint: "Esc",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::ESC],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Dismiss active overlay, step back, or return to composer",
    },
    CommandSpec {
        id: CommandId::InterruptTask,
        label: "Interrupt Task",
        hint: "Esc Esc (running)",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[],
        slash: Some("/interrupt"),
        availability: avail_running,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Cautious,
        description: "Interrupt currently executing turn / task",
    },
    CommandSpec {
        id: CommandId::Quit,
        label: "Quit Nuo",
        hint: "Ctrl-c",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::CTRL_C],
        slash: Some("/exit"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Exit client gracefully (Detach without killing thread)",
    },
    CommandSpec {
        id: CommandId::CloseTab,
        label: "Close Tab",
        hint: "Alt-w",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::ALT_W],
        slash: Some("/close"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Close active tab (Detaches thread if attached)",
    },
    CommandSpec {
        id: CommandId::KillThread,
        label: "Kill Thread",
        hint: "",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[],
        slash: Some("/kill"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Dangerous,
        description: "Terminate active thread and halt all background tasks",
    },
    CommandSpec {
        id: CommandId::CopySelection,
        label: "Copy Selection",
        hint: "Ctrl-Shift-c",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::CTRL_SHIFT_C, Key::CMD_C],
        slash: None,
        availability: avail_copyable,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Copy selected text, or the focused entry, to the clipboard",
    },
    // Transcript entry copy. Both verbs are focus-scoped (`y`/`c` and
    // `Y`/`C` on a focused entry) — they are declared here so the palette can
    // list them and so their availability is stated once. They carry no global
    // chord: a bare letter pressed with no focus belongs to the composer, and
    // binding it globally would make typing impossible.
    CommandSpec {
        id: CommandId::CopyFocusedEntry,
        label: "Copy Focused Entry",
        hint: "y",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[],
        slash: None,
        availability: avail_focused_entry,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Copy the focused transcript entry verbatim",
    },
    CommandSpec {
        id: CommandId::CopyDiagnostic,
        label: "Copy Entry as Diagnostic",
        hint: "Shift-y",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[],
        slash: None,
        availability: avail_focused_entry,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Copy the focused entry with its kind, severity, route, attempt, timestamp, thread and log path",
    },
    CommandSpec {
        id: CommandId::CopyRecentDiagnostics,
        label: "Copy Recent Notices",
        // No chord: this is a recovery verb you reach for *after* missing a
        // toast, so it belongs in the palette rather than on a key you would
        // have to memorise in advance. (`Ctrl-o` opens the same record to read.)
        hint: "",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[],
        slash: None,
        availability: avail_has_notices,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Copy every notice the client surfaced, including ones whose bubble has already faded",
    },
    CommandSpec {
        id: CommandId::OpenNotices,
        label: "Notices",
        // The chord exists because the *bubble* advertises it: a fault bubble
        // says "Ctrl-o · all notices", so the chord must be one the user can
        // press without having memorised anything. Ctrl-o is otherwise unused
        // (it was the open-file chord in editors; nothing here claims it).
        hint: "Ctrl-o",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[Key::CTRL_O],
        slash: Some("/notices"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Review every notice, warning and error the client has surfaced",
    },
    // Session & Composer Controls
    CommandSpec {
        id: CommandId::SendPrompt,
        label: "Send Prompt",
        hint: "Enter",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::ENTER],
        slash: Some("/send"),
        availability: avail_idle_composer,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Send prompt text to agent",
    },
    CommandSpec {
        id: CommandId::QueueFollowUp,
        label: "Queue Follow-up",
        hint: "Enter (follow-up mode)",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[],
        slash: None,
        availability: avail_running,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Enqueue prompt as next-round follow-up message",
    },
    CommandSpec {
        id: CommandId::SteerImmediate,
        label: "Steer Now",
        hint: "Enter (steer mode)",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[],
        slash: Some("/steer"),
        availability: avail_running,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Cautious,
        description: "Inject prompt immediately at next safe boundary",
    },
    CommandSpec {
        id: CommandId::ToggleSendMode,
        label: "Toggle Send Mode",
        hint: "Tab (running)",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::TAB],
        slash: None,
        availability: avail_running,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Toggle between steer and follow-up queue mode while running",
    },
    CommandSpec {
        id: CommandId::HistorySearch,
        label: "Search History",
        hint: "Ctrl-r",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::CTRL_R],
        slash: Some("/history"),
        availability: avail_thread,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Search and recall past prompt history",
    },
    // Surface Navigation
    CommandSpec {
        id: CommandId::NavigateSession,
        label: "Session",
        hint: "/session",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/session"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Switch to live thread session view",
    },
    CommandSpec {
        id: CommandId::NavigateDashboard,
        label: "Session Dashboard",
        hint: "C-x d",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Open server session orchestrator dashboard (C-x d)",
    },
    CommandSpec {
        id: CommandId::NavigateSettings,
        label: "Settings",
        hint: "C-x ,",
        category: CommandCategory::Settings,
        scope: Scope::Global,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Open application and appearance settings (C-x ,)",
    },
    CommandSpec {
        id: CommandId::OpenQueue,
        label: "Queue (Outbox)",
        hint: "/queue",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[Key::CTRL_Q],
        slash: Some("/queue"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Inspect and manage pending message outbox",
    },
    CommandSpec {
        id: CommandId::OpenThreadStats,
        label: "Thread Stats",
        hint: "/stats",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/stats"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View context token accounting and thread stats",
    },
    CommandSpec {
        id: CommandId::OpenSessionTrace,
        label: "Session Trace",
        hint: "/trace",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/trace"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Inspect session execution trace and latency waterfall",
    },
    CommandSpec {
        id: CommandId::OpenModels,
        label: "Switch Model",
        hint: "/models",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/models"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Browse and select available LLM models",
    },
    CommandSpec {
        id: CommandId::OpenConnections,
        label: "Connections",
        hint: "/connections",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/connections"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Manage LLM provider endpoints and API credentials",
    },
    CommandSpec {
        id: CommandId::OpenActiveConnectionDetail,
        label: "Active Connection Detail",
        hint: "/connections",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Open the active provider connection detail sheet",
    },
    CommandSpec {
        id: CommandId::OpenPortShare,
        label: "Port & Remote Sharing",
        hint: "/port",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/port"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Inspect, enable, or disable server TCP port and remote access",
    },
    CommandSpec {
        id: CommandId::OpenTools,
        label: "Tools",
        hint: "/tools",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/tools"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Inspect and configure session tool capability pool",
    },
    CommandSpec {
        id: CommandId::OpenMcp,
        label: "MCP Servers",
        hint: "/mcp",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/mcp"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Manage Model Context Protocol server connections",
    },
    CommandSpec {
        id: CommandId::OpenSkills,
        label: "Skills",
        hint: "/skills",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/skills"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Inspect discovered workspace skills and guidelines",
    },
    CommandSpec {
        id: CommandId::OpenPermissions,
        label: "Permissions",
        hint: "/permissions",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/permissions"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Review and revoke cached tool execution rules",
    },
    CommandSpec {
        id: CommandId::OpenUsage,
        label: "Usage Statistics",
        hint: "/usage",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/usage"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View cross-session token usage and activity ledger",
    },
    CommandSpec {
        id: CommandId::OpenQuotas,
        label: "Provider Quotas",
        hint: "/quotas",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/quotas"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View provider quota pools, sliding-window allowances, and account status",
    },
    CommandSpec {
        id: CommandId::OpenTree,
        label: "Session Tree",
        hint: "/tree",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/tree"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View DAG tree of session rounds and turns",
    },
    CommandSpec {
        id: CommandId::ForkThread,
        label: "Fork Thread",
        hint: "/fork",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[],
        slash: Some("/fork"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Fork current thread into a new sovereign tab (ADR-0058)",
    },
    CommandSpec {
        id: CommandId::OpenSessions,
        label: "Threads",
        hint: "C-x s",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/threads"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Switch between saved project threads (C-x s / /threads)",
    },
    // Management Actions
    CommandSpec {
        id: CommandId::ToggleQueueBlock,
        label: "Block / Resume Queue",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/queue block"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Cautious,
        description: "Toggle dispatch latch on outgoing follow-up messages",
    },
    CommandSpec {
        id: CommandId::ClearQueue,
        label: "Clear Queue",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/queue clear"),
        availability: avail_queue_nonempty,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Dangerous,
        description: "Discard all staged outgoing messages in outbox",
    },
    CommandSpec {
        id: CommandId::PermissionsClearAll,
        label: "Revoke All Permissions",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/permissions clear"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Dangerous,
        description: "Clear all cached execution rules for workspace",
    },
    CommandSpec {
        id: CommandId::ProviderAddConnection,
        label: "Add Provider Connection",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::Global,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Configure a new provider or custom endpoint",
    },
    CommandSpec {
        id: CommandId::RedrawScreen,
        label: "Redraw Screen",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/redraw"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Force full TUI terminal redraw and layout sync",
    },
    // Dialog Actions: Threads
    CommandSpec {
        id: CommandId::SessionOpenSelected,
        label: "Open Thread",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Threads),
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Open the highlighted thread",
    },
    CommandSpec {
        id: CommandId::SessionDeleteSelected,
        label: "Delete Thread",
        hint: "d",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Threads),
        bindings: &[Key::plain('d')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Dangerous,
        description: "Delete the selected thread permanently",
    },
    CommandSpec {
        id: CommandId::SessionCreateNew,
        label: "New Thread",
        hint: "n",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Threads),
        bindings: &[Key::plain('n')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Create a new thread",
    },
    CommandSpec {
        id: CommandId::SessionOpenInfo,
        label: "Thread Info",
        hint: "i",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Threads),
        bindings: &[Key::plain('i')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "View thread details, token usage, and history",
    },
    // Dialog Actions: Models
    CommandSpec {
        id: CommandId::ModelSelect,
        label: "Select Model",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Models),
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Activate the highlighted model",
    },
    CommandSpec {
        id: CommandId::ModelEnterSearch,
        label: "Search Models",
        hint: "/",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Models),
        bindings: &[Key::plain('/')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Filter models by name or id",
    },
    CommandSpec {
        id: CommandId::ModelToggleFavorite,
        label: "Toggle Favorite",
        hint: "*",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Models),
        bindings: &[Key::plain('*')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Star or unstar model as favorite",
    },
    CommandSpec {
        id: CommandId::ModelBlock,
        label: "Block Model",
        hint: "x",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Models),
        bindings: &[Key::plain('x')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Cautious,
        description: "Block or unblock model from routing",
    },
    CommandSpec {
        id: CommandId::ModelEditSettings,
        label: "Model Settings",
        hint: "e",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Models),
        bindings: &[Key::plain('e')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Configure per-model temperature and options",
    },
    CommandSpec {
        id: CommandId::ModelRefresh,
        label: "Refresh Models",
        hint: "r",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Models),
        bindings: &[Key::plain('r')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Fetch updated model list from provider",
    },
    // Dialog Actions: Connections
    CommandSpec {
        id: CommandId::ConnectionOpenDetail,
        label: "Connection Details",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Connections),
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "View connection configuration and status",
    },
    CommandSpec {
        id: CommandId::ConnectionEnterSearch,
        label: "Search Connections",
        hint: "/",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Connections),
        bindings: &[Key::plain('/')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Filter connections by name",
    },
    CommandSpec {
        id: CommandId::ConnectionOpenPreset,
        label: "Add Preset Connection",
        hint: "a",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Connections),
        bindings: &[Key::plain('a')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Add a provider connection from curated presets",
    },
    CommandSpec {
        id: CommandId::ConnectionOpenCustom,
        label: "Add Custom Connection",
        hint: "c",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Connections),
        bindings: &[Key::plain('c')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Add an OpenAI-compatible custom endpoint",
    },
    CommandSpec {
        id: CommandId::ConnectionEdit,
        label: "Edit Provider",
        hint: "e",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Connections),
        bindings: &[Key::plain('e')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Edit provider credentials and base URL",
    },
    CommandSpec {
        id: CommandId::ConnectionRefresh,
        label: "Refresh Provider Models",
        hint: "r",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Connections),
        bindings: &[Key::plain('r')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Refresh models published by connection",
    },
    CommandSpec {
        id: CommandId::ConnectionDelete,
        label: "Delete Provider",
        hint: "Shift-d",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Connections),
        bindings: &[Key::plain('D')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Dangerous,
        description: "Remove custom provider connection",
    },
    // Dialog Actions: MCP
    CommandSpec {
        id: CommandId::McpToggleServer,
        label: "Toggle MCP Server",
        hint: "Space",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Mcp),
        bindings: &[Key::plain(' ')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Enable or disable highlighted MCP server",
    },
    CommandSpec {
        id: CommandId::McpReconnectServer,
        label: "Reconnect Server",
        hint: "r",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Mcp),
        bindings: &[Key::plain('r')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Restart and reconnect highlighted MCP server",
    },
    // Dialog Actions: Permissions
    CommandSpec {
        id: CommandId::PermissionToggleRule,
        label: "Toggle Permission Rule",
        hint: "Space",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Permissions),
        bindings: &[Key::plain(' ')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Toggle rule allow / deny status",
    },
    // Dialog Actions: Queue
    CommandSpec {
        id: CommandId::QueueRecallItem,
        label: "Recall Queued Message",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Queue),
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Recall queued message back into composer",
    },
    CommandSpec {
        id: CommandId::QueueDeleteItem,
        label: "Delete Queued Message",
        hint: "Shift-d",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Queue),
        bindings: &[Key::plain('D')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Dangerous,
        description: "Remove highlighted message from outgoing queue",
    },
    CommandSpec {
        id: CommandId::QueueMoveItemUp,
        label: "Move Up in Queue",
        hint: "Shift-k",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Queue),
        bindings: &[Key::plain('K')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Move highlighted message earlier in dispatch order",
    },
    CommandSpec {
        id: CommandId::QueueMoveItemDown,
        label: "Move Down in Queue",
        hint: "Shift-j",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Queue),
        bindings: &[Key::plain('J')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Move highlighted message later in dispatch order",
    },
    // Dialog Actions: Thread Stats
    CommandSpec {
        id: CommandId::ThreadStatsOpenTrace,
        label: "Open Trace",
        hint: "t",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::ThreadStats),
        bindings: &[Key::plain('t')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Open session execution trace from thread stats",
    },
    // Dialog Actions: Session Trace
    CommandSpec {
        id: CommandId::TraceInspectDetail,
        label: "Inspect Attempt Details",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::SessionTrace),
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Inspect individual round attempt breakdown",
    },
    // Dialog Actions: Skills
    CommandSpec {
        id: CommandId::SkillsToggleDetail,
        label: "Toggle Skill Details",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Skills),
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Expand or collapse skill documentation",
    },
    // Dialog Actions: History Search
    CommandSpec {
        id: CommandId::HistorySearchInsert,
        label: "Insert History Entry",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::HistorySearch),
        bindings: &[Key::ENTER, Key::TAB],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Insert selected prompt history into composer",
    },
    CommandSpec {
        id: CommandId::HistorySearchDelete,
        label: "Delete History Entry",
        hint: "Shift-Delete",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::HistorySearch),
        bindings: &[Key::SHIFT_DELETE],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Dangerous,
        description: "Permanently delete prompt from history",
    },
];

// Registry Lookup & Derivation Utilities

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
        CommandId::CommandPalette => None,
        CommandId::Quit => Some(Key::CTRL_C),
        CommandId::CloseTab => Some(Key::ALT_W),
        CommandId::KillThread => None,
        CommandId::CopySelection => Some(Key::CTRL_SHIFT_C),
        CommandId::OpenThreadStats => None,
        CommandId::OpenSessionTrace => None,
        CommandId::OpenActiveConnectionDetail => None,
        CommandId::CancelOrBack => Some(Key::ESC),
        CommandId::OpenQueue => Some(Key::CTRL_Q),
        CommandId::OpenNotices => Some(Key::CTRL_O),
        CommandId::OpenHelp => Some(Key::F1),
        _ => None,
    }
}

/// The canonical resolution table (the hard-bound globals + the bar chords:
/// Ctrl+Q queue, Esc dismiss/step-back, Ctrl+C quit,
/// Ctrl+Shift+C copy), ignoring user overrides. Esc resolves to
/// [`CommandId::CancelOrBack`], which never navigates between Scenes.
/// The Command Palette is accessible via `/palette` and `p` in the Help dialog (ADR-0131).
fn canonical_global_key(key: Key) -> Option<CommandId> {
    if key == Key::F1 || key == Key::ALT_H {
        Some(CommandId::OpenHelp)
    } else if key == Key::CTRL_Q {
        Some(CommandId::OpenQueue)
    } else if key == Key::CTRL_O {
        Some(CommandId::OpenNotices)
    } else if key == Key::ESC {
        Some(CommandId::CancelOrBack)
    } else if key == Key::CTRL_C {
        Some(CommandId::Quit)
    } else if key == Key::ALT_W {
        Some(CommandId::CloseTab)
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
        "interrupt" | "interrupt_task" | "interrupt-task" => CommandId::InterruptTask,
        "quit" | "quit_nuo" | "quit-nuo" => CommandId::Quit,
        "close_tab" | "close-tab" | "close" => CommandId::CloseTab,
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
    /// Unknown command ids and unparseable chords are skipped; `Esc`/Back is
    /// deliberately not remappable (it is the universal escape hatch).
    pub fn from_config(map: &HashMap<String, String>) -> Self {
        let mut o = Self::default();
        for (id, spec) in map {
            if let (Some(cmd), Some(key)) = (command_id_from_name(id), parse_key(spec))
                && cmd != CommandId::CancelOrBack
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
/// surface's run state. The multi-mode interaction grammar (Enter send/queue/
/// activate/commit, Tab commit/focus, Esc dismiss/focus/interrupt, ↑/↓ walk,
/// printable text) is deliberately **not** remappable — it is the surface's
/// language, not a shortcut, mirroring how `Esc`/Back are never remappable in
/// the global layer.
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
    /// Clear step focus back to the composer (`Esc`).
    ClearFocusedTarget,
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
            SurfaceVerb::ClearFocusedTarget => Key::ESC,
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
            SurfaceVerb::ClearFocusedTarget => "clear_focus",
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
    pub const ALL: [SurfaceVerb; 11] = [
        SurfaceVerb::OpenHistory,
        SurfaceVerb::ToggleSendMode,
        SurfaceVerb::HistoryPrev,
        SurfaceVerb::HistoryNext,
        SurfaceVerb::FocusPrevTarget,
        SurfaceVerb::FocusNextTarget,
        SurfaceVerb::ClearFocusedTarget,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_keys_resolve_correctly() {
        // The Command Palette has no canonical single-stroke chord: it is
        // opened by the `C-x` scene namespace's `p` verb (ADR-0023).
        assert_eq!(resolve_global_key(Key::CTRL_L), None);
        // Ctrl-o is the notice viewer's chord: the fault bubble advertises it,
        // so it must resolve to the verb the bubble names.
        assert_eq!(
            resolve_global_key(Key::CTRL_O),
            Some(CommandId::OpenNotices)
        );
        assert_eq!(resolve_global_key(Key::ESC), Some(CommandId::CancelOrBack));
        assert_eq!(resolve_global_key(Key::CTRL_C), Some(CommandId::Quit));
        assert_eq!(resolve_global_key(Key::CTRL_Q), Some(CommandId::OpenQueue));
        assert_eq!(
            resolve_global_key(Key::CTRL_SHIFT_C),
            Some(CommandId::CopySelection)
        );
        assert_eq!(
            resolve_global_key(Key::CMD_C),
            Some(CommandId::CopySelection)
        );
        // Ctrl+P and Ctrl+N are session navigation verbs, not global commands.
        assert_eq!(resolve_global_key(Key::CTRL_P), None);
        assert_eq!(resolve_global_key(Key::CTRL_N), None);
    }

    #[test]
    fn every_global_binding_has_a_resolver_handler() {
        // Any Global-scope command that declares a key binding must be
        // resolvable by resolve_global_key. This is the guard against the
        // "hint advertised but no handler" desync — e.g. the former dead
        // Ctrl+O / Ctrl+N / top-level Ctrl+P.
        for cmd in COMMAND_REGISTRY {
            if cmd.scope == Scope::Global && !cmd.bindings.is_empty() {
                assert!(
                    cmd.bindings
                        .iter()
                        .any(|&k| resolve_global_key(k) == Some(cmd.id)),
                    "global command {:?} binding(s) are not resolvable: {:?}",
                    cmd.id,
                    cmd.bindings
                );
            }
        }
    }

    #[test]
    fn no_global_binding_is_shared_between_two_commands() {
        // Within the Global layer a single chord must never mean two different
        // commands, or a global hint could resolve to the wrong action.
        // (Contextual commands may deliberately reuse a chord — e.g. `Enter`
        // is both SendPrompt and QueueFollowUp depending on run state — so
        // the invariant is enforced for Global scope only.)
        let mut seen: Vec<(Key, CommandId)> = Vec::new();
        for cmd in COMMAND_REGISTRY {
            if cmd.scope != Scope::Global {
                continue;
            }
            for &k in cmd.bindings {
                if let Some((_, prev)) = seen.iter().find(|(sk, _)| *sk == k) {
                    panic!("global binding {k:?} shared by {:?} and {:?}", prev, cmd.id);
                }
                seen.push((k, cmd.id));
            }
        }
    }

    #[test]
    fn non_global_keys_do_not_resolve_as_global() {
        assert_eq!(resolve_global_key(Key::ENTER), None);
        assert_eq!(resolve_global_key(Key::TAB), None);
        assert_eq!(resolve_global_key(Key::CTRL_R), None);
        assert_eq!(resolve_global_key(Key::ctrl('x')), None);
        assert_eq!(resolve_global_key(Key::alt('x')), None);
    }

    #[test]
    fn parse_key_round_trips_chord_specs() {
        // Each spec parses to the exact Key the input layer produces for that
        // keystroke (via Key::from_event), so comparisons hold.
        // Seamlessly supports both '-' and '+' delimiters.
        assert_eq!(parse_key("ctrl+p"), Some(Key::CTRL_P));
        assert_eq!(parse_key("ctrl-p"), Some(Key::CTRL_P));
        assert_eq!(parse_key("ctrl+shift+c"), Some(Key::CTRL_SHIFT_C));
        assert_eq!(parse_key("ctrl-shift-c"), Some(Key::CTRL_SHIFT_C));
        assert_eq!(parse_key("f5"), Some(Key::F5));
        assert_eq!(parse_key("esc"), Some(Key::ESC));
        assert_eq!(
            parse_key("space"),
            Some(Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char(' ')
            })
        );
        assert_eq!(parse_key("shift+tab"), Some(Key::BACKTAB));
        assert_eq!(parse_key("shift-tab"), Some(Key::BACKTAB));
        assert_eq!(parse_key("alt+s"), Some(Key::ALT_S));
        assert_eq!(parse_key("alt-s"), Some(Key::ALT_S));
        assert_eq!(parse_key("ctrl+x"), Some(Key::ctrl('x')));
        assert_eq!(parse_key("ctrl-x"), Some(Key::ctrl('x')));
        assert_eq!(parse_key("nonsense"), None);
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn key_display_preserves_case_and_uses_hyphen_convention() {
        assert_eq!(Key::CTRL_O.display(), "Ctrl-o");
        assert_eq!(Key::CTRL_N.display(), "Ctrl-n");
        assert_eq!(Key::CTRL_P.display(), "Ctrl-p");
        assert_eq!(Key::CTRL_C.display(), "Ctrl-c");
        assert_eq!(Key::CTRL_SHIFT_C.display(), "Ctrl-Shift-c");
        assert_eq!(Key::ALT_S.display(), "Alt-s");
        assert_eq!(Key::ALT_ENTER.display(), "Alt-Enter");
        assert_eq!(Key::CMD_C.display(), "Cmd-c");
        assert_eq!(Key::ESC.display(), "Esc");
        assert_eq!(Key::ENTER.display(), "Enter");
        assert_eq!(Key::TAB.display(), "Tab");
        assert_eq!(Key::BACKTAB.display(), "⇧Tab");
        assert_eq!(Key::F5.display(), "F5");
        assert_eq!(
            Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('o')
            }
            .display(),
            "o"
        );
        assert_eq!(
            Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('O')
            }
            .display(),
            "O"
        );
        assert_eq!(
            Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('?')
            }
            .display(),
            "?"
        );
        assert_eq!(
            Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('/')
            }
            .display(),
            "/"
        );
    }

    #[test]
    fn global_overrides_remap_resolution_and_effective_binding() {
        let mut map = std::collections::HashMap::new();
        map.insert("palette".to_string(), "ctrl+k".to_string());
        map.insert("quit".to_string(), "ctrl+shift+q".to_string());
        map.insert("not_a_command".to_string(), "ctrl+z".to_string());
        let o = GlobalOverrides::from_config(&map);

        // The remapped chord fires; the retired canonical chord stays dead
        // (the palette has no canonical single-stroke chord at all now — ADR-0023).
        assert_eq!(
            resolve_global_key_with(Key::ctrl('k'), &o),
            Some(CommandId::CommandPalette)
        );
        assert_eq!(resolve_global_key_with(Key::CTRL_L, &o), None);
        // The remapped quit fires; canonical Ctrl+C / Ctrl+Q is dead.
        let ctrl_shift_q = Key {
            modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
            code: KeyCode::Char('q'),
        };
        assert_eq!(
            resolve_global_key_with(ctrl_shift_q, &o),
            Some(CommandId::Quit)
        );
        assert_eq!(resolve_global_key_with(Key::CTRL_C, &o), None);
        // Ctrl+Q was never touched by this override set, so it still fires.
        assert_eq!(
            resolve_global_key_with(Key::CTRL_Q, &o),
            Some(CommandId::OpenQueue)
        );
        // Commands without canonical chords resolve to None. Ctrl-o *has* a
        // canonical chord (the notice viewer) and is untouched by this override
        // set, so it still fires.
        assert_eq!(
            resolve_global_key_with(Key::CTRL_O, &o),
            Some(CommandId::OpenNotices)
        );
        assert_eq!(resolve_global_key_with(Key::CTRL_L, &o), None);
        // Effective binding follows the override for remapped commands.
        assert_eq!(o.effective_binding(CommandId::Quit), Some(ctrl_shift_q));
        // A command with no canonical chord reports none — chrome renders no
        // keycap for it rather than a placeholder.
        assert_eq!(o.effective_binding(CommandId::InterruptTask), None);
        // Esc / Back is never remappable.
        let mut esc_map = std::collections::HashMap::new();
        esc_map.insert("cancel".to_string(), "ctrl+k".to_string());
        let o2 = GlobalOverrides::from_config(&esc_map);
        assert_eq!(
            o2.effective_binding(CommandId::CancelOrBack),
            Some(Key::ESC)
        );
        assert!(o2.is_empty());
    }

    /// Chrome's keycaps and the dispatcher read one table: every chord a
    /// `Global` command advertises in the registry resolves back to that exact
    /// command. This is the guard that would have caught the queue bar's
    /// hardcoded `Ctrl-q`.
    #[test]
    fn every_advertised_global_binding_resolves_to_its_command() {
        for cmd in COMMAND_REGISTRY {
            if cmd.scope != Scope::Global {
                continue;
            }
            for &key in cmd.bindings {
                assert_eq!(
                    resolve_global_key(key),
                    Some(cmd.id),
                    "registry advertises {key:?} for {:?}, but dispatch resolves it elsewhere",
                    cmd.id
                );
            }
            // The display path agrees with the dispatch path.
            let display = GlobalOverrides::default().effective_binding(cmd.id);
            if !cmd.bindings.is_empty() {
                assert_eq!(
                    display,
                    Some(cmd.bindings[0]),
                    "{:?} advertises {:?} but would render {display:?}",
                    cmd.id,
                    cmd.bindings[0]
                );
            }
        }
    }

    #[test]
    fn command_id_from_name_accepts_snake_and_kebab() {
        assert_eq!(
            command_id_from_name("interrupt"),
            Some(CommandId::InterruptTask)
        );
        assert_eq!(
            command_id_from_name("command-palette"),
            Some(CommandId::CommandPalette)
        );
        assert_eq!(
            command_id_from_name("telemetry"),
            Some(CommandId::OpenSessionTrace)
        );
        assert_eq!(command_id_from_name("nope"), None);
    }

    #[test]
    fn surface_overrides_remap_effective_binding_and_matches() {
        let mut map = std::collections::HashMap::new();
        map.insert("open_history".to_string(), "ctrl+shift+r".to_string());
        map.insert("toggle_send_mode".to_string(), "ctrl+t".to_string());
        map.insert("not_a_verb".to_string(), "ctrl+z".to_string());
        map.insert("interrupt".to_string(), "ctrl+x".to_string()); // global, ignored here
        map.insert("history_prev".to_string(), "bogus!!".to_string()); // bad chord, skipped
        let o = SurfaceOverrides::from_config(&map);

        assert_eq!(
            o.effective_binding(SurfaceVerb::OpenHistory),
            Key::CTRL_SHIFT_R
        );
        assert_eq!(
            o.effective_binding(SurfaceVerb::ToggleSendMode),
            Key::CTRL_T
        );
        // Unremapped verbs keep their canonical chord.
        assert_eq!(o.effective_binding(SurfaceVerb::HistoryNext), Key::ALT_N);
        assert_eq!(o.effective_binding(SurfaceVerb::ScrollTop), Key::HOME);
        // matches() follows the effective binding; the canonical chord is dead.
        assert!(o.matches(Key::CTRL_SHIFT_R, SurfaceVerb::OpenHistory));
        assert!(!o.matches(Key::CTRL_R, SurfaceVerb::OpenHistory));
        assert!(o.matches(Key::CTRL_T, SurfaceVerb::ToggleSendMode));
        assert!(!o.matches(Key::TAB, SurfaceVerb::ToggleSendMode));
        assert!(o.matches(Key::ALT_N, SurfaceVerb::HistoryNext));
        assert!(!o.is_empty());
    }

    #[test]
    fn every_surface_verb_has_a_name_and_a_canonical_chord() {
        for verb in SurfaceVerb::ALL {
            assert!(!verb.name().is_empty(), "{verb:?} has no config name");
            assert!(
                SurfaceVerb::from_name(verb.name()) == Some(verb),
                "{verb:?} name does not round-trip"
            );
            let _ = verb.canonical();
            // A remapped verb must keep resolving through its own chord.
            let mut map = std::collections::HashMap::new();
            map.insert(verb.name().to_string(), "ctrl+shift+p".to_string());
            let o = SurfaceOverrides::from_config(&map);
            assert!(
                o.matches(Key::CTRL_SHIFT_P, verb),
                "{verb:?} override did not land on its effective chord"
            );
        }
    }

    #[test]
    fn all_commands_have_valid_labels_and_descriptions() {
        for cmd in COMMAND_REGISTRY {
            assert!(
                !cmd.label.is_empty(),
                "command {:?} has empty label",
                cmd.id
            );
            assert!(
                !cmd.description.is_empty(),
                "command {:?} has empty description",
                cmd.id
            );
        }
    }

    #[test]
    fn find_by_slash_resolves_all_slash_triggers() {
        assert_eq!(
            find_by_slash("/models").map(|c| c.id),
            Some(CommandId::OpenModels)
        );
        // ADR-0041 [INV-CMD-01]: Workspace views are purged from thread slash triggers
        assert!(find_by_slash("/settings").is_none());
        assert!(find_by_slash("/dashboard").is_none());
        assert_eq!(
            find_by_slash("/commands").map(|c| c.id),
            Some(CommandId::CommandPalette)
        );
    }
}
