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
mod find;
mod read;
mod version;
pub mod walker;

pub use edit::{EditTextArgs, EditTextTool};
pub use find::{FindFilesArgs, FindFilesTool};
pub use read::{ReadArgs, ReadTool};

/// The `path` argument of a file tool's raw JSON arguments, when present and
/// non-empty.
///
/// The single mapping from "what the model said" to "which path this call
/// touches". Both [`Tool::scope_target`] (permission) and [`Tool::accesses`]
/// (scheduler arbitration and effect classification) read it, so a tool cannot
/// declare one path to the permission gate and another to the scheduler.
pub(crate) fn path_argument(arguments: &Value) -> Option<String> {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

/// The read-side declaration shared by every pure-inspection fs tool.
///
/// Reads declare a real access so the scheduler can arbitrate them against a
/// concurrent write to the same path (a reader never conflicts with another
/// reader, but must not run inside a write), while their
/// [`scope_target`](Tool::scope_target) stays `Unspecified` so they remain
/// auto-admitted by the permission chain — reading is not an authority the user
/// should ever be asked to grant.
pub(crate) fn read_accesses(arguments: &Value) -> ToolAccesses {
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
    /// Boundary text extractor for document formats such as PDF. `None` means
    /// the capability is unavailable in this embedding; `read` then reports a
    /// typed "no extractor" error instead of decoding document bytes as text.
    pub document_extractor: Option<Arc<dyn nuo_tool::DocumentExtractor>>,
    /// Sparse line checkpoints shared by every `read` call in this workspace.
    pub(crate) line_index: Arc<read::LineIndexCache>,
    /// Bounded LRU cache of extracted PDF text keyed by `(path, version, page_range)`.
    pub(crate) document_cache: Arc<read::DocumentTextCache>,
}

/// Semantic alias for filesystem tool context ([INV-TOOL-15]).
pub type FsToolContext = SystemToolContext;

impl std::fmt::Debug for SystemToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemToolContext")
            .field("workspace_root", &self.workspace_root)
            .field("vfs_resolver", &self.vfs_resolver.is_some())
            .field("image_normalizer", &self.image_normalizer.is_some())
            .field("document_extractor", &self.document_extractor.is_some())
            .field("line_index", &"sparse")
            .field("document_cache", &"lru")
            .finish()
    }
}

impl SystemToolContext {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            vfs_resolver: None,
            image_normalizer: None,
            document_extractor: None,
            line_index: Arc::new(read::LineIndexCache::default()),
            document_cache: Arc::new(read::DocumentTextCache::default()),
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

