//! Multi-file `patch` tool (`BuiltinTool::Patch`) supporting `*** Begin Patch`
//! envelopes with `*** Add File:`, `*** Update File:` (including optional
//! `*** Move to:`), and `*** Delete File:` sections.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tool::{
    BuiltinTool, PatchOp, PreviewResult, RiskProfile, ScopeTarget, Tool, ToolAccesses, ToolContext,
    ToolError, ToolOutput, ToolSchema, ToolScope,
};

use super::{SystemToolContext, atomic_write};

/// Typed parameters for [`PatchTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct PatchArgs {
    #[tool(
        desc = "Multi-file patch in `*** Begin Patch` ... `*** End Patch` format supporting `*** Add File:`, `*** Update File:` (with optional `*** Move to:`), and `*** Delete File:` sections."
    )]
    pub patch_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePatch {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<UpdateHunk>,
    },
}

impl FilePatch {
    fn affected_paths(&self) -> Vec<&str> {
        match self {
            FilePatch::Add { path, .. } | FilePatch::Delete { path } => vec![path.as_str()],
            FilePatch::Update { path, move_to, .. } => {
                let mut paths = vec![path.as_str()];
                if let Some(dest) = move_to.as_deref() {
                    paths.push(dest);
                }
                paths
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateHunk {
    pub context: Option<String>,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

/// Parses a `*** Begin Patch` ... `*** End Patch` document into structured
/// per-file operations.
pub fn parse_patch(patch_text: &str) -> Result<Vec<FilePatch>, String> {
    let normalized = patch_text.replace("\r\n", "\n");
    let raw_lines: Vec<&str> = normalized.lines().collect();
    if raw_lines.is_empty() {
        return Err("patch_text is empty".to_string());
    }

    let mut idx = 0;
    let mut saw_begin = false;
    while idx < raw_lines.len() && raw_lines[idx].trim().is_empty() {
        idx += 1;
    }
    if idx < raw_lines.len() && raw_lines[idx].trim() == "*** Begin Patch" {
        saw_begin = true;
        idx += 1;
    }

    let mut ops = Vec::new();
    while idx < raw_lines.len() {
        let line = raw_lines[idx];
        let trimmed = line.trim();
        if trimmed == "*** End Patch" {
            break;
        }
        if trimmed.is_empty() {
            idx += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Add File:") {
            let path = clean_header_path(path, "Add File")?;
            idx += 1;
            let mut added_lines = Vec::new();
            while idx < raw_lines.len() {
                let cur = raw_lines[idx];
                if cur.starts_with("*** ") {
                    break;
                }
                let body = cur.strip_prefix('+').ok_or_else(|| {
                    format!("invalid line in `*** Add File: {path}` (expected `+` prefix): {cur:?}")
                })?;
                added_lines.push(body);
                idx += 1;
            }
            let mut content = added_lines.join("\n");
            if !added_lines.is_empty() {
                content.push('\n');
            }
            ops.push(FilePatch::Add { path, content });
        } else if let Some(path) = line.strip_prefix("*** Delete File:") {
            let path = clean_header_path(path, "Delete File")?;
            idx += 1;
            ops.push(FilePatch::Delete { path });
        } else if let Some(path) = line.strip_prefix("*** Update File:") {
            let path = clean_header_path(path, "Update File")?;
            idx += 1;
            let mut move_to = None;
            if idx < raw_lines.len()
                && let Some(dest) = raw_lines[idx].strip_prefix("*** Move to:")
            {
                move_to = Some(clean_header_path(dest, "Move to")?);
                idx += 1;
            }

            let mut hunks = Vec::new();
            while idx < raw_lines.len() {
                let cur = raw_lines[idx];
                if cur.starts_with("*** ") {
                    break;
                }
                if cur.trim().is_empty() {
                    idx += 1;
                    continue;
                }
                let header = cur.strip_prefix("@@").ok_or_else(|| {
                    format!(
                        "expected `@@` hunk header in `*** Update File: {path}`, found: {cur:?}"
                    )
                })?;
                let context = {
                    let trimmed_ctx = header.trim();
                    if trimmed_ctx.is_empty() {
                        None
                    } else {
                        Some(trimmed_ctx.to_string())
                    }
                };
                idx += 1;

                let mut hunk_lines = Vec::new();
                while idx < raw_lines.len() {
                    let hline = raw_lines[idx];
                    if hline.starts_with("*** ") || hline.starts_with("@@") {
                        break;
                    }
                    if hline == r"\ No newline at end of file" {
                        idx += 1;
                        continue;
                    }
                    if let Some(rest) = hline.strip_prefix(' ') {
                        hunk_lines.push(HunkLine::Context(rest.to_string()));
                    } else if let Some(rest) = hline.strip_prefix('-') {
                        hunk_lines.push(HunkLine::Remove(rest.to_string()));
                    } else if let Some(rest) = hline.strip_prefix('+') {
                        hunk_lines.push(HunkLine::Add(rest.to_string()));
                    } else if hline.is_empty() {
                        hunk_lines.push(HunkLine::Context(String::new()));
                    } else {
                        return Err(format!(
                            "invalid hunk line in `*** Update File: {path}`: {hline:?}"
                        ));
                    }
                    idx += 1;
                }
                if hunk_lines.is_empty() {
                    return Err(format!("empty hunk in `*** Update File: {path}`"));
                }
                hunks.push(UpdateHunk {
                    context,
                    lines: hunk_lines,
                });
            }

            if hunks.is_empty() && move_to.is_none() {
                return Err(format!(
                    "`*** Update File: {path}` has neither hunks nor `*** Move to:`"
                ));
            }
            ops.push(FilePatch::Update {
                path,
                move_to,
                hunks,
            });
        } else {
            return Err(format!("unexpected patch directive: {line:?}"));
        }
    }

    if saw_begin && !raw_lines.iter().any(|l| l.trim() == "*** End Patch") {
        return Err("missing `*** End Patch` terminator".to_string());
    }
    if ops.is_empty() {
        return Err("patch contains no file operations".to_string());
    }
    Ok(ops)
}

fn clean_header_path(raw: &str, directive: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("`*** {directive}:` requires a non-empty path"));
    }
    Ok(trimmed.to_string())
}

/// Applies ordered [`UpdateHunk`]s to `original`, preserving line endings and
/// trailing-newline presence.
pub fn apply_update_hunks(
    original: &str,
    hunks: &[UpdateHunk],
    path: &str,
) -> Result<String, String> {
    if hunks.is_empty() {
        return Ok(original.to_string());
    }

    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let had_trailing_newline = original.ends_with('\n');
    let normalized = original.replace("\r\n", "\n");
    let mut lines: Vec<String> = if normalized.is_empty() {
        Vec::new()
    } else {
        normalized.lines().map(ToString::to_string).collect()
    };

    let mut search_from = 0usize;
    for (hunk_idx, hunk) in hunks.iter().enumerate() {
        let old_block: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.as_str()),
                HunkLine::Add(_) => None,
            })
            .collect();
        let new_block: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                HunkLine::Context(s) | HunkLine::Add(s) => Some(s.clone()),
                HunkLine::Remove(_) => None,
            })
            .collect();

        let match_start = if old_block.is_empty() {
            search_from.min(lines.len())
        } else {
            find_hunk_match(&lines, &old_block, search_from, path, hunk_idx)?
        };

        let match_end = match_start + old_block.len();
        lines.splice(match_start..match_end, new_block.iter().cloned());
        search_from = match_start + new_block.len();
    }

    let mut out = lines.join(line_ending);
    if had_trailing_newline && !lines.is_empty() {
        out.push_str(line_ending);
    }
    Ok(out)
}

