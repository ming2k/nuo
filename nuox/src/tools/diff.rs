//! Line-level diff used to visualize `edit_text` / `write_file` changes.
//!
//! Backed by `similar`'s Myers line diff (so multi-hunk, interleaved edits
//! render correctly rather than collapsing to all-removed-then-all-added).
//! Each output line carries its 1-based old/new line number, and adjacent
//! delete/insert pairs are further split into word-level fragments so the
//! exact edited span is highlighted within a changed line.

use std::collections::HashMap;
use std::sync::Arc;

use similar::{ChangeTag, DiffOp as SimilarDiffOp, TextDiff};

/// Maximum number of completed edit diffs retained by one transcript renderer.
/// Entries are small contextual patches, but the bound keeps session switches
/// and long-running processes from accumulating render-only state forever.
const DIFF_CACHE_CAPACITY: usize = 256;

/// `(added, removed)` line counts for the change from `old` to `new`. Used for
/// the `+N -M` summary suffix in the step header. Computed from the real diff
/// so the count always matches what the body renders.
pub fn line_diff_counts(old: &str, new: &str) -> (usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0usize;
    let mut removed = 0usize;
    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            match change.tag() {
                ChangeTag::Insert => added += 1,
                ChangeTag::Delete => removed += 1,
                ChangeTag::Equal => {}
            }
        }
    }
    (added, removed)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffOp {
    /// Unchanged line shown for context.
    Context,
    /// Line present only in the new text.
    Add,
    /// Line present only in the old text.
    Remove,
}

/// One intra-line fragment: `text` plus whether it is part of the edited span
/// (highlighted by the renderer). Lines that were not word-diffed carry a
/// single `changed = false` fragment equal to the whole line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFrag {
    pub text: String,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub op: DiffOp,
    /// 1-based line number in the old text (set for `Remove` and `Context`).
    pub old_no: Option<usize>,
    /// 1-based line number in the new text (set for `Add` and `Context`).
    pub new_no: Option<usize>,
    /// Word-level fragments; the concatenation equals the line text.
    pub frags: Vec<DiffFrag>,
}

/// One Git-style change hunk with an explicit old/new source range.
///
/// Ranges are stored in the same display form used by unified diff headers:
/// `start` is 1-based for non-empty ranges, while an empty range names the
/// line immediately before the insertion/deletion point. Keeping this
/// semantic data out of the renderer avoids reconstructing it from elided
/// presentation rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
    /// Optional function/class/scope header for the hunk (standard Git unified diff convention).
    pub hint: Option<String>,
}

impl DiffHunk {
    /// Standard unified-diff range header without scope hint.
    pub fn range_header(&self) -> String {
        format!(
            "@@ -{} +{} @@",
            format_hunk_range(self.old_start, self.old_count),
            format_hunk_range(self.new_start, self.new_count),
        )
    }

