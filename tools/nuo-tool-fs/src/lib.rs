//! Filesystem inspection, mutation, and search tools conforming to [`nuo_tool::Tool`].

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nuo_tool::{
    BuiltinTool, RiskProfile, Tool, ToolContext, ToolError, ToolOutput, ToolScope, ToolSchema,
};
use serde::Deserialize;
use serde_json::Value;

/// Context holding the active workspace root and system tool settings.
#[derive(Debug, Clone)]
pub struct SystemToolContext {
    pub workspace_root: PathBuf,
    pub default_timeout: Duration,
}

impl SystemToolContext {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            default_timeout: Duration::from_secs(60),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    pub fn resolve_path(&self, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.workspace_root.join(p)
        }
    }
}

/// Render `file_path` for a tool result line, preferring a path relative to the
/// search root so results stay short and stable. When the search root *is* the
/// file itself — a single-file search such as `path: "src/lib.rs"` — stripping
/// it yields an empty string, which would emit a pathless `:LINE: content`
/// line (unreadable for both the model and the UI), so fall back first to the
/// workspace-relative path and finally to the bare filename.
fn display_match_path(file_path: &Path, search_root: &Path, workspace_root: &Path) -> String {
    for base in [search_root, workspace_root] {
        if let Ok(rel) = file_path.strip_prefix(base)
            && !rel.as_os_str().is_empty()
        {
            return rel.to_string_lossy().into_owned();
        }
    }
    file_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.to_string_lossy().into_owned())
}

/// Typed parameters for [`ReadTextTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ReadTextArgs {
    #[tool(desc = "Path to the text file; relative paths use the workspace root")]
    pub path: String,
    #[tool(desc = "1-based line number to start reading from (default 1)")]
    pub offset: Option<usize>,
    #[tool(desc = "Maximum number of lines to read")]
    pub limit: Option<usize>,
}

/// Reads a text file with optional line-level pagination.
pub struct ReadTextTool {
    ctx: Arc<SystemToolContext>,
}

impl ReadTextTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for ReadTextTool {
    fn name(&self) -> &str {
        BuiltinTool::ReadText.as_str()
    }

    fn description(&self) -> &str {
        "Reads a text file with line numbers. Supports offset (1-based start line) and limit (max lines)."
    }

    fn parameters_schema(&self) -> Value {
        ReadTextArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: ReadTextArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::execution(self.name(), format!("invalid arguments: {err}")))?;

        let raw_path = &args.path;
        let offset = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.map(|l| l.max(1));

        let path = self.ctx.resolve_path(raw_path);
        if !path.exists() {
            return Err(ToolError::execution(
                self.name(),
                format!("file does not exist: `{raw_path}`"),
            ));
        }

        let content = fs::read_to_string(&path).map_err(|err| {
            ToolError::execution(
                self.name(),
                format!("failed to read file `{raw_path}`: {err}"),
            )
        })?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let start_idx = (offset - 1).min(total_lines);
        let end_idx = match limit {
            Some(l) => (start_idx + l).min(total_lines),
            None => total_lines,
        };

        let slice = &lines[start_idx..end_idx];
        let start_line = start_idx + 1;

        // Keep presentation out of the payload: `text` is pure file content
        // and `start_line` carries the read offset, so the TUI's code band
        // numbers each row from its true file line and syntax-highlights by
        // path. The model still needs to know where the slice sits and how to
        // continue, so that framing rides in the model-only `prefix` / `suffix`
        // that `ToolOutput::to_text` composes (and the renderer ignores).
        let prefix = Some(format!(
            "[Lines {}-{} of {} from `{}`]",
            start_line, end_idx, total_lines, raw_path
        ));
        let remaining = total_lines - end_idx;
        let suffix = (remaining > 0).then(|| {
            format!(
                "[{remaining} more line{} — read with offset={}]",
                if remaining == 1 { "" } else { "s" },
                end_idx + 1
            )
        });
        let lang = Path::new(raw_path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);

        Ok(ToolOutput::Code {
            lang,
            text: slice.join("\n"),
            start_line,
            prefix,
            suffix,
        })
    }
}

/// Typed parameters for [`WriteFileTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct WriteFileArgs {
    #[tool(desc = "Path to the file to create or overwrite; relative paths use the workspace root")]
    pub path: String,
    #[tool(desc = "The complete file content to write")]
    pub content: String,
}

