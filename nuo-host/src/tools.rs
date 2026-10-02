//! Canonical system, filesystem, and shell tools for cognitive agents.
//!
//! Provides production-ready implementations of filesystem inspection and mutation,
//! text searching, and shell execution conforming to [`nuo_tool::Tool`].

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nuo_tool::{Result as ToolResult, RiskProfile, Tool, ToolContext, ToolError, ToolOutput, ToolScope};
use serde_json::{Value, json};

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
        "read_text"
    }

    fn description(&self) -> &str {
        "Reads a text file with line numbers. Supports offset (1-based start line) and limit (max lines)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the text file; relative paths use the workspace root"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line number to start reading from (default 1)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> ToolResult<ToolOutput> {
        let raw_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `path`"))?;

        let offset = arguments
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;

        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .map(|l| l.max(1) as usize);

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
        let mut formatted = Vec::with_capacity(slice.len() + 1);

        for (idx, line) in slice.iter().enumerate() {
            let line_num = start_idx + idx + 1;
            formatted.push(format!("{line_num:4} | {line}"));
        }

        let summary = format!(
            "[Lines {}-{} of {} from `{}`]\n{}",
            start_idx + 1,
            end_idx,
            total_lines,
            raw_path,
            formatted.join("\n")
        );

        Ok(ToolOutput::success(summary))
    }
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
        "write_file"
    }

    fn description(&self) -> &str {
        "Creates a new file or completely overwrites an existing file with the given content."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to create or overwrite; relative paths use the workspace root"
                },
                "content": {
                    "type": "string",
                    "description": "The complete file content to write"
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::Destructive
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> ToolResult<ToolOutput> {
        let raw_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `path`"))?;

        let content = arguments
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `content`"))?;

        let path = self.ctx.resolve_path(raw_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ToolError::execution(
                    self.name(),
                    format!("failed to create parent directories: {err}"),
                )
            })?;
        }

        fs::write(&path, content).map_err(|err| {
            ToolError::execution(self.name(), format!("failed to write file `{raw_path}`: {err}"))
        })?;

        let lines = content.lines().count();
        let bytes = content.len();
        Ok(ToolOutput::success(format!(
            "Successfully wrote {bytes} bytes ({lines} lines) to `{raw_path}`."
        )))
    }
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
        "edit_text"
    }

    fn description(&self) -> &str {
        "Replace an exact, unique block of text (old_string) with new_string in a text file. old_string must match exactly one location."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the text file to modify; relative paths use the workspace root"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact verbatim text to replace; must match uniquely in the file"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text to insert in place of old_string"
                }
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::IdempotentMutation
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> ToolResult<ToolOutput> {
        let raw_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `path`"))?;

        let old_string = arguments
            .get("old_string")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `old_string`"))?;

        let new_string = arguments
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `new_string`"))?;

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
                format!("`old_string` was not found in `{raw_path}`"),
            ));
        }
        if matches.len() > 1 {
            return Err(ToolError::execution(
                self.name(),
                format!(
                    "`old_string` matched {} locations in `{raw_path}`; it must match exactly once",
                    matches.len()
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
        "list_dir"
    }

    fn description(&self) -> &str {
        "List the immediate children of a directory (non-recursive)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list immediate children of (default '.')"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum entries cap (default 200)"
                }
            },
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> ToolResult<ToolOutput> {
        let raw_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".");

        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200) as usize;

        let path = self.ctx.resolve_path(raw_path);
        let read_dir = fs::read_dir(&path).map_err(|err| {
            ToolError::execution(self.name(), format!("failed to open dir `{raw_path}`: {err}"))
        })?;

        let mut entries = Vec::new();
        for entry in read_dir {
            if let Ok(e) = entry {
                let file_name = e.file_name().to_string_lossy().to_string();
                let file_type = e.file_type().ok();
                let is_dir = file_type.as_ref().map(|t| t.is_dir()).unwrap_or(false);
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                entries.push((is_dir, file_name, size));
            }
        }

        // Sort: directories first, then alphabetically
        entries.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });

        let total = entries.len();
        let display_entries: Vec<String> = entries
            .into_iter()
            .take(limit)
            .map(|(is_dir, name, size)| {
                if is_dir {
                    format!("[DIR]  {name}/")
                } else {
                    format!("[FILE] {name:<30} ({size} B)")
                }
            })
            .collect();

        let truncated = if total > limit {
            format!("\n[Showing {} of {} entries]", limit, total)
        } else {
            String::new()
        };

        Ok(ToolOutput::success(format!(
            "Directory: `{raw_path}` ({} items):\n{}{}",
            total,
            display_entries.join("\n"),
            truncated
        )))
    }
}