    /// Full unified-diff hunk header, including scope hint if present.
    #[allow(dead_code)]
    pub fn header(&self) -> String {
        let range = self.range_header();
        if let Some(hint) = &self.hint {
            format!("{range} {hint}")
        } else {
            range
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DiffSource {
    Patch {
        old: String,
        new: String,
        start_line: usize,
    },
    LegacyArguments {
        name: String,
        arguments: String,
    },
}

struct CachedDiff {
    source: DiffSource,
    hunks: Arc<[DiffHunk]>,
    last_used: u64,
}

/// Bounded render-layer cache for completed edit diffs.
///
/// The durable transcript keeps the canonical [`ToolOutput::Patch`](nuo_contracts::ToolOutput)
/// only. Myers/word diffing and context collapsing are presentation work, so
/// their derived rows live here and are reused across animation frames. A
/// source equality check, rather than a hash alone, makes reuse collision-free
/// when a message id is replaced during session navigation.
#[derive(Default)]
pub struct DiffCache {
    entries: HashMap<u64, CachedDiff>,
    clock: u64,
}

impl DiffCache {
    /// Return the completed Patch's derived rows, computing them only when this
    /// message id is first seen or its canonical Patch content changes.
    pub(crate) fn patch(
        &mut self,
        message_id: u64,
        old: &str,
        new: &str,
        start_line: usize,
    ) -> Arc<[DiffHunk]> {
        let now = self.next_tick();
        if let Some(lines) = self.reuse(message_id, now, |source| {
            matches!(
                source,
                DiffSource::Patch {
                    old: cached_old,
                    new: cached_new,
                    start_line: cached_start,
                } if cached_old == old && cached_new == new && *cached_start == start_line
            )
        }) {
            return lines;
        }

        let source = DiffSource::Patch {
            old: old.to_string(),
            new: new.to_string(),
            start_line,
        };
        let offset = start_line.saturating_sub(1);
        let hunks = line_diff_hunks(old, new, offset);
        self.insert(message_id, source, hunks, now)
    }

    /// Cache the argument-derived compatibility diff used by restored sessions
    /// created before structured Patch results were persisted.
    pub(crate) fn legacy_arguments(
        &mut self,
        message_id: u64,
        name: &str,
        arguments: &str,
    ) -> Arc<[DiffHunk]> {
        let now = self.next_tick();
        if let Some(lines) = self.reuse(message_id, now, |source| {
            matches!(
                source,
                DiffSource::LegacyArguments {
                    name: cached_name,
                    arguments: cached_arguments,
                } if cached_name == name && cached_arguments == arguments
            )
        }) {
            return lines;
        }

        let source = DiffSource::LegacyArguments {
            name: name.to_string(),
            arguments: arguments.to_string(),
        };
        let hunks = super::diff_hunks_for(name, arguments);
        self.insert(message_id, source, hunks, now)
    }

    fn next_tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1).max(1);
        self.clock
    }

    fn reuse(
        &mut self,
        message_id: u64,
        now: u64,
        matches_source: impl FnOnce(&DiffSource) -> bool,
    ) -> Option<Arc<[DiffHunk]>> {
        if let Some(cached) = self.entries.get_mut(&message_id)
            && matches_source(&cached.source)
        {
            cached.last_used = now;
            return Some(Arc::clone(&cached.hunks));
        }
        None
    }

    fn insert(
        &mut self,
        message_id: u64,
        source: DiffSource,
        hunks: Vec<DiffHunk>,
        now: u64,
    ) -> Arc<[DiffHunk]> {
        let hunks: Arc<[DiffHunk]> = Arc::from(hunks);
        self.entries.insert(
            message_id,
            CachedDiff {
                source,
                hunks: Arc::clone(&hunks),
                last_used: now,
            },
        );
        if self.entries.len() > DIFF_CACHE_CAPACITY
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(id, _)| *id)
        {
            self.entries.remove(&oldest);
        }
        hunks
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

impl DiffLine {
    fn context(text: &str, old_no: usize, new_no: usize) -> Self {
        DiffLine {
            op: DiffOp::Context,
            old_no: Some(old_no),
            new_no: Some(new_no),
            frags: vec![DiffFrag {
                text: text.to_string(),
                changed: false,
            }],
        }
    }

    fn plain(op: DiffOp, text: &str, no: usize) -> Self {
        let (old_no, new_no) = match op {
            DiffOp::Remove => (Some(no), None),
            _ => (None, Some(no)),
        };
        DiffLine {
            op,
            old_no,
            new_no,
            frags: vec![DiffFrag {
                text: text.to_string(),
                changed: false,
            }],
        }
    }

    /// The full line text (all fragments concatenated).
    pub fn text(&self) -> String {
        self.frags.iter().map(|f| f.text.as_str()).collect()
    }
}

/// Word-diff a removed/added line pair, returning the fragments for each side
/// with the differing spans marked `changed`. Uses `similar`'s Unicode word
/// segmentation so identifiers and operators stay intact.
fn word_diff_pair<'a>(old: &'a str, new: &'a str) -> (Vec<DiffFrag>, Vec<DiffFrag>) {
    let diff = TextDiff::from_words(old, new);
    let mut old_frags = Vec::new();
    let mut new_frags = Vec::new();
    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            let text = change.value().to_string();
            match change.tag() {
                ChangeTag::Equal => {
                    old_frags.push(DiffFrag {
                        text: text.clone(),
                        changed: false,
                    });
                    new_frags.push(DiffFrag {
                        text,
                        changed: false,
                    });
                }
                ChangeTag::Delete => old_frags.push(DiffFrag {
                    text,
                    changed: true,
                }),
                ChangeTag::Insert => new_frags.push(DiffFrag {
                    text,
                    changed: true,
                }),
            }
        }
    }
    (old_frags, new_frags)
}