/// Atomically creates or overwrites a file with new content.
pub struct WriteFileTool {
    ctx: Arc<SystemToolContext>,
}

impl WriteFileTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        BuiltinTool::WriteFile.as_str()
    }

    fn description(&self) -> &str {
        "Creates a new file or completely overwrites an existing file with the given content."
    }

    fn parameters_schema(&self) -> Value {
        WriteFileArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::IdempotentMutation
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: WriteFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::execution(self.name(), format!("invalid arguments: {err}")))?;

        let raw_path = &args.path;
        let content = &args.content;

        let path = self.ctx.resolve_path(raw_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ToolError::execution(
                    self.name(),
                    format!("failed to create parent directories for `{raw_path}`: {err}"),
                )
            })?;
        }

        fs::write(&path, content).map_err(|err| {
            ToolError::execution(self.name(), format!("failed to write file `{raw_path}`: {err}"))
        })?;

        Ok(ToolOutput::success(format!(
            "Successfully wrote {} bytes to `{}`.",
            content.len(),
            raw_path
        )))
    }
}

/// Typed parameters for [`EditTextTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct EditTextArgs {
    #[tool(desc = "Path to the text file to modify; relative paths use the workspace root")]
    pub path: String,
    #[tool(desc = "The exact verbatim text to replace; must match uniquely in the file")]
    pub old_string: String,
    #[tool(desc = "The replacement text to insert in place of old_string")]
    pub new_string: String,
}

/// Surgically replaces an exact, unique occurrence of text in a file.
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
        "Replace an exact, unique block of text (old_string) with new_string in a text file. old_string must match exactly one location."
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

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: EditTextArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::execution(self.name(), format!("invalid arguments: {err}")))?;

        let raw_path = &args.path;
        let old_string = &args.old_string;
        let new_string = &args.new_string;

        if old_string.is_empty() {
            return Err(ToolError::execution(
                self.name(),
                "`old_string` cannot be empty",
            ));
        }

        let path = self.ctx.resolve_path(raw_path);
        let content = fs::read_to_string(&path).map_err(|err| {
            ToolError::execution(self.name(), format!("failed to read file `{raw_path}`: {err}"))
        })?;

        let matches: Vec<_> = content.match_indices(old_string).collect();
        if matches.is_empty() {
            return Err(ToolError::execution(
                self.name(),
                diagnose_edit_failure(&content, old_string, raw_path),
            ));
        }
        if matches.len() > 1 {
            let line_numbers: Vec<String> = matches
                .iter()
                .map(|(offset, _)| (content[..*offset].matches('\n').count() + 1).to_string())
                .collect();
            return Err(ToolError::execution(
                self.name(),
                format!(
                    "`old_string` matched in {} places in `{raw_path}` (lines {}). It must match exactly once. Provide more surrounding context to disambiguate.",
                    matches.len(),
                    line_numbers.join(", ")
                ),
            ));
        }

        let modified = content.replacen(old_string, new_string, 1);
        fs::write(&path, modified).map_err(|err| {
            ToolError::execution(self.name(), format!("failed to write edited file `{raw_path}`: {err}"))
        })?;

        Ok(ToolOutput::success(format!(
            "Successfully edited `{raw_path}`."
        )))
    }
}

/// Diagnoses edit failure when `old_string` cannot be matched in `content`.
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

/// Typed parameters for [`ListDirTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ListDirArgs {
    #[tool(desc = "Directory to list immediate children of (default '.')")]
    pub path: Option<String>,
    #[tool(desc = "Maximum entries cap (default 200)")]
    pub limit: Option<usize>,
}

/// Lists immediate children of a directory.
pub struct ListDirTool {
    ctx: Arc<SystemToolContext>,
}

