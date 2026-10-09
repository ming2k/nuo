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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::surfaces::{DialogKind, SceneKind};

// Canonical key vocabulary and display formatting

/// Repeated legend tokens — glyph strings that stand for an affordance.
pub mod keyvocab {
    pub const ARROWS_UD: &str = "↑↓";
    pub const ARROWS_LR: &str = "←→";
    pub const UP: &str = "↑";
    pub const DOWN: &str = "↓";
    pub const SPACE: &str = "Space";
    pub const SHIFT_TAB: &str = "⇧Tab";
    pub const SHIFT_ENTER: &str = "⇧Enter";
    pub const SHIFT_DELETE: &str = "⇧Del";
    pub const DELETE: &str = "Del";
}

/// The compact token for a core [`KeyCode`] in lowercase chord notation.
pub const fn chord_token(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Char(c) => match c.to_ascii_lowercase() {
            'a' => "a",
            'b' => "b",
            'c' => "c",
            'd' => "d",
            'e' => "e",
            'f' => "f",
            'g' => "g",
            'h' => "h",
            'i' => "i",
            'j' => "j",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'o' => "o",
            'p' => "p",
            'q' => "q",
            'r' => "r",
            's' => "s",
            't' => "t",
            'u' => "u",
            'v' => "v",
            'w' => "w",
            'x' => "x",
            'y' => "y",
            'z' => "z",
            '0' => "0",
            '1' => "1",
            '2' => "2",
            '3' => "3",
            '4' => "4",
            '5' => "5",
            '6' => "6",
            '7' => "7",
            '8' => "8",
            '9' => "9",
            '?' => "?",
            '/' => "/",
            _ => "·",
        },
        KeyCode::Enter => "enter",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => "shift-tab",
        KeyCode::Delete => "del",
        KeyCode::Backspace => "backspace",
        KeyCode::Esc => "esc",
        KeyCode::Up => "↑",
        KeyCode::Down => "↓",
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "pgup",
        KeyCode::PageDown => "pgdn",
        KeyCode::F(1) => "f1",
        KeyCode::F(2) => "f2",
        KeyCode::F(3) => "f3",
        KeyCode::F(4) => "f4",
        KeyCode::F(5) => "f5",
        _ => "·",
    }
}

/// The display token for a core [`KeyCode`] in human notation, preserving exact case.
pub const fn display_token(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Char(c) => match c {
            'a' => "a",
            'b' => "b",
            'c' => "c",
            'd' => "d",
            'e' => "e",
            'f' => "f",
            'g' => "g",
            'h' => "h",
            'i' => "i",
            'j' => "j",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'o' => "o",
            'p' => "p",
            'q' => "q",
            'r' => "r",
            's' => "s",
            't' => "t",
            'u' => "u",
            'v' => "v",
            'w' => "w",
            'x' => "x",
            'y' => "y",
            'z' => "z",
            'A' => "A",
            'B' => "B",
            'C' => "C",
            'D' => "D",
            'E' => "E",
            'F' => "F",
            'G' => "G",
            'H' => "H",
            'I' => "I",
            'J' => "J",
            'K' => "K",
            'L' => "L",
            'M' => "M",
            'N' => "N",
            'O' => "O",
            'P' => "P",
            'Q' => "Q",
            'R' => "R",
            'S' => "S",
            'T' => "T",
            'U' => "U",
            'V' => "V",
            'W' => "W",
            'X' => "X",
            'Y' => "Y",
            'Z' => "Z",
            '0' => "0",
            '1' => "1",
            '2' => "2",
            '3' => "3",
            '4' => "4",
            '5' => "5",
            '6' => "6",
            '7' => "7",
            '8' => "8",
            '9' => "9",
            '?' => "?",
            '/' => "/",
            '[' => "[",
            ']' => "]",
            ' ' => "Space",
            _ => "·",
        },
        KeyCode::Enter => "Enter",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => keyvocab::SHIFT_TAB,
        KeyCode::Delete => keyvocab::DELETE,
        KeyCode::Backspace => "Backspace",
        KeyCode::Esc => "Esc",
        KeyCode::Up => keyvocab::UP,
        KeyCode::Down => keyvocab::DOWN,
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::F(1) => "F1",
        KeyCode::F(2) => "F2",
        KeyCode::F(3) => "F3",
        KeyCode::F(4) => "F4",
        KeyCode::F(5) => "F5",
        _ => "·",
    }
}

