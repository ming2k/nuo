//! Encapsulated dialog entities (ADR-0035, `[INV-SURFACE-01]`).
//!
//! Every floating dialog owns its own presentation state — selection cursor,
//! body scroll, follow mode, embedded text input, and sub-layer flags. No
//! dialog state is shared between dialogs or aliased onto `App` scratchpad
//! fields, and no dialog borrows the conversation composer line.
//!
//! The [`DialogView`] contract binds each entity to a [`DialogKind`] and its
//! [`DialogScope`] domain; the [`Dialogs`] registry is owned by the surface
//! router and is the single source of truth for dialog state. Daemon-fed model
//! data (provider snapshots, session context, reports) stays on `App` as
//! read-only model, never as dialog scratchpad.

#![allow(dead_code)]

use std::any::Any;
use std::collections::HashSet;

use crate::TelemetryTab;
use crate::surfaces::{DialogKind, DialogScope};

/// The presentation contract every floating dialog implements.
pub trait DialogView: std::fmt::Debug {
    /// This dialog's identity.
    fn kind(&self) -> DialogKind;

    /// This dialog's ownership domain. Defaults to the static classification
    /// on [`DialogKind::scope`] so the matrix has exactly one definition.
    fn scope(&self) -> DialogScope {
        self.kind().scope()
    }

    /// Deterministic dismissal hook, executed by the overlay stack on pop and
    /// on scene/session unwinding (`[INV-SURFACE-04]`).
    fn on_dismiss(&mut self) {}