fn find_hunk_match(
    lines: &[String],
    needle: &[&str],
    search_from: usize,
    path: &str,
    hunk_idx: usize,
) -> Result<usize, String> {
    if needle.len() > lines.len() {
        return Err(format!(
            "hunk #{} for `{path}` did not match (hunk longer than file)",
            hunk_idx + 1
        ));
    }

    // 1. Exact match at or after `search_from`.
    let forward_exact = find_windows(lines, needle, search_from, false);
    if let Some(&pos) = forward_exact.first() {
        if forward_exact.len() == 1 {
            return Ok(pos);
        }
        return Err(format!(
            "hunk #{} for `{path}` matched {} places; provide more context",
            hunk_idx + 1,
            forward_exact.len()
        ));
    }

    // 2. Trailing-whitespace-trimmed match at or after `search_from`.
    let forward_trimmed = find_windows(lines, needle, search_from, true);
    if let Some(&pos) = forward_trimmed.first() {
        if forward_trimmed.len() == 1 {
            return Ok(pos);
        }
        return Err(format!(
            "hunk #{} for `{path}` matched {} places after whitespace trimming; provide more context",
            hunk_idx + 1,
            forward_trimmed.len()
        ));
    }

    // 3. Fallback anywhere in file if out-of-order hunks.
    let anywhere = find_windows(lines, needle, 0, false);
    if anywhere.len() == 1 {
        return Ok(anywhere[0]);
    }
    let anywhere_trimmed = find_windows(lines, needle, 0, true);
    if anywhere_trimmed.len() == 1 {
        return Ok(anywhere_trimmed[0]);
    }

    Err(format!(
        "hunk #{} for `{path}` did not match file contents",
        hunk_idx + 1
    ))
}