/// A physical key with optional modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub modifiers: KeyModifiers,
    pub code: KeyCode,
}

impl Key {
    pub const ESC: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Esc,
    };
    pub const ENTER: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Enter,
    };
    pub const TAB: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Tab,
    };
    pub const BACKTAB: Key = Key {
        modifiers: KeyModifiers::SHIFT,
        code: KeyCode::BackTab,
    };
    pub const BRACKET_LEFT: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char('['),
    };
    pub const BRACKET_RIGHT: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char(']'),
    };
    pub const ALT_BRACKET_LEFT: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Char('['),
    };
    pub const ALT_BRACKET_RIGHT: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Char(']'),
    };
    pub const UP: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Up,
    };
    pub const DOWN: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Down,
    };
    pub const DELETE: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Delete,
    };
    pub const SHIFT_DELETE: Key = Key {
        modifiers: KeyModifiers::SHIFT,
        code: KeyCode::Delete,
    };
    pub const PAGE_UP: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::PageUp,
    };
    pub const PAGE_DOWN: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::PageDown,
    };
    pub const HOME: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Home,
    };
    pub const END: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::End,
    };
    pub const F5: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(5),
    };

    pub const CTRL_L: Key = Key::ctrl('l');
    pub const CTRL_C: Key = Key::ctrl('c');
    pub const CTRL_X: Key = Key::ctrl('x');
    pub const CTRL_P: Key = Key::ctrl('p');
    pub const CTRL_Q: Key = Key::ctrl('q');
    pub const CTRL_R: Key = Key::ctrl('r');
    pub const CTRL_O: Key = Key::ctrl('o');
    pub const CTRL_N: Key = Key::ctrl('n');
    pub const CTRL_T: Key = Key::ctrl('t');
    pub const CTRL_M: Key = Key::ctrl('m');
    pub const CTRL_S: Key = Key::ctrl('s');
    pub const CTRL_G: Key = Key::ctrl('g');
    pub const CTRL_J: Key = Key::ctrl('j');
    pub const CTRL_U: Key = Key::ctrl('u');
    pub const CTRL_A: Key = Key::ctrl('a');
    pub const CTRL_E: Key = Key::ctrl('e');
    pub const CTRL_K: Key = Key::ctrl('k');
    pub const CTRL_W: Key = Key::ctrl('w');
    pub const CTRL_V: Key = Key::ctrl('v');

    pub const ALT_S: Key = Key::alt('s');
    pub const ALT_W: Key = Key::alt('w');
    pub const ALT_P: Key = Key::alt('p');
    pub const ALT_N: Key = Key::alt('n');
    pub const ALT_UP: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Up,
    };
    pub const ALT_DOWN: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Down,
    };
    pub const ALT_ENTER: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Enter,
    };

    pub const CTRL_SHIFT_C: Key = Key {
        modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        code: KeyCode::Char('c'),
    };
    pub const CTRL_SHIFT_P: Key = Key {
        modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        code: KeyCode::Char('p'),
    };
    pub const CTRL_SHIFT_R: Key = Key {
        modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        code: KeyCode::Char('r'),
    };
    pub const CMD_C: Key = Key {
        modifiers: KeyModifiers::SUPER,
        code: KeyCode::Char('c'),
    };

    pub const fn ctrl(c: char) -> Self {
        Self {
            modifiers: KeyModifiers::CONTROL,
            code: KeyCode::Char(c),
        }
    }

    pub const fn alt(c: char) -> Self {
        Self {
            modifiers: KeyModifiers::ALT,
            code: KeyCode::Char(c),
        }
    }

    pub const fn plain(c: char) -> Self {
        Self {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char(c),
        }
    }

    pub fn from_event(event: KeyEvent) -> Self {
        let mut code = event.code;
        let mut modifiers = event.modifiers;

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

        Self { modifiers, code }
    }

    pub const fn shift_code(code: KeyCode) -> Self {
        Self {
            modifiers: KeyModifiers::SHIFT,
            code,
        }
    }

    pub const fn chord(&self) -> &'static str {
        let ctrl = self.modifiers.contains(KeyModifiers::CONTROL);
        let alt = self.modifiers.contains(KeyModifiers::ALT);
        let shift = self.modifiers.contains(KeyModifiers::SHIFT);
        let cmd = self.modifiers.contains(KeyModifiers::SUPER);

        if ctrl && shift {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "ctrl-shift-c",
                _ => "·",
            }
        } else if ctrl {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "ctrl-a",
                    'b' => "ctrl-b",
                    'c' => "ctrl-c",
                    'd' => "ctrl-d",
                    'e' => "ctrl-e",
                    'f' => "ctrl-f",
                    'g' => "ctrl-g",
                    'h' => "ctrl-h",
                    'i' => "ctrl-i",
                    'j' => "ctrl-j",
                    'k' => "ctrl-k",
                    'l' => "ctrl-l",
                    'm' => "ctrl-m",
                    'n' => "ctrl-n",
                    'o' => "ctrl-o",
                    'p' => "ctrl-p",
                    'q' => "ctrl-q",
                    'r' => "ctrl-r",
                    's' => "ctrl-s",
                    't' => "ctrl-t",
                    'u' => "ctrl-u",
                    'v' => "ctrl-v",
                    'w' => "ctrl-w",
                    'x' => "ctrl-x",
                    'y' => "ctrl-y",
                    'z' => "ctrl-z",
                    _ => "·",
                },
                KeyCode::Up => "ctrl-↑",
                KeyCode::Down => "ctrl-↓",
                KeyCode::Left => "ctrl-←",
                KeyCode::Right => "ctrl-→",
                _ => "·",
            }
        } else if alt {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "alt-a",
                    'b' => "alt-b",
                    'c' => "alt-c",
                    'd' => "alt-d",
                    'e' => "alt-e",
                    'f' => "alt-f",
                    'g' => "alt-g",
                    'h' => "alt-h",
                    'i' => "alt-i",
                    'j' => "alt-j",
                    'k' => "alt-k",
                    'l' => "alt-l",
                    'm' => "alt-m",
                    'n' => "alt-n",
                    'o' => "alt-o",
                    'p' => "alt-p",
                    'q' => "alt-q",
                    'r' => "alt-r",
                    's' => "alt-s",
                    't' => "alt-t",
                    'u' => "alt-u",
                    'v' => "alt-v",
                    'w' => "alt-w",
                    'x' => "alt-x",
                    'y' => "alt-y",
                    'z' => "alt-z",
                    _ => "·",
                },
                KeyCode::Enter => "alt-enter",
                KeyCode::Up => "alt-↑",
                KeyCode::Down => "alt-↓",
                _ => "·",
            }
        } else if shift {
            match self.code {
                KeyCode::Tab | KeyCode::BackTab => "shift-tab",
                KeyCode::Delete => "shift-delete",
                _ => chord_token(self.code),
            }
        } else if cmd {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "cmd-c",
                _ => "·",
            }
        } else {
            chord_token(self.code)
        }
    }

    pub const fn display(&self) -> &'static str {
        let ctrl = self.modifiers.contains(KeyModifiers::CONTROL);
        let alt = self.modifiers.contains(KeyModifiers::ALT);
        let shift = self.modifiers.contains(KeyModifiers::SHIFT);
        let cmd = self.modifiers.contains(KeyModifiers::SUPER);

        if ctrl && shift {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "Ctrl-Shift-c",
                KeyCode::Char('p') | KeyCode::Char('P') => "Ctrl-Shift-p",
                KeyCode::Char('r') | KeyCode::Char('R') => "Ctrl-Shift-r",
                KeyCode::Char('q') | KeyCode::Char('Q') => "Ctrl-Shift-q",
                _ => "·",
            }
        } else if ctrl {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "Ctrl-a",
                    'b' => "Ctrl-b",
                    'c' => "Ctrl-c",
                    'd' => "Ctrl-d",
                    'e' => "Ctrl-e",
                    'f' => "Ctrl-f",
                    'g' => "Ctrl-g",
                    'h' => "Ctrl-h",
                    'i' => "Ctrl-i",
                    'j' => "Ctrl-j",
                    'k' => "Ctrl-k",
                    'l' => "Ctrl-l",
                    'm' => "Ctrl-m",
                    'n' => "Ctrl-n",
                    'o' => "Ctrl-o",
                    'p' => "Ctrl-p",
                    'q' => "Ctrl-q",
                    'r' => "Ctrl-r",
                    's' => "Ctrl-s",
                    't' => "Ctrl-t",
                    'u' => "Ctrl-u",
                    'v' => "Ctrl-v",
                    'w' => "Ctrl-w",
                    'x' => "Ctrl-x",
                    'y' => "Ctrl-y",
                    'z' => "Ctrl-z",
                    _ => "·",
                },
                KeyCode::Up => "Ctrl-↑",
                KeyCode::Down => "Ctrl-↓",
                KeyCode::Left => "Ctrl-←",
                KeyCode::Right => "Ctrl-→",
                _ => "·",
            }
        } else if alt {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "Alt-a",
                    'b' => "Alt-b",
                    'c' => "Alt-c",
                    'd' => "Alt-d",
                    'e' => "Alt-e",
                    'f' => "Alt-f",
                    'g' => "Alt-g",
                    'h' => "Alt-h",
                    'i' => "Alt-i",
                    'j' => "Alt-j",
                    'k' => "Alt-k",
                    'l' => "Alt-l",
                    'm' => "Alt-m",
                    'n' => "Alt-n",
                    'o' => "Alt-o",
                    'p' => "Alt-p",
                    'q' => "Alt-q",
                    'r' => "Alt-r",
                    's' => "Alt-s",
                    't' => "Alt-t",
                    'u' => "Alt-u",
                    'v' => "Alt-v",
                    'w' => "Alt-w",
                    'x' => "Alt-x",
                    'y' => "Alt-y",
                    'z' => "Alt-z",
                    _ => "·",
                },
                KeyCode::Enter => "Alt-Enter",
                KeyCode::Up => "Alt-↑",
                KeyCode::Down => "Alt-↓",
                _ => "·",
            }
        } else if shift {
            match self.code {
                KeyCode::Tab | KeyCode::BackTab => keyvocab::SHIFT_TAB,
                KeyCode::Delete => keyvocab::SHIFT_DELETE,
                KeyCode::Char(c) => match c {
                    'a' => "Shift-a",
                    'b' => "Shift-b",
                    'c' => "Shift-c",
                    'd' => "Shift-d",
                    'e' => "Shift-e",
                    'f' => "Shift-f",
                    'g' => "Shift-g",
                    'h' => "Shift-h",
                    'i' => "Shift-i",
                    'j' => "Shift-j",
                    'k' => "Shift-k",
                    'l' => "Shift-l",
                    'm' => "Shift-m",
                    'n' => "Shift-n",
                    'o' => "Shift-o",
                    'p' => "Shift-p",
                    'q' => "Shift-q",
                    'r' => "Shift-r",
                    's' => "Shift-s",
                    't' => "Shift-t",
                    'u' => "Shift-u",
                    'v' => "Shift-v",
                    'w' => "Shift-w",
                    'x' => "Shift-x",
                    'y' => "Shift-y",
                    'z' => "Shift-z",
                    _ => display_token(self.code),
                },
                _ => display_token(self.code),
            }
        } else if cmd {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "Cmd-c",
                _ => "·",
            }
        } else {
            display_token(self.code)
        }
    }
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
    OpenSessionStats,
    OpenSessionTrace,
    OpenModels,
    OpenConnections,
    OpenActiveConnectionDetail,
    OpenTools,
    OpenMcp,
    OpenSkills,
    OpenPermissions,
    OpenUsage,
    OpenQuotas,
    OpenTree,
    OpenBtw,
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

    // Dialog Actions: Session Stats
    SessionStatsOpenTrace,

    // Dialog Actions: Session Trace
    TraceInspectDetail,

    // Dialog Actions: Skills
    SkillsToggleDetail,

    // Dialog Actions: Asides
    AsideFocus,
    AsideClose,
    AsideRefresh,

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