/// Build the renderable diff with line numbers and intra-line word highlight.
/// Adjacent delete/insert runs are paired up so a one-token edit highlights just
/// that token instead of repainting whole lines.
///
/// `line_offset` is the number of file lines preceding the `old` snippet
/// (typically `start_line - 1` from `ToolOutput::Patch`). It is added to
/// every emitted line number so the gutter shows real file line numbers.
/// Pass `0` when the offset is unknown or irrelevant (e.g. `write_file`).
#[cfg(test)]
pub fn line_diff(old: &str, new: &str, line_offset: usize) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    lines_for_ops(&diff, diff.ops(), line_offset)
}

/// Default amount of unchanged source context on each side of a hunk.
/// This matches Git's unified-diff default (`-U3`).
pub const DEFAULT_DIFF_CONTEXT_LINES: usize = 3;

/// Build explicit Git-style hunks with configurable context lines and scope hint detection.
pub fn line_diff_hunks_with_context(
    old: &str,
    new: &str,
    line_offset: usize,
    context_lines: usize,
) -> Vec<DiffHunk> {
    let diff = TextDiff::from_lines(old, new);
    let old_lines: Vec<&str> = old.split('\n').collect();
    diff.grouped_ops(context_lines)
        .into_iter()
        .filter_map(|ops| {
            // `grouped_ops` never yields empty groups; guard defensively so the
            // range logic can never see a missing endpoint.
            let (first, rest) = ops.split_first()?;
            let last = rest.last().unwrap_or(first);
            let old_range = first.old_range().start..last.old_range().end;
            let new_range = first.new_range().start..last.new_range().end;

            // Find first change line (Delete/Replace) or fallback to insertion point
            let change_start = ops
                .iter()
                .find(|op| {
                    matches!(
                        op,
                        SimilarDiffOp::Delete { .. } | SimilarDiffOp::Replace { .. }
                    )
                })
                .map(|op| op.old_range().start)
                .unwrap_or(old_range.start);

            let hint = find_hunk_scope_hint(&old_lines, change_start);
            Some(DiffHunk {
                old_start: display_hunk_start(old_range.start, old_range.len(), line_offset),
                old_count: old_range.len(),
                new_start: display_hunk_start(new_range.start, new_range.len(), line_offset),
                new_count: new_range.len(),
                lines: lines_for_ops(&diff, &ops, line_offset),
                hint,
            })
        })
        .collect()
}

/// Build explicit Git-style hunks with default 3 lines of context.
pub fn line_diff_hunks(old: &str, new: &str, line_offset: usize) -> Vec<DiffHunk> {
    line_diff_hunks_with_context(old, new, line_offset, DEFAULT_DIFF_CONTEXT_LINES)
}

/// Find the enclosing function, class, or scope declaration for a hunk.
/// Scans backward from `start_line_idx` in `old_lines`.
fn find_hunk_scope_hint(old_lines: &[&str], start_line_idx: usize) -> Option<String> {
    if old_lines.is_empty() {
        return None;
    }
    let limit = start_line_idx.min(old_lines.len());
    for line in old_lines[..limit].iter().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
        {
            continue;
        }

        if is_scope_declaration(trimmed) {
            let clean = trimmed.trim_end_matches('{').trim();
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
    }
    None
}

