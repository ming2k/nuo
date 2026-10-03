use async_trait::async_trait;
use nuo_wire::Tool;
use nuo_tool::ToolSchema;
use serde::Deserialize;

use crate::tools::helpers::{
    WorkspaceBase, check_expected_version, env_from_root, execution_environment, json_string,
    resolve_workspace_path, workspace_base,
};

#[derive(ToolSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditTextArgs {
    #[tool(desc = "Path to the text file to modify; relative paths use the primary workspace")]
    path: String,
    #[tool(
        desc = "The exact verbatim text to replace; must match uniquely in the file. Globs and regexes are NOT supported."
    )]
    old_string: String,
    #[tool(desc = "The replacement text to insert in place of old_string")]
    new_string: String,
    #[tool(
        desc = "Optional content version the caller last saw for this file (as reported by read_text or code_query). The edit is rejected if the file has changed since."
    )]
    expected_version: Option<String>,
}

/// Apply a text edit to a file (safer than write_file — requires old_string match).
///
/// Relative paths resolve against the session's workspace root (captured at
/// factory time), not the daemon process's cwd — under the unified daemon
/// (ADR-0096) those differ whenever the daemon was first spawned from another
/// project, and an edit is exactly where that divergence does damage.
pub struct EditTextTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>>,
}

impl EditTextTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self { root, env: None }
    }

    pub fn with_env(env: std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }
}

/// Number of unchanged context lines to include above and below the edit in the
/// diff display (GitHub-style: 3 lines of surrounding context).
const DIFF_CONTEXT: usize = 3;

/// Extract up to [`DIFF_CONTEXT`] lines above and below a match in `content`,
/// returning the context-bracketed `old`/`new` snippets and an adjusted
/// `start_line` so the line-number gutter reflects true file positions.
fn contextual_patch(
    content: &str,
    match_offset: usize,
    old_str: &str,
    new_str: &str,
) -> (String, String, usize) {
    let match_end = match_offset + old_str.len();

    // Anchor the patch at the beginning of the line containing the match, then
    // walk back over complete source lines. Keeping byte slices instead of
    // splitting/rejoining with `str::lines()` preserves CRLF and prevents a
    // leading newline after the match from becoming a phantom context row.
    let mut context_start = content[..match_offset]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    for _ in 0..DIFF_CONTEXT {
        if context_start == 0 {
            break;
        }
        let previous_line_end = context_start - 1;
        context_start = content[..previous_line_end]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
    }

    // If the match includes its trailing newline, it already ends at a line
    // boundary. Otherwise include the unchanged suffix of the line containing
    // the match before walking forward over the requested context lines.
    let mut context_end = if old_str.as_bytes().last() == Some(&b'\n') {
        match_end
    } else {
        content[match_end..]
            .find('\n')
            .map(|idx| match_end + idx + 1)
            .unwrap_or(content.len())
    };
    for _ in 0..DIFF_CONTEXT {
        if context_end >= content.len() {
            break;
        }
        context_end = content[context_end..]
            .find('\n')
            .map(|idx| context_end + idx + 1)
            .unwrap_or(content.len());
    }

    let start_line = content[..context_start]
        .bytes()
        .filter(|&byte| byte == b'\n')
        .count()
        + 1;

    let prefix = &content[context_start..match_offset];
    let suffix = &content[match_end..context_end];

    let old_ctx = format!("{prefix}{old_str}{suffix}");
    let new_ctx = format!("{prefix}{new_str}{suffix}");

    (old_ctx, new_ctx, start_line)
}

#[derive(Debug)]
struct AppliedEdit {
    new_content: String,
    old_ctx: String,
    new_ctx: String,
    ctx_start: usize,
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut cursor = 0;
    while let Some(idx) = haystack[cursor..].find(needle) {
        count += 1;
        cursor += idx + needle.len();
    }
    count
}