fn avail_selection(ctx: &AppContext) -> Availability {
    if ctx.has_selection {
        Availability::Available
    } else {
        Availability::Unavailable("no active selection")
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
        // Advertised as `C-x p` — the scene namespace's switcher verb
        // ([`scene_namespace::SceneVerb::Switcher`]), which is the palette's
        // sole canonical entry point (ADR-0023). There is **no**
        // single-stroke default binding: the namespace resolves *before* modal
        // dispatch, so `C-x p` reaches the palette from every context, and the
        // former `Ctrl-L` chord (whose modal prohibition the namespace makes
        // redundant) was retired rather than kept as a shadow path. The command
        // stays user-remappable through `[keybindings]`.
        hint: "C-x p",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/commands"),
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Open unified command palette and surface switcher (C-x p)",
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
        availability: avail_selection,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Copy selected text to clipboard",
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
        id: CommandId::OpenSessionStats,
        label: "Session Stats",
        hint: "Ctrl-o",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[Key::CTRL_O],
        slash: Some("/stats"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View context token accounting and session stats",
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
        id: CommandId::OpenBtw,
        label: "Asides (/btw)",
        hint: "/btw",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/btw"),
        availability: avail_session,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "List background aside threads",
    },
    CommandSpec {
        id: CommandId::OpenSessions,
        label: "Sessions",
        hint: "C-x s",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/sessions"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Switch between saved project sessions (C-x s)",
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
    // Dialog Actions: Sessions
    CommandSpec {
        id: CommandId::SessionOpenSelected,
        label: "Open Session",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Sessions),
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Open the highlighted session",
    },
    CommandSpec {
        id: CommandId::SessionDeleteSelected,
        label: "Delete Session",
        hint: "d",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Sessions),
        bindings: &[Key::plain('d')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Dangerous,
        description: "Delete the selected session permanently",
    },
    CommandSpec {
        id: CommandId::SessionCreateNew,
        label: "New Session",
        hint: "n",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Sessions),
        bindings: &[Key::plain('n')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Create a new session",
    },
    CommandSpec {
        id: CommandId::SessionOpenInfo,
        label: "Session Info",
        hint: "i",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Sessions),
        bindings: &[Key::plain('i')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "View session details, token usage, and history",
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
    // Dialog Actions: Session Stats
    CommandSpec {
        id: CommandId::SessionStatsOpenTrace,
        label: "Open Trace",
        hint: "t",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::SessionStats),
        bindings: &[Key::plain('t')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Open session execution trace from session stats",
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
    // Dialog Actions: Asides
    CommandSpec {
        id: CommandId::AsideFocus,
        label: "Focus Aside",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Asides),
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Open and focus highlighted aside thread",
    },
    CommandSpec {
        id: CommandId::AsideClose,
        label: "Close Aside",
        hint: "Shift-d",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Asides),
        bindings: &[Key::plain('D')],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Dangerous,
        description: "Close highlighted aside thread",
    },
    CommandSpec {
        id: CommandId::AsideRefresh,
        label: "Refresh Asides",
        hint: "F5",
        category: CommandCategory::Actions,
        scope: Scope::Dialog(DialogKind::Asides),
        bindings: &[Key::F5],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Reload active aside threads list",
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
/// shows. The discovery side of a keybinding scheme (ADR-0172): a hint renders
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

/// The `Ctrl+X` **scene namespace**: a two-stroke chord whose second stroke is
/// a scene-lifecycle verb (ADR-0298).
///
/// This table is the single owner of the namespace. The router resolves the
/// second stroke through [`SceneVerb::from_key`] and the which-key card renders
/// rows straight from [`SceneVerb::ALL`] — so a verb cannot be dispatchable
/// without being advertised, nor advertised without being dispatchable (the
/// defect class ADR-0238 exists to prevent: the queue bar once rendered a
/// `Ctrl-q` that resolved to nothing).
pub mod scene_namespace {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::Key;

    /// The namespace's opening stroke.
    pub const OPEN: Key = Key::CTRL_X;

    /// One scene-lifecycle verb reachable as `C-x <stroke>`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum SceneVerb {
        /// Leave the current scene (or dismiss a foreground dialog first).
        Leave,
        /// Open (or close) the Command Palette / surface switcher.
        Switcher,
        /// Open the saved sessions picker dialog (`/sessions`).
        Sessions,
        /// Navigate directly to the server session orchestrator dashboard (`/dashboard`).
        Dashboard,
        /// Quit nuo — the same armed double-press as the global `Ctrl+C`.
        Quit,
    }

    impl SceneVerb {
        /// Every verb, in advertisement order. The which-key card renders this
        /// slice; the router resolves against it. One list, two consumers.
        pub const ALL: &'static [SceneVerb] = &[
            SceneVerb::Leave,
            SceneVerb::Switcher,
            SceneVerb::Sessions,
            SceneVerb::Dashboard,
            SceneVerb::Quit,
        ];

        /// The second-stroke chords that fire this verb. More than one spelling
        /// is allowed when a convention is genuinely shared (Emacs' `C-x w`
        /// close and vi's `q`-adjacent `k`); the first entry is the one the
        /// card advertises.
        pub const fn strokes(self) -> &'static [Key] {
            match self {
                // `w` (Emacs `C-x C-w`-family window/`C-x w` muscle memory) and
                // `k` (the close-window convention). Case-insensitive: the
                // router folds the second stroke, since a leader chord is
                // typed blind.
                SceneVerb::Leave => &[
                    Key {
                        modifiers: KeyModifiers::NONE,
                        code: KeyCode::Char('w'),
                    },
                    Key {
                        modifiers: KeyModifiers::NONE,
                        code: KeyCode::Char('k'),
                    },
                ],
                // `p` (palette) and `b` (buffer/switch — the Emacs `C-x b`
                // muscle memory for "switch what I'm looking at").
                SceneVerb::Switcher => &[
                    Key {
                        modifiers: KeyModifiers::NONE,
                        code: KeyCode::Char('p'),
                    },
                    Key {
                        modifiers: KeyModifiers::NONE,
                        code: KeyCode::Char('b'),
                    },
                ],
                // `s` (sessions — opens the saved sessions overview picker).
                SceneVerb::Sessions => &[Key {
                    modifiers: KeyModifiers::NONE,
                    code: KeyCode::Char('s'),
                }],
                // `d` (dashboard — opens server session orchestrator dashboard).
                SceneVerb::Dashboard => &[Key {
                    modifiers: KeyModifiers::NONE,
                    code: KeyCode::Char('d'),
                }],
                // `C-c` mirrors the global quit chord's spelling inside the
                // namespace (Emacs' `C-x C-c`).
                SceneVerb::Quit => &[Key {
                    modifiers: KeyModifiers::CONTROL,
                    code: KeyCode::Char('c'),
                }],
            }
        }

        /// The chord the which-key card advertises for this verb.
        pub const fn advertised_stroke(self) -> Key {
            self.strokes()[0]
        }

        /// The card's label for this verb. The leave verb's wording is
        /// state-dependent and is supplied by the caller
        /// (`components::which_key::close_label_for`); this is its fallback.
        pub const fn label(self) -> &'static str {
            match self {
                SceneVerb::Leave => "leave scene",
                SceneVerb::Switcher => "command palette",
                SceneVerb::Sessions => "sessions",
                SceneVerb::Dashboard => "dashboard",
                SceneVerb::Quit => "quit nuo",
            }
        }

        /// The chord the which-key card prints for this verb, in the card's own
        /// two-stroke notation: a plain letter prints bare (`w`), a modified
        /// one prints its `C-` spelling (`C-c`), because the card is already
        /// inside the `C-x …` prefix.
        pub const fn advertised_stroke_display(self) -> &'static str {
            match self.advertised_stroke().modifiers {
                KeyModifiers::NONE => match self.advertised_stroke().code {
                    KeyCode::Char('w') => "w",
                    KeyCode::Char('k') => "k",
                    KeyCode::Char('p') => "p",
                    KeyCode::Char('b') => "b",
                    KeyCode::Char('s') => "s",
                    KeyCode::Char('d') => "d",
                    _ => "?",
                },
                _ => match self.advertised_stroke().code {
                    KeyCode::Char('c') => "C-c",
                    _ => "C-?",
                },
            }
        }

        /// Resolve a second stroke to its verb. `stroke` is the *folded* key
        /// (the router lowercases a shifted letter before calling): a leader
        /// chord is typed without looking, so case carries no meaning here.
        pub fn from_stroke(stroke: Key) -> Option<Self> {
            Self::ALL
                .iter()
                .copied()
                .find(|verb| verb.strokes().contains(&stroke))
        }

        /// Resolve a raw second-stroke keypress. Terminals deliver `C-x W` as
        /// `Char('W')` (the shift bit is folded into the character), so the
        /// case is normalized here rather than at each call site — the
        /// namespace is case-insensitive by design.
        pub fn from_second_stroke(key: Key) -> Option<Self> {
            let folded = match key.code {
                KeyCode::Char(c) => Key {
                    modifiers: key.modifiers,
                    code: KeyCode::Char(c.to_ascii_lowercase()),
                },
                _ => key,
            };
            Self::from_stroke(folded)
        }
    }

    /// Whether `key` is the namespace's opening stroke.
    pub fn opens(key: Key) -> bool {
        key == OPEN
    }

    /// Every second-stroke chord the namespace resolves, across all verbs. Used
    /// by tests to assert the table is internally consistent.
    pub fn strokes_of() -> Vec<Key> {
        SceneVerb::ALL
            .iter()
            .flat_map(|verb| verb.strokes().iter().copied())
            .collect()
    }
}

