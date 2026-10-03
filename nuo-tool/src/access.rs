//! Declarative resource-access declarations and conflict detection.
//!
//! Each tool, at resolution time, declares *what* it touches — which files, in
//! what mode — via a [`ToolAccesses`] list. The dispatcher's scheduler pairs
//! those declarations to decide whether two tool calls in the same batch may
//! run concurrently or must serialize. This replaces ad-hoc "parallelize
//! everything" dispatch with a static, predictable concurrency model: two
//! reads of the same file parallelize freely, but a write against any other
//! access to the same path serializes.
//!
//! Ported from kimi-code's `loop/tool-access.ts`. Three design invariants
//! preserved verbatim:
//!
//! 1. **Empty list never conflicts.** `[]` is the "side-effect-free" marker,
//!    not "read-only on everything". A tool returning `none()` is freely
//!    parallelizable with *anything*, which is the correct default for pure
//!    tools (todo, skill lookups, ask_user).
//! 2. **Any `All` conflicts with everything.** [`ToolAccess::All`] is the
//!    global-exclusive marker — a tool that mutates unbounded state (e.g.
//!    `select_tools`, which injects a schema message into conversation
//!    history) declares it to serialize with the whole batch.
//! 3. **`recursive` is a one-directional subtree claim.** Access A on `dir/`
//!    with `recursive: true` conflicts with access B on `dir/file`; but two
//!    non-recursive accesses on a path and its ancestor do *not* conflict
//!    unless the paths are identical — a single-file access never claims its
//!    parent directory.
//!
//! This module is pure domain logic (ADR-0005): no I/O, no async, no
//! `Tool`-trait dependency. The [`Tool::accesses`](crate::capability::Tool)
//! default returns [`ToolAccesses::none`].

use serde::{Deserialize, Serialize};

/// The file-access operation a tool performs. `Read` and `Search` are
/// non-mutating; `Write` and `ReadWrite` mutate. Conflicts are decided by
/// "any writer present": readers never conflict with each other, a writer
/// conflicts with any operation (read or write) on an overlapping path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolFileAccessOperation {
    /// Read a single file or tree, without modifying it.
    Read,
    /// Search across a tree. Treated as read-side for conflict purposes:
    /// two searches never conflict.
    Search,
    /// Write/overwrite a file.
    Write,
    /// Read-modify-write (e.g. `edit_text`). Mutating.
    ReadWrite,
}

impl ToolFileAccessOperation {
    /// Whether this operation mutates state. Used by the conflict rule: any
    /// mutating operation (on either side) makes the pair a potential conflict,
    /// subject then to path overlap.
    #[inline]
    fn writes(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// A single declared resource access by a tool.
///
/// Discriminated by `kind`: either a [`ToolAccess::File`] (an operation on a
/// path, optionally recursive over its subtree) or [`ToolAccess::All`] (global
/// exclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAccess {
    /// Access to a filesystem path.
    File {
        operation: ToolFileAccessOperation,
        /// Normalized absolute or relative path. Normalization
        /// ([`normalize_access_path`]) collapses redundant separators and
        /// trailing slashes; case-folding is *not* applied (paths are
        /// case-sensitive, matching POSIX tooling).
        path: String,
        /// If `true`, this access covers the whole subtree rooted at `path`.
        /// A recursive access conflicts with any access whose path is inside
        /// the subtree (see invariant 3 above).
        recursive: bool,
    },
    /// Global exclusive: conflicts with every other access unconditionally.
    /// Reserved for tools that mutate unbounded, un-locatable state.
    All,
}

/// A list of accesses declared by a single tool call.
///
/// Construct with the [`ToolAccesses`] factories. Stored as a `SmallVec`-free
/// `Vec` — tool access lists are tiny (1–3 entries) and `Vec` keeps the type
/// `Send + Sync` without extra deps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolAccesses(pub Vec<ToolAccess>);

impl ToolAccesses {
    /// No accesses — side-effect-free / freely parallelizable. **Never
    /// conflicts** with anything, by design (see invariant 1).
    #[inline]
    pub fn none() -> Self {
        Self(Vec::new())
    }

    /// A single global-exclusive access.
    #[inline]
    pub fn all() -> Self {
        Self(vec![ToolAccess::All])
    }

    /// Whether this declaration claims a **mutating** access — a `Write`/
    /// `ReadWrite` file operation or a global-exclusive `All`. Used by the
    /// toolset-collection safety check (see `debug_assert_safe_targets`):
    /// a tool that declares a write but leaves its `scope_target` at the
    /// default `Unspecified` would bypass the bash-policy and broker entirely
    /// — a load-bearing safety hole. Read-only declarations
    /// (`Read`/`Search`/`none()`) return `false`.
    #[inline]
    pub fn declares_write(&self) -> bool {
        self.0.iter().any(|access| match access {
            ToolAccess::All => true,
            ToolAccess::File { operation, .. } => operation.writes(),
        })
    }

