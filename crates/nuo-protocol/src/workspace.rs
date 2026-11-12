//! Role-scoped thread identity and the history query filter.
//!
//! A thread's scope is `(role, workspace binding)`. The role is the primary
//! operator identity: it owns the workspace policy that decides whether the
//! thread is anchored to a filesystem workspace at all. A workspace-bound role
//! (the default `developer`, or a custom role with `workspace = "inherit"` /
//! a fixed path) is scoped to exactly that workspace; a workspace-free role
//! (`philosophist`, `ops`, or a custom `workspace = "none"`) is scoped to the
//! unbound set for that role.
//!
//! `WorkspaceFilter` is the workspace dimension alone (used by cross-project
//! search); `ThreadFilter` combines it with an optional role filter for
//! list/purge operations.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The optional filesystem binding for an agent's tools. When present, its
/// root is the workspace half of the thread's `(role, workspace)` scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceBinding {
    pub root: PathBuf,
    #[serde(default)]
    pub additional_roots: Vec<PathBuf>,
}

impl WorkspaceBinding {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            additional_roots: Vec::new(),
        }
    }

    pub fn with_additional_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.additional_roots = roots.into_iter().collect();
        self
    }

    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.root.as_path())
            .chain(self.additional_roots.iter().map(PathBuf::as_path))
    }
}

/// The durable identity of one thread: the role that staffs it plus its
/// resolved workspace binding.
///
/// The role is always present. `workspace = None` is the workspace-free set
/// for that role, not a wildcard: a workspace-bound role and a workspace-free
/// role with the same id are impossible because the role policy decides the
/// binding at birth.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadScope {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
}

impl ThreadScope {
    pub fn new(role: impl Into<String>, workspace: Option<PathBuf>) -> Self {
        Self {
            role: role.into(),
            workspace,
        }
    }

    /// Build the scope from the durable binding and role id. `None` role
    /// resolves to the default `developer`.
    pub fn from_binding(role: Option<&str>, binding: Option<&WorkspaceBinding>) -> Self {
        Self::new(
            role.unwrap_or("developer"),
            binding.map(|binding| binding.root.clone()),
        )
    }

    pub fn workspace_filter(&self) -> WorkspaceFilter {
        match &self.workspace {
            Some(path) => WorkspaceFilter::Path(path.clone()),
            None => WorkspaceFilter::Unbound,
        }
    }

    /// The exact query filter this scope denotes (role + binding).
    pub fn filter(&self) -> ThreadFilter {
        ThreadFilter {
            role: Some(self.role.clone()),
            workspace: self.workspace_filter(),
        }
    }
}

/// Thread-list/purge filter over the two scope dimensions. `role = None` means
/// every role; `workspace = Any` means every binding. Both omitted is the
/// explicit global administrative view, never the default of a command.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ThreadFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub workspace: WorkspaceFilter,
}

impl ThreadFilter {
    /// Every role in one workspace binding.
    pub fn workspace_only(workspace: WorkspaceFilter) -> Self {
        Self {
            role: None,
            workspace,
        }
    }

    /// Every role, every binding. Only an explicit admin flag may produce it.
    pub fn global() -> Self {
        Self::default()
    }

    pub fn is_global(&self) -> bool {
        self.role.is_none() && self.workspace.is_any()
    }
}

/// The typed result of a batch stop or purge (ADR-0171).
///
/// A bulk removal reports partial success honestly: one stuck driver must not
/// hide the threads that were processed, nor masquerade as a complete purge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRemovalReport {
    /// User-facing threads stopped or deleted.
    pub removed: usize,
    /// Internal subagent runs deleted with their parent. Never counted in
    /// `removed`, so the operator sees the browsing effect and the storage
    /// effect separately.
    pub subagents_removed: usize,
    /// Targets that were already absent from the runtime and durable store.
    pub already_gone: usize,
    /// Targets that could not be processed, with the honest reason.
    pub skipped: Vec<ThreadRemovalSkip>,
}

/// One target a batch stop/purge could not process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRemovalSkip {
    pub thread_id: String,
    pub reason: String,
}

/// The workspace dimension of a history query. Not a domain entity: it is the
/// shape of a `WHERE` clause over `workspace_root`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceFilter {
    /// No workspace constraint (global history, e.g. cross-project search).
    #[default]
    Any,
    /// Exactly this workspace root.
    Path(PathBuf),
    /// Sessions with no workspace (the workspace-free set).
    Unbound,
}

impl WorkspaceFilter {
    /// The filter for a session's own binding: its workspace path, or unbound.
    pub fn from_binding(binding: Option<&WorkspaceBinding>) -> Self {
        match binding {
            Some(binding) => WorkspaceFilter::Path(binding.root.clone()),
            None => WorkspaceFilter::Unbound,
        }
    }

    pub fn is_any(&self) -> bool {
        matches!(self, WorkspaceFilter::Any)
    }

    pub fn as_path(&self) -> Option<&Path> {
        match self {
            WorkspaceFilter::Path(path) => Some(path),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_from_binding_is_path_or_unbound() {
        let binding = WorkspaceBinding::new("/repo");
        assert_eq!(
            WorkspaceFilter::from_binding(Some(&binding)),
            WorkspaceFilter::Path(PathBuf::from("/repo"))
        );
        assert_eq!(
            WorkspaceFilter::from_binding(None),
            WorkspaceFilter::Unbound
        );
    }

    #[test]
    fn any_is_the_default() {
        assert_eq!(WorkspaceFilter::default(), WorkspaceFilter::Any);
        assert!(WorkspaceFilter::Any.is_any());
    }

    #[test]
    fn binding_enumerates_root_then_additional() {
        let binding =
            WorkspaceBinding::new("/proj").with_additional_roots(vec![PathBuf::from("/data")]);
        let roots: Vec<_> = binding.roots().map(Path::to_path_buf).collect();
        assert_eq!(roots, vec![PathBuf::from("/proj"), PathBuf::from("/data")]);
    }

    #[test]
    fn scope_is_role_plus_binding() {
        let scope = ThreadScope::new("developer", Some(PathBuf::from("/repo")));
        assert_eq!(scope.role, "developer");
        assert_eq!(
            scope.workspace_filter(),
            WorkspaceFilter::Path("/repo".into())
        );
        assert_eq!(
            scope.filter(),
            ThreadFilter {
                role: Some("developer".into()),
                workspace: WorkspaceFilter::Path("/repo".into()),
            }
        );

        let unbound = ThreadScope::new("philosophist", None);
        assert_eq!(unbound.workspace_filter(), WorkspaceFilter::Unbound);
    }

    #[test]
    fn scope_defaults_role_to_developer() {
        let scope = ThreadScope::from_binding(None, None);
        assert_eq!(scope.role, "developer");
        assert_eq!(scope.workspace, None);
    }

    #[test]
    fn filter_global_requires_no_role_and_any_workspace() {
        assert!(ThreadFilter::global().is_global());
        assert!(
            !ThreadFilter {
                role: Some("developer".into()),
                workspace: WorkspaceFilter::Any,
            }
            .is_global()
        );
        assert!(!ThreadFilter::workspace_only(WorkspaceFilter::Path("/repo".into())).is_global());
    }
}
