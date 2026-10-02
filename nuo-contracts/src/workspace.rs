//! Workspace binding and the history query filter (ADR-0226, revised).
//!
//! A session is partitioned by its **workspace** alone: sessions bound to a
//! directory group by that directory, and sessions with no workspace form the
//! unbound set. There is no separate "space" concept — an agent that needs no
//! workspace simply has `workspace = None`. `WorkspaceFilter` is the query
//! filter that expresses "any / a specific workspace / unbound".

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The optional filesystem binding for an agent's tools. Its root is also the
/// session's history partition when present.
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

/// Canonical session domain partition (ADR-0250).
/// A session is symmetrically anchored either to a physical workspace directory
/// or to an autonomous cognitive role.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPartition {
    /// Filesystem-anchored project history (e.g. developer role in a repo)
    Workspace(PathBuf),
    /// Role-anchored autonomous cognitive thread (e.g. philosophist)
    Role(String),
}

impl SessionPartition {
    /// Derive the partition truthfully from optional workspace path and role id.
    pub fn from_parts(workspace: Option<&Path>, role: Option<&str>) -> Self {
        match (workspace, role) {
            (Some(ws), _) => Self::Workspace(ws.to_path_buf()),
            (None, Some(r)) => Self::Role(r.to_string()),
            (None, None) => Self::Role("developer".to_string()),
        }
    }

    /// Derive truthfully from optional `WorkspaceBinding` and role id.
    pub fn from_binding(binding: Option<&WorkspaceBinding>, role: Option<&str>) -> Self {
        Self::from_parts(binding.map(|b| b.root.as_path()), role)
    }

    pub fn workspace_path(&self) -> Option<&Path> {
        match self {
            Self::Workspace(p) => Some(p),
            _ => None,
        }
    }

    pub fn role_id(&self) -> Option<&str> {
        match self {
            Self::Role(r) => Some(r),
            _ => None,
        }
    }
}

/// History query filter over the workspace partition. Not a domain entity: it
/// is the shape of a `WHERE` clause for listing/resuming/searching sessions.
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
}