fn apply_unique_edit(
    content: &str,
    old_str: &str,
    new_str: &str,
    path: &str,
) -> Result<Option<AppliedEdit>, String> {
    let match_count = count_occurrences(content, old_str);
    if match_count == 0 {
        return Ok(None);
    }
    if match_count > 1 {
        let lines: Vec<usize> = content
            .match_indices(old_str)
            .map(|(offset, _)| content[..offset].matches('\n').count() + 1)
            .collect();
        let lines_str = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "`old_string` matches in {match_count} places in '{path}' (lines {lines_str}). \
             It must match exactly once. Provide more surrounding lines to disambiguate."
        ));
    }

    // match_count >= 1 is guaranteed above; the else arm is fail-safe.
    let Some(match_offset) = content.find(old_str) else {
        return Ok(None);
    };
    let match_end = match_offset + old_str.len();

    let mut new_content = String::with_capacity(content.len() + new_str.len() - old_str.len());
    new_content.push_str(&content[..match_offset]);
    new_content.push_str(new_str);
    new_content.push_str(&content[match_end..]);

    let (old_ctx, new_ctx, ctx_start) = contextual_patch(content, match_offset, old_str, new_str);
    Ok(Some(AppliedEdit {
        new_content,
        old_ctx,
        new_ctx,
        ctx_start,
    }))
}

