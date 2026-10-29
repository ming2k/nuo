//! Pure previews of a tool call's effect (ADR-0125).
//!
//! A preview answers "what would this call do?" **without doing it**. It exists for
//! one reason: a human authorizing a write should see the change, not a path. "Allow
//! `edit_text` on `src/lib.rs`?" is a question nobody can answer well; "allow *this*
//! two-line change?" is one everybody can.
//!
//! ## Why this is not a diff of the model's arguments
//!
//! The obvious shortcut — diff `old_string` against `new_string` — is wrong in a way
//! that matters. The user is approving a change to a *file*, and the file's state is
//! what makes the edit safe or dangerous: a three-line replacement is trivial in one
//! place and catastrophic two lines below a `return`. So a preview carries the
//! file's current content against its post-call content, computed by the same
//! function the execution path uses.
//!
//! ## Why `ToolOutput::Patch`
//!
//! The preview's shape *is* the shape of an applied change, so it reuses the result
//! type rather than inventing a parallel one. That is what lets one diff renderer
//! serve both "what will happen" and "what happened" — a second renderer is a second
//! place for the two to disagree about what the change looks like.
//!
//! ## Purity
//!
//! [`Tool::preview`](crate::Tool::preview) may read the workspace and nothing else.
//! It must not mutate, execute, spend tokens, or emit events. The type cannot enforce
//! that (a preview implementation could call `fs::write`), so it is enforced where it
//! can be observed: the approval gate calls `preview` and then `execute` on the same
//! arguments, so a preview with side effects corrupts the very change it was
//! describing — a failure the tests would catch immediately rather than in
//! production.

use serde::{Deserialize, Serialize};

use crate::tool_output::{PatchOp, ToolOutput};

/// What a call would do, computed without doing it.
///
/// A dedicated struct rather than a bare [`ToolOutput`] because a preview is not a
/// result: it has no duration, no failure mode, and no token accounting, and giving
/// it the full result vocabulary would invite a frontend to render a preview through
/// code that assumes a call already happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewResult {
    /// The path the call would change, as the model named it (workspace-relative
    /// when relative), so the user recognises the file they asked about.
    pub path: String,
    /// What the call would do to that path.
    pub op: PatchOp,
    /// The content before the call: the file's current bytes, or empty for a
    /// creation. Never the model's `old_string` needle — see the module doc.
    pub old: String,
    /// The content after the call.
    pub new: String,
    /// 1-based line where `old` begins, or `0` when the whole file is carried.
    pub start_line: usize,
    /// Advisory diagnostics the call would emit (e.g. a trailing-whitespace note).
    pub warnings: Vec<String>,
}

impl PreviewResult {
    /// The preview expressed as the [`ToolOutput::Patch`] the call would produce.
    ///
    /// This is the bridge that lets the existing diff renderer draw a preview
    /// unchanged: the same `line_diff`, the same hunks, the same word-level
    /// highlighting. A frontend that already renders an applied edit therefore
    /// renders a previewed one for free, and cannot drift from it.
    pub fn as_patch(&self) -> ToolOutput {
        ToolOutput::Patch {
            path: self.path.clone(),
            op: self.op.clone(),
            old: self.old.clone(),
            new: self.new.clone(),
            start_line: self.start_line,
            warnings: self.warnings.clone(),
        }
    }

    /// Lines added and removed, for a one-line summary ("+2 -1").
    ///
    /// Deliberately derived from its own content rather than trusted from a caller:
    /// the number shown beside an approval prompt is the only quantitative signal a
    /// user has about scale, and a stale or hand-computed count would understate a
    /// large change.
    pub fn line_counts(&self) -> (usize, usize) {
        crate::tool_output::line_change_counts(&self.old, &self.new)
    }

    /// Whether this preview represents the creation of a new path.
    pub fn is_creation(&self) -> bool {
        matches!(self.op, PatchOp::Create)
    }

    /// Whether the call would change nothing — an idempotent re-write, or an edit
    /// whose replacement equals its needle. A caller can use this to avoid asking a
    /// human to approve a no-op.
    pub fn is_unchanged(&self) -> bool {
        self.old == self.new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit() -> PreviewResult {
        PreviewResult {
            path: "src/lib.rs".into(),
            op: PatchOp::Edit,
            old: "a\nb\nc\n".into(),
            new: "a\nB\nc\n".into(),
            start_line: 1,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn preview_renders_through_the_applied_result_type() {
        let preview = edit();
        let ToolOutput::Patch {
            path,
            op,
            old,
            new,
            start_line,
            warnings,
        } = preview.as_patch()
        else {
            panic!("a preview must lower to a patch");
        };
        assert_eq!(path, "src/lib.rs");
        assert_eq!(op, PatchOp::Edit);
        assert_eq!(old, "a\nb\nc\n");
        assert_eq!(new, "a\nB\nc\n");
        assert_eq!(start_line, 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn line_counts_describe_the_change_not_the_caller() {
        assert_eq!(edit().line_counts(), (1, 1));

        let creation = PreviewResult {
            op: PatchOp::Create,
            old: String::new(),
            new: "one\ntwo\n".into(),
            ..edit()
        };
        assert!(creation.is_creation());
        assert_eq!(creation.line_counts(), (2, 0));
    }

    #[test]
    fn unchanged_detects_a_noop() {
        assert!(!edit().is_unchanged());
        let noop = PreviewResult {
            old: "same".into(),
            new: "same".into(),
            ..edit()
        };
        assert!(noop.is_unchanged(), "an idempotent write changes nothing");
    }
}
