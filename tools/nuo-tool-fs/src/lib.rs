//! Filesystem inspection, mutation, and search tools conforming to [`nuo_tool::Tool`].

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use nuo_tool::{
    BuiltinTool, PatchOp, PreviewResult, RiskProfile, ScopeTarget, Tool, ToolAccesses, ToolContext,
    ToolError, ToolOutput, ToolSchema, ToolScope,
};
use serde::Deserialize;
use serde_json::Value;

mod edit;
mod read;

pub use edit::{EditTextArgs, EditTextTool};
pub use read::{ReadArgs, ReadTool};

/// The `path` argument of a file tool's raw JSON arguments, when present and
/// non-empty.
///
/// The single mapping from "what the model said" to "which path this call
/// touches". Both [`Tool::scope_target`] (permission) and [`Tool::accesses`]
/// (scheduler arbitration and effect classification) read it, so a tool cannot
/// declare one path to the permission gate and another to the scheduler.
pub(crate) fn path_argument(arguments: &str) -> Option<String> {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_string)
        })
}

/// The read-side declaration shared by every pure-inspection fs tool.
///
/// Reads declare a real access so the scheduler can arbitrate them against a
/// concurrent write to the same path (a reader never conflicts with another
/// reader, but must not run inside a write), while their
/// [`scope_target`](Tool::scope_target) stays `Unspecified` so they remain
/// auto-admitted by the permission chain — reading is not an authority the user
/// should ever be asked to grant.
pub(crate) fn read_accesses(arguments: &str) -> ToolAccesses {
    match path_argument(arguments) {
        Some(path) => ToolAccesses::read_file(path),
        None => ToolAccesses::none(),
    }
}

/// Context holding the active workspace root and filesystem tool settings.
#[derive(Clone)]
pub struct SystemToolContext {
    pub workspace_root: PathBuf,
    pub vfs_resolver: Option<Arc<dyn nuo_tool::VirtualFileResolver>>,
    /// Boundary normalizer for images read from disk. `None` means "pass the
    /// bytes through unchanged"; the server injects the host implementation so
    /// a large screenshot does not enter the durable transcript at full size.
    pub image_normalizer: Option<Arc<dyn nuo_tool::ImageNormalizer>>,
}

/// Semantic alias for filesystem tool context ([INV-TOOL-15]).
pub type FsToolContext = SystemToolContext;

impl std::fmt::Debug for SystemToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemToolContext")
            .field("workspace_root", &self.workspace_root)
            .field("vfs_resolver", &self.vfs_resolver.is_some())
            .field("image_normalizer", &self.image_normalizer.is_some())
            .finish()
    }
}

impl SystemToolContext {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            vfs_resolver: None,
            image_normalizer: None,
        }
    }

    pub fn with_vfs_resolver(mut self, resolver: Arc<dyn nuo_tool::VirtualFileResolver>) -> Self {
        self.vfs_resolver = Some(resolver);
        self
    }

    pub fn with_image_normalizer(mut self, normalizer: Arc<dyn nuo_tool::ImageNormalizer>) -> Self {
        self.image_normalizer = Some(normalizer);
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

/// Typed parameters for [`WriteFileTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct WriteFileArgs {
    #[tool(desc = "Path to the file to create or overwrite relative to the workspace root")]
    pub path: String,
    #[tool(
        desc = "The complete, unabridged content of the file. Never use placeholders like '// ... existing code'"
    )]
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
        "Creates a new file or fully overwrites an entire file. PREFER edit_text for modifying existing files to prevent accidental truncation or loss of unchanged code."
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

    fn scope_target(&self, arguments: &str) -> ScopeTarget {
        match path_argument(arguments) {
            Some(path) => ScopeTarget::Path(PathBuf::from(path)),
            // Unparseable arguments are the schema gate's problem, not the
            // permission gate's: leaving this `Unspecified` lets the call
            // through to the schema policy, which rejects it with a precise
            // message instead of a bogus permission prompt for "*".
            None => ScopeTarget::Unspecified,
        }
    }

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        match path_argument(arguments) {
            Some(path) => ToolAccesses::write_file(path),
            None => ToolAccesses::none(),
        }
    }

    /// The overwrite's diff: current bytes against the proposed bytes.
    ///
    /// A creating write shows an empty `old`, so the preview reads as a creation
    /// rather than as an edit of nothing.
    fn preview(&self, arguments: &Value) -> Option<PreviewResult> {
        let args: WriteFileArgs = serde_json::from_value(arguments.clone()).ok()?;
        let path = self.ctx.resolve_path(&args.path);
        let existing = fs::read_to_string(&path).ok();
        Some(PreviewResult {
            path: args.path,
            op: if existing.is_some() {
                PatchOp::Edit
            } else {
                PatchOp::Create
            },
            old: existing.unwrap_or_default(),
            new: args.content,
            start_line: 1,
            warnings: Vec::new(),
        })
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: WriteFileArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution(self.name(), format!("invalid arguments: {err}"))
        })?;

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

        let is_existing = path.exists();
        let old = if is_existing {
            fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };

        fs::write(&path, content).map_err(|err| {
            ToolError::execution(
                self.name(),
                format!("failed to write file `{raw_path}`: {err}"),
            )
        })?;

        Ok(ToolOutput::Patch {
            path: raw_path.clone(),
            op: if is_existing {
                PatchOp::Edit
            } else {
                PatchOp::Create
            },
            old,
            new: content.clone(),
            start_line: 1,
            warnings: Vec::new(),
        })
    }
}