/// The canonical global chords a user may remap via the `[keybindings]`
/// config (ADR-0172 §"user-overridable schemes"). `Ctrl+X` is deliberately
/// absent: it is the two-stroke *scene namespace*'s opening stroke
/// ([`scene_namespace`]), resolved in the router before this table, so it has
/// no single-stroke `CommandId` to name. The Command Palette has
/// **no** canonical single-stroke chord either: it is opened by that
/// namespace's `p` verb (`C-x p`, ADR-0023) and remains remappable here.
fn canonical_global_chord(cmd: CommandId) -> Option<Key> {
    match cmd {
        // No canonical single-stroke chord: the palette is opened by the
        // `C-x` scene namespace's switcher verb (ADR-0023),
        // which the router resolves before the global table. This keeps the
        // keycap/footer display path honest — chrome renders no
        // keycap for a chord the global layer does not own.
        CommandId::CommandPalette => None,
        CommandId::Quit => Some(Key::CTRL_C),
        CommandId::CloseTab => Some(Key::ALT_W),
        CommandId::KillThread => None,
        CommandId::CopySelection => Some(Key::CTRL_SHIFT_C),
        CommandId::OpenSessionStats => Some(Key::CTRL_O),
        CommandId::OpenSessionTrace => None,
        CommandId::OpenActiveConnectionDetail => None,
        // `Esc` is a real chord, not a placeholder: it is the registry's
        // declared binding for CancelOrBack and it resolves there. (It is also
        // the value the display path used to fall back to for *every* chordless
        // command, which is what made an unbound command look bound.)
        CommandId::CancelOrBack => Some(Key::ESC),
        // The queue bar's expand affordance (ADR-0126's Ctrl row). Raw mode
        // clears `IXON` on both the direct and the multiplexer path, so the
        // chord reaches the app (ADR-0156 scopes the same reasoning).
        CommandId::OpenQueue => Some(Key::CTRL_Q),
        _ => None,
    }
}