impl ListDirTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        BuiltinTool::ListDir.as_str()
    }

    fn description(&self) -> &str {
        "List the immediate children of a directory (non-recursive)."
    }

    fn parameters_schema(&self) -> Value {
        ListDirArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: ListDirArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::execution(self.name(), format!("invalid arguments: {err}")))?;

        let raw_path = args.path.as_deref().unwrap_or(".");
        let limit = args.limit.unwrap_or(200).max(1);

        let path = self.ctx.resolve_path(raw_path);
        if !path.exists() {
            return Err(ToolError::execution(
                self.name(),
                format!("directory does not exist: `{raw_path}`"),
            ));
        }
        if !path.is_dir() {
            return Err(ToolError::execution(
                self.name(),
                format!("path is not a directory: `{raw_path}`"),
            ));
        }

        let entries = fs::read_dir(&path).map_err(|err| {
            ToolError::execution(
                self.name(),
                format!("failed to read directory `{raw_path}`: {err}"),
            )
        })?;

        let mut items = Vec::new();
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let file_type = entry.file_type().ok();
            let is_dir = file_type.map(|t| t.is_dir()).unwrap_or(false);
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);

            let type_indicator = if is_dir { "[DIR] " } else { "[FILE]" };
            items.push(format!("{type_indicator} {:<25} ({size} B)", file_name));
        }

        items.sort();
        let total = items.len();
        let truncated = items.into_iter().take(limit).collect::<Vec<_>>();

        let mut summary = format!("Directory: `{raw_path}` ({total} items):\n{}", truncated.join("\n"));
        if total > limit {
            let omitted = total - limit;
            summary.push_str(&format!("\n... ({omitted} additional entries omitted)"));
        }

        Ok(ToolOutput::success(summary))
    }
}

/// Typed parameters for [`FindFilesTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct FindFilesArgs {
    #[tool(desc = "Directory to search (default '.')")]
    pub path: Option<String>,
    #[tool(desc = "Path globs to match files (e.g. ['*.rs'], ['src/**'])")]
    pub patterns: Vec<String>,
    #[tool(desc = "Maximum results cap (default 200)")]
    pub limit: Option<usize>,
}

/// Finds files recursively matching glob patterns.
pub struct FindFilesTool {
    ctx: Arc<SystemToolContext>,
}

impl FindFilesTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for FindFilesTool {
    fn name(&self) -> &str {
        BuiltinTool::FindFiles.as_str()
    }

    fn description(&self) -> &str {
        "Find files recursively by path glob patterns. Globs match file paths relative to path. Project ignore rules apply."
    }

    fn parameters_schema(&self) -> Value {
        FindFilesArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: FindFilesArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::execution(self.name(), format!("invalid arguments: {err}")))?;

        let raw_path = args.path.as_deref().unwrap_or(".");
        let limit = args.limit.unwrap_or(200).max(1);

        let search_root = self.ctx.resolve_path(raw_path);
        if !search_root.exists() {
            return Err(ToolError::execution(
                self.name(),
                format!("path does not exist: `{raw_path}`"),
            ));
        }

        let mut compiled_globs = Vec::new();
        for pat in &args.patterns {
            let glob = glob::Pattern::new(pat).map_err(|err| {
                ToolError::execution(self.name(), format!("invalid glob pattern `{pat}`: {err}"))
            })?;
            compiled_globs.push(glob);
        }

        let mut matches = Vec::new();
        let walker = ignore::WalkBuilder::new(&search_root)
            .standard_filters(true)
            .hidden(false)
            .build();

        for entry in walker.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }

            let path = entry.path();
            let relative = display_match_path(path, &search_root, &self.ctx.workspace_root);

            let matched = compiled_globs.is_empty()
                || compiled_globs.iter().any(|g| g.matches(&relative));

            if matched {
                matches.push(relative.to_string());
                if matches.len() >= limit {
                    break;
                }
            }
        }

        if matches.is_empty() {
            Ok(ToolOutput::success(format!(
                "No matching files found under `{raw_path}`."
            )))
        } else {
            Ok(ToolOutput::success(format!(
                "Found {} matching files:\n{}",
                matches.len(),
                matches.join("\n")
            )))
        }
    }
}

/// Typed parameters for [`SearchTextTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct SearchTextArgs {
    #[tool(desc = "Directory or file to search (default '.')")]
    pub path: Option<String>,
    #[tool(desc = "Exact text or regex to search for")]
    pub query: String,
    #[tool(desc = "Treat query as a regular expression (default false)")]
    pub regex: Option<bool>,
    #[tool(desc = "Maximum matching lines (default 200)")]
    pub limit: Option<usize>,
}

/// Searches file contents recursively for literal text or regular expressions.
pub struct SearchTextTool {
    ctx: Arc<SystemToolContext>,
}

impl SearchTextTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for SearchTextTool {
    fn name(&self) -> &str {
        BuiltinTool::SearchText.as_str()
    }

    fn description(&self) -> &str {
        "Search file contents recursively for literal text or regular expressions. Returns path:line:content matches."
    }