    pub fn with_document_extractor(
        mut self,
        extractor: Arc<dyn nuo_tool::DocumentExtractor>,
    ) -> Self {
        self.document_extractor = Some(extractor);
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

/// Refuse a write whose caller saw an older version of the file.
pub(crate) fn verify_expected_version(
    path: &Path,
    expected: Option<&str>,
    tool: &str,
) -> Result<(), ToolError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let meta = fs::metadata(path).map_err(|error| {
        ToolError::execution(
            tool,
            format!(
                "`{}` does not exist or cannot be stat'd ({error}); omit `expected_version` to create it",
                path.display()
            ),
        )
    })?;
    let actual = version::file_version(path, &meta);
    if actual == expected {
        Ok(())
    } else {
        Err(ToolError::execution(
            tool,
            format!(
                "`{}` changed since it was read (expected version {expected}, found {actual}).                  Re-read the file and base the write on its current bytes.",
                path.display()
            ),
        ))
    }
}

/// Replace a file's contents with a same-directory temp file + rename.
///
/// A direct `fs::write` truncates the target before the new bytes are durable:
/// a crash, a full disk, or a killed process leaves a zero-length or partial
/// file. The rename is atomic on the same filesystem, so readers see either the
/// old bytes or the complete new bytes. The target's permissions are preserved
/// when it already exists.
pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let temp = parent.join(format!(
        ".{file_name}.nuo-tmp-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        if let Ok(metadata) = fs::metadata(path) {
            let _ = fs::set_permissions(&temp, metadata.permissions());
        }
        #[cfg(windows)]
        if path.exists() {
            // Windows cannot replace an existing file with rename; the
            // fallback still avoids truncating the target until the complete
            // temp file exists.
            fs::remove_file(path)?;
        }
        fs::rename(&temp, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Render `file_path` for a tool result line, preferring a path relative to the
/// search root so results stay short and stable. When the search root *is* the
/// file itself — a single-file search such as `path: "src/lib.rs"` — stripping
/// it yields an empty string, which would emit a pathless `:LINE: content`
/// line (unreadable for both the model and the UI), so fall back first to the
/// workspace-relative path and finally to the bare filename.
pub(crate) fn display_match_path(
    file_path: &Path,
    search_root: &Path,
    workspace_root: &Path,
) -> String {
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
    #[tool(desc = "File path to create or overwrite (relative to workspace root)")]
    pub path: String,
    #[tool(desc = "Complete file content (no truncation placeholders)")]
    pub content: String,
    #[tool(desc = "Version returned by read; refuses a write if the file changed since")]
    pub expected_version: Option<String>,
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

    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::WriteFile.aliases()
    }

    fn description(&self) -> &str {
        "Create a new file or overwrite an entire file. Prefer edit_text for localized edits to existing files."
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

    fn path_bound(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(30))
    }

    fn scope_target(&self, arguments: &Value) -> ScopeTarget {
        match path_argument(arguments) {
            Some(path) => ScopeTarget::Path(PathBuf::from(path)),
            // Unparseable arguments are the schema gate's problem, not the
            // permission gate's: leaving this `Unspecified` lets the call
            // through to the schema policy, which rejects it with a precise
            // message instead of a bogus permission prompt for "*".
            None => ScopeTarget::Unspecified,
        }
    }

    fn accesses(&self, arguments: &Value) -> ToolAccesses {
        match path_argument(arguments) {
            Some(path) => ToolAccesses::write_file(path),
            None => ToolAccesses::none(),
        }
    }

    /// The overwrite's diff: current bytes against the proposed bytes.
    ///
    /// A creating write shows an empty `old`, so the preview reads as a creation
    /// rather than as an edit of nothing.
    async fn preview(&self, arguments: &Value) -> Option<PreviewResult> {
        let ctx = Arc::clone(&self.ctx);
        let arguments = arguments.clone();
        let tool_name = self.name().to_string();
        tokio::task::spawn_blocking(move || {
            let args: WriteFileArgs = serde_json::from_value(arguments).ok()?;
            let path = ctx.resolve_path(&args.path);
            verify_expected_version(&path, args.expected_version.as_deref(), &tool_name).ok()?;
            if !preview_input_allowed(&path) {
                return None;
            }
            let existing = fs::read_to_string(&path).ok();
            Some(PreviewResult::bounded(
                args.path,
                if existing.is_some() {
                    PatchOp::Edit
                } else {
                    PatchOp::Create
                },
                existing.unwrap_or_default(),
                args.content,
                Vec::new(),
            ))
        })
        .await
        .ok()
        .flatten()
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let ctx = Arc::clone(&self.ctx);
        tokio::task::spawn_blocking(move || WriteFileTool::execute_blocking(&ctx, arguments))
            .await
            .map_err(|error| {
                ToolError::execution("write_file", format!("write worker failed: {error}"))
            })?
    }
}

impl WriteFileTool {
    fn execute_blocking(
        ctx: &SystemToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, ToolError> {
        let args: WriteFileArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution("write_file", format!("invalid arguments: {err}"))
        })?;

        let raw_path = &args.path;
        let content = &args.content;

        let path = ctx.resolve_path(raw_path);
        verify_expected_version(&path, args.expected_version.as_deref(), "write_file")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ToolError::execution(
                    "write_file",
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

        atomic_write(&path, content.as_bytes()).map_err(|err| {
            ToolError::execution(
                "write_file",
                format!("failed to write file `{raw_path}`: {err}"),
            )
        })?;

        let mut preview = PreviewResult::bounded(
            raw_path.clone(),
            if is_existing {
                PatchOp::Edit
            } else {
                PatchOp::Create
            },
            old,
            content.clone(),
            Vec::new(),
        );
        if preview.truncated {
            preview.warnings.push(
                "The displayed diff is a bounded window around the change; the file itself was written in full."
                    .to_string(),
            );
        }
        Ok(preview.as_patch())
    }
}

/// Preview is computed synchronously on the dispatch path. Reading a huge file
/// to build an approval diff would block the async runtime; beyond this ceiling
/// the prompt reports that no preview is available.
pub(crate) const MAX_PREVIEW_INPUT_BYTES: u64 = 8 * 1024 * 1024;

/// Whether a preview may synchronously read this path.
///
/// A missing path is allowed through so the producer can report the more
/// specific "does not exist" result; only an existing file above the cap is
/// refused. Preview is a UI convenience and must never become an unbounded
/// blocking read in the dispatch pipeline.
pub(crate) fn preview_input_allowed(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.len() <= MAX_PREVIEW_INPUT_BYTES)
        .unwrap_or(true)
}

/// Typed parameters for [`SearchTextTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct SearchTextArgs {
    #[tool(desc = "Directory or file to search (default '.')")]
    pub path: Option<String>,
    #[tool(desc = "Literal text or regex pattern")]
    pub query: String,
    #[tool(desc = "Interpret query as regex (default false)")]
    pub regex: Option<bool>,
    #[serde(default, alias = "glob", alias = "globs", alias = "includes")]
    #[tool(desc = "File glob filters (e.g. ['*.rs'])")]
    pub include: Option<Vec<String>>,
    #[tool(desc = "Case-insensitive search (default false)")]
    pub case_insensitive: Option<bool>,
    #[tool(desc = "Context lines around each match (default 0, max 5)")]
    pub context: Option<usize>,
    #[tool(desc = "Max matching lines (default 200)")]
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

    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::SearchText.aliases()
    }

    fn description(&self) -> &str {
        "Search file contents recursively for text or regex, returning path:line:content matches."
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

    fn path_bound(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(30))
    }

    fn accesses(&self, arguments: &Value) -> ToolAccesses {
        match path_argument(arguments) {
            Some(path) => ToolAccesses::search_tree(path),
            None => ToolAccesses::search_tree("."),
        }
    }

    async fn execute(
        &self,
        call_ctx: &ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, ToolError> {
        let args: SearchTextArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution(self.name(), format!("invalid arguments: {err}"))
        })?;

        let raw_path = args.path.clone().unwrap_or_else(|| ".".to_string());
        let limit = args.limit.unwrap_or(200).max(1);
        let context = args.context.unwrap_or(0).min(5);
        let case_insensitive = args.case_insensitive.unwrap_or(false);
        let is_regex = args.regex.unwrap_or(false);
        let query = args.query.clone();
        let include = args.include.unwrap_or_default();
        let search_root = self.ctx.resolve_path(&raw_path);
        if !search_root.exists() {
            return Err(ToolError::execution(
                self.name(),
                format!("path does not exist: `{raw_path}`"),
            ));
        }

        let ctx = Arc::clone(&self.ctx);
        let root = search_root.clone();
        let blocking_ctx = call_ctx.clone();
        let scanned = tokio::task::spawn_blocking(move || {
            scan_text_blocking(
                &ctx,
                &root,
                &query,
                is_regex,
                case_insensitive,
                &include,
                context,
                limit,
                &blocking_ctx,
            )
        })
        .await
        .map_err(|err| ToolError::execution(self.name(), format!("search worker failed: {err}")))?;
        let scanned = scanned.map_err(|err| ToolError::execution(self.name(), err))?;

        if scanned.is_empty() {
            Ok(ToolOutput::success("No matching lines found.".to_string()))
        } else {
            Ok(ToolOutput::Matches {
                pattern: args.query,
                lines: scanned,
            })
        }
    }
}