/// The canonical resolution table (the hard-bound globals + the bar chords:
/// Ctrl+O session stats, Ctrl+Q queue, Esc dismiss/step-back, Ctrl+C quit,
/// Ctrl+Shift+C copy), ignoring user overrides. Esc resolves to
/// [`CommandId::CancelOrBack`], which never navigates between Scenes.
/// The Command Palette has **no** single-stroke global chord — it
/// is opened by the `C-x` scene namespace (ADR-0023).
fn canonical_global_key(key: Key) -> Option<CommandId> {
    if key == Key::CTRL_O {
        Some(CommandId::OpenSessionStats)
    } else if key == Key::CTRL_Q {
        Some(CommandId::OpenQueue)
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
        "stats" | "session_stats" | "session-stats" | "open_session_stats"
        | "open-session-stats" => CommandId::OpenSessionStats,
        "trace" | "session_trace" | "session-trace" | "open_session_trace"
        | "open-session-trace" | "telemetry" | "open_telemetry" | "open-telemetry" => {
            CommandId::OpenSessionTrace
        }
        "connection"
        | "connection_detail"
        | "active_connection_detail"
        | "active-connection-detail" => CommandId::OpenActiveConnectionDetail,
        _ => return None,
    })
}

/// Parse a `[keybindings]` chord spec like `"ctrl-shift-p"`, `"ctrl+p"`, `"f1"`, or
/// `"alt-enter"` into the exact [`Key`] the input layer produces for that
/// keystroke (normalized through [`Key::from_event`]), so a config chord and a
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
    Some(Key::from_event(KeyEvent::new(code?, modifiers)))
}

