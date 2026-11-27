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

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::tool_output::{PatchOp, ToolOutput};

/// Bound on one preview computation at the dispatch and permission choke
/// points. A preview is a convenience; it must never hold a turn hostage to a
/// wedged filesystem. The blocking worker itself cannot be force-cancelled on
/// every platform, but the caller stops waiting.
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// True when `old`/`new` are a bounded window around the change rather than
    /// the complete file. A renderer must not present the window as the whole
    /// file.
    #[serde(default)]
    pub truncated: bool,
    /// Total bytes of the real pre-image, regardless of the window.
    #[serde(default)]
    pub old_total_bytes: u64,
    /// Total bytes of the real post-image, regardless of the window.
    #[serde(default)]
    pub new_total_bytes: u64,
}

/// Ceiling for the bytes carried by one preview.
///
/// Approval needs enough context to judge a change, not a copy of a 500 MB
/// file. The window keeps a generous margin around the changed region; beyond
/// that the extremes are truncated and `truncated` is set.
pub const MAX_PREVIEW_BYTES: usize = 512 * 1024;

/// Context kept on each side of the changed region when windowing.
const PREVIEW_CONTEXT_BYTES: usize = 16 * 1024;

impl PreviewResult {
    /// Build a bounded preview from the real pre- and post-images.
    ///
    /// The common prefix and suffix are unchanged by definition, so they are
    /// dropped first (expanded to whole lines) and a context margin is kept
    /// around the remaining window. This is O(n) and produces a genuine diff
    /// for the localized edits that dominate tool use; a whole-file replacement
    /// that still exceeds the ceiling is head/tail-truncated and marked.
    pub fn bounded(
        path: impl Into<String>,
        op: PatchOp,
        old: String,
        new: String,
        warnings: Vec<String>,
    ) -> Self {
        let old_total_bytes = old.len() as u64;
        let new_total_bytes = new.len() as u64;
        let (old_window, new_window, start_line, truncated) = window_change(&old, &new);
        Self {
            path: path.into(),
            op,
            old: old_window,
            new: new_window,
            start_line,
            warnings,
            truncated,
            old_total_bytes,
            new_total_bytes,
        }
    }

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

/// Return `(old_window, new_window, start_line, truncated)`.
fn window_change(old: &str, new: &str) -> (String, String, usize, bool) {
    if old.len() + new.len() <= MAX_PREVIEW_BYTES {
        return (old.to_string(), new.to_string(), 1, false);
    }

    let prefix = common_prefix_len(old.as_bytes(), new.as_bytes());
    let suffix = common_suffix_len(old.as_bytes(), new.as_bytes(), prefix);
    let old_prefix = floor_char_boundary(old, prefix.saturating_sub(PREVIEW_CONTEXT_BYTES));
    let new_prefix = floor_char_boundary(new, prefix.saturating_sub(PREVIEW_CONTEXT_BYTES));
    let old_end = ceil_char_boundary(
        old,
        old.len()
            .saturating_sub(suffix)
            .saturating_add(PREVIEW_CONTEXT_BYTES),
    );
    let new_end = ceil_char_boundary(
        new,
        new.len()
            .saturating_sub(suffix)
            .saturating_add(PREVIEW_CONTEXT_BYTES),
    );
    let start_line = 1 + old[..old_prefix]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let old_window = &old[old_prefix..old_end.max(old_prefix)];
    let new_window = &new[new_prefix..new_end.max(new_prefix)];
    let truncated = old_prefix > 0 || old_end < old.len() || new_prefix > 0 || new_end < new.len();
    (
        cap_window(old_window),
        cap_window(new_window),
        start_line,
        truncated,
    )
}

fn cap_window(text: &str) -> String {
    if text.len() <= MAX_PREVIEW_BYTES {
        return text.to_string();
    }
    let head = MAX_PREVIEW_BYTES / 4;
    let tail = MAX_PREVIEW_BYTES - head;
    let head_end = floor_char_boundary(text, head);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(tail));
    format!(
        "{}\n… [preview truncated: {} bytes omitted] …\n{}",
        &text[..head_end],
        tail_start.saturating_sub(head_end),
        &text[tail_start..]
    )
}

fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count()
}

fn common_suffix_len(left: &[u8], right: &[u8], prefix: usize) -> usize {
    let max = left.len().min(right.len()) - prefix.min(left.len().min(right.len()));
    left.iter()
        .rev()
        .zip(right.iter().rev())
        .take(max)
        .take_while(|(left, right)| left == right)
        .count()
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
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
            truncated: false,
            old_total_bytes: 6,
            new_total_bytes: 6,
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
    fn a_large_file_is_windowed_around_the_change() {
        let old = (0..100_000)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let new = old.replace("line 90000\n", "line 90000 changed\n");
        let preview = PreviewResult::bounded("big.txt", PatchOp::Edit, old, new, Vec::new());
        assert!(preview.truncated);
        assert!(preview.old.len() < MAX_PREVIEW_BYTES);
        assert!(preview.new.len() < MAX_PREVIEW_BYTES);
        assert!(preview.start_line > 1);
        assert!(preview.old.contains("line 90000"));
        assert!(preview.new.contains("line 90000 changed"));
        assert!(preview.old_total_bytes > preview.old.len() as u64);
    }

    #[test]
    fn a_small_change_keeps_the_whole_file() {
        let preview = PreviewResult::bounded(
            "small.txt",
            PatchOp::Edit,
            "a\nb\nc\n".to_string(),
            "a\nB\nc\n".to_string(),
            Vec::new(),
        );
        assert!(!preview.truncated);
        assert_eq!(preview.old, "a\nb\nc\n");
        assert_eq!(preview.start_line, 1);
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