    fn parameters_schema(&self) -> Value {
        SearchTextArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: SearchTextArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::execution(self.name(), format!("invalid arguments: {err}")))?;

        let raw_path = args.path.as_deref().unwrap_or(".");
        let limit = args.limit.unwrap_or(200).max(1);
        let is_regex = args.regex.unwrap_or(false);

        let search_root = self.ctx.resolve_path(raw_path);
        if !search_root.exists() {
            return Err(ToolError::execution(
                self.name(),
                format!("path does not exist: `{raw_path}`"),
            ));
        }

        let regex_matcher = if is_regex {
            Some(regex::Regex::new(&args.query).map_err(|err| {
                ToolError::execution(self.name(), format!("invalid regex pattern: {err}"))
            })?)
        } else {
            None
        };

        let mut matches = Vec::new();
        let walker = ignore::WalkBuilder::new(&search_root)
            .standard_filters(true)
            .hidden(false)
            .build();

        for entry in walker.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }

            let file_path = entry.path();
            let relative = display_match_path(file_path, &search_root, &self.ctx.workspace_root);

            let Ok(content) = fs::read_to_string(file_path) else {
                continue;
            };

            for (idx, line) in content.lines().enumerate() {
                let hit = match &regex_matcher {
                    Some(re) => re.is_match(line),
                    None => line.contains(&args.query),
                };

                if hit {
                    matches.push(format!("{}:{}: {}", relative, idx + 1, line.trim()));
                    if matches.len() >= limit {
                        break;
                    }
                }
            }

            if matches.len() >= limit {
                break;
            }
        }

        if matches.is_empty() {
            Ok(ToolOutput::success("No matching lines found.".to_string()))
        } else {
            Ok(ToolOutput::success(format!(
                "Found {} match(es):\n{}",
                matches.len(),
                matches.join("\n")
            )))
        }
    }
}