/// Maximum characters emitted per matched/context line in [`SearchTextTool`].
/// Prevents minified assets or single-line data dumps from blowing past the
/// transcript budget or hitting the universal spooler on a single match.
const MAX_SEARCH_LINE_CHARS: usize = 500;
/// Maximum cumulative payload bytes emitted by [`SearchTextTool`] before
/// stopping early with a truncation marker (kept under the 64 KB spooler).
const MAX_SEARCH_BYTES: usize = 48 * 1024;
/// Largest file the lexical search will read. A generated bundle or a database
/// dump is not a source file; reading it whole would spend memory and time for
/// matches no one wants, and `search_text` has no ripgrep-style streaming
/// reader. Oversized files are skipped rather than partially scanned.
const MAX_SEARCH_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Blocking text scan used by [`SearchTextTool`]. Runs on the blocking pool so
/// a large tree never stalls the async runtime, and keeps the whole match set
/// in one pass so a context window cannot re-read a file.
#[allow(clippy::too_many_arguments)]
fn scan_text_blocking(
    ctx: &SystemToolContext,
    root: &Path,
    query: &str,
    is_regex: bool,
    case_insensitive: bool,
    include: &[String],
    context: usize,
    limit: usize,
    call_ctx: &ToolContext,
) -> Result<Vec<String>, String> {
    let pattern = if is_regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    let matcher = regex::RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|err| format!("invalid regex pattern: {err}"))?;

    let include_filter = walker::PathFilter::compile(root, include, &[])?;
    let walker = walker::build_file_walker(root, &[], None, false)?.build();
    let mut matches = Vec::new();
    let mut total_bytes = 0usize;
    let mut emitted_hits = 0usize;
    'files: for (seen, entry) in walker.flatten().enumerate() {
        if seen % 128 == 0 && call_ctx.is_cancelled() {
            return Err("tool `search_text` execution was cancelled".to_string());
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let file_path = entry.path();
        let relative = display_match_path(file_path, root, &ctx.workspace_root);
        if let Some(filter) = &include_filter
            && !filter.matches(Path::new(&relative), false)
        {
            continue;
        }
        if entry
            .metadata()
            .map(|metadata| metadata.len() > MAX_SEARCH_FILE_BYTES)
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(content) = fs::read_to_string(file_path) else {
            continue;
        };
        let lines = content.lines().collect::<Vec<_>>();
        let hits = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| matcher.is_match(line).then_some(index))
            .collect::<Vec<_>>();
        if hits.is_empty() {
            continue;
        }

        let mut last_emitted = None;
        for &hit in &hits {
            if emitted_hits >= limit {
                break 'files;
            }
            let lo = hit.saturating_sub(context);
            let hi = hit
                .saturating_add(context)
                .min(lines.len().saturating_sub(1));
            for (index, line) in lines.iter().enumerate().take(hi + 1).skip(lo) {
                if last_emitted.is_some_and(|last| index <= last) {
                    continue;
                }
                let separator = if index == hit { ':' } else { '-' };
                let clamped = read::clamp_line_to(line.trim(), MAX_SEARCH_LINE_CHARS);
                let formatted = format!("{relative}{separator}{}{separator} {clamped}", index + 1);
                let next_bytes = total_bytes + formatted.len() + 1;
                if !matches.is_empty() && next_bytes > MAX_SEARCH_BYTES {
                    matches.push(format!(
                        "... (search output truncated at {MAX_SEARCH_BYTES} bytes; narrow path, include filter, or limit)"
                    ));
                    break 'files;
                }
                total_bytes = next_bytes;
                matches.push(formatted);
                last_emitted = Some(index);
            }
            emitted_hits += 1;
        }
    }
    Ok(matches)
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
