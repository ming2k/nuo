//! Unified filesystem discovery and directory listing tool (`find_files`).
//!
//! Covers shallow directory inspection (`max_depth=1`, replacing `ls`),
//! multi-level tree previews (`max_depth=2`), and recursive glob searches
//! (`**/*.{ts,tsx}`) over one `ignore::WalkBuilder` + `OverrideBuilder` engine.

use std::cmp::Reverse;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use nuo_tool::{
    BuiltinTool, RiskProfile, Tool, ToolAccesses, ToolContext, ToolError, ToolOutput, ToolSchema,
    ToolScope,
};
use serde::Deserialize;
use serde_json::Value;

use super::walker::{PathFilter, build_file_walker};
use super::{SystemToolContext, display_match_path, path_argument};

const DEFAULT_FIND_LIMIT: usize = 200;
const MAX_FIND_LIMIT: usize = 1_000;
const MAX_FIND_SCAN_ENTRIES: usize = 25_000;

/// Entry classes returned by [`FindFilesTool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Files,
    Dirs,
    All,
}

impl EntryKind {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "files" => Ok(Self::Files),
            "dirs" => Ok(Self::Dirs),
            "all" => Ok(Self::All),
            other => Err(format!(
                "invalid `kind` '{other}'; expected 'files', 'dirs', or 'all'"
            )),
        }
    }

    fn matches(self, is_dir: bool, is_file: bool, is_symlink: bool) -> bool {
        match self {
            Self::Files => is_file,
            Self::Dirs => is_dir,
            Self::All => is_dir || is_file || is_symlink,
        }
    }
}

/// Ordering applied to [`FindFilesTool`] matches before capping at `limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortOrder {
    Mtime,
    Path,
}

impl SortOrder {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mtime" => Ok(Self::Mtime),
            "path" => Ok(Self::Path),
            other => Err(format!(
                "invalid `sort` '{other}'; expected 'mtime' or 'path'"
            )),
        }
    }
}

/// Typed parameters for [`FindFilesTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct FindFilesArgs {
    #[tool(desc = "Directory to search (default '.')")]
    pub path: Option<String>,
    #[serde(default, alias = "name", alias = "glob", alias = "query")]
    #[tool(
        desc = "Glob pattern (supports '*.rs', '**/*.{ts,tsx}', 'src/**'); omit to list entries"
    )]
    pub pattern: Option<String>,
    #[serde(default, alias = "globs", alias = "includes")]
    #[tool(desc = "Multiple glob patterns to match")]
    pub patterns: Option<Vec<String>>,
    #[tool(desc = "Exclude glob patterns (e.g. ['**/fixtures/**'])")]
    pub exclude: Option<Vec<String>>,
    #[tool(
        desc = "Max directory depth relative to path (1 = immediate children like ls, 2 = tree; omit for recursive)"
    )]
    pub max_depth: Option<usize>,
    #[tool(
        desc = "Entry types: 'files' (default for recursive), 'dirs', or 'all' (default when max_depth=1 without pattern)",
        choices = ["files", "dirs", "all"]
    )]
    pub kind: Option<String>,
    #[tool(
        desc = "Include gitignored and default-ignored directories such as target/ or build/ (default false)"
    )]
    pub include_ignored: Option<bool>,
    #[tool(
        desc = "Sort order: 'mtime' (newest first, default for recursive) or 'path' (default when max_depth is set)",
        choices = ["mtime", "path"]
    )]
    pub sort: Option<String>,
    #[tool(desc = "Max results to return (default 200, max 1000)")]
    pub limit: Option<usize>,
}

/// Discovers files and directories matching glob, depth, and visibility filters.
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

    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::FindFiles.aliases()
    }

    fn description(&self) -> &str {
        "List directories or find files by glob pattern and depth, respecting ignore rules. \
         Use max_depth=1 to list a directory (instead of shell ls)."
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
        let args =
            parse_find_args(arguments).map_err(|msg| ToolError::execution(self.name(), msg))?;

        let raw_path = args.path.as_deref().unwrap_or(".");
        let search_root = self.ctx.resolve_path(raw_path);
        if !search_root.exists() {
            return Err(ToolError::execution(
                self.name(),
                format!("path does not exist: `{raw_path}`"),
            ));
        }
        if !search_root.is_dir() {
            return Err(ToolError::execution(
                self.name(),
                format!("`{raw_path}` is not a directory; use `read` to read a file"),
            ));
        }

        let plan = build_find_plan(&args).map_err(|msg| ToolError::execution(self.name(), msg))?;
        let workspace_root = self.ctx.workspace_root.clone();
        let blocking_ctx = call_ctx.clone();
        let raw_path_owned = raw_path.to_string();

        tokio::task::spawn_blocking(move || {
            run_find_blocking(
                &search_root,
                &workspace_root,
                &raw_path_owned,
                &plan,
                &blocking_ctx,
            )
        })
        .await
        .map_err(|error| ToolError::execution(self.name(), format!("find worker failed: {error}")))?
        .map_err(|error| ToolError::execution(self.name(), error))
    }
}

struct FindPlan {
    include: Vec<String>,
    exclude: Vec<String>,
    max_depth: Option<usize>,
    kind: EntryKind,
    include_ignored: bool,
    sort: SortOrder,
    limit: usize,
}

fn parse_find_args(mut arguments: Value) -> Result<FindFilesArgs, String> {
    if let Some(obj) = arguments.as_object_mut()
        && let Some(p) = obj.get("patterns").and_then(|v| v.as_str())
    {
        let p_str = p.to_string();
        obj.insert("patterns".to_string(), serde_json::json!([p_str]));
    }
    serde_json::from_value(arguments).map_err(|err| format!("invalid arguments: {err}"))
}