/// User remaps of the global chords (ADR-0172, `[keybindings]` config).
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
    /// none does (ADR-0238): a keycap is a promise, so there is no fallback
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
/// views own (ADR-0172 step 9). Verbs are the *shortcut* layer of a surface's
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
    /// Subagent-zoom sibling navigation (`[` / `]`).
    PrevSibling,
    NextSibling,
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
            SurfaceVerb::PrevSibling => Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('['),
            },
            SurfaceVerb::NextSibling => Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char(']'),
            },
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
            SurfaceVerb::PrevSibling => "prev_sibling",
            SurfaceVerb::NextSibling => "next_sibling",
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
        SurfaceVerb::PrevSibling,
        SurfaceVerb::NextSibling,
    ];
}

/// User remaps of the surface verbs (`[keybindings.session]` config table —
/// ADR-0172 step 9).
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
        .filter(|cmd| {
            if let Scope::Dialog(dialog) = cmd.scope {
                return ctx.active_dialog == Some(dialog);
            }
            cmd.disclosure >= DisclosurePriority::L2Palette || cmd.scope == Scope::Global
        })
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
        assert_eq!(
            resolve_global_key(Key::CTRL_O),
            Some(CommandId::OpenSessionStats)
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
        // Unremapped commands keep their canonical behavior.
        assert_eq!(
            resolve_global_key_with(Key::CTRL_O, &o),
            Some(CommandId::OpenSessionStats)
        );
        // Effective binding follows the override for remapped commands.
        assert_eq!(o.effective_binding(CommandId::Quit), Some(ctrl_shift_q));
        // A command with no canonical chord reports none — chrome renders no
        // keycap for it rather than a placeholder (ADR-0238).
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
    /// hardcoded `Ctrl-q` (ADR-0238).
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

    /// ADR-0298 §1: the `Ctrl+X` namespace's verb table is the single owner of
    /// both dispatch and advertisement. This is the guard that would have caught
    /// the queue bar's hardcoded `Ctrl-q` (ADR-0238) had the namespace been
    /// written the same way — a stroke that renders but resolves to nothing, or
    /// resolves but is never advertised.
    #[test]
    fn scene_namespace_is_internally_consistent() {
        use scene_namespace::{SceneVerb, strokes_of};

        // Every declared stroke resolves back to the verb that declares it,
        // under both case spellings a terminal can deliver.
        for verb in SceneVerb::ALL {
            for stroke in verb.strokes() {
                assert_eq!(
                    SceneVerb::from_stroke(*stroke),
                    Some(*verb),
                    "declared stroke {stroke:?} of {verb:?} does not resolve"
                );
                assert_eq!(
                    SceneVerb::from_second_stroke(*stroke),
                    Some(*verb),
                    "declared stroke {stroke:?} of {verb:?} fails case-folding"
                );
                if let KeyCode::Char(c) = stroke.code {
                    let upper = Key {
                        modifiers: stroke.modifiers,
                        code: KeyCode::Char(c.to_ascii_uppercase()),
                    };
                    assert_eq!(
                        SceneVerb::from_second_stroke(upper),
                        Some(*verb),
                        "the uppercase spelling of {stroke:?} must fold"
                    );
                }
            }
        }

        // No stroke is claimed by two verbs: a colliding table would make
        // `from_stroke` order-dependent and the card ambiguous.
        let mut seen: Vec<Key> = Vec::new();
        for stroke in strokes_of() {
            assert!(
                !seen.contains(&stroke),
                "stroke {stroke:?} is claimed by more than one scene verb"
            );
            seen.push(stroke);
        }

        // The advertised stroke is always one of the verb's real strokes — the
        // card can never print a key the dispatcher does not honour.
        for verb in SceneVerb::ALL {
            assert!(
                verb.strokes().contains(&verb.advertised_stroke()),
                "{verb:?} advertises a stroke it does not declare"
            );
        }

        // No advertised stroke is a bare `c`: `C-x c` must stay inert so a
        // mistyped `C-c` cannot quit the app through the namespace.
        for verb in SceneVerb::ALL {
            let stroke = verb.advertised_stroke();
            if stroke.code == KeyCode::Char('c') {
                assert!(
                    stroke.modifiers.contains(KeyModifiers::CONTROL),
                    "{verb:?} advertises a bare `c`, which would shadow the quit chord"
                );
            }
        }

        // The opening stroke is not itself a verb stroke (it re-arms).
        assert!(
            !strokes_of().contains(&scene_namespace::OPEN),
            "the opening stroke must not double as a verb stroke"
        );
    }

    /// Every verb the card can print has a label, and the leave verb's label is
    /// only a fallback — the caller supplies the state-dependent wording.
    #[test]
    fn scene_namespace_verbs_all_carry_labels() {
        for verb in scene_namespace::SceneVerb::ALL {
            assert!(!verb.label().is_empty(), "{verb:?} has no label");
            assert!(
                !verb.advertised_stroke_display().is_empty()
                    && !verb.advertised_stroke_display().contains('?'),
                "{verb:?} prints an unrenderable stroke"
            );
        }
    }
}
