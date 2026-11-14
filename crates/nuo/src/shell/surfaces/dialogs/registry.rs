//! The dialog registry and its per-session archives.

use super::entities::*;
use super::*;

/// A snapshot of every session-scoped dialog entity, archived under a
/// [`SessionId`](crate::shell::surfaces) so returning to a session reinstates exactly
/// the modal state the user left behind (`[INV-SURFACE-05]`).
#[derive(Debug, Clone, Default)]
pub struct SessionSnapshot {
    pub tools: ToolsDialog,
    pub mcp: McpDialog,
    pub skills: SkillsDialog,
    pub permissions: PermissionsDialog,
    pub thread_stats: ThreadStatsDialog,
    pub session_trace: SessionTraceDialog,
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
    pub quotas: QuotasDialog,
    pub thread_stats: ThreadStatsDialog,
    pub session_trace: SessionTraceDialog,
    pub models: ModelsDialog,
    pub connections: ConnectionsDialog,
    pub history_search: HistorySearchDialog,
    pub queue: QueueDialog,
    pub threads: ThreadsDialog,
    pub session_tree: SessionTreeDialog,
    pub switcher: SwitcherDialog,
    pub port_share: PortShareDialog,
    pub help: HelpDialog,
    pub notices: NoticesDialog,
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
            thread_stats: self.thread_stats.clone(),
            session_trace: self.session_trace.clone(),
            queue: self.queue.clone(),
            session_tree: self.session_tree.clone(),
        }
    }

    fn apply_session_snapshot(&mut self, snapshot: SessionSnapshot) {
        self.tools = snapshot.tools;
        self.mcp = snapshot.mcp;
        self.skills = snapshot.skills;
        self.permissions = snapshot.permissions;
        self.thread_stats = snapshot.thread_stats;
        self.session_trace = snapshot.session_trace;
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
            DialogKind::Quotas => &self.quotas,
            DialogKind::ThreadStats => &self.thread_stats,
            DialogKind::SessionTrace => &self.session_trace,
            DialogKind::Models => &self.models,
            DialogKind::Connections => &self.connections,
            DialogKind::HistorySearch => &self.history_search,
            DialogKind::Queue => &self.queue,
            DialogKind::Threads => &self.threads,
            DialogKind::SessionTree => &self.session_tree,
            DialogKind::Switcher => &self.switcher,
            DialogKind::PortShare => &self.port_share,
            DialogKind::Help => &self.help,
            DialogKind::Notices => &self.notices,
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
            DialogKind::Quotas => &mut self.quotas,
            DialogKind::ThreadStats => &mut self.thread_stats,
            DialogKind::SessionTrace => &mut self.session_trace,
            DialogKind::Models => &mut self.models,
            DialogKind::Connections => &mut self.connections,
            DialogKind::HistorySearch => &mut self.history_search,
            DialogKind::Queue => &mut self.queue,
            DialogKind::Threads => &mut self.threads,
            DialogKind::SessionTree => &mut self.session_tree,
            DialogKind::Switcher => &mut self.switcher,
            DialogKind::PortShare => &mut self.port_share,
            DialogKind::Help => &mut self.help,
            DialogKind::Notices => &mut self.notices,
        }
    }

    /// Take the entity for `kind` out of the registry (leaving a fresh
    /// placeholder), so the caller can invoke its `render`/`handle_input`
    /// against a disjoint `&App` borrow. Must be paired with `Self::put`.
    #[allow(clippy::expect_used)]
    pub fn take(&mut self, kind: DialogKind) -> Box<dyn DialogView> {
        // Move the entity out, leaving a `Default` placeholder; the stack owns
        // it while open and returns it via `put` on dismissal. The
        // render-time read-consistency clone lives in the stack entry
        // (`SurfaceRouter::take_active_view`).
        match kind {
            DialogKind::Tools => Box::new(std::mem::take(&mut self.tools)),
            DialogKind::Mcp => Box::new(std::mem::take(&mut self.mcp)),
            DialogKind::Skills => Box::new(std::mem::take(&mut self.skills)),
            DialogKind::Permissions => Box::new(std::mem::take(&mut self.permissions)),
            DialogKind::UsageStats => Box::new(std::mem::take(&mut self.usage_stats)),
            DialogKind::Quotas => Box::new(std::mem::take(&mut self.quotas)),
            DialogKind::ThreadStats => Box::new(std::mem::take(&mut self.thread_stats)),
            DialogKind::SessionTrace => Box::new(std::mem::take(&mut self.session_trace)),
            DialogKind::Models => Box::new(std::mem::take(&mut self.models)),
            DialogKind::Connections => Box::new(std::mem::take(&mut self.connections)),
            DialogKind::HistorySearch => Box::new(std::mem::take(&mut self.history_search)),
            DialogKind::Queue => Box::new(std::mem::take(&mut self.queue)),
            DialogKind::Threads => Box::new(std::mem::take(&mut self.threads)),
            DialogKind::SessionTree => Box::new(std::mem::take(&mut self.session_tree)),
            DialogKind::Switcher => Box::new(std::mem::take(&mut self.switcher)),
            DialogKind::PortShare => Box::new(std::mem::take(&mut self.port_share)),
            DialogKind::Help => Box::new(std::mem::take(&mut self.help)),
            DialogKind::Notices => Box::new(std::mem::take(&mut self.notices)),
        }
    }

    /// Put an entity taken with `Self::take` back into the registry.
    #[allow(clippy::expect_used)]
    pub fn put(&mut self, kind: DialogKind, view: Box<dyn DialogView>) {
        match kind {
            DialogKind::Tools => {
                self.tools = *view.into_any().downcast::<ToolsDialog>().expect("kind")
            }
            DialogKind::Mcp => self.mcp = *view.into_any().downcast::<McpDialog>().expect("kind"),
            DialogKind::Skills => {
                self.skills = *view.into_any().downcast::<SkillsDialog>().expect("kind")
            }
            DialogKind::Permissions => {
                self.permissions = *view
                    .into_any()
                    .downcast::<PermissionsDialog>()
                    .expect("kind")
            }
            DialogKind::UsageStats => {
                self.usage_stats = *view
                    .into_any()
                    .downcast::<UsageStatsDialog>()
                    .expect("kind")
            }
            DialogKind::Quotas => {
                self.quotas = *view.into_any().downcast::<QuotasDialog>().expect("kind")
            }
            DialogKind::ThreadStats => {
                self.thread_stats = *view
                    .into_any()
                    .downcast::<ThreadStatsDialog>()
                    .expect("kind")
            }
            DialogKind::SessionTrace => {
                self.session_trace = *view
                    .into_any()
                    .downcast::<SessionTraceDialog>()
                    .expect("kind")
            }
            DialogKind::Models => {
                self.models = *view.into_any().downcast::<ModelsDialog>().expect("kind")
            }
            DialogKind::Connections => {
                self.connections = *view
                    .into_any()
                    .downcast::<ConnectionsDialog>()
                    .expect("kind")
            }
            DialogKind::HistorySearch => {
                self.history_search = *view
                    .into_any()
                    .downcast::<HistorySearchDialog>()
                    .expect("kind")
            }
            DialogKind::Queue => {
                self.queue = *view.into_any().downcast::<QueueDialog>().expect("kind")
            }
            DialogKind::Threads => {
                self.threads = *view.into_any().downcast::<ThreadsDialog>().expect("kind")
            }
            DialogKind::SessionTree => {
                self.session_tree = *view
                    .into_any()
                    .downcast::<SessionTreeDialog>()
                    .expect("kind")
            }
            DialogKind::Switcher => {
                self.switcher = *view.into_any().downcast::<SwitcherDialog>().expect("kind")
            }
            DialogKind::PortShare => {
                self.port_share = *view.into_any().downcast::<PortShareDialog>().expect("kind")
            }
            DialogKind::Help => {
                self.help = *view.into_any().downcast::<HelpDialog>().expect("kind")
            }
            DialogKind::Notices => {
                self.notices = *view.into_any().downcast::<NoticesDialog>().expect("kind")
            }
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
            DialogKind::Quotas => self.quotas.index,
            DialogKind::ThreadStats => self.thread_stats.index,
            DialogKind::SessionTrace => self.session_trace.index,
            DialogKind::Models => self.models.index,
            DialogKind::Connections => self.connections.index,
            DialogKind::HistorySearch => self.history_search.index,
            DialogKind::Queue => self.queue.index,
            DialogKind::Threads => self.threads.index,
            DialogKind::SessionTree => self.session_tree.index,
            DialogKind::Switcher => self.switcher.selected,
            DialogKind::PortShare => 0,
            DialogKind::Help => 0,
            DialogKind::Notices => self.notices.index,
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
            DialogKind::Quotas => self.quotas.index = value,
            DialogKind::ThreadStats => self.thread_stats.index = value,
            DialogKind::SessionTrace => self.session_trace.index = value,
            DialogKind::Models => self.models.index = value,
            DialogKind::Connections => self.connections.index = value,
            DialogKind::HistorySearch => self.history_search.index = value,
            DialogKind::Queue => self.queue.index = value,
            DialogKind::Threads => self.threads.index = value,
            DialogKind::SessionTree => self.session_tree.index = value,
            DialogKind::Switcher => self.switcher.selected = value,
            DialogKind::PortShare => {}
            DialogKind::Help => {}
            DialogKind::Notices => self.notices.index = value,
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
            DialogKind::Quotas => self.quotas.follow,
            DialogKind::ThreadStats => self.thread_stats.follow,
            DialogKind::SessionTrace => self.session_trace.follow,
            DialogKind::Models => self.models.follow,
            DialogKind::Connections => self.connections.follow,
            DialogKind::HistorySearch => self.history_search.follow,
            DialogKind::Queue => self.queue.follow,
            DialogKind::Threads => self.threads.follow,
            DialogKind::SessionTree => self.session_tree.follow,
            DialogKind::Switcher => self.switcher.follow,
            DialogKind::PortShare => false,
            DialogKind::Help => false,
            DialogKind::Notices => self.notices.follow,
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
            DialogKind::Quotas => self.quotas.keys_open,
            DialogKind::ThreadStats => self.thread_stats.keys_open,
            DialogKind::SessionTrace => self.session_trace.keys_open,
            DialogKind::Models => self.models.keys_open,
            DialogKind::Connections => self.connections.keys_open,
            DialogKind::HistorySearch => self.history_search.keys_open,
            DialogKind::Queue => self.queue.keys_open,
            DialogKind::Threads => self.threads.keys_open,
            DialogKind::SessionTree => self.session_tree.keys_open,
            DialogKind::Switcher => self.switcher.keys_open,
            DialogKind::PortShare => false,
            DialogKind::Help => false,
            DialogKind::Notices => self.notices.keys_open,
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
            DialogKind::Quotas => self.quotas.keys_open = open,
            DialogKind::ThreadStats => self.thread_stats.keys_open = open,
            DialogKind::SessionTrace => self.session_trace.keys_open = open,
            DialogKind::Models => self.models.keys_open = open,
            DialogKind::Connections => self.connections.keys_open = open,
            DialogKind::HistorySearch => self.history_search.keys_open = open,
            DialogKind::Queue => self.queue.keys_open = open,
            DialogKind::Threads => self.threads.keys_open = open,
            DialogKind::SessionTree => self.session_tree.keys_open = open,
            DialogKind::Switcher => self.switcher.keys_open = open,
            DialogKind::PortShare => {}
            DialogKind::Help => {}
            DialogKind::Notices => self.notices.keys_open = open,
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
            DialogKind::Quotas => &mut self.quotas.keys_scroll,
            DialogKind::ThreadStats => &mut self.thread_stats.keys_scroll,
            DialogKind::SessionTrace => &mut self.session_trace.keys_scroll,
            DialogKind::Models => &mut self.models.keys_scroll,
            DialogKind::Connections => &mut self.connections.keys_scroll,
            DialogKind::HistorySearch => &mut self.history_search.keys_scroll,
            DialogKind::Queue => &mut self.queue.keys_scroll,
            DialogKind::Threads => &mut self.threads.keys_scroll,
            DialogKind::SessionTree => &mut self.session_tree.keys_scroll,
            DialogKind::Switcher => &mut self.switcher.keys_scroll,
            DialogKind::PortShare => &mut self.port_share.scroll,
            DialogKind::Help => &mut self.help.scroll,
            DialogKind::Notices => &mut self.notices.keys_scroll,
        }
    }

    /// Run the dismissal hook for `kind`: the entity's own hook plus the
    /// per-dialog teardown of the embedded search field, so a dismissed
    /// picker never leaves a stale query behind (`[INV-SURFACE-01]`).
    pub fn on_dismiss(&mut self, kind: DialogKind) {
        self.view_mut(kind).on_dismiss();
        dismiss_cleanup(self.view_mut(kind));
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