    /// Push a file access and return self, for chaining.
    #[inline]
    pub fn with_file(
        mut self,
        operation: ToolFileAccessOperation,
        path: impl Into<String>,
        recursive: bool,
    ) -> Self {
        self.0.push(ToolAccess::File {
            operation,
            path: normalize_access_path(path),
            recursive,
        });
        self
    }

    // convenience constructors mirroring kimi-code's factories

    /// Read a single file.
    #[inline]
    pub fn read_file(path: impl Into<String>) -> Self {
        Self::none().with_file(ToolFileAccessOperation::Read, path, false)
    }
    /// Read a directory tree (recursive).
    #[inline]
    pub fn read_tree(path: impl Into<String>) -> Self {
        Self::none().with_file(ToolFileAccessOperation::Read, path, true)
    }
    /// Write a single file.
    #[inline]
    pub fn write_file(path: impl Into<String>) -> Self {
        Self::none().with_file(ToolFileAccessOperation::Write, path, false)
    }
    /// Write a directory tree (recursive).
    #[inline]
    pub fn write_tree(path: impl Into<String>) -> Self {
        Self::none().with_file(ToolFileAccessOperation::Write, path, true)
    }
    /// Read-modify-write a single file (e.g. `edit_text`).
    #[inline]
    pub fn read_write_file(path: impl Into<String>) -> Self {
        Self::none().with_file(ToolFileAccessOperation::ReadWrite, path, false)
    }
    /// Read-modify-write a directory tree (recursive).
    #[inline]
    pub fn read_write_tree(path: impl Into<String>) -> Self {
        Self::none().with_file(ToolFileAccessOperation::ReadWrite, path, true)
    }
    /// Search a tree.
    #[inline]
    pub fn search_tree(path: impl Into<String>) -> Self {
        Self::none().with_file(ToolFileAccessOperation::Search, path, true)
    }