fn find_windows(lines: &[String], needle: &[&str], start: usize, trim_end: bool) -> Vec<usize> {
    let mut hits = Vec::new();
    let max_start = lines.len().saturating_sub(needle.len());
    for i in start..=max_start {
        let matched = lines[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| {
                if trim_end {
                    a.trim_end() == b.trim_end()
                } else {
                    a.as_str() == *b
                }
            });
        if matched {
            hits.push(i);
        }
    }
    hits
}

/// Resolves `raw` inside `workspace_root` and rejects any lexical or symlink
/// escape outside `workspace_root`.
fn confine_workspace_path(workspace_root: &Path, raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("path cannot be empty".to_string());
    }
    let candidate = Path::new(trimmed);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace_root.join(candidate)
    };

    let normalized_root = normalize_lexical(workspace_root);
    let normalized_target = normalize_lexical(&joined);
    if !normalized_target.starts_with(&normalized_root) {
        return Err(format!(
            "path `{raw}` escapes workspace root `{}`",
            workspace_root.display()
        ));
    }

    let canonical_root = fs::canonicalize(workspace_root).unwrap_or(normalized_root);
    let mut probe = normalized_target.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !probe.exists() {
        let Some(name) = probe.file_name().map(|s| s.to_os_string()) else {
            break;
        };
        tail.push(name);
        let Some(parent) = probe.parent() else {
            break;
        };
        probe = parent.to_path_buf();
    }
    if probe.exists() {
        let canonical_base = fs::canonicalize(&probe)
            .map_err(|err| format!("failed to canonicalize `{}`: {err}", probe.display()))?;
        if !canonical_base.starts_with(&canonical_root) {
            return Err(format!(
                "path `{raw}` resolves outside workspace root `{}`",
                workspace_root.display()
            ));
        }
        let mut resolved = canonical_base;
        for part in tail.into_iter().rev() {
            resolved.push(part);
        }
        Ok(resolved)
    } else {
        Ok(normalized_target)
    }
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

enum PlannedFileOp {
    Write {
        display_path: String,
        target: PathBuf,
        old_content: Option<String>,
        new_content: String,
        op: PatchOp,
    },
    Move {
        from_display: String,
        to_display: String,
        from: PathBuf,
        to: PathBuf,
        new_content: String,
    },
    Delete {
        display_path: String,
        target: PathBuf,
        old_content: String,
    },
}

/// Applies multi-file patches atomically within the workspace root.
pub struct PatchTool {
    ctx: Arc<SystemToolContext>,
}