/// Creates the suite of filesystem tools for an agent.
pub fn create_fs_tools(ctx: Arc<SystemToolContext>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadTextTool::new(ctx.clone())),
        Arc::new(WriteFileTool::new(ctx.clone())),
        Arc::new(EditTextTool::new(ctx.clone())),
        Arc::new(ListDirTool::new(ctx.clone())),
        Arc::new(FindFilesTool::new(ctx.clone())),
        Arc::new(SearchTextTool::new(ctx)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_fs_tools_lifecycle() {
        let dir = tempdir().unwrap();
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
        let t_ctx = ToolContext::default();

        let write_tool = WriteFileTool::new(ctx.clone());
        let read_tool = ReadTextTool::new(ctx.clone());
        let edit_tool = EditTextTool::new(ctx.clone());
        let list_tool = ListDirTool::new(ctx.clone());
        let search_tool = SearchTextTool::new(ctx.clone());

        // 1. Write file
        let write_res = write_tool
            .execute(
                &t_ctx,
                json!({
                    "path": "hello.txt",
                    "content": "line 1: hello\nline 2: world\nline 3: end"
                }),
            )
            .await
            .unwrap();
        assert!(!write_res.is_error());

        // 2. Read file
        let read_res = read_tool
            .execute(&t_ctx, json!({"path": "hello.txt", "offset": 2, "limit": 1}))
            .await
            .unwrap();
        assert!(read_res.content().contains("line 2: world"));

        // 3. Edit file
        let edit_res = edit_tool
            .execute(
                &t_ctx,
                json!({
                    "path": "hello.txt",
                    "old_string": "line 2: world",
                    "new_string": "line 2: nuo"
                }),
            )
            .await
            .unwrap();
        assert!(!edit_res.is_error());

        // 4. Search text
        let search_res = search_tool
            .execute(&t_ctx, json!({"path": ".", "query": "nuo"}))
            .await
            .unwrap();
        assert!(search_res.content().contains("hello.txt:2: line 2: nuo"));

        // 5. List dir
        let list_res = list_tool
            .execute(&t_ctx, json!({"path": "."}))
            .await
            .unwrap();
        assert!(list_res.content().contains("hello.txt"));
    }

    #[tokio::test]
    async fn read_text_emits_structured_code_with_pagination_framing() {
        let dir = tempdir().unwrap();
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
        let t_ctx = ToolContext::default();
        fs::write(dir.path().join("src.rs"), "a\nb\nc\nd\n").unwrap();

        let out = ReadTextTool::new(ctx)
            .execute(&t_ctx, json!({"path": "src.rs", "offset": 2, "limit": 2}))
            .await
            .unwrap();

        match &out {
            ToolOutput::Code {
                lang,
                text,
                start_line,
                prefix,
                suffix,
            } => {
                // Pure content: no baked gutter and no range header.
                assert_eq!(text, "b\nc");
                assert_eq!(*start_line, 2);
                assert_eq!(lang.as_deref(), Some("rs"));
                assert_eq!(prefix.as_deref(), Some("[Lines 2-3 of 4 from `src.rs`]"));
                assert_eq!(suffix.as_deref(), Some("[1 more line — read with offset=4]"));
            }
            other => panic!("expected ToolOutput::Code, got {other:?}"),
        }

        // The model still sees the range header and per-line file numbers,
        // composed at `to_text` time instead of baked into `text`.
        assert_eq!(
            out.to_text(),
            "[Lines 2-3 of 4 from `src.rs`]\n2: b\n3: c\n[1 more line — read with offset=4]"
        );
    }

    #[tokio::test]
    async fn read_text_to_eof_has_no_continuation_hint() {
        let dir = tempdir().unwrap();
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
        let t_ctx = ToolContext::default();
        fs::write(dir.path().join("full.txt"), "one\ntwo\n").unwrap();

        let out = ReadTextTool::new(ctx)
            .execute(&t_ctx, json!({"path": "full.txt"}))
            .await
            .unwrap();

        match out {
            ToolOutput::Code {
                text,
                start_line,
                prefix,
                suffix,
                ..
            } => {
                assert_eq!(text, "one\ntwo");
                assert_eq!(start_line, 1);
                assert_eq!(prefix.as_deref(), Some("[Lines 1-2 of 2 from `full.txt`]"));
                assert_eq!(suffix, None);
            }
            other => panic!("expected ToolOutput::Code, got {other:?}"),
        }
    }

    #[test]
    fn display_match_path_falls_back_when_root_is_the_file() {
        let ws = Path::new("/ws");
        let file = Path::new("/ws/src/actions.rs");
        // Directory root: relative to the search root.
        assert_eq!(
            display_match_path(file, Path::new("/ws/src"), ws),
            "actions.rs"
        );
        // File root (single-file search): stripping would be empty, so fall
        // back to the workspace-relative path rather than the empty string.
        assert_eq!(display_match_path(file, file, ws), "src/actions.rs");
        // No usable base at all -> bare filename, never empty.
        assert_eq!(
            display_match_path(file, Path::new("/x"), Path::new("/y")),
            "actions.rs"
        );
    }

    #[tokio::test]
    async fn search_text_single_file_search_emits_a_real_path() {
        // Regression: searching a *file* (not a directory) used to strip the
        // file's own path as a prefix, yielding an empty path and emitting a
        // pathless `:LINE: content` line — unreadable for both the model and
        // the TUI (which then tallied 0 files and dropped the file title row).
        let dir = tempdir().unwrap();
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
        let t_ctx = ToolContext::default();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let file = dir.path().join("src").join("actions.rs");
        fs::write(&file, "fn a() {}\npub(super) fn enter_scene(\nfn b() {}\n").unwrap();

        let out = SearchTextTool::new(ctx)
            .execute(&t_ctx, json!({"path": "src/actions.rs", "query": "enter_scene"}))
            .await
            .unwrap();

        let text = out.content();
        assert!(
            text.contains("src/actions.rs:2: pub(super) fn enter_scene("),
            "the match line must carry a real, workspace-relative path; got:\n{text}"
        );
        assert!(
            !text.contains("\n:2:"),
            "no pathless match line may be emitted; got:\n{text}"
        );
    }

    #[tokio::test]
    async fn test_diagnose_edit_failure_whitespace() {
        let dir = tempdir().unwrap();
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
        let t_ctx = ToolContext::default();

        let file_path = dir.path().join("code.rs");
        fs::write(&file_path, "fn hello() {\n    let a = 1;\n}\n").unwrap();

        let edit_tool = EditTextTool::new(ctx.clone());
        let err = edit_tool
            .execute(
                &t_ctx,
                json!({
                    "path": "code.rs",
                    "old_string": "    let a = 1;  ",
                    "new_string": "    let a = 2;"
                }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("trailing whitespace"));
    }
}
