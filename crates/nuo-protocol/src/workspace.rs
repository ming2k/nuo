//! Optional workspace attachment and orthogonal thread query filters (ADR-0254).
//!
//! A thread is identified solely by its `thread_id`. `WorkspaceBinding` is an
//! optional filesystem environment attached to a thread; `ThreadFilter` filters
//! thread history independently by optional `role` and optional `workspace`.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The optional filesystem environment attached to a thread for workspace tools
/// and workspace trust boundaries.
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

/// Orthogonal filter over thread history (`role` and `workspace` are
/// independent, optional attributes — ADR-0254).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ThreadFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
}

impl ThreadFilter {
    /// Unfiltered view across all threads.
    pub fn all() -> Self {
        Self::default()
    }

    /// Threads staffed with a specific role (across all workspaces).
    pub fn for_role(role: impl Into<String>) -> Self {
        Self {
            role: Some(role.into()),
            workspace: None,
        }
    }

    /// Threads attached to a specific workspace (across all roles).
    pub fn for_workspace(workspace: impl Into<PathBuf>) -> Self {
        Self {
            role: None,
            workspace: Some(workspace.into()),
        }
    }

    pub fn is_unfiltered(&self) -> bool {
        self.role.is_none() && self.workspace.is_none()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_enumerates_root_then_additional() {
        let binding =
            WorkspaceBinding::new("/proj").with_additional_roots(vec![PathBuf::from("/data")]);
        let roots: Vec<_> = binding.roots().map(Path::to_path_buf).collect();
        assert_eq!(roots, vec![PathBuf::from("/proj"), PathBuf::from("/data")]);
    }

    #[test]
    fn filter_dimensions_are_orthogonal() {
        assert!(ThreadFilter::all().is_unfiltered());
        let by_role = ThreadFilter::for_role("developer");
        assert_eq!(by_role.role.as_deref(), Some("developer"));
        assert_eq!(by_role.workspace, None);
        assert!(!by_role.is_unfiltered());

        let by_ws = ThreadFilter::for_workspace("/repo");
        assert_eq!(by_ws.role, None);
        assert_eq!(by_ws.workspace.as_deref(), Some(Path::new("/repo")));
        assert!(!by_ws.is_unfiltered());
    }
}