fn build_find_plan(args: &FindFilesArgs) -> Result<FindPlan, String> {
    if args.max_depth == Some(0) {
        return Err("`max_depth` must be at least 1".to_string());
    }
    let mut raw_patterns = args.patterns.clone().unwrap_or_default();
    if let Some(p) = &args.pattern
        && !raw_patterns.contains(p)
    {
        raw_patterns.push(p.clone());
    }
    let include: Vec<String> = raw_patterns
        .into_iter()
        .filter(|pat| !pat.is_empty() && pat != "*" && pat != "**/*" && pat != "**")
        .collect();
    let exclude = args.exclude.clone().unwrap_or_default();

    let kind = match args.kind.as_deref() {
        Some(raw) => EntryKind::parse(raw)?,
        None if args.max_depth == Some(1) && include.is_empty() => EntryKind::All,
        None => EntryKind::Files,
    };
    let sort = match args.sort.as_deref() {
        Some(raw) => SortOrder::parse(raw)?,
        None if args.max_depth.is_some() => SortOrder::Path,
        None => SortOrder::Mtime,
    };
    let limit = args
        .limit
        .unwrap_or(DEFAULT_FIND_LIMIT)
        .clamp(1, MAX_FIND_LIMIT);

    Ok(FindPlan {
        include,
        exclude,
        max_depth: args.max_depth,
        kind,
        include_ignored: args.include_ignored.unwrap_or(false),
        sort,
        limit,
    })
}

struct FoundEntry {
    display: String,
    is_dir: bool,
    modified: SystemTime,
}

fn run_find_blocking(
    search_root: &Path,
    workspace_root: &Path,
    raw_path: &str,
    plan: &FindPlan,
    call_ctx: &ToolContext,
) -> Result<ToolOutput, String> {
    let filter = PathFilter::compile(search_root, &plan.include, &plan.exclude)?;
    let walker = build_file_walker(
        search_root,
        &plan.exclude,
        plan.max_depth,
        plan.include_ignored,
    )?
    .build();

    let fast_path_cap =
        (plan.sort == SortOrder::Path && plan.max_depth != Some(1)).then_some(plan.limit + 1);
    let scan_limit = fast_path_cap.unwrap_or(MAX_FIND_SCAN_ENTRIES + 1);

    let mut entries = Vec::new();
    let mut scan_capped = false;

    for (seen, entry) in walker.flatten().enumerate() {
        if seen % 256 == 0 && call_ctx.is_cancelled() {
            return Err("tool `find_files` execution was cancelled".to_string());
        }
        if entry.depth() == 0 {
            continue;
        }
        let Some(ft) = entry.file_type() else {
            continue;
        };
        let is_dir = ft.is_dir();
        let is_file = ft.is_file();
        let is_symlink = ft.is_symlink();
        if !plan.kind.matches(is_dir, is_file, is_symlink) {
            continue;
        }

        let path = entry.path();
        let relative = display_match_path(path, search_root, workspace_root);
        if let Some(filter) = &filter
            && !filter.matches(Path::new(&relative), is_dir)
        {
            continue;
        }

        let display = if is_dir {
            format!("{relative}/")
        } else if is_symlink {
            format!("{relative}@")
        } else {
            relative
        };

        let modified = if plan.sort == SortOrder::Mtime {
            entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        } else {
            SystemTime::UNIX_EPOCH
        };

        entries.push(FoundEntry {
            display,
            is_dir,
            modified,
        });

        if entries.len() >= scan_limit {
            scan_capped = true;
            break;
        }
    }

    if entries.is_empty() {
        return Ok(ToolOutput::Listing {
            entries: Vec::new(),
            total: Some(0),
            truncated: false,
            hint: Some(format!("No matching entries found under `{raw_path}`.")),
        });
    }

    let capped_by_fast_path = scan_capped && fast_path_cap.is_some();
    let total = if scan_capped {
        None
    } else {
        Some(entries.len())
    };
    if scan_capped && entries.len() > plan.limit {
        entries.truncate(plan.limit);
    }

    match plan.sort {
        SortOrder::Mtime => {
            entries.sort_by(|left, right| {
                Reverse(left.modified)
                    .cmp(&Reverse(right.modified))
                    .then_with(|| left.display.cmp(&right.display))
            });
        }
        SortOrder::Path if plan.max_depth == Some(1) => {
            entries.sort_by(|left, right| {
                (!left.is_dir)
                    .cmp(&!right.is_dir)
                    .then_with(|| {
                        left.display
                            .to_lowercase()
                            .cmp(&right.display.to_lowercase())
                    })
                    .then_with(|| left.display.cmp(&right.display))
            });
        }
        SortOrder::Path => {
            entries.sort_by(|left, right| left.display.cmp(&right.display));
        }
    }

    let truncated_by_limit = total.is_some_and(|count| count > plan.limit);
    let truncated = scan_capped || truncated_by_limit;
    if entries.len() > plan.limit {
        entries.truncate(plan.limit);
    }

    let hint = if capped_by_fast_path || truncated_by_limit {
        Some(format!(
            "Truncated at limit={} — narrow 'path', add 'pattern', or set 'max_depth'",
            plan.limit
        ))
    } else if scan_capped {
        Some(format!(
            "Scan cap ({MAX_FIND_SCAN_ENTRIES} entries) reached — narrow 'path', add 'pattern', or set 'max_depth'"
        ))
    } else {
        None
    };

    Ok(ToolOutput::Listing {
        entries: entries.into_iter().map(|entry| entry.display).collect(),
        total,
        truncated,
        hint,
    })
}