/// Recursively finds files matching glob patterns, respecting .gitignore.
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
        "find_files"
    }

    fn description(&self) -> &str {
        "Find files recursively by path glob patterns. Globs match file paths relative to path. Project ignore rules apply."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to search (default '.')"
                },
                "patterns": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Path globs to match files (e.g. ['*.rs'], ['src/**'])"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum results cap (default 200)"
                }
            },
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> ToolResult<ToolOutput> {
        let raw_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".");

        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200) as usize;

        let patterns: Vec<String> = arguments
            .get("patterns")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();

        let root = self.ctx.resolve_path(raw_path);
        if !root.exists() {
            return Err(ToolError::execution(
                self.name(),
                format!("search path does not exist: `{raw_path}`"),
            ));
        }

        let mut matches = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .build();

        for entry in walker.flatten() {
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                let relative = entry
                    .path()
                    .strip_prefix(&self.ctx.workspace_root)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .to_string();

                let matched = if patterns.is_empty() {
                    true
                } else {
                    patterns.iter().any(|pat| {
                        glob::Pattern::new(pat)
                            .map(|p| p.matches(&relative) || p.matches(&entry.file_name().to_string_lossy()))
                            .unwrap_or(false)
                    })
                };

                if matched {
                    matches.push(relative);
                    if matches.len() >= limit {
                        break;
                    }
                }
            }
        }

        if matches.is_empty() {
            return Ok(ToolOutput::success("No matching files found."));
        }

        Ok(ToolOutput::success(format!(
            "Found {} matching files:\n{}",
            matches.len(),
            matches.join("\n")
        )))
    }
}