/// Directories that are almost never interesting to search or list and can be
/// enormous: VCS metadata, dependency trees, and build output.
pub(crate) const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "target",
    "__pycache__",
    ".next",
    ".nuxt",
    ".turbo",
    "dist",
    "build",
    ".venv",
    "venv",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    "coverage",
    ".gradle",
    ".idea",
    ".vscode",
    "proc",
    "sys",
    "dev",
    "run",
];

pub(crate) const IGNORED_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

/// Build a ripgrep-style walker that respects project ignore rules (`.gitignore`,
/// `.ignore`, global gitignore) while pruning metadata and dependency trees (`.git`,
/// `node_modules`, `target`, etc.).
pub(crate) fn build_workspace_walker(root: &Path) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .standard_filters(true)
        .hidden(false)
        .require_git(false)
        .sort_by_file_path(|left, right| left.cmp(right))
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if let Some(name) = entry.file_name().to_str() {
                if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    return !IGNORED_DIRS.contains(&name);
                }
                return !IGNORED_FILES.contains(&name);
            }
            true
        });
    builder
}

/// Typed parameters for [`FindFilesTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct FindFilesArgs {
    #[tool(desc = "Directory to search (default '.')")]
    pub path: Option<String>,
    #[serde(default, alias = "name", alias = "glob", alias = "query")]
    #[tool(
        desc = "Glob pattern or filename to search for (e.g. '*.rs', 'Cargo.toml', 'src/**'). If omitted or empty, matches all files."
    )]
    pub pattern: Option<String>,
    #[serde(default, alias = "globs", alias = "includes")]
    #[tool(desc = "Optional list of glob patterns to match files")]
    pub patterns: Option<Vec<String>>,
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
        "Find files recursively by glob pattern or filename (e.g. '*.rs', 'Cargo.toml'). If pattern is omitted, lists all files. Respects .gitignore and project ignore rules."
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

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        // Recursive walk: claims the whole subtree so a concurrent write
        // anywhere under the search root serializes against this scan.
        match path_argument(arguments) {
            Some(path) => ToolAccesses::search_tree(path),
            None => ToolAccesses::search_tree("."),
        }
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let mut arguments = arguments;
        if let Some(obj) = arguments.as_object_mut()
            && let Some(p) = obj.get("patterns").and_then(|v| v.as_str())
        {
            let p_str = p.to_string();
            obj.insert("patterns".to_string(), serde_json::json!([p_str]));
        }

        let args: FindFilesArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution(self.name(), format!("invalid arguments: {err}"))
        })?;

        let raw_path = args.path.as_deref().unwrap_or(".");
        let limit = args.limit.unwrap_or(200).max(1);

        let search_root = self.ctx.resolve_path(raw_path);
        if !search_root.exists() {
            return Err(ToolError::execution(
                self.name(),
                format!("path does not exist: `{raw_path}`"),
            ));
        }

        let mut raw_patterns = args.patterns.unwrap_or_default();
        if let Some(p) = args.pattern
            && !raw_patterns.contains(&p)
        {
            raw_patterns.push(p);
        }

        let mut compiled_globs = Vec::new();
        for pat in &raw_patterns {
            if pat.is_empty() || pat == "*" || pat == "**/*" || pat == "**" {
                continue;
            }
            let glob = glob::Pattern::new(pat).map_err(|err| {
                ToolError::execution(self.name(), format!("invalid glob pattern `{pat}`: {err}"))
            })?;
            compiled_globs.push((pat.clone(), glob));
        }

        let mut matches = Vec::new();
        let walker = build_workspace_walker(&search_root).build();

        for entry in walker.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }

            let path = entry.path();
            let relative = display_match_path(path, &search_root, &self.ctx.workspace_root);

            let matched = if compiled_globs.is_empty() {
                true
            } else {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                compiled_globs.iter().any(|(pat_str, g)| {
                    g.matches(&relative) || (!pat_str.contains('/') && g.matches(file_name))
                })
            };

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
            Ok(ToolOutput::success(matches.join("\n")))
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

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        match path_argument(arguments) {
            Some(path) => ToolAccesses::search_tree(path),
            None => ToolAccesses::search_tree("."),
        }
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: SearchTextArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution(self.name(), format!("invalid arguments: {err}"))
        })?;

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
        let walker = build_workspace_walker(&search_root).build();

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
            Ok(ToolOutput::Matches {
                pattern: args.query,
                lines: matches,
            })
        }
    }
}

/// Creates the suite of filesystem tools for an agent.
pub fn create_fs_tools(ctx: Arc<SystemToolContext>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadTool::new(ctx.clone())),
        Arc::new(WriteFileTool::new(ctx.clone())),
        Arc::new(EditTextTool::new(ctx.clone())),
        Arc::new(FindFilesTool::new(ctx.clone())),
        Arc::new(SearchTextTool::new(ctx)),
    ]
}

#[cfg(test)]
mod preview_tests;

#[cfg(test)]
mod tests;
