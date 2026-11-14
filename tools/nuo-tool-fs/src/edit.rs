//! `edit_text` — a pure, exact-first matcher with a unique line-window
//! normalization fallback. See ADR-0177.
//!
//! The matcher is split from the tool so preview and execution share exactly
//! one decision. A preview that re-implemented matching would eventually show a
//! diff for an edit that execution rejects, or apply a different one.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use nuo_tool::{
    BuiltinTool, PatchOp, PreviewResult, RiskProfile, ScopeTarget, Tool, ToolAccesses, ToolContext,
    ToolError, ToolOutput, ToolSchema, ToolScope,
};
use serde::Deserialize;
use serde_json::Value;

use super::{SystemToolContext, path_argument};

/// Typed parameters for [`EditTextTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct EditTextArgs {
    #[tool(desc = "Path to an existing file relative to the workspace root")]
    pub path: String,
    #[tool(
        desc = "The text block to replace. Must match exactly once unless replace_all is true. Matching retries with line-ending/whitespace/typographic normalization, but never ignores leading indentation."
    )]
    pub old_string: String,
    #[tool(desc = "The replacement text. Must differ from old_string.")]
    pub new_string: String,
    #[tool(desc = "Replace every occurrence instead of requiring a unique match (default false)")]
    pub replace_all: Option<bool>,
}

/// One accepted edit: the new file content, where it starts, and how the match
/// was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedEdit {
    pub content: String,
    pub start_line: usize,
    pub replacements: usize,
    /// True when the exact path missed and the line-window fallback matched.
    pub normalized: bool,
}

impl PlannedEdit {
    pub fn warnings(&self) -> Vec<String> {
        if self.normalized {
            vec![
                "Matched after normalizing line endings, trailing whitespace, or \
                 typographic quote/dash characters. Leading indentation was not ignored."
                    .to_string(),
            ]
        } else {
            Vec::new()
        }
    }
}

/// The pure half of an `edit_text`: given the file's current content, decide
/// whether the edit is legal and what it produces.
///
/// Exact match is tried first. Only when it finds nothing does the line-window
/// normalization run; it compares whole lines, never rewrites unmatched bytes,
/// and still requires a unique window unless `replace_all` is set.
pub fn plan_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    raw_path: &str,
) -> Result<PlannedEdit, String> {
    if old_string.is_empty() {
        return Err("`old_string` cannot be empty".to_string());
    }
    if old_string == new_string {
        return Err(
            "`old_string` and `new_string` are identical; this call would not change the file"
                .to_string(),
        );
    }

    let exact: Vec<usize> = content
        .match_indices(old_string)
        .map(|(offset, _)| offset)
        .collect();
    if !exact.is_empty() {
        if exact.len() > 1 && !replace_all {
            return Err(ambiguous_message(
                exact.len(),
                &exact
                    .iter()
                    .map(|offset| line_of(content, *offset))
                    .collect::<Vec<_>>(),
                raw_path,
                false,
            ));
        }
        let start_line = line_of(content, exact[0]);
        let content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        return Ok(PlannedEdit {
            content,
            start_line,
            replacements: exact.len(),
            normalized: false,
        });
    }

    // A trailing newline in `old_string` is an explicit byte-span request.
    // The line-window fallback replaces line *text* and would silently drop
    // that boundary, so it deliberately does not run for this shape.
    if old_string.ends_with('\n') || old_string.ends_with('\r') {
        return Err(diagnose_edit_failure(content, old_string, raw_path));
    }

    let file_lines = split_lines(content);
    let old_lines: Vec<&str> = old_string.lines().collect();
    if old_lines.is_empty() {
        return Err("`old_string` has no matchable lines".to_string());
    }

    let mut windows = Vec::new();
    if old_lines.len() <= file_lines.len() {
        for start in 0..=file_lines.len() - old_lines.len() {
            let matched = old_lines.iter().enumerate().all(|(offset, expected)| {
                normalize_line(file_lines[start + offset].text) == normalize_line(expected)
            });
            if matched {
                windows.push(start);
            }
        }
    }

    if windows.is_empty() {
        return Err(diagnose_edit_failure(content, old_string, raw_path));
    }
    if windows.len() > 1 && !replace_all {
        return Err(ambiguous_message(
            windows.len(),
            &windows.iter().map(|start| start + 1).collect::<Vec<_>>(),
            raw_path,
            true,
        ));
    }

    let start_line = windows[0] + 1;
    // The replacement follows the file's dominant line ending, not the last
    // matched line's: a match that ends on an unterminated final line must not
    // switch the inserted lines to LF just because `ending` was empty there.
    let file_ending = file_lines
        .iter()
        .find(|line| !line.ending.is_empty())
        .map(|line| line.ending)
        .unwrap_or("");
    // `replace_all` follows the same non-overlapping, left-to-right rule as
    // `str::replace`: a later window may not start inside an earlier accepted
    // one. Without this, a two-line pattern over three repeated lines would
    // replace overlapping spans and corrupt the file.
    let selected: Vec<usize> = if replace_all {
        let mut selected = Vec::new();
        let mut next_allowed = 0;
        for start in &windows {
            if *start >= next_allowed {
                selected.push(*start);
                next_allowed = *start + old_lines.len();
            }
        }
        selected
    } else {
        vec![windows[0]]
    };

    let mut content = content.to_string();
    for start in selected.iter().rev() {
        let first = &file_lines[*start];
        let last = &file_lines[start + old_lines.len() - 1];
        let replacement = adapt_line_endings(new_string, file_ending);
        content.replace_range(first.start..last.end, &replacement);
    }
    Ok(PlannedEdit {
        content,
        start_line,
        replacements: selected.len(),
        normalized: true,
    })
}