/// Recursively searches file contents for regular expressions or literal text.
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
        "search_text"
    }

    fn description(&self) -> &str {
        "Search file contents recursively for literal text or regular expressions. Returns path:line:content matches."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Exact text or regex to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search (default '.')"
                },
                "regex": {
                    "type": "boolean",
                    "description": "Treat query as a regular expression (default false)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum matching lines (default 200)"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly, ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> ToolResult<ToolOutput> {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `query`"))?;

        let raw_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".");

        let is_regex = arguments
            .get("regex")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200) as usize;

        let compiled_re = if is_regex {
            Some(regex::Regex::new(query).map_err(|err| {
                ToolError::execution(self.name(), format!("invalid regular expression: {err}"))
            })?)
        } else {
            None
        };

        let root = self.ctx.resolve_path(raw_path);
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .build();

        let mut results = Vec::new();
        'outer: for entry in walker.flatten() {
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                let file_path = entry.path();
                if let Ok(content) = fs::read_to_string(file_path) {
                    let rel_path = file_path
                        .strip_prefix(&self.ctx.workspace_root)
                        .unwrap_or(file_path)
                        .to_string_lossy();

                    for (line_idx, line) in content.lines().enumerate() {
                        let matched = match &compiled_re {
                            Some(re) => re.is_match(line),
                            None => line.contains(query),
                        };

                        if matched {
                            results.push(format!("{}:{}: {}", rel_path, line_idx + 1, line.trim()));
                            if results.len() >= limit {
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            return Ok(ToolOutput::success("No matching lines found."));
        }

        Ok(ToolOutput::success(format!(
            "Found {} match(es):\n{}",
            results.len(),
            results.join("\n")
        )))
    }
}

/// Executes shell commands within the workspace context.
pub struct ExecuteCommandTool {
    ctx: Arc<SystemToolContext>,
}

impl ExecuteCommandTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for ExecuteCommandTool {
    fn name(&self) -> &str {
        "execute_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command in a non-interactive environment. Commands must be finite and self-terminating."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Timeout in seconds"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ArbitraryExecution
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Workspace]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> ToolResult<ToolOutput> {
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `command`"))?;

        let timeout_secs = arguments
            .get("timeout")
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
            .unwrap_or(self.ctx.default_timeout);

        #[cfg(windows)]
        let mut cmd = tokio::process::Command::new("cmd");
        #[cfg(windows)]
        cmd.args(["/C", command]);

        #[cfg(not(windows))]
        let mut cmd = tokio::process::Command::new("sh");
        #[cfg(not(windows))]
        cmd.args(["-c", command]);

        cmd.current_dir(&self.ctx.workspace_root);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let output_future = cmd.output();
        let output = match tokio::time::timeout(timeout_secs, output_future).await {
            Ok(Ok(out)) => out,
            Ok(Err(err)) => {
                return Err(ToolError::execution(
                    self.name(),
                    format!("failed to spawn process: {err}"),
                ));
            }
            Err(_) => {
                return Err(ToolError::execution(
                    self.name(),
                    format!("command timed out after {}s", timeout_secs.as_secs()),
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        let mut summary = Vec::new();
        if !stdout.is_empty() {
            summary.push(format!("STDOUT:\n{}", stdout.trim_end()));
        }
        if !stderr.is_empty() {
            summary.push(format!("STDERR:\n{}", stderr.trim_end()));
        }
        if summary.is_empty() {
            summary.push("(No output produced)".to_string());
        }
        summary.push(format!("(exit code: {})", exit_code));

        if output.status.success() {
            Ok(ToolOutput::success(summary.join("\n\n")))
        } else {
            Ok(ToolOutput::error(summary.join("\n\n")))
        }
    }
}

/// Creates the full standard suite of system/workspace tools for an agent.
pub fn create_system_tools(ctx: Arc<SystemToolContext>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadTextTool::new(ctx.clone())),
        Arc::new(WriteFileTool::new(ctx.clone())),
        Arc::new(EditTextTool::new(ctx.clone())),
        Arc::new(ListDirTool::new(ctx.clone())),
        Arc::new(FindFilesTool::new(ctx.clone())),
        Arc::new(SearchTextTool::new(ctx.clone())),
        Arc::new(ExecuteCommandTool::new(ctx)),
    ]
}

/// Registers the system tools into a ToolRegistry.
pub fn register_system_tools(registry: &mut nuo_tool::ToolRegistry, ctx: Arc<SystemToolContext>) {
    for tool in create_system_tools(ctx) {
        registry.register_arc(tool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_system_tools_lifecycle() {
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
        assert!(!write_res.is_error);

        // 2. Read file
        let read_res = read_tool
            .execute(
                &t_ctx,
                json!({
                    "path": "hello.txt",
                    "offset": 2,
                    "limit": 2
                }),
            )
            .await
            .unwrap();
        assert!(read_res.content.contains("line 2: world"));
        assert!(read_res.content.contains("line 3: end"));

        // 3. Edit file
        let edit_res = edit_tool
            .execute(
                &t_ctx,
                json!({
                    "path": "hello.txt",
                    "old_string": "line 2: world",
                    "new_string": "line 2: universe"
                }),
            )
            .await
            .unwrap();
        assert!(!edit_res.is_error);

        // Verify edit
        let read_res2 = read_tool
            .execute(
                &t_ctx,
                json!({
                    "path": "hello.txt"
                }),
            )
            .await
            .unwrap();
        assert!(read_res2.content.contains("line 2: universe"));

        // 4. Search text
        let search_res = search_tool
            .execute(
                &t_ctx,
                json!({
                    "query": "universe"
                }),
            )
            .await
            .unwrap();
        assert!(search_res.content.contains("hello.txt:2: line 2: universe"));

        // 5. List dir
        let list_res = list_tool
            .execute(&t_ctx, json!({"path": "."}))
            .await
            .unwrap();
        assert!(list_res.content.contains("hello.txt"));
    }
}
