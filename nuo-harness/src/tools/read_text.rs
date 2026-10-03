use async_trait::async_trait;
use nuo_wire::Tool;
use nuo_tool::ToolSchema;
use serde_json::json;

use serde::Deserialize;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, resolve_workspace_path, workspace_base,
};

/// Typed parameters for [`ReadTextTool`].
#[derive(ToolSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    #[tool(desc = "Path to the text file; relative paths use the primary workspace")]
    path: String,
    #[tool(desc = "1-based line number to start reading from (default 1)")]
    offset: Option<i64>,
    #[tool(desc = "Maximum number of lines to read")]
    limit: Option<i64>,
}

/// Read a text file from disk.
///
/// Relative paths resolve against the session's workspace root (captured at
/// factory time), not the daemon process's cwd — under the unified daemon
/// (ADR-0096) those differ whenever the daemon was first spawned from another
/// project.
pub struct ReadTextTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>>,
}

impl ReadTextTool {
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

#[async_trait]
impl Tool for ReadTextTool {
    fn name(&self) -> &str {
        "read_text"
    }
    fn description(&self) -> &str {
        "Read a text file with line numbers. Supports offset (1-based start line) and limit (max lines). Large files are paginated."
    }
    fn parameters(&self) -> serde_json::Value {
        // Schema derived from the typed `ReadArgs` struct — no hand-written
        // JSON to drift. See `ReadArgs` / `nuo_tool::ToolSchema`.
        ReadArgs::parameters_schema()
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<nuo_wire::ToolOutput, String> {
        let args: ReadArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let path = &args.path;
        // Filesystem access goes through the workspace-resolved path; the
        // model-facing `path` text (errors, framing, display) stays exactly
        // what the model sent.
        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        let resolved = resolve_workspace_path(&self.root, path);

        // Reject directories with an explicit, actionable message instead of
        // the raw OS "Is a directory (os error 21)". A model that sees the OS
        // error cannot infer it should switch to `list_dir`, and may re-read
        // the same directory in a loop. This mirrors the empty/EOF guidance
        // pattern: a clear reason breaks the loop.
        if env.fs().is_dir(&resolved).await {
            return Err(format!(
                "'{}' is a directory, not a file. Use the `list_dir` tool to see its contents.",
                path
            ));
        }

        let lang = std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_string());

        // Binary rejection happens *before* reading the whole file. The old
        // code did `std::fs::read(path)` (loading the entire file into memory)
        // and only then sniffed the first 4 KB — so a multi-gigabyte binary was
        // fully read just to be refused. Now:
        //   1. A known binary extension refuses immediately (no read at all).
        //   2. Otherwise read only the leading 4 KB to sniff for NUL/control
        //      bytes; only if that looks textual do we read the full file.
        if is_binary_extension(path) {
            return Err(format!("Cannot read binary file: {}", path));
        }

        let bytes = env
            .fs()
            .read(&resolved)
            .await
            .map_err(|e| format!("Failed to read '{}': {}", path, e))?;

        let sniff_len = bytes.len().min(4096);
        if is_binary_content(&bytes[..sniff_len]) {
            return Err(format!("Cannot read binary file: {}", path));
        }

        let content =
            String::from_utf8(bytes).map_err(|_| format!("File '{}' is not valid UTF-8", path))?;

        let offset = args.offset.unwrap_or(1).max(1) as usize;
        let limit = args.limit.unwrap_or(0).max(0) as usize;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        // Empty file / offset past EOF: nothing to show. Surface an explicit,
        // machine-actionable note so the model does NOT re-read in a loop
        // wondering whether the call failed. `text` stays empty (the renderer
        // draws nothing) and the note explains why via `to_text()`.
        let start = offset - 1;
        if total_lines == 0 {
            return Ok(nuo_wire::ToolOutput::Code {
                lang,
                text: String::new(),
                start_line: offset,
                prefix: Some(format!("[{}: empty file]", path)),
                suffix: None,
            });
        }
        if start >= total_lines {
            return Ok(nuo_wire::ToolOutput::Code {
                lang,
                text: String::new(),
                start_line: offset,
                prefix: Some(format!(
                    "[{}: offset {} is past end of file ({} line{})]",
                    path,
                    offset,
                    total_lines,
                    if total_lines == 1 { "" } else { "s" }
                )),
                suffix: None,
            });
        }

        // Requested window [start, requested_end): offset..offset+limit
        // (limit 0 = to EOF). We then snap this to a byte budget AT LINE
        // BOUNDARIES, which is what makes pagination deterministic and
        // loop-safe: every read returns whole lines plus a concrete
        // continuation offset, so the model can always compute the next
        // `offset` and can never get stuck re-truncating the same window.
        // The first line is always included even if it alone exceeds the
        // budget, so a read always makes forward progress.
        const READ_BUDGET_BYTES: usize = 50_000;
        const MAX_LINE_LENGTH: usize = 2000;
        const MAX_LINE_SUFFIX: &str = "... (line truncated)";
        let requested_end = if limit > 0 {
            (start + limit).min(total_lines)
        } else {
            total_lines
        };
        // Pre-compute truncated lines so cost reflects what we actually return.
        let truncated_lines: Vec<String> = lines[start..requested_end]
            .iter()
            .map(|line| {
                if line.len() > MAX_LINE_LENGTH {
                    let truncated: String = line.chars().take(MAX_LINE_LENGTH).collect();
                    format!("{}{}", truncated, MAX_LINE_SUFFIX)
                } else {
                    line.to_string()
                }
            })
            .collect();
        let mut used = 0usize;
        let mut shown_end = start; // exclusive index into `lines`
        for (idx, truncated) in truncated_lines.iter().enumerate() {
            let i = start + idx;
            let cost = truncated.len() + 1; // +1 for the '\n' we rejoin with
            if i > start && used + cost > READ_BUDGET_BYTES {
                break;
            }
            used += cost;
            shown_end = i + 1;
        }

        let shown_count = shown_end - start;
        let text = truncated_lines[..shown_count].join("\n");
        // 1-based range of what we actually returned.
        let first_line = offset; // == start + 1
        let last_line = shown_end; // exclusive 0-based index → 1-based last
        let more_remain = shown_end < total_lines;

        // Model-facing framing. Omitted entirely for the plain "read whole
        // small file from line 1" case (zero overhead, byte-identical to the
        // legacy model output); added whenever position or pagination matters
        // so the model always knows where it is and how to continue. The
        // renderer ignores prefix/suffix and gutter-numbers `text`.
        let (prefix, suffix) = if offset == 1 && !more_remain {
            (None, None)
        } else {
            let header = format!(
                "[{}: lines {}-{} of {}{}]",
                path,
                first_line,
                last_line,
                total_lines,
                if more_remain { "" } else { " (end of file)" }
            );
            let suffix = if more_remain {
                let remaining = total_lines - shown_end;
                Some(format!(
                    "[{} more line{} below — read {} with offset={}]",
                    remaining,
                    if remaining == 1 { "" } else { "s" },
                    path,
                    shown_end + 1
                ))
            } else {
                None
            };
            (Some(header), suffix)
        };

        Ok(nuo_wire::ToolOutput::Code {
            lang,
            text,
            start_line: offset,
            prefix,
            suffix,
        })
    }
}
nuo_wire::register_tool!(ReadTextFactory => |ctx| ReadTextTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

/// The terse `read_text` variant: same capability name and identical behaviour
/// (it delegates execution to [`ReadTextTool`]), but a stripped-down,
/// instruction-light description and schema. Selected by the **model** under
/// `[tool_variants."<model-id>"] read_text = "terse"` for models that follow a
/// tight contract better than verbose usage guidance. An subagent on such a
/// model inherits this choice automatically (variant is the model's axis, not
/// the profile's). This is the concrete demonstration that a capability can
/// offer a genuinely different *presentation* of the same tool rather than a
/// re-worded copy patched in at schema-build time.
///
/// Delegates execution to [`ReadTextTool`], forwarding its own captured
/// workspace root so both variants resolve paths identically.
pub struct ReadTextTerseTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>>,
}

#[async_trait]
impl Tool for ReadTextTerseTool {
    fn name(&self) -> &str {
        "read_text"
    }
    fn variant(&self) -> &str {
        "terse"
    }
    fn description(&self) -> &str {
        "Read a text file; lines are prefixed with line numbers. Optional \
         `offset` (1-based) and `limit`. Large reads paginate and report the \
         next `offset`."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer" },
                "limit": { "type": "integer" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        ReadTextTool {
            root: self.root.clone(),
            env: self.env.clone(),
        }
        .call(arguments)
        .await
    }
    async fn call_structured(&self, arguments: &str) -> Result<nuo_wire::ToolOutput, String> {
        ReadTextTool {
            root: self.root.clone(),
            env: self.env.clone(),
        }
        .call_structured(arguments)
        .await
    }
}
nuo_wire::register_tool!(ReadTextTerseFactory => |ctx| ReadTextTerseTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

/// Extensions that are always treated as binary and never read as text.
const BINARY_EXTENSIONS: &[&str] = &[
    "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib", "o", "a", "lib",
    "class", "jar", "war", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "bin",
    "dat", "obj", "wasm", "pyc", "pyo", "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff",
    "tif", "mp3", "mp4", "avi", "mov", "mkv", "flv", "wav", "flac", "ogg", "pdf", "sqlite", "db",
    "mdb",
];

fn is_binary_extension(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| BINARY_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_binary_content(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&0) {
        return true;
    }
    let non_printable = bytes
        .iter()
        .filter(|&&b| b < 9 || (b > 13 && b < 32))
        .count();
    non_printable as f64 / bytes.len() as f64 > 0.3
}