    /// Whether this access list conflicts with `other`. The pair conflicts iff
    /// *some* access on the left conflicts with *some* access on the right
    /// (kimi-code's double `some`).
    #[inline]
    pub fn conflicts(&self, other: &Self) -> bool {
        self.0
            .iter()
            .any(|left| other.0.iter().any(|right| access_conflict(left, right)))
    }
}

/// Per-access-pair conflict rule (kimi-code's `resourceAccessesConflict`):
/// 1. either side `All` → conflict;
/// 2. otherwise both are `File` — if neither operation writes, no conflict;
/// 3. else (at least one writes) conflict iff the paths overlap.
fn access_conflict(left: &ToolAccess, right: &ToolAccess) -> bool {
    match (left, right) {
        (ToolAccess::All, _) | (_, ToolAccess::All) => true,
        (
            ToolAccess::File {
                operation: left_op,
                path: left_path,
                recursive: left_rec,
            },
            ToolAccess::File {
                operation: right_op,
                path: right_path,
                recursive: right_rec,
            },
        ) => {
            // (B) operation compatibility: no writer → no conflict.
            if !left_op.writes() && !right_op.writes() {
                return false;
            }
            // (C) path overlap.
            file_accesses_overlap(left_path, *left_rec, right_path, *right_rec)
        }
    }
}

/// Path overlap test (kimi-code's `fileAccessesOverlap`). Two accesses overlap
/// iff:
/// - the normalized paths are identical; **or**
/// - exactly the recursive party's path is a prefix-directory of the other
///   (i.e. the other lives inside the recursive party's subtree).
///
/// A *non-recursive* access never claims its parent directory, so
/// `dir/file` vs `dir/` (neither recursive) do **not** overlap.
fn file_accesses_overlap(
    left_path: &str,
    left_recursive: bool,
    right_path: &str,
    right_recursive: bool,
) -> bool {
    if left_path == right_path {
        return true;
    }
    // Only a recursive party claims its subtree. Check each direction.
    left_recursive && is_subtree(left_path, right_path)
        || right_recursive && is_subtree(right_path, left_path)
}

/// Is `child` inside the subtree rooted at `parent`? `parent` must be a strict
/// prefix directory: `child` starts with `parent` followed by a separator
/// (and has at least one char after). Equal paths are handled by the caller
/// (`==` short-circuit above), so this returns false for equality.
///
/// Both inputs are assumed already normalized by [`normalize_access_path`]
/// (no trailing slash except root, no doubled separators), so `parent` never
/// ends in `/` unless it is the root `/`.
fn is_subtree(parent: &str, child: &str) -> bool {
    // parent.len() + 1 <= child.len() ensures "parent/" is a strict prefix,
    // i.e. there is at least one path component under `parent`.
    child.len() > parent.len() + 1
        && child.starts_with(parent)
        && child.as_bytes()[parent.len()] == b'/'
}

/// Normalize an access path: collapse repeated `/`, strip trailing `/`.
/// Backslashes are *not* folded to forward slashes (we stay case-sensitive
/// POSIX-first; kimi-code folds backslashes for Windows — we keep it simple
/// and correct on Unix, which is the deployment target).
pub fn normalize_access_path(path: impl Into<String>) -> String {
    let mut raw = path.into();
    // Strip trailing slashes (but keep "/" itself as the root).
    while raw.len() > 1 && raw.ends_with('/') {
        raw.pop();
    }
    // Collapse repeated separators.
    let mut out = String::with_capacity(raw.len());
    let mut prev_sep = false;
    for ch in raw.chars() {
        if ch == '/' {
            if !prev_sep {
                out.push(ch);
            }
            prev_sep = true;
        } else {
            out.push(ch);
            prev_sep = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn rd(p: &str) -> ToolAccesses {
        ToolAccesses::read_file(p)
    }
    fn wt(p: &str) -> ToolAccesses {
        ToolAccesses::write_file(p)
    }
    fn rdt(p: &str) -> ToolAccesses {
        ToolAccesses::read_tree(p)
    }
    fn rwt(p: &str) -> ToolAccesses {
        ToolAccesses::read_write_file(p)
    }

    // operation compatibility (B)

    #[test]
    fn two_reads_same_file_do_not_conflict() {
        assert!(!rd("src/lib.rs").conflicts(&rd("src/lib.rs")));
    }

    #[test]
    fn read_vs_write_same_file_conflicts() {
        assert!(rd("src/lib.rs").conflicts(&wt("src/lib.rs")));
    }

    #[test]
    fn read_vs_readwrite_same_file_conflicts() {
        assert!(rd("src/lib.rs").conflicts(&rwt("src/lib.rs")));
    }

    #[test]
    fn two_writes_same_file_conflict() {
        assert!(wt("a.txt").conflicts(&wt("a.txt")));
    }

    #[test]
    fn two_searches_do_not_conflict() {
        let s = ToolAccesses::search_tree("src");
        assert!(!s.conflicts(&ToolAccesses::search_tree("src")));
    }

    // path overlap (C)

    #[test]
    fn reads_of_different_files_do_not_conflict() {
        assert!(!rd("a.rs").conflicts(&rd("b.rs")));
    }

    #[test]
    fn write_outside_read_path_does_not_conflict() {
        assert!(!rd("src/a.rs").conflicts(&wt("tests/b.rs")));
    }

    #[test]
    fn recursive_read_conflicts_with_write_inside_subtree() {
        assert!(rdt("src").conflicts(&wt("src/deep/mod.rs")));
    }

    #[test]
    fn recursive_write_conflicts_with_read_inside_subtree() {
        assert!(ToolAccesses::write_tree("src").conflicts(&rd("src/x.rs")));
    }

    #[test]
    fn non_recursive_access_does_not_claim_parent_dir() {
        // src/lib.rs vs src/ (neither recursive) — no overlap.
        assert!(!rd("src/lib.rs").conflicts(&rd("src")));
        // Even a write to src/lib.rs does not conflict with a read of dir src/
        // unless the dir read is recursive.
        assert!(!wt("src/lib.rs").conflicts(&rd("src")));
    }

    // All

    #[test]
    fn all_conflicts_with_everything() {
        let all = ToolAccesses::all();
        assert!(all.conflicts(&rd("any")));
        assert!(!all.conflicts(&ToolAccesses::none())); // none never conflicts, even with all
        assert!(all.conflicts(&ToolAccesses::all()));
    }

    // none

    #[test]
    fn none_never_conflicts() {
        let n = ToolAccesses::none();
        assert!(!n.conflicts(&rd("x")));
        assert!(!n.conflicts(&wt("x")));
        assert!(!n.conflicts(&ToolAccesses::all()));
        assert!(!n.conflicts(&n));
    }

    // normalization

    #[test]
    fn normalization_collapses_and_strips() {
        assert_eq!(normalize_access_path("src//a.rs"), "src/a.rs");
        assert_eq!(normalize_access_path("src/a.rs/"), "src/a.rs");
        assert_eq!(normalize_access_path("src///"), "src");
        assert_eq!(normalize_access_path("/"), "/");
        assert_eq!(normalize_access_path("a/b/../c.rs"), "a/b/../c.rs"); // no .. resolution (intentional)
    }

    #[test]
    fn double_separator_paths_normalized_before_conflict() {
        // built-in constructors normalize; raw conflict sees raw strings,
        // but constructors guarantee normalized input.
        assert!(rd("src//a.rs").conflicts(&wt("src/a.rs/")));
    }
}