    /// Clear all state back to its first-open value. Used for ephemeral
    /// dismissal and for session-scoped domain isolation (`[INV-SURFACE-05]`).
    fn reset(&mut self);

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Define a dialog entity with the flattened navigation fields shared by every
/// dialog, plus entity-specific fields, and the `DialogView` boilerplate.
macro_rules! dialog_entity {
    (
        $name:ident, $kind:ident,
        { $( $(#[$meta:meta])* $field:ident : $ty:ty = $default:expr ),* $(,)? }
    ) => {
        #[derive(Debug, Clone)]
        pub struct $name {
            /// Selection cursor over the dialog's projected rows.
            pub index: usize,
            /// Body scroll offset.
            pub scroll: usize,
            /// Whether body scroll follows the selection.
            pub follow: bool,
            /// Whether the in-dialog key-reference sub-layer is open.
            pub keys_open: bool,
            /// Scroll offset of the key-reference sub-layer.
            pub keys_scroll: usize,
            $( $(#[$meta])* pub $field: $ty, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    index: 0,
                    scroll: 0,
                    follow: true,
                    keys_open: false,
                    keys_scroll: 0,
                    $($field: $default,)*
                }
            }
        }

        impl DialogView for $name {
            fn kind(&self) -> DialogKind {
                DialogKind::$kind
            }
            fn reset(&mut self) {
                *self = Self::default();
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn Any {
                self
            }
        }
    };
}

dialog_entity!(ToolsDialog, Tools, {});
dialog_entity!(McpDialog, Mcp, {});

dialog_entity!(SkillsDialog, Skills, {
    /// Row whose detail block is expanded, if any.
    expanded: Option<usize> = None,
});

dialog_entity!(PermissionsDialog, Permissions, {});
dialog_entity!(UsageStatsDialog, UsageStats, {});

dialog_entity!(TelemetryDialog, Telemetry, {
    /// Active tab (`Overview` or `Activity`).
    tab: TelemetryTab = TelemetryTab::Overview,
    /// `true` when drilled into one round's turns (L2).
    detail: bool = false,
    /// `Some((round, attempt))` when drilled into an attempt inspector (L3).
    turn: Option<(u32, u32)> = None,
    /// Selected turn index in the L2 turns table.
    turn_cursor: usize = 0,
});

dialog_entity!(AsidesDialog, Asides, {});
dialog_entity!(QueueDialog, Queue, {});

dialog_entity!(ModelsDialog, Models, {
    /// Whether the search sub-layer owns the embedded input.
    search: bool = false,
    /// Embedded filter query (self-contained; never the composer line).
    query: String = String::new(),
    /// Caret byte position within `query`.
    query_cursor: usize = 0,
    /// Whether a catalog refresh is in flight.
    refreshing: bool = false,
});

dialog_entity!(ConnectionsDialog, Connections, {
    /// Whether the search sub-layer owns the embedded input.
    search: bool = false,
    /// Embedded filter query (self-contained; never the composer line).
    query: String = String::new(),
    /// Caret byte position within `query`.
    query_cursor: usize = 0,
    /// Whether a catalog refresh is in flight.
    refreshing: bool = false,
    /// `true` while the connection-info sub-view is open.
    info_detail: bool = false,
    /// `true` when the info sub-view was opened standalone (not drilled in).
    info_standalone: bool = false,
    /// Body scroll offset of the connection-info sub-view.
    info_scroll: usize = 0,
    /// Whether the served-models list is expanded in the info sub-view.
    models_expanded: bool = false,
});

dialog_entity!(HistorySearchDialog, HistorySearch, {
    /// Whether the search sub-layer is active.
    search: bool = false,
    /// Embedded fuzzy query (self-contained; never the composer line).
    query: String = String::new(),
    /// Caret byte position within `query`.
    query_cursor: usize = 0,
});

dialog_entity!(SessionsDialog, Sessions, {
    /// `true` while the session-info sub-view is open.
    info_detail: bool = false,
    /// Body scroll offset of the session-info sub-view.
    info_scroll: usize = 0,
    /// Whether the sessions list round-trip is in flight.
    loading: bool = true,
    /// Expanded trunk session ids in the hierarchical picker.
    expanded: HashSet<String> = HashSet::new(),
});

dialog_entity!(SessionTreeDialog, SessionTree, {});

dialog_entity!(SwitcherDialog, Switcher, {
    /// Live fuzzy query.
    query: String = String::new(),
    /// Caret byte position within `query`.
    query_cursor: usize = 0,
    /// Selected row in the palette.
    selected: usize = 0,
});

impl SwitcherDialog {
    /// The switcher is ephemeral: opening it always starts from a clean slate.
    pub fn begin(&mut self) {
        self.query.clear();
        self.query_cursor = 0;
        self.selected = 0;
        self.scroll = 0;
    }
}

/// A snapshot of every session-scoped dialog entity, archived under a
/// [`SessionId`](crate::surfaces) so returning to a session reinstates exactly
/// the modal state the user left behind (`[INV-SURFACE-05]`).
#[derive(Debug, Clone, Default)]
pub struct SessionSnapshot {
    pub tools: ToolsDialog,
    pub mcp: McpDialog,
    pub skills: SkillsDialog,
    pub permissions: PermissionsDialog,
    pub telemetry: TelemetryDialog,
    pub asides: AsidesDialog,
    pub queue: QueueDialog,
    pub session_tree: SessionTreeDialog,
}

/// The single registry of every dialog entity, owned by the surface router.
///
/// Each field is a distinct, encapsulated entity: dialogs never share state.
/// Global entities persist across sessions untouched; session-scoped entities
/// are archived per session so a session switch neither loses nor leaks them.
#[derive(Debug, Default)]
pub struct Dialogs {
    pub tools: ToolsDialog,
    pub mcp: McpDialog,
    pub skills: SkillsDialog,
    pub permissions: PermissionsDialog,
    pub usage_stats: UsageStatsDialog,
    pub telemetry: TelemetryDialog,
    pub asides: AsidesDialog,
    pub models: ModelsDialog,
    pub connections: ConnectionsDialog,
    pub history_search: HistorySearchDialog,
    pub queue: QueueDialog,
    pub sessions: SessionsDialog,
    pub session_tree: SessionTreeDialog,
    pub switcher: SwitcherDialog,
    /// Per-session archives of the session-scoped entities.
    session_archive: std::collections::HashMap<String, SessionSnapshot>,
    /// The session whose live state the session-scoped entities currently
    /// hold (empty before the first session is bound).
    current_session: String,
}

impl Dialogs {
    /// Archive the live session-scoped state under `session_id`.
    pub fn archive_session(&mut self, session_id: &str) {
        if session_id.is_empty() {
            return;
        }
        self.session_archive
            .insert(session_id.to_string(), self.session_snapshot());
    }

    /// Reinstate `session_id`'s archived session-scoped state, or reset the
    /// session-scoped entities to first-open when no archive exists.
    pub fn restore_session(&mut self, session_id: &str) {
        match self.session_archive.get(session_id).cloned() {
            Some(snapshot) => self.apply_session_snapshot(snapshot),
            None => self.reset_scope(DialogScope::Session),
        }
    }

    /// Transition the session-scoped domain from the current session to
    /// `new_id`: archive the outgoing session, then reinstate the incoming one.
    /// Global dialog state is never touched (`[INV-SURFACE-05]`).
    pub fn switch_session(&mut self, new_id: &str) {
        if self.current_session == new_id {
            return;
        }
        if !self.current_session.is_empty() {
            let outgoing = self.current_session.clone();
            self.archive_session(&outgoing);
        }
        self.restore_session(new_id);
        self.current_session = new_id.to_string();
    }

    fn session_snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            tools: self.tools.clone(),
            mcp: self.mcp.clone(),
            skills: self.skills.clone(),
            permissions: self.permissions.clone(),
            telemetry: self.telemetry.clone(),
            asides: self.asides.clone(),
            queue: self.queue.clone(),
            session_tree: self.session_tree.clone(),
        }
    }

    fn apply_session_snapshot(&mut self, snapshot: SessionSnapshot) {
        self.tools = snapshot.tools;
        self.mcp = snapshot.mcp;
        self.skills = snapshot.skills;
        self.permissions = snapshot.permissions;
        self.telemetry = snapshot.telemetry;
        self.asides = snapshot.asides;
        self.queue = snapshot.queue;
        self.session_tree = snapshot.session_tree;
    }
}

impl Dialogs {
    /// The entity for `kind`, as the `DialogView` contract.
    pub fn view(&self, kind: DialogKind) -> &dyn DialogView {
        match kind {
            DialogKind::Tools => &self.tools,
            DialogKind::Mcp => &self.mcp,
            DialogKind::Skills => &self.skills,
            DialogKind::Permissions => &self.permissions,
            DialogKind::UsageStats => &self.usage_stats,
            DialogKind::Telemetry => &self.telemetry,
            DialogKind::Asides => &self.asides,
            DialogKind::Models => &self.models,
            DialogKind::Connections => &self.connections,
            DialogKind::HistorySearch => &self.history_search,
            DialogKind::Queue => &self.queue,
            DialogKind::Sessions => &self.sessions,
            DialogKind::SessionTree => &self.session_tree,
            DialogKind::Switcher => &self.switcher,
        }
    }

    /// The mutable entity for `kind`.
    pub fn view_mut(&mut self, kind: DialogKind) -> &mut dyn DialogView {
        match kind {
            DialogKind::Tools => &mut self.tools,
            DialogKind::Mcp => &mut self.mcp,
            DialogKind::Skills => &mut self.skills,
            DialogKind::Permissions => &mut self.permissions,
            DialogKind::UsageStats => &mut self.usage_stats,
            DialogKind::Telemetry => &mut self.telemetry,
            DialogKind::Asides => &mut self.asides,
            DialogKind::Models => &mut self.models,
            DialogKind::Connections => &mut self.connections,
            DialogKind::HistorySearch => &mut self.history_search,
            DialogKind::Queue => &mut self.queue,
            DialogKind::Sessions => &mut self.sessions,
            DialogKind::SessionTree => &mut self.session_tree,
            DialogKind::Switcher => &mut self.switcher,
        }
    }

    /// The dialog's selection cursor.
    pub fn index(&self, kind: DialogKind) -> usize {
        match kind {
            DialogKind::Tools => self.tools.index,
            DialogKind::Mcp => self.mcp.index,
            DialogKind::Skills => self.skills.index,
            DialogKind::Permissions => self.permissions.index,
            DialogKind::UsageStats => self.usage_stats.index,
            DialogKind::Telemetry => self.telemetry.index,
            DialogKind::Asides => self.asides.index,
            DialogKind::Models => self.models.index,
            DialogKind::Connections => self.connections.index,
            DialogKind::HistorySearch => self.history_search.index,
            DialogKind::Queue => self.queue.index,
            DialogKind::Sessions => self.sessions.index,
            DialogKind::SessionTree => self.session_tree.index,
            DialogKind::Switcher => self.switcher.selected,
        }
    }

    /// Set the dialog's selection cursor.
    pub fn set_index(&mut self, kind: DialogKind, value: usize) {
        match kind {
            DialogKind::Tools => self.tools.index = value,
            DialogKind::Mcp => self.mcp.index = value,
            DialogKind::Skills => self.skills.index = value,
            DialogKind::Permissions => self.permissions.index = value,
            DialogKind::UsageStats => self.usage_stats.index = value,
            DialogKind::Telemetry => self.telemetry.index = value,
            DialogKind::Asides => self.asides.index = value,
            DialogKind::Models => self.models.index = value,
            DialogKind::Connections => self.connections.index = value,
            DialogKind::HistorySearch => self.history_search.index = value,
            DialogKind::Queue => self.queue.index = value,
            DialogKind::Sessions => self.sessions.index = value,
            DialogKind::SessionTree => self.session_tree.index = value,
            DialogKind::Switcher => self.switcher.selected = value,
        }
    }

    /// Whether the dialog's body scroll follows its selection.
    pub fn follow(&self, kind: DialogKind) -> bool {
        match kind {
            DialogKind::Tools => self.tools.follow,
            DialogKind::Mcp => self.mcp.follow,
            DialogKind::Skills => self.skills.follow,
            DialogKind::Permissions => self.permissions.follow,
            DialogKind::UsageStats => self.usage_stats.follow,
            DialogKind::Telemetry => self.telemetry.follow,
            DialogKind::Asides => self.asides.follow,
            DialogKind::Models => self.models.follow,
            DialogKind::Connections => self.connections.follow,
            DialogKind::HistorySearch => self.history_search.follow,
            DialogKind::Queue => self.queue.follow,
            DialogKind::Sessions => self.sessions.follow,
            DialogKind::SessionTree => self.session_tree.follow,
            DialogKind::Switcher => self.switcher.follow,
        }
    }

    /// Whether the dialog's key-reference sub-layer is open.
    pub fn keys_open(&self, kind: DialogKind) -> bool {
        match kind {
            DialogKind::Tools => self.tools.keys_open,
            DialogKind::Mcp => self.mcp.keys_open,
            DialogKind::Skills => self.skills.keys_open,
            DialogKind::Permissions => self.permissions.keys_open,
            DialogKind::UsageStats => self.usage_stats.keys_open,
            DialogKind::Telemetry => self.telemetry.keys_open,
            DialogKind::Asides => self.asides.keys_open,
            DialogKind::Models => self.models.keys_open,
            DialogKind::Connections => self.connections.keys_open,
            DialogKind::HistorySearch => self.history_search.keys_open,
            DialogKind::Queue => self.queue.keys_open,
            DialogKind::Sessions => self.sessions.keys_open,
            DialogKind::SessionTree => self.session_tree.keys_open,
            DialogKind::Switcher => self.switcher.keys_open,
        }
    }

    /// Open or close the dialog's key-reference sub-layer.
    pub fn set_keys_open(&mut self, kind: DialogKind, open: bool) {
        // Every entity exposes the same flattened fields; route by kind.
        match kind {
            DialogKind::Tools => self.tools.keys_open = open,
            DialogKind::Mcp => self.mcp.keys_open = open,
            DialogKind::Skills => self.skills.keys_open = open,
            DialogKind::Permissions => self.permissions.keys_open = open,
            DialogKind::UsageStats => self.usage_stats.keys_open = open,
            DialogKind::Telemetry => self.telemetry.keys_open = open,
            DialogKind::Asides => self.asides.keys_open = open,
            DialogKind::Models => self.models.keys_open = open,
            DialogKind::Connections => self.connections.keys_open = open,
            DialogKind::HistorySearch => self.history_search.keys_open = open,
            DialogKind::Queue => self.queue.keys_open = open,
            DialogKind::Sessions => self.sessions.keys_open = open,
            DialogKind::SessionTree => self.session_tree.keys_open = open,
            DialogKind::Switcher => self.switcher.keys_open = open,
        }
        if !open {
            *self.keys_scroll_mut(kind) = 0;
        }
    }

    /// Mutable key-reference scroll offset.
    pub fn keys_scroll_mut(&mut self, kind: DialogKind) -> &mut usize {
        match kind {
            DialogKind::Tools => &mut self.tools.keys_scroll,
            DialogKind::Mcp => &mut self.mcp.keys_scroll,
            DialogKind::Skills => &mut self.skills.keys_scroll,
            DialogKind::Permissions => &mut self.permissions.keys_scroll,
            DialogKind::UsageStats => &mut self.usage_stats.keys_scroll,
            DialogKind::Telemetry => &mut self.telemetry.keys_scroll,
            DialogKind::Asides => &mut self.asides.keys_scroll,
            DialogKind::Models => &mut self.models.keys_scroll,
            DialogKind::Connections => &mut self.connections.keys_scroll,
            DialogKind::HistorySearch => &mut self.history_search.keys_scroll,
            DialogKind::Queue => &mut self.queue.keys_scroll,
            DialogKind::Sessions => &mut self.sessions.keys_scroll,
            DialogKind::SessionTree => &mut self.session_tree.keys_scroll,
            DialogKind::Switcher => &mut self.switcher.keys_scroll,
        }
    }

    /// Run the dismissal hook for `kind`: the entity's own hook plus the
    /// per-dialog teardown of the embedded search field, so a dismissed
    /// picker never leaves a stale query behind (`[INV-SURFACE-01]`).
    pub fn on_dismiss(&mut self, kind: DialogKind) {
        self.view_mut(kind).on_dismiss();
        match kind {
            DialogKind::Models => {
                self.models.search = false;
                self.models.query.clear();
                self.models.query_cursor = 0;
            }
            DialogKind::Connections => {
                self.connections.search = false;
                self.connections.query.clear();
                self.connections.query_cursor = 0;
            }
            DialogKind::HistorySearch => {
                self.history_search.search = false;
                self.history_search.query.clear();
                self.history_search.query_cursor = 0;
                self.history_search.index = 0;
                self.history_search.scroll = 0;
                self.history_search.follow = true;
            }
            _ => {}
        }
    }

    /// Clear one entity's state back to first-open.
    pub fn reset(&mut self, kind: DialogKind) {
        self.view_mut(kind).reset();
    }

    /// Clear every entity whose domain matches `scope`.
    pub fn reset_scope(&mut self, scope: DialogScope) {
        for kind in DialogKind::ALL {
            if kind.scope() == scope {
                self.reset(kind);
            }
        }
        if scope == DialogScope::Global {
            self.switcher.reset();
        }
    }
}