fn is_scope_declaration(line: &str) -> bool {
    let kw_prefixes = [
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "async fn ",
        "pub async fn ",
        "impl ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "def ",
        "async def ",
        "class ",
        "function ",
        "export function ",
        "export default ",
        "export class ",
        "func ",
        "type ",
    ];
    for prefix in &kw_prefixes {
        if line.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Convert a zero-based range start to unified-diff display semantics.
/// Non-empty ranges are 1-based. Empty ranges identify the line immediately
/// before the insertion/deletion point and therefore do not add one.
fn display_hunk_start(start: usize, count: usize, line_offset: usize) -> usize {
    start + line_offset + usize::from(count > 0)
}

fn format_hunk_range(start: usize, count: usize) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{start},{count}")
    }
}

fn lines_for_ops(
    diff: &TextDiff<'_, '_, '_, str>,
    ops: &[SimilarDiffOp],
    line_offset: usize,
) -> Vec<DiffLine> {
    // Buffer consecutive deletes/inserts so they can be paired into word-diffs.
    let mut pending_del: Vec<(usize, &str)> = Vec::new();
    let mut pending_ins: Vec<(usize, &str)> = Vec::new();
    let mut out: Vec<DiffLine> = Vec::new();

    let flush =
        |del: &mut Vec<(usize, &str)>, ins: &mut Vec<(usize, &str)>, out: &mut Vec<DiffLine>| {
            let pair = del.len().min(ins.len());
            for i in 0..pair {
                let (old_no, old_text) = del[i];
                let (new_no, new_text) = ins[i];
                let (old_frags, new_frags) = word_diff_pair(old_text, new_text);
                out.push(DiffLine {
                    op: DiffOp::Remove,
                    old_no: Some(old_no + 1 + line_offset),
                    new_no: None,
                    frags: old_frags,
                });
                out.push(DiffLine {
                    op: DiffOp::Add,
                    old_no: None,
                    new_no: Some(new_no + 1 + line_offset),
                    frags: new_frags,
                });
            }
            for &(old_no, old_text) in del.iter().skip(pair) {
                out.push(DiffLine::plain(
                    DiffOp::Remove,
                    old_text,
                    old_no + 1 + line_offset,
                ));
            }
            for &(new_no, new_text) in ins.iter().skip(pair) {
                out.push(DiffLine::plain(
                    DiffOp::Add,
                    new_text,
                    new_no + 1 + line_offset,
                ));
            }
            del.clear();
            ins.clear();
        };

    for op in ops {
        for change in diff.iter_changes(op) {
            match change.tag() {
                ChangeTag::Equal => {
                    flush(&mut pending_del, &mut pending_ins, &mut out);
                    let text = change.value();
                    let old_no = change.old_index().map(|i| i + 1 + line_offset).unwrap_or(0);
                    let new_no = change.new_index().map(|i| i + 1 + line_offset).unwrap_or(0);
                    out.push(DiffLine::context(text, old_no, new_no));
                }
                ChangeTag::Delete => {
                    if !pending_ins.is_empty() {
                        // A new change block started after an insert; flush first.
                        flush(&mut pending_del, &mut pending_ins, &mut out);
                    }
                    if let Some(i) = change.old_index() {
                        pending_del.push((i, change.value()));
                    }
                }
                ChangeTag::Insert => {
                    if let Some(i) = change.new_index() {
                        pending_ins.push((i, change.value()));
                    }
                }
            }
        }
    }
    flush(&mut pending_del, &mut pending_ins, &mut out);

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_patch_diff_is_reused_until_its_source_changes() {
        let mut cache = DiffCache::default();
        let first = cache.patch(7, "let x = 1;", "let x = 2;", 10);
        let second = cache.patch(7, "let x = 1;", "let x = 2;", 10);
        assert!(Arc::ptr_eq(&first, &second));

        let changed = cache.patch(7, "let x = 2;", "let x = 3;", 10);
        assert!(!Arc::ptr_eq(&second, &changed));
        assert_eq!(cache.len(), 1, "one message id replaces its prior source");
    }

    #[test]
    fn diff_cache_is_bounded_and_evicts_old_entries() {
        let mut cache = DiffCache::default();
        for id in 0..=DIFF_CACHE_CAPACITY as u64 {
            cache.patch(id, "old", "new", 1);
        }
        assert_eq!(cache.len(), DIFF_CACHE_CAPACITY);
        assert!(!cache.entries.contains_key(&0));
    }

    #[test]
    fn counts_match_real_diff() {
        assert_eq!(line_diff_counts("a\nb\nc\nd", "a\nB\nc\nd"), (1, 1));
        assert_eq!(line_diff_counts("", "x\ny\nz"), (3, 0));
        assert_eq!(line_diff_counts("x\ny", ""), (0, 2));
    }

    #[test]
    fn paired_change_highlights_only_the_word() {
        let diff = line_diff("let x = 1;", "let x = 2;", 0);
        // Context-free single-line edit: one Remove, one Add.
        assert_eq!(diff.len(), 2);
        assert_eq!(diff[0].op, DiffOp::Remove);
        assert_eq!(diff[1].op, DiffOp::Add);
        // The differing token is marked changed; the shared prefix is not.
        // (Token boundaries depend on `similar`'s word segmentation, so we
        // assert membership rather than an exact token.)
        let del_changed: String = diff[0]
            .frags
            .iter()
            .filter(|f| f.changed)
            .map(|f| f.text.as_str())
            .collect();
        let del_unchanged: String = diff[0]
            .frags
            .iter()
            .filter(|f| !f.changed)
            .map(|f| f.text.as_str())
            .collect();
        let add_changed: String = diff[1]
            .frags
            .iter()
            .filter(|f| f.changed)
            .map(|f| f.text.as_str())
            .collect();
        assert!(del_changed.contains('1'), "del changed: {del_changed:?}");
        assert!(!del_changed.contains("let"));
        assert!(del_unchanged.contains("let"));
        assert!(add_changed.contains('2'), "add changed: {add_changed:?}");
        assert!(!add_changed.contains("let"));
    }

    #[test]
    fn line_numbers_are_set_and_one_based() {
        let diff = line_diff("a\nb\nc", "a\nB\nc", 0);
        // a(ctx old1/new1), b(del old2), B(add new2), c(ctx old3/new3)
        assert_eq!(diff[0].op, DiffOp::Context);
        assert_eq!(diff[0].old_no, Some(1));
        assert_eq!(diff[0].new_no, Some(1));
        assert_eq!(diff[1].op, DiffOp::Remove);
        assert_eq!(diff[1].old_no, Some(2));
        assert_eq!(diff[2].op, DiffOp::Add);
        assert_eq!(diff[2].new_no, Some(2));
        assert_eq!(diff[3].op, DiffOp::Context);
        assert_eq!(diff[3].old_no, Some(3));
    }

    #[test]
    fn interleaved_edits_do_not_collapse_to_all_remove_then_all_add() {
        let old = "a\nX\nb\nY\nc";
        let new = "a\nx\nb\ny\nc";
        let diff = line_diff(old, new, 0);
        let ops: Vec<_> = diff.iter().map(|d| d.op).collect();
        // Should interleave: Ctx, Remove, Add, Ctx, Remove, Add, Ctx.
        assert_eq!(
            ops,
            vec![
                DiffOp::Context,
                DiffOp::Remove,
                DiffOp::Add,
                DiffOp::Context,
                DiffOp::Remove,
                DiffOp::Add,
                DiffOp::Context,
            ]
        );
    }

    #[test]
    fn line_offset_shifts_all_line_numbers() {
        // The snippet starts at file line 15, so offset = 14.
        let diff = line_diff("a\nb\nc", "a\nB\nc", 14);
        // Context line "a": file line 15 (was 1 + 14).
        assert_eq!(diff[0].op, DiffOp::Context);
        assert_eq!(diff[0].old_no, Some(15));
        assert_eq!(diff[0].new_no, Some(15));
        // Removed "b": file line 16.
        assert_eq!(diff[1].op, DiffOp::Remove);
        assert_eq!(diff[1].old_no, Some(16));
        // Added "B": file line 16.
        assert_eq!(diff[2].op, DiffOp::Add);
        assert_eq!(diff[2].new_no, Some(16));
        // Context line "c": file line 17.
        assert_eq!(diff[3].op, DiffOp::Context);
        assert_eq!(diff[3].old_no, Some(17));
    }

    #[test]
    fn distant_changes_form_explicit_hunks() {
        let old = "a\nCHANGE1\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\np\nq\nr\ns\nCHANGE2\nz";
        let new = "a\nchange1\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\np\nq\nr\ns\nchange2\nz";
        let hunks = line_diff_hunks(old, new, 0);

        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].header(), "@@ -1,5 +1,5 @@");
        assert_eq!(hunks[1].header(), "@@ -17,5 +17,5 @@");
        assert!(
            hunks
                .iter()
                .all(|hunk| hunk.lines.iter().any(|line| line.op == DiffOp::Remove))
        );
        assert!(
            hunks
                .iter()
                .all(|hunk| hunk.lines.iter().any(|line| line.op == DiffOp::Add))
        );
    }

    #[test]
    fn nearby_changes_share_one_hunk() {
        let old = "a\nCHANGE1\nc\nd\ne\nf\nCHANGE2\nz";
        let new = "a\nchange1\nc\nd\ne\nf\nchange2\nz";
        let hunks = line_diff_hunks(old, new, 0);

        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].header(), "@@ -1,8 +1,8 @@");
    }

    #[test]
    fn pure_insert_and_delete_use_standard_zero_length_ranges() {
        let insert = line_diff_hunks("", "new\n", 0);
        assert_eq!(insert.len(), 1);
        assert_eq!(insert[0].header(), "@@ -0,0 +1 @@");

        let delete = line_diff_hunks("old\n", "", 0);
        assert_eq!(delete.len(), 1);
        assert_eq!(delete[0].header(), "@@ -1 +0,0 @@");
    }

    #[test]
    fn unchanged_text_has_no_hunks() {
        assert!(line_diff_hunks("same\n", "same\n", 0).is_empty());
    }

    #[test]
    fn hunk_grouping_keeps_three_lines_without_edge_ellipsis() {
        let old = "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nCHANGE\nl11\nl12\nl13\nl14\nl15\nl16\nl17\nl18\nl19\nl20";
        let new = "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nchange\nl11\nl12\nl13\nl14\nl15\nl16\nl17\nl18\nl19\nl20";
        let hunks = line_diff_hunks(old, new, 0);

        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].header(), "@@ -8,7 +8,7 @@");
        assert_eq!(hunks[0].lines.first().unwrap().old_no, Some(8));
        assert_eq!(hunks[0].lines.last().unwrap().old_no, Some(14));
    }

    #[test]
    fn hunk_headers_include_file_line_offset() {
        let hunks = line_diff_hunks("a\nb\nc", "a\nB\nc", 14);

        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].header(), "@@ -15,3 +15,3 @@");
    }

    #[test]
    fn hunk_header_detects_enclosing_function_scope_hint() {
        let old = "pub fn calculate(val: i32) -> i32 {\n    let a = 1;\n    let b = val * 2;\n    return b;\n}\n";
        let new = "pub fn calculate(val: i32) -> i32 {\n    let a = 1;\n    let b = val * 10;\n    return b;\n}\n";
        let hunks = line_diff_hunks(old, new, 0);

        assert_eq!(hunks.len(), 1);
        assert_eq!(
            hunks[0].hint.as_deref(),
            Some("pub fn calculate(val: i32) -> i32")
        );
        assert_eq!(
            hunks[0].header(),
            "@@ -1,5 +1,5 @@ pub fn calculate(val: i32) -> i32"
        );
    }

    #[test]
    fn hunk_with_custom_context_lines() {
        let old = "l1\nl2\nl3\nl4\nl5\nCHANGE\nl7\nl8\nl9\nl10\nl11";
        let new = "l1\nl2\nl3\nl4\nl5\nchange\nl7\nl8\nl9\nl10\nl11";

        let hunks_3 = line_diff_hunks_with_context(old, new, 0, 3);
        let hunks_5 = line_diff_hunks_with_context(old, new, 0, 5);
        assert!(hunks_5[0].lines.len() > hunks_3[0].lines.len());
    }
}
