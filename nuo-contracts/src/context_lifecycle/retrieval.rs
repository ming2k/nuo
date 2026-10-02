//! Scoped retrieval handles and the inspect error taxonomy (ADR-0279 §1,
//! `INV-RET-01/02`).
//!
//! A handle is an **address, not a credential**. It names a resource category
//! and a scoped identity; authorization is decided against the caller's
//! session/branch scope, never by possession of the handle. Handles carry
//! internal execution/checkpoint IDs, not provider call IDs, so a duplicate
//! provider call ID in another turn cannot cross-read.

use crate::context_lifecycle::axes::{Capture, Deletion, Representation, Validity};
use crate::context_lifecycle::ids::{ArtifactId, BranchId, CheckpointId, ExecutionId};
use std::fmt;

/// The resource category a handle addresses (ADR-0279 §1).
///
/// Categories are not authorization scopes: `call:` and `fold:` describe *what*
/// is addressed, not *who* may read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandleScheme {
    /// A tool execution's result.
    Call,
    /// A folded (deterministically collapsed) region.
    Fold,
    /// A subagent transcript.
    Sub,
    /// A standalone media/file snapshot.
    Artifact,
}

impl HandleScheme {
    /// The URL-style prefix.
    pub const fn prefix(self) -> &'static str {
        match self {
            HandleScheme::Call => "call:",
            HandleScheme::Fold => "fold:",
            HandleScheme::Sub => "sub:",
            HandleScheme::Artifact => "artifact:",
        }
    }
}

/// The authorization scope a caller presents for a retrieval.
///
/// Retrieval is authorized by session/branch scope: a handle whose scope is not
/// covered by this one is denied, and a handle never widens the scope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthScope {
    /// The session the caller is acting within.
    pub session_id: String,
    /// The branches the caller may read.
    pub branches: Vec<BranchId>,
}

impl AuthScope {
    /// Whether this scope covers `branch`.
    pub fn covers_branch(&self, branch: &BranchId) -> bool {
        self.branches.iter().any(|b| b == branch)
    }
}

/// The scope bound to a handle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HandleScope {
    /// Owning session.
    pub session_id: String,
    /// Owning branch.
    pub branch_id: BranchId,
}

/// A retrieval handle: a scoped address plus an internal identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InspectHandle {
    /// Resource category.
    pub scheme: HandleScheme,
    /// The scope this handle belongs to.
    pub scope: HandleScope,
    /// The execution this handle addresses, for `Call`/`Sub`/`Artifact`.
    pub execution_id: Option<ExecutionId>,
    /// The checkpoint this handle addresses, for `Fold`.
    pub checkpoint_id: Option<CheckpointId>,
    /// The artifact this handle addresses, for `Artifact`.
    pub artifact_id: Option<ArtifactId>,
}

impl InspectHandle {
    /// Whether the caller's scope authorizes reading this handle.
    ///
    /// Possession is not authority: both the session and the branch must match.
    pub fn is_authorized(&self, auth: &AuthScope) -> bool {
        auth.session_id == self.scope.session_id && auth.covers_branch(&self.scope.branch_id)
    }

    /// The `scheme:identity` rendering.
    pub fn render(&self) -> String {
        let id = self
            .execution_id
            .as_ref()
            .map(|e| e.as_str().to_string())
            .or_else(|| self.checkpoint_id.as_ref().map(|c| c.as_str().to_string()))
            .or_else(|| self.artifact_id.as_ref().map(|a| a.as_str().to_string()))
            .unwrap_or_default();
        format!("{}{}", self.scheme.prefix(), id)
    }
}

impl fmt::Display for InspectHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// A pagination cursor, bound to the content and query that produced it
/// (ADR-0279 §1, `INV-RET-02`).
///
/// Changing the query must not reuse an old cursor, so the cursor carries the
/// query and the content hash it was issued against. The `offset` advances as
/// pages are read and is deliberately **not** part of the binding check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CursorBinding {
    /// Hash of the content the cursor pages over.
    pub content_hash: String,
    /// The query that produced the cursor.
    pub query: String,
    /// The revision the cursor was issued against.
    pub revision: u64,
    /// Byte offset into the content for the next page.
    pub offset: u64,
}

impl CursorBinding {
    /// Whether this cursor may continue the given request. The offset is not
    /// compared: it changes legitimately between pages.
    pub fn matches(&self, content_hash: &str, query: &str, revision: u64) -> bool {
        self.content_hash == content_hash && self.query == query && self.revision == revision
    }