/// Generate a helpful, diagnostic error message when `old_string` fails to match.
///
/// Checks common failure modes in order of specificity:
/// 1. Whitespace mismatches (trailing spaces, tab vs space)
/// 2. Multi-line prefix matched but diverged at a specific line
/// 3. Zero matches with advice to re-read
fn diagnose_edit_failure(content: &str, old_str: &str, path: &str) -> String {
    let file_lines: Vec<&str> = content.lines().collect();
    let old_lines: Vec<&str> = old_str.lines().collect();

    // Check 1: Trailing whitespace mismatch
    let content_trimmed: String = file_lines
        .iter()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    let old_trimmed: String = old_lines
        .iter()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    if content_trimmed.contains(&old_trimmed) && !content.contains(old_str) {
        // Find which line has the mismatch
        let trimmed_old_first = old_lines.first().map(|l| l.trim_end()).unwrap_or("");
        let matching_line = file_lines
            .iter()
            .position(|l| l.trim_end() == trimmed_old_first)
            .map(|idx| idx + 1)
            .unwrap_or(1);
        return format!(
            "Could not find exact match for `old_string` in '{path}', but a match exists \
             when ignoring trailing whitespace (around line {matching_line}). \
             Check for trailing spaces or tabs in your `old_string`."
        );
    }

    // Check 2: Multi-line divergence — find where the match starts breaking down
    if old_lines.len() > 1 {
        let first_old = old_lines[0];
        // Find all lines in the file that match the first line of old_string
        let candidate_starts: Vec<usize> = file_lines
            .iter()
            .enumerate()
            .filter(|(_, line)| **line == first_old)
            .map(|(idx, _)| idx)
            .collect();

        if candidate_starts.len() == 1 {
            let start = candidate_starts[0];
            // Walk forward to find the exact line that diverged
            for (offset, old_line) in old_lines.iter().enumerate() {
                let file_idx = start + offset;
                if file_idx >= file_lines.len() {
                    return format!(
                        "Could not find exact match for `old_string` in '{path}'. \
                         Match started at line {}, but `old_string` extends past the end of the file \
                         (file has {} lines, `old_string` expected at least {}).",
                        start + 1,
                        file_lines.len(),
                        file_idx + 1
                    );
                }
                if file_lines[file_idx] != *old_line {
                    let max_disp = 80;
                    let exp = if old_line.len() > max_disp {
                        format!("{}...", &old_line[..max_disp])
                    } else {
                        old_line.to_string()
                    };
                    let got = if file_lines[file_idx].len() > max_disp {
                        format!("{}...", &file_lines[file_idx][..max_disp])
                    } else {
                        file_lines[file_idx].to_string()
                    };
                    return format!(
                        "Could not find exact match for `old_string` in '{path}'. \
                         Found matching start at line {} (first {} line{} matched), but diverged at line {}:\n\
                         Expected: `{exp}`\n\
                         File has: `{got}`\n\
                         Please re-read '{path}' around line {} to get the latest content.",
                        start + 1,
                        offset,
                        if offset == 1 { "" } else { "s" },
                        file_idx + 1,
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
            let first_line = if first_old.len() > 60 {
                format!("{}...", &first_old[..60])
            } else {
                first_old.to_string()
            };
            return format!(
                "Could not find exact match for `old_string` in '{path}'. \
                 The first line `{first_line}` appears multiple times (lines {lines_str}), \
                 but subsequent lines did not match. Please provide more surrounding context or re-read '{path}'."
            );
        }
    }

    format!(
        "Could not find exact match for `old_string` in '{path}' (0 matches found). \
         The content of '{path}' may have changed. Please use `read_text` to inspect the latest file contents before editing."
    )
}

#[async_trait]
impl Tool for EditTextTool {
    fn name(&self) -> &str {
        "edit_text"
    }
    fn description(&self) -> &str {
        "Replace an exact, unique block of text (old_string) with new_string in a text file. old_string must match exactly one location verbatim."
    }
    fn parameters(&self) -> serde_json::Value {
        EditTextArgs::parameters_schema()
    }
    fn scope_target(&self, arguments: &str) -> nuo_wire::ScopeTarget {
        nuo_wire::ScopeTarget::Path(std::path::PathBuf::from(json_string(arguments, "path")))
    }
    fn hazard_level(&self) -> nuo_wire::HazardLevel {
        nuo_wire::HazardLevel::FileModification
    }
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<nuo_wire::ToolPermissionSubmission> {
        let path = json_string(arguments, "path");
        Some(nuo_wire::ToolPermissionSubmission {
            hazard_level: nuo_wire::HazardLevel::FileModification,
            label: format!("Edit text in `{path}`"),
            description: format!("Modifies text content within file `{path}`."),
            scope: path.clone(),
            payload: nuo_wire::ToolPermissionPayload::FileEdit {
                paths: vec![path],
                operation: "edit_text".to_string(),
            },
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<nuo_wire::ToolOutput, String> {
        let args: EditTextArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let path = &args.path;
        let old_str = &args.old_string;
        let new_str = &args.new_string;
        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        let resolved = resolve_workspace_path(&self.root, path);

        let content = env
            .fs()
            .read_to_string(&resolved)
            .await
            .map_err(|e| format!("Failed to read '{}': {}", path, e))?;

        // Freshness precondition first, before any matching work: a caller that
        // read a snapshot refuses to edit drift it never saw (ADR-0237). The
        // `old_string` anchor below pins the edited region only; this pins the
        // whole file.
        check_expected_version(
            path,
            args.expected_version.as_deref(),
            Some(content.as_bytes()),
        )?;

        // Exact match first; fall back to a CRLF-normalized comparison so an
        // edit authored with LF line endings works against a CRLF file. Either
        // path requires the match to be *unique* — an ambiguous old_string is an
        // error, never a silent global replace.
        let edit = match apply_unique_edit(&content, old_str, new_str, path)? {
            Some(e) => e,
            None => {
                let normalized_content = content.replace("\r\n", "\n");
                let normalized_old = old_str.replace("\r\n", "\n");
                match apply_unique_edit(&normalized_content, &normalized_old, new_str, path)? {
                    Some(e) => e,
                    None => {
                        return Err(diagnose_edit_failure(&content, old_str, path));
                    }
                }
            }
        };

        // Atomically commit the new content (temp file + fsync + rename) so an
        // interrupted edit never corrupts the file in place.
        env.fs()
            .write(&resolved, edit.new_content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write '{}': {}", path, e))?;
        Ok(super::syntax_guard::mutation_output(
            &resolved,
            &edit.new_content,
            nuo_wire::ToolOutput::Patch {
                path: path.to_string(),
                op: nuo_wire::PatchOp::Edit,
                old: edit.old_ctx,
                new: edit.new_ctx,
                start_line: edit.ctx_start,
                warnings: Vec::new(),
            },
        ))
    }
}

nuo_wire::register_tool!(EditTextFactory => |ctx| EditTextTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_occurrences_is_zero_for_empty_needle() {
        assert_eq!(count_occurrences("abc", ""), 0);
    }

    #[test]
    fn count_occurrences_handles_overlapping_and_repeats() {
        assert_eq!(count_occurrences("aaa", "a"), 3);
        // `str::matches` is non-overlapping, so "aaaa"/"aa" is 2, not 3.
        assert_eq!(count_occurrences("aaaa", "aa"), 2);
        assert_eq!(count_occurrences("abcabc", "abc"), 2);
        assert_eq!(count_occurrences("abc", "z"), 0);
    }

    #[test]
    fn apply_unique_edit_replaces_exactly_one_occurrence() {
        let content = "foo\nbar\nbaz".to_string();
        let edit = apply_unique_edit(&content, "bar", "qux", "f.txt")
            .unwrap()
            .expect("single match should apply");
        assert_eq!(edit.new_content, "foo\nqux\nbaz");
        assert_eq!(edit.old_ctx, "foo\nbar\nbaz");
        assert_eq!(edit.new_ctx, "foo\nqux\nbaz");
        assert_eq!(edit.ctx_start, 1);
    }

    #[test]
    fn contextual_patch_keeps_three_real_lines_without_phantom_blank() {
        let content = "above 1\nabove 2\nabove 3\nold\nbelow 1\nbelow 2\nbelow 3\nbelow 4\n";
        let offset = content.find("old").unwrap();
        let (old, new, start_line) = contextual_patch(content, offset, "old", "new");

        assert_eq!(
            old,
            "above 1\nabove 2\nabove 3\nold\nbelow 1\nbelow 2\nbelow 3\n"
        );
        assert_eq!(
            new,
            "above 1\nabove 2\nabove 3\nnew\nbelow 1\nbelow 2\nbelow 3\n"
        );
        assert_eq!(start_line, 1);
    }

    #[test]
    fn contextual_patch_preserves_inline_prefix_suffix_and_line_number() {
        let content =
            "discard\nheader\nkeep 1\nkeep 2\nlet value = old;\nafter 1\nafter 2\nafter 3\n";
        let offset = content.find("old").unwrap();
        let (old, new, start_line) = contextual_patch(content, offset, "old", "new");

        assert_eq!(
            old,
            "header\nkeep 1\nkeep 2\nlet value = old;\nafter 1\nafter 2\nafter 3\n"
        );
        assert_eq!(
            new,
            "header\nkeep 1\nkeep 2\nlet value = new;\nafter 1\nafter 2\nafter 3\n"
        );
        assert_eq!(start_line, 2);
    }

    #[test]
    fn contextual_patch_preserves_crlf_and_trailing_newline_match() {
        let content = "before\r\nold\r\nafter 1\r\nafter 2\r\nafter 3\r\nafter 4\r\n";
        let offset = content.find("old\r\n").unwrap();
        let (old, new, start_line) = contextual_patch(content, offset, "old\r\n", "new\r\n");

        assert_eq!(old, "before\r\nold\r\nafter 1\r\nafter 2\r\nafter 3\r\n");
        assert_eq!(new, "before\r\nnew\r\nafter 1\r\nafter 2\r\nafter 3\r\n");
        assert_eq!(start_line, 1);
    }

    #[test]
    fn apply_unique_edit_errors_on_ambiguous_match() {
        let content = "dup\ndup\nother".to_string();
        let err = apply_unique_edit(&content, "dup", "x", "f.txt").unwrap_err();
        assert!(
            err.contains("2 places"),
            "ambiguous match must report count: {err}"
        );
    }

    #[test]
    fn apply_unique_edit_returns_none_when_absent() {
        let content = "hello world".to_string();
        assert!(
            apply_unique_edit(&content, "goodbye", "x", "f.txt")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn apply_unique_edit_replaces_between_occurrences() {
        // The replacement must not touch the second occurrence (regression for
        // the old `str::replace`-replaces-all behaviour).
        let content = "x KEEP x".to_string();
        let edit = apply_unique_edit(&content, "x", "Y", "f.txt").unwrap_err(); // ambiguous (2 occurrences) -> error
        assert!(edit.contains("2 places"));
    }

    #[test]
    fn diagnose_edit_failure_detects_whitespace_mismatch() {
        let content = "fn foo() {\n    let x = 1;  \n}\n";
        let old = "    let x = 1;\n";
        let diag = diagnose_edit_failure(content, old, "test.rs");
        assert!(
            diag.contains("when ignoring trailing whitespace"),
            "diagnostic: {diag}"
        );
        assert!(diag.contains("line 2"));
    }

    #[test]
    fn diagnose_edit_failure_detects_divergence() {
        let content = "fn foo() {\n    let x = 1;\n    let y = 200;\n    let z = 3;\n}\n";
        let old = "    let x = 1;\n    let y = 2;\n    let z = 3;";
        let diag = diagnose_edit_failure(content, old, "test.rs");
        assert!(diag.contains("diverged at line 3"), "diagnostic: {diag}");
        assert!(diag.contains("let y = 200"));
    }

    #[test]
    fn diagnose_edit_failure_reports_zero_matches() {
        let content = "fn foo() {}\n";
        let old = "fn bar() {}";
        let diag = diagnose_edit_failure(content, old, "test.rs");
        assert!(diag.contains("0 matches found"), "diagnostic: {diag}");
    }

    /// ADR-0237: `expected_version` refuses an edit whose basis snapshot is
    /// gone, and does so *before* any matching work — an `old_string` anchor
    /// only pins the edited region, so it cannot see drift elsewhere in the
    /// file.
    #[tokio::test]
    async fn expected_version_rejects_an_edit_against_a_stale_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, "pub fn alpha() {}\n").unwrap();
        let tool = EditTextTool::new(Some(dir.path().to_path_buf()));

        let stale = crate::tools::helpers::content_version(b"pub fn something_else() {}\n");
        let error = tool
            .call(
                &serde_json::json!({
                    "path": "lib.rs",
                    "old_string": "pub fn alpha() {}",
                    "new_string": "pub fn alpha_v2() {}",
                    "expected_version": stale,
                })
                .to_string(),
            )
            .await
            .unwrap_err();
        assert!(error.contains("has changed since version"), "{error}");
        assert!(
            std::fs::read_to_string(&file).unwrap().contains("alpha()"),
            "the rejected edit must not have been written"
        );
    }

    /// The happy path: a matching version lets the edit through, and omitting
    /// the parameter preserves the previous behaviour exactly.
    #[tokio::test]
    async fn expected_version_admits_a_matching_snapshot_and_is_optional() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        let original = "pub fn alpha() {}\n";
        std::fs::write(&file, original).unwrap();
        let tool = EditTextTool::new(Some(dir.path().to_path_buf()));

        let version = crate::tools::helpers::content_version(original.as_bytes());
        tool.call(
            &serde_json::json!({
                "path": "lib.rs",
                "old_string": "alpha",
                "new_string": "beta",
                "expected_version": version,
            })
            .to_string(),
        )
        .await
        .expect("matching version must admit the edit");
        assert!(std::fs::read_to_string(&file).unwrap().contains("beta"));

        // No precondition supplied: unchanged legacy behaviour.
        tool.call(
            &serde_json::json!({
                "path": "lib.rs",
                "old_string": "beta",
                "new_string": "gamma",
            })
            .to_string(),
        )
        .await
        .expect("omitting expected_version must not change behaviour");
        assert!(std::fs::read_to_string(&file).unwrap().contains("gamma"));
    }
}