impl PatchTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        BuiltinTool::Patch.as_str()
    }

    fn description(&self) -> &str {
        "Apply a multi-file patch in `*** Begin Patch` ... `*** End Patch` format. \
         Supports `*** Add File:`, `*** Update File:` (with optional `*** Move to:`), \
         and `*** Delete File:`. All paths are confined to the workspace root and \
         validated before any file is modified."
    }

    fn parameters_schema(&self) -> Value {
        PatchArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::IdempotentMutation
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Workspace]
    }

    fn scope_target(&self, arguments: &str) -> ScopeTarget {
        let Ok(args) = serde_json::from_str::<PatchArgs>(arguments) else {
            return ScopeTarget::Unspecified;
        };
        let Ok(ops) = parse_patch(&args.patch_text) else {
            return ScopeTarget::Unspecified;
        };
        match ops.iter().flat_map(FilePatch::affected_paths).next() {
            Some(first) => ScopeTarget::Path(PathBuf::from(first)),
            None => ScopeTarget::Unspecified,
        }
    }

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        let Ok(args) = serde_json::from_str::<PatchArgs>(arguments) else {
            return ToolAccesses::none();
        };
        let Ok(ops) = parse_patch(&args.patch_text) else {
            return ToolAccesses::none();
        };
        let mut unique = BTreeSet::new();
        for op in &ops {
            for path in op.affected_paths() {
                unique.insert(path.to_string());
            }
        }
        let mut accesses = ToolAccesses::none();
        for path in unique {
            accesses.0.extend(ToolAccesses::read_write_file(path).0);
        }
        accesses
    }

    fn preview(&self, arguments: &Value) -> Option<PreviewResult> {
        let args: PatchArgs = serde_json::from_value(arguments.clone()).ok()?;
        let ops = parse_patch(&args.patch_text).ok()?;
        if ops.len() != 1 {
            return None;
        }
        match &ops[0] {
            FilePatch::Add { path, content } => Some(PreviewResult {
                path: path.clone(),
                op: PatchOp::Create,
                old: String::new(),
                new: content.clone(),
                start_line: 1,
                warnings: Vec::new(),
            }),
            FilePatch::Delete { path } => {
                let target = confine_workspace_path(&self.ctx.workspace_root, path).ok()?;
                let old = fs::read_to_string(&target).ok()?;
                Some(PreviewResult {
                    path: path.clone(),
                    op: PatchOp::Delete,
                    old,
                    new: String::new(),
                    start_line: 1,
                    warnings: Vec::new(),
                })
            }
            FilePatch::Update {
                path,
                move_to: None,
                hunks,
            } => {
                let target = confine_workspace_path(&self.ctx.workspace_root, path).ok()?;
                let old = fs::read_to_string(&target).ok()?;
                let new = apply_update_hunks(&old, hunks, path).ok()?;
                Some(PreviewResult {
                    path: path.clone(),
                    op: PatchOp::Edit,
                    old,
                    new,
                    start_line: 1,
                    warnings: Vec::new(),
                })
            }
            FilePatch::Update {
                move_to: Some(_), ..
            } => None,
        }
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let exec_err = |msg: String| ToolError::execution(BuiltinTool::Patch.as_str(), msg);
        let args: PatchArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::invalid_args(BuiltinTool::Patch.as_str(), err.to_string()))?;
        let parsed = parse_patch(&args.patch_text).map_err(exec_err)?;

        let mut planned = Vec::with_capacity(parsed.len());
        let mut deleted_in_patch: BTreeSet<PathBuf> = BTreeSet::new();

        for op in parsed {
            match op {
                FilePatch::Add { path, content } => {
                    let target = confine_workspace_path(&self.ctx.workspace_root, &path)
                        .map_err(exec_err)?;
                    if target.exists() && !deleted_in_patch.contains(&target) {
                        return Err(exec_err(format!(
                            "cannot add `{path}`: file already exists"
                        )));
                    }
                    planned.push(PlannedFileOp::Write {
                        display_path: path,
                        target,
                        old_content: None,
                        new_content: content,
                        op: PatchOp::Create,
                    });
                }
                FilePatch::Delete { path } => {
                    let target = confine_workspace_path(&self.ctx.workspace_root, &path)
                        .map_err(exec_err)?;
                    let old_content = fs::read_to_string(&target)
                        .map_err(|err| exec_err(format!("cannot delete `{path}`: {err}")))?;
                    deleted_in_patch.insert(target.clone());
                    planned.push(PlannedFileOp::Delete {
                        display_path: path,
                        target,
                        old_content,
                    });
                }
                FilePatch::Update {
                    path,
                    move_to,
                    hunks,
                } => {
                    let source = confine_workspace_path(&self.ctx.workspace_root, &path)
                        .map_err(exec_err)?;
                    let old_content = fs::read_to_string(&source)
                        .map_err(|err| exec_err(format!("cannot read `{path}`: {err}")))?;
                    let new_content =
                        apply_update_hunks(&old_content, &hunks, &path).map_err(exec_err)?;
                    if let Some(dest_display) = move_to {
                        let dest = confine_workspace_path(&self.ctx.workspace_root, &dest_display)
                            .map_err(exec_err)?;
                        if dest != source && dest.exists() && !deleted_in_patch.contains(&dest) {
                            return Err(exec_err(format!(
                                "cannot move `{path}` to `{dest_display}`: destination already exists"
                            )));
                        }
                        deleted_in_patch.insert(source.clone());
                        planned.push(PlannedFileOp::Move {
                            from_display: path,
                            to_display: dest_display,
                            from: source,
                            to: dest,
                            new_content,
                        });
                    } else {
                        planned.push(PlannedFileOp::Write {
                            display_path: path,
                            target: source,
                            old_content: Some(old_content),
                            new_content,
                            op: PatchOp::Edit,
                        });
                    }
                }
            }
        }

        let mut summary_lines = Vec::with_capacity(planned.len());
        let mut single_patch_output: Option<ToolOutput> = None;
        let is_single = planned.len() == 1;

        for item in planned {
            match item {
                PlannedFileOp::Write {
                    display_path,
                    target,
                    old_content,
                    new_content,
                    op,
                } => {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(|err| {
                            exec_err(format!(
                                "failed to create parent directory for `{display_path}`: {err}"
                            ))
                        })?;
                    }
                    atomic_write(&target, new_content.as_bytes()).map_err(|err| {
                        exec_err(format!("failed to write `{display_path}`: {err}"))
                    })?;
                    let tag = match op {
                        PatchOp::Create => "A",
                        PatchOp::Edit => "M",
                        PatchOp::Delete => "D",
                    };
                    summary_lines.push(format!("{tag} {display_path}"));
                    if is_single {
                        single_patch_output = Some(ToolOutput::Patch {
                            path: display_path,
                            op,
                            old: old_content.unwrap_or_default(),
                            new: new_content,
                            start_line: 1,
                            warnings: Vec::new(),
                        });
                    }
                }
                PlannedFileOp::Move {
                    from_display,
                    to_display,
                    from,
                    to,
                    new_content,
                } => {
                    if let Some(parent) = to.parent() {
                        fs::create_dir_all(parent).map_err(|err| {
                            exec_err(format!(
                                "failed to create parent directory for `{to_display}`: {err}"
                            ))
                        })?;
                    }
                    atomic_write(&to, new_content.as_bytes()).map_err(|err| {
                        exec_err(format!("failed to write `{to_display}`: {err}"))
                    })?;
                    if from != to {
                        fs::remove_file(&from).map_err(|err| {
                            exec_err(format!(
                                "failed to remove `{from_display}` after move: {err}"
                            ))
                        })?;
                    }
                    summary_lines.push(format!("R {from_display} -> {to_display}"));
                }
                PlannedFileOp::Delete {
                    display_path,
                    target,
                    old_content,
                } => {
                    fs::remove_file(&target).map_err(|err| {
                        exec_err(format!("failed to delete `{display_path}`: {err}"))
                    })?;
                    summary_lines.push(format!("D {display_path}"));
                    if is_single {
                        single_patch_output = Some(ToolOutput::Patch {
                            path: display_path,
                            op: PatchOp::Delete,
                            old: old_content,
                            new: String::new(),
                            start_line: 1,
                            warnings: Vec::new(),
                        });
                    }
                }
            }
        }

        if let Some(output) = single_patch_output {
            Ok(output)
        } else {
            Ok(ToolOutput::text(format!(
                "Success. Updated the following files:\n{}",
                summary_lines.join("\n")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn patch_applies_add_update_move_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("modify.txt"), "alpha\nbeta\ngamma\n").unwrap();
        fs::write(root.join("remove.txt"), "bye\n").unwrap();

        let tool = PatchTool::new(Arc::new(SystemToolContext::new(root)));
        let patch = "\
*** Begin Patch
*** Add File: sub/new.txt
+hello
+world
*** Update File: modify.txt
*** Move to: moved/modify.txt
@@
 alpha
-beta
+BETA
 gamma
*** Delete File: remove.txt
*** End Patch";

        let out = tool
            .execute(
                &ToolContext::default(),
                serde_json::json!({ "patch_text": patch }),
            )
            .await
            .unwrap();
        let text = out.to_text();
        assert!(text.contains("A sub/new.txt"), "{text}");
        assert!(text.contains("R modify.txt -> moved/modify.txt"), "{text}");
        assert!(text.contains("D remove.txt"), "{text}");

        assert_eq!(
            fs::read_to_string(root.join("sub/new.txt")).unwrap(),
            "hello\nworld\n"
        );
        assert!(!root.join("modify.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("moved/modify.txt")).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
        assert!(!root.join("remove.txt").exists());
    }

    #[tokio::test]
    async fn patch_rejects_workspace_escape_without_mutating() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("ok.txt"), "one\n").unwrap();

        let tool = PatchTool::new(Arc::new(SystemToolContext::new(root)));
        let patch = "\
*** Begin Patch
*** Add File: ok2.txt
+should not exist
*** Add File: ../escape.txt
+bad
*** End Patch";

        let err = tool
            .execute(
                &ToolContext::default(),
                serde_json::json!({ "patch_text": patch }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("escapes workspace root"), "{err}");
        assert!(!root.join("ok2.txt").exists());
    }
}