    /// A continuation at a new byte offset.
    pub fn advance(&self, offset: u64) -> Self {
        Self {
            offset,
            ..self.clone()
        }
    }
}

/// Per-read resource ceilings (ADR-0279 §1, `INV-RET-02`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageLimits {
    /// Token ceiling for one page.
    pub tokens: u64,
    /// Byte ceiling for one page.
    pub bytes: u64,
    /// Compute-time ceiling in milliseconds for one page.
    pub compute_ms: u64,
}

/// The separated status fields a retrieval returns (ADR-0279 §1).
///
/// These are never collapsed: `Purged` is not `Omitted`, an incomplete capture
/// is not an empty result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InspectStatus {
    /// Deletion state of the addressed content.
    pub deletion: Deletion,
    /// Capture completeness of the addressed content.
    pub capture: Capture,
    /// Validity relative to the caller's branch and versions.
    pub validity: Validity,
    /// How the returned page represents the content.
    pub representation: Representation,
}

/// The typed inspect failure taxonomy (ADR-0279 §1).
///
/// Every variant is distinct; none degrades into an empty success.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectError {
    /// A requested page cannot make progress within its resource budget.
    BudgetExceeded,
    /// The handle addresses nothing.
    NotFound,
    /// The caller's scope does not cover the handle.
    NotAuthorized,
    /// The content was deleted.
    Purged,
    /// A retention deadline passed.
    Expired,
    /// The stored content failed integrity checks.
    Corrupt,
    /// The capture is interrupted or truncated.
    Incomplete,
    /// The cursor does not match the content/query/revision.
    CursorMismatch,
}

impl InspectError {
    /// Whether the error is a client-fixable mismatch.
    pub const fn is_client_error(self) -> bool {
        matches!(
            self,
            InspectError::CursorMismatch | InspectError::NotAuthorized
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(session: &str, branch: &str) -> InspectHandle {
        InspectHandle {
            scheme: HandleScheme::Call,
            scope: HandleScope {
                session_id: session.into(),
                branch_id: BranchId::from(branch),
            },
            execution_id: Some(ExecutionId::from("exec-1")),
            checkpoint_id: None,
            artifact_id: None,
        }
    }

    #[test]
    fn a_handle_confers_no_authority() {
        let h = handle("s1", "main");
        let owner = AuthScope {
            session_id: "s1".into(),
            branches: vec![BranchId::from("main")],
        };
        let stranger = AuthScope {
            session_id: "s2".into(),
            branches: vec![BranchId::from("main")],
        };
        assert!(h.is_authorized(&owner));
        assert!(
            !h.is_authorized(&stranger),
            "a foreign session cannot read the handle"
        );
    }

    #[test]
    fn cross_branch_reads_are_denied() {
        let h = handle("s1", "feature");
        let main_only = AuthScope {
            session_id: "s1".into(),
            branches: vec![BranchId::from("main")],
        };
        assert!(!h.is_authorized(&main_only));
    }

    #[test]
    fn cursor_is_bound_to_content_query_and_revision() {
        let cursor = CursorBinding {
            content_hash: "h1".into(),
            query: "error".into(),
            revision: 7,
            offset: 0,
        };
        assert!(cursor.matches("h1", "error", 7));
        assert!(
            !cursor.matches("h1", "warning", 7),
            "a changed query must not reuse the cursor"
        );
        assert!(!cursor.matches("h2", "error", 7));
        assert!(!cursor.matches("h1", "error", 8));
        // The offset advances without breaking the binding.
        assert!(cursor.advance(4096).matches("h1", "error", 7));
        assert_eq!(cursor.advance(4096).offset, 4096);
    }

    #[test]
    fn every_inspect_error_is_distinct() {
        // Not all failures collapse into an empty result: each carries meaning.
        let all = [
            InspectError::NotFound,
            InspectError::NotAuthorized,
            InspectError::Purged,
            InspectError::Expired,
            InspectError::Corrupt,
            InspectError::Incomplete,
            InspectError::CursorMismatch,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
        assert!(InspectError::NotAuthorized.is_client_error());
        assert!(!InspectError::Purged.is_client_error());
    }

    #[test]
    fn handle_renders_with_its_scheme_prefix() {
        assert_eq!(handle("s1", "main").render(), "call:exec-1");
        assert_eq!(HandleScheme::Fold.prefix(), "fold:");
        assert_eq!(HandleScheme::Artifact.prefix(), "artifact:");
    }
}