/// One source line with its byte span and original ending.
struct SourceLine<'a> {
    text: &'a str,
    ending: &'a str,
    start: usize,
    end: usize,
}

fn split_lines(content: &str) -> Vec<SourceLine<'_>> {
    let bytes = content.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let mut end = index;
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        lines.push(SourceLine {
            text: &content[start..end],
            ending: &content[end..=index],
            start,
            end,
        });
        start = index + 1;
    }
    if start < content.len() {
        lines.push(SourceLine {
            text: &content[start..],
            ending: "",
            start,
            end: content.len(),
        });
    }
    lines
}

/// Fold only the differences that are not semantic in source code. Leading
/// indentation is intentionally preserved: it is part of the match.
fn normalize_line(line: &str) -> String {
    line.trim_end()
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Keep the file's existing line-ending style inside the replaced region.
fn adapt_line_endings(replacement: &str, ending: &str) -> String {
    let normalized = replacement.replace("\r\n", "\n");
    if ending == "\r\n" {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    }
}

fn line_of(content: &str, byte_offset: usize) -> usize {
    content[..byte_offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn ambiguous_message(count: usize, lines: &[usize], path: &str, normalized: bool) -> String {
    let qualifier = if normalized {
        " after normalizing line endings, trailing whitespace, or typographic characters"
    } else {
        ""
    };
    format!(
        "`old_string` matched {count} places in `{path}`{qualifier} (lines {}). \
         It must match exactly once. Provide more surrounding context to disambiguate, \
         or set `replace_all` to true to replace every occurrence.",
        lines
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Surgically replaces a unique, exact or normalized line-window occurrence.
pub struct EditTextTool {
    ctx: Arc<SystemToolContext>,
}

impl EditTextTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for EditTextTool {
    fn name(&self) -> &str {
        BuiltinTool::EditText.as_str()
    }

    fn description(&self) -> &str {
        "Edit an existing file by replacing a text block. `old_string` must match \
         exactly once unless `replace_all` is true. Matching retries with line-ending, \
         trailing-whitespace, and typographic normalization, but never ignores leading \
         indentation. Include surrounding lines as anchors when the block is ambiguous. \
         Never use this to create a new file."
    }

    fn parameters_schema(&self) -> Value {
        EditTextArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::IdempotentMutation
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Workspace]
    }

    fn scope_target(&self, arguments: &str) -> ScopeTarget {
        match path_argument(arguments) {
            Some(path) => ScopeTarget::Path(PathBuf::from(path)),
            None => ScopeTarget::Unspecified,
        }
    }

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        match path_argument(arguments) {
            // Read-modify-write: `edit_text` both reads and rewrites the file,
            // so it conflicts with any other access to the same path.
            Some(path) => ToolAccesses::read_write_file(path),
            None => ToolAccesses::none(),
        }
    }

    /// The edit's diff, derived by the *same* function execution uses.
    ///
    /// `old` is the file's current bytes (not `old_string`): a preview is what
    /// the user needs to approve, and that is the whole file region that
    /// changes, not the needle the model happened to write. `new` is the full
    /// post-edit content for the same reason.
    fn preview(&self, arguments: &Value) -> Option<PreviewResult> {
        let args: EditTextArgs = serde_json::from_value(arguments.clone()).ok()?;
        let path = self.ctx.resolve_path(&args.path);
        let content = fs::read_to_string(&path).ok()?;
        let planned = plan_edit(
            &content,
            &args.old_string,
            &args.new_string,
            args.replace_all.unwrap_or(false),
            &args.path,
        )
        .ok()?;
        let warnings = planned.warnings();
        Some(PreviewResult {
            path: args.path,
            op: PatchOp::Edit,
            old: content,
            new: planned.content,
            start_line: 1,
            warnings,
        })
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: EditTextArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution(self.name(), format!("invalid arguments: {err}"))
        })?;

        let raw_path = &args.path;
        let path = self.ctx.resolve_path(raw_path);
        let content = fs::read_to_string(&path).map_err(|err| {
            ToolError::execution(
                self.name(),
                format!("failed to read file `{raw_path}`: {err}"),
            )
        })?;

        let planned = plan_edit(
            &content,
            &args.old_string,
            &args.new_string,
            args.replace_all.unwrap_or(false),
            raw_path,
        )
        .map_err(|error| ToolError::execution(self.name(), error))?;

        fs::write(&path, &planned.content).map_err(|err| {
            ToolError::execution(
                self.name(),
                format!("failed to write edited file `{raw_path}`: {err}"),
            )
        })?;

        let warnings = planned.warnings();
        Ok(ToolOutput::Patch {
            path: raw_path.clone(),
            op: PatchOp::Edit,
            old: args.old_string,
            new: args.new_string,
            start_line: planned.start_line,
            warnings,
        })
    }
}

/// Diagnoses edit failure when no exact or normalized window matches.
fn diagnose_edit_failure(content: &str, old_str: &str, path: &str) -> String {
    let file_lines: Vec<&str> = content.lines().collect();
    let old_lines: Vec<&str> = old_str.lines().collect();

    // Check 1: leading-indentation mismatch. Trailing whitespace, CRLF, and
    // typographic differences are already handled by the line-window fallback,
    // so the remaining whitespace failure is the semantic one.
    let first_old = old_lines.first().copied().unwrap_or("");
    let trimmed_first = first_old.trim_start();
    if let Some(index) = file_lines
        .iter()
        .position(|line| line.trim_start() == trimmed_first)
    {
        return format!(
            "Could not match `old_string` in '{path}': the text exists around line {}, \
             but its leading indentation differs. Leading whitespace is semantic and is \
             not ignored; re-read '{path}' and copy the indentation exactly.",
            index + 1
        );
    }

    // Check 2: multi-line divergence — find where the match starts breaking down.
    if old_lines.len() > 1 {
        let candidate_starts: Vec<usize> = file_lines
            .iter()
            .enumerate()
            .filter(|(_, line)| **line == first_old)
            .map(|(idx, _)| idx)
            .collect();

        if candidate_starts.len() == 1 {
            let start = candidate_starts[0];
            for (offset, old_line) in old_lines.iter().enumerate() {
                let file_idx = start + offset;
                if file_idx >= file_lines.len() {
                    return format!(
                        "Could not match `old_string` in '{path}'. Match started at line {}, \
                         but `old_string` extends past the end of the file (file has {} lines, \
                         `old_string` expected at least {}).",
                        start + 1,
                        file_lines.len(),
                        file_idx + 1
                    );
                }
                if file_lines[file_idx] != *old_line {
                    return format!(
                        "Could not match `old_string` in '{path}'. Found a matching start at \
                         line {} (first {} line{} matched), but diverged at line {}:\n\
                         Expected: `{}`\n\
                         File has: `{}`\n\
                         Please re-read '{path}' around line {} to get the latest content.",
                        start + 1,
                        offset,
                        if offset == 1 { "" } else { "s" },
                        file_idx + 1,
                        truncate_for_diagnostic(old_line),
                        truncate_for_diagnostic(file_lines[file_idx]),
                        file_idx + 1
                    );
                }
            }
        } else if candidate_starts.len() > 1 {
            let lines_str = candidate_starts
                .iter()
                .map(|idx| (idx + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return format!(
                "Could not match `old_string` in '{path}'. The first line `{}` appears \
                 multiple times (lines {lines_str}), but subsequent lines did not match. \
                 Please provide more surrounding context or re-read '{path}'.",
                truncate_for_diagnostic(first_old)
            );
        }
    }

    format!(
        "Could not match `old_string` in '{path}' (0 matches found). The content may have \
         changed. Use `read` to inspect the latest file contents before editing."
    )
}

fn truncate_for_diagnostic(line: &str) -> String {
    const MAX: usize = 80;
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let cut = line
        .char_indices()
        .nth(MAX)
        .map(|(offset, _)| offset)
        .unwrap_or(line.len());
    format!("{}...", &line[..cut])
}

#[cfg(test)]
mod tests {
    use super::{plan_edit, split_lines};

    fn file() -> &'static str {
        "alpha\nbeta\ngamma\n"
    }

    #[test]
    fn exact_unique_match_replaces_only_the_needle() {
        let planned = plan_edit(file(), "beta", "BETA", false, "f.rs").unwrap();
        assert_eq!(planned.content, "alpha\nBETA\ngamma\n");
        assert_eq!(planned.start_line, 2);
        assert!(!planned.normalized);
    }

    #[test]
    fn exact_ambiguous_match_is_rejected() {
        let err = plan_edit("x\nx\n", "x", "y", false, "f.rs").unwrap_err();
        assert!(err.contains("2 places"));
        assert!(err.contains("lines 1, 2"));
    }

    #[test]
    fn replace_all_replaces_every_exact_occurrence() {
        let planned = plan_edit("x\nx\n", "x", "y", true, "f.rs").unwrap();
        assert_eq!(planned.content, "y\ny\n");
        assert_eq!(planned.replacements, 2);
    }

    #[test]
    fn no_op_is_rejected() {
        let err = plan_edit(file(), "beta", "beta", false, "f.rs").unwrap_err();
        assert!(err.contains("identical"));
    }

    #[test]
    fn crlf_file_matches_exact_needle_without_rewriting_endings() {
        let planned = plan_edit("a\r\nb\r\nc\r\n", "b", "B", false, "f.rs").unwrap();
        assert_eq!(planned.content, "a\r\nB\r\nc\r\n");
        assert!(!planned.normalized);
    }

    #[test]
    fn crlf_file_matches_lf_block_and_stays_crlf() {
        let planned = plan_edit("a\r\nb\r\nc\r\n", "a\nb", "A\nB", false, "f.rs").unwrap();
        assert_eq!(planned.content, "A\r\nB\r\nc\r\n");
        assert!(planned.normalized);
    }

    #[test]
    fn trailing_whitespace_is_normalized_in_a_block() {
        let planned = plan_edit(
            "alpha  \nbeta\n",
            "alpha\nbeta",
            "ALPHA\nBETA",
            false,
            "f.rs",
        )
        .unwrap();
        assert_eq!(planned.content, "ALPHA\nBETA\n");
        assert!(planned.normalized);
    }

    #[test]
    fn typographic_quotes_are_normalized() {
        let planned = plan_edit(
            "let s = “hello”;\n",
            "let s = \"hello\";",
            "let s = \"hi\";",
            false,
            "f.rs",
        )
        .unwrap();
        assert_eq!(planned.content, "let s = \"hi\";\n");
        assert!(planned.normalized);
    }

    #[test]
    fn multi_line_block_matches_with_normalization() {
        let planned = plan_edit(
            "fn f() {\r\n    let x = 1;  \r\n}\r\n",
            "fn f() {\n    let x = 1;\n}",
            "fn f() {\n    let x = 2;\n}",
            false,
            "f.rs",
        )
        .unwrap();
        assert_eq!(planned.content, "fn f() {\r\n    let x = 2;\r\n}\r\n");
        assert_eq!(planned.start_line, 1);
    }

    #[test]
    fn leading_indentation_is_not_ignored() {
        let err = plan_edit("  alpha\n  beta\n", "alpha\nbeta", "x", false, "f.rs").unwrap_err();
        assert!(err.contains("leading indentation"));
    }

    #[test]
    fn replace_all_normalized_windows_do_not_overlap() {
        let planned = plan_edit("a a  \na a  \na a  \n", "a a\na a", "X", true, "f.rs").unwrap();
        assert_eq!(planned.content, "X\na a  \n");
        assert_eq!(planned.replacements, 1);
    }

    #[test]
    fn normalized_ambiguous_window_is_rejected() {
        let err = plan_edit("a  \na\n", "a", "b", false, "f.rs").unwrap_err();
        assert!(err.contains("2 places"));
    }

    #[test]
    fn split_lines_preserves_offsets_and_endings() {
        let lines = split_lines("a\r\nb\nc");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "a");
        assert_eq!(lines[0].ending, "\r\n");
        assert_eq!(lines[0].start, 0);
        assert_eq!(lines[0].end, 1);
        assert_eq!(lines[1].text, "b");
        assert_eq!(lines[1].ending, "\n");
        assert_eq!(lines[2].text, "c");
        assert_eq!(lines[2].ending, "");
    }
}
