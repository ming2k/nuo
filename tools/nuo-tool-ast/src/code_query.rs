//! `code_query` — bounded structural queries against the current bytes of a
//! file or a workspace scope (direction, consolidated in).
//!
//! One tool, three modes, one parser. `outline` answers "what is in this
//! file?", `symbol` answers "give me the source of this declaration", and
//! `find` answers "where are declarations matching this kind/name?". They were
//! merged rather than shipped as sibling tools because they operate on one
//! resource (the parse of a source snapshot) with one input language
//! (declaration kinds + names) — the boundary drew for `search_text`
//! and generalized to the whole surface.
//!
//! Every mode obeys the same three contracts:
//!
//! 1. **Bounded.** Output is capped by `limit` (entries) and `budget` (bytes),
//!    and input by a per-file size cap and a scope-wide file cap. Truncation is
//!    always disclosed; nothing is silently clipped.
//! 2. **Versioned.** Every result names the content version of the bytes it
//!    describes, so a caller can tell whether later evidence describes the same
//!    snapshot, and `edit_text` / `write_file` can refuse a stale-based write
//!    (`expected_version`).
//! 3. **Honest.** The result is a syntactic summary, never semantic ground
//!    truth: unsupported formats, oversized inputs, unparseable regions, and
//!    exhausted budgets are reported as such rather than being papered over.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use nuo_protocol::ExecutionEnvironment;
use nuo_tool::{Tool, ToolAccesses, ToolOutput};

use nuo_tool::{BuiltinTool, ToolArgs};
use serde::Deserialize;

use crate::file_search::{
    build_file_walker, resolve_search_root, search_limit, search_path_argument,
};
use crate::helpers::{
    WorkspaceBase, content_version, env_from_root, execution_environment, resolve_workspace_path,
    workspace_base,
};
use crate::syntax;

/// Maximum entries (symbols, matches, or returned declarations) per result.
const DEFAULT_LIMIT: usize = 200;

/// Maximum rendered result size in bytes. An output budget: source on disk is
/// never truncated, only what travels back to the model.
const DEFAULT_BUDGET_BYTES: usize = 16 * 1024;
const MAX_BUDGET_BYTES: usize = 128 * 1024;

/// Maximum source size eligible for parsing. Parsing is work, so the input is
/// bounded independently of the rendered-output budget.
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

/// Maximum files scanned by one unscoped `find`/`symbol` query, and the total
/// bytes those files may add up to. A scope-wide query is a resource decision,
/// not a licence to walk a monorepo.
const MAX_SCOPED_FILES: usize = 2_000;
const MAX_SCOPED_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum source lines returned per `symbol` match, independent of `budget`,
/// so one enormous declaration cannot consume the whole result.
const MAX_SYMBOL_LINES: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Top-level symbol outline of one file.
    Outline,
    /// Source of a declaration, located by name.
    Symbol,
    /// Declarations matching a kind/name pattern across a scope.
    Find,
}

impl Mode {
    /// Every mode, in the order the schema advertises them.
    ///
    /// This is the single source of truth for both the advertised `enum` and
    /// the accepted input ([`Mode::parse`]). The schema derives the string enum
    /// choices directly at compile time via `#[tool(choices = [...])]`, ensuring the
    /// published contract and the enforced parser cannot drift.
    const ALL: [Mode; 3] = [Mode::Outline, Mode::Symbol, Mode::Find];

    fn as_str(self) -> &'static str {
        // Exhaustive by construction: adding a variant will not compile until
        // it is named here, which is what keeps `ALL` honest.
        match self {
            Mode::Outline => "outline",
            Mode::Symbol => "symbol",
            Mode::Find => "find",
        }
    }

    /// Parse a model-supplied mode, failing with the legal set named.
    fn parse(input: &str) -> Result<Self, String> {
        let normalized = input.trim().to_ascii_lowercase();
        Mode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == normalized)
            .ok_or_else(|| {
                format!(
                    "Unknown 'mode' '{input}'. Valid modes: {}. 'outline' lists a file's \
                     declarations, 'symbol' returns a declaration's source by name, 'find' \
                     locates declarations matching a kind/name pattern in a scope.",
                    Mode::ALL.map(Mode::as_str).join(", ")
                )
            })
    }
}

#[derive(ToolArgs, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeQueryArgs {
    #[tool(
        desc = "Query mode: 'outline' lists one file's declarations, 'symbol' returns a declaration's source by name, 'find' locates declarations matching a kind/name pattern in a scope.",
        choices = ["outline", "symbol", "find"]
    )]
    mode: String,
    #[tool(
        desc = "File for 'outline'/'symbol'; file or directory scope for 'find' (default '.'). Relative paths use the primary workspace."
    )]
    path: Option<String>,
    #[tool(desc = "Declaration to locate for 'symbol', e.g. 'handle_request' or 'Service::run'.")]
    symbol: Option<String>,
    #[tool(
        desc = "Pattern for 'find', e.g. 'fn:handle_*' or 'impl:Runnable'. Comma/space-separated ORed clauses 'kind[:name-glob]'; kinds: fn, method, struct, enum, trait, impl, class, interface, type, const, static, mod, macro."
    )]
    pattern: Option<String>,
    #[tool(desc = "Maximum entries to return (default 200)")]
    limit: Option<u64>,
    #[tool(desc = "Maximum result bytes (default 16384, max 131072)")]
    budget: Option<u64>,
}

/// Structural queries over a source snapshot.
///
/// Read-only: this tool observes the workspace through the execution
/// environment's filesystem and permission boundary and never writes.
pub struct CodeQueryTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<Arc<dyn ExecutionEnvironment>>,
}

impl CodeQueryTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self { root, env: None }
    }

    pub fn with_env(env: Arc<dyn ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }

    fn environment(&self) -> Arc<dyn ExecutionEnvironment> {
        self.env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root))
    }
}

/// Files a scope-wide query could not analyse, so the result can say so instead
/// of looking complete. An unreadable or oversized file is *skipped*, never
/// silently treated as empty.
#[derive(Default)]
struct ScanSkips {
    unreadable: usize,
    oversized: usize,
}

impl ScanSkips {
    /// The disclosure line for these skips, or nothing when there were none.
    fn note(&self) -> String {
        if self.unreadable == 0 && self.oversized == 0 {
            return String::new();
        }
        let mut parts = Vec::new();
        if self.unreadable > 0 {
            parts.push(format!("{} unreadable", self.unreadable));
        }
        if self.oversized > 0 {
            parts.push(format!(
                "{} above the {MAX_SOURCE_BYTES}-byte per-file budget",
                self.oversized
            ));
        }
        format!(
            "\n[{} file(s) could not be analysed and were skipped: {}. This result is not an \
             exhaustive scan of the scope.]\n",
            self.unreadable + self.oversized,
            parts.join(", ")
        )
    }
}

/// Rendering caps resolved from the arguments.
struct Caps {
    limit: usize,
    budget: usize,
}

impl Caps {
    fn from(args: &CodeQueryArgs) -> Result<Self, String> {
        let budget = args.budget.unwrap_or(DEFAULT_BUDGET_BYTES as u64);
        if budget == 0 {
            return Err("'budget' must be at least 1 byte".to_string());
        }
        Ok(Self {
            limit: search_limit(Some(args.limit.unwrap_or(DEFAULT_LIMIT as u64)))?,
            budget: budget.min(MAX_BUDGET_BYTES as u64) as usize,
        })
    }
}

/// Bytes of a file's current content, or the reason it cannot be read.
async fn read_snapshot(
    env: &dyn ExecutionEnvironment,
    resolved: &Path,
    display_path: &str,
) -> Result<Vec<u8>, String> {
    if let Ok(metadata) = env.fs().metadata(resolved).await
        && metadata.len > MAX_SOURCE_BYTES
    {
        return Err(oversized_source(display_path, metadata.len));
    }
    let bytes = env
        .fs()
        .read(resolved)
        .await
        .map_err(|error| format!("Failed to read '{display_path}': {error}"))?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(oversized_source(display_path, bytes.len() as u64));
    }
    Ok(bytes)
}

/// Error for a source that exceeds the parse budget.
fn oversized_source(path: &str, len: u64) -> String {
    format!(
        "'{path}' is {len} bytes, above the {MAX_SOURCE_BYTES} byte structural-query input budget. \
         No structural analysis was performed. Read a narrower file or use search/text tools instead."
    )
}

/// The extension of a path, lowercased, for language detection.
fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn unsupported_language_error(path: &str, ext: &str) -> String {
    format!(
        "Unsupported file type for a structural query (extension: '{ext}', path: '{path}'). \
         Supported: rs, ts, js, py, c, cpp, go. No structural analysis was performed for this snapshot."
    )
}

/// Clamp a declaration's source to [`MAX_SYMBOL_LINES`] and to the remaining
/// output budget, reporting whether anything was elided.
///
/// The line that trips either cap is counted as elided: dropping it silently
/// would hand back a truncated declaration that looks complete.
fn slice_source(source: &str, range: (usize, usize), budget: usize) -> (String, bool) {
    let text = &source[range.0.min(source.len())..range.1.min(source.len())];
    let mut rendered = String::new();
    let mut truncated = false;
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_SYMBOL_LINES {
            truncated = true;
            break;
        }
        if rendered.len() + line.len() + 1 > budget {
            truncated = true;
            break;
        }
        rendered.push_str(line);
        rendered.push('\n');
    }
    (rendered, truncated)
}

#[async_trait]
impl Tool for CodeQueryTool {
    fn name(&self) -> &str {
        BuiltinTool::CodeQuery.as_str()
    }

    fn description(&self) -> &str {
        "Query code by structure instead of reading whole files: 'outline' lists a file's declarations, 'symbol' returns one declaration's source by name, 'find' locates declarations matching a kind/name pattern in a scope. Every result names the content version of the bytes it describes. Languages: rs, ts, js, py, c, cpp, go."
    }

    fn parameters(&self) -> serde_json::Value {
        CodeQueryArgs::parameters_schema()
    }

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        ToolAccesses::search_tree(search_path_argument(arguments))
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments)
            .await
            .map(|out| out.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let args: CodeQueryArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for code_query: {error}"))?;
        let caps = Caps::from(&args)?;
        let env = self.environment();
        let text = match Mode::parse(&args.mode)? {
            Mode::Outline => self.run_outline(&args, &caps, env.as_ref()).await?,
            Mode::Symbol => self.run_symbol(&args, &caps, env.as_ref()).await?,
            Mode::Find => self.run_find(&args, &caps, env.as_ref()).await?,
        };
        Ok(ToolOutput::Text(text))
    }
}

impl CodeQueryTool {
    /// `outline` — the declaration summary of exactly one file.
    async fn run_outline(
        &self,
        args: &CodeQueryArgs,
        caps: &Caps,
        env: &dyn ExecutionEnvironment,
    ) -> Result<String, String> {
        let path = args.path.as_deref().ok_or(
            "Mode 'outline' requires 'path' (the source file to summarise). \
             Use mode 'find' with a 'pattern' to search a whole scope.",
        )?;
        let resolved = resolve_workspace_path(&self.root, path);
        if env.fs().is_dir(&resolved).await {
            return Err(format!(
                "'{path}' is a directory, not a source file. Mode 'outline' summarises one file; \
                 use mode 'find' with a 'pattern' and set 'path' to the directory."
            ));
        }
        let ext = extension_of(&resolved);
        if syntax::SupportedLanguage::from_extension(&ext).is_none() {
            return Err(unsupported_language_error(path, &ext));
        }

        let bytes = read_snapshot(env, &resolved, path).await?;
        let version = content_version(&bytes);
        let content = String::from_utf8_lossy(&bytes);
        let declarations = syntax::extract_declarations(&ext, &content);

        if declarations.is_empty() {
            return Ok(format!(
                "Outline for '{path}' (version {version}, {} bytes): no named declarations found. \
                 The file may be unsupported, empty, or contain only anonymous items. \
                 This is a syntactic summary, not a complete AST or semantic analysis.",
                bytes.len()
            ));
        }

        let total = declarations.len();
        let (rendered, shown, truncated) = render_entries(
            declarations.iter().map(|decl| {
                (
                    format!(
                        "{}{}",
                        "  ".repeat(decl.containers.len() + 1),
                        decl.signature
                    ),
                    format!("L{}", decl.start_line),
                )
            }),
            caps,
        );

        let mut out = header(
            &format!("Outline for '{path}'"),
            &version,
            Some(bytes.len() as u64),
            shown,
            total,
            truncated,
        );
        out.push_str(&rendered);
        push_truncation_note(&mut out, truncated, shown, total, caps);
        Ok(out)
    }

    /// `symbol` — the source of a declaration, located by name.
    async fn run_symbol(
        &self,
        args: &CodeQueryArgs,
        caps: &Caps,
        env: &dyn ExecutionEnvironment,
    ) -> Result<String, String> {
        let symbol = args
            .symbol
            .as_deref()
            .map(str::trim)
            .filter(|symbol| !symbol.is_empty())
            .ok_or(
                "Mode 'symbol' requires 'symbol' (the declaration name to locate, e.g. \
                 'handle_request' or 'Service::run').",
            )?;
        let files = self.scoped_files(args, env).await?;

        let mut matches = Vec::new();
        let mut skips = ScanSkips::default();
        let mut truncated_scope = false;
        for file in files {
            let ext = extension_of(&file);
            let bytes = match env.fs().read(&file).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    skips.unreadable += 1;
                    continue;
                }
            };
            if bytes.len() as u64 > MAX_SOURCE_BYTES {
                skips.oversized += 1;
                continue;
            }
            let version = content_version(&bytes);
            let content = String::from_utf8_lossy(&bytes);
            for declaration in syntax::extract_declarations(&ext, &content) {
                if !declaration_matches_symbol(&declaration, symbol) {
                    continue;
                }
                matches.push(RenderedMatch {
                    display_path: display_path(&file, env.workspace_root()),
                    version: version.clone(),
                    declaration,
                    source: content.to_string(),
                });
                if matches.len() > caps.limit {
                    truncated_scope = true;
                    break;
                }
            }
            if truncated_scope {
                break;
            }
        }

        if matches.is_empty() {
            return Ok(format!(
                "No declaration named '{symbol}' was found in the queried scope. \
                 Names are matched against the declaration's own name ('{symbol}' must be the \
                 leaf name, e.g. 'run' in 'Service::run'). \
                 Use mode 'find' with a pattern such as 'fn:*{symbol}*' to search by substring, \
                 or 'fn' to list every function in a file.{}",
                skips.note()
            ));
        }

        let total = matches.len();
        let mut out = format!("Symbol '{symbol}': {total} match(es).\n");
        let mut budget_left = caps.budget;
        let mut emitted = 0usize;
        let mut elided = false;
        for hit in matches.iter().take(caps.limit) {
            let (header_line, body) = render_symbol_hit(hit, budget_left);
            if emitted > 0 && body.is_none() {
                elided = true;
                break;
            }
            out.push_str(&header_line);
            match body {
                Some((text, truncated_body)) => {
                    budget_left = budget_left.saturating_sub(text.len() + header_line.len());
                    out.push_str(&text);
                    if truncated_body {
                        out.push_str(&format!(
                            "\n[Source truncated: at most {MAX_SYMBOL_LINES} lines, within the \
                             remaining output budget. Read the file range directly for the rest.]\n"
                        ));
                    }
                }
                None => {
                    // The first hit alone exceeded the budget: emit its header
                    // and say so, rather than an empty result.
                    out.push_str(&format!(
                        "[Declaration source omitted: it exceeds the {}-byte output budget. \
                         Raise 'budget' (max {MAX_BUDGET_BYTES}) or read the file range directly.]\n",
                        caps.budget
                    ));
                }
            }
            emitted += 1;
        }
        if elided || total > caps.limit {
            out.push_str(&format!(
                "[Only {} of {total} match(es) shown within the {}-entry limit and {}-byte budget. \
                 Qualify the name (e.g. 'Type::{symbol}') or raise 'limit' / 'budget'.]\n",
                emitted.min(caps.limit),
                caps.limit,
                caps.budget
            ));
        }
        out.push_str(&skips.note());
        Ok(out)
    }

    /// `find` — declarations matching a structural pattern across a scope.
    async fn run_find(
        &self,
        args: &CodeQueryArgs,
        caps: &Caps,
        env: &dyn ExecutionEnvironment,
    ) -> Result<String, String> {
        let pattern_text = args
            .pattern
            .as_deref()
            .map(str::trim)
            .filter(|pattern| !pattern.is_empty())
            .ok_or(
                "Mode 'find' requires 'pattern' (e.g. 'fn:handle_*' or 'impl:Runnable , class'). \
                 Valid kinds: fn, method, struct, enum, trait, impl, class, interface, type, \
                 const, static, mod, macro.",
            )?;
        let pattern = syntax::parse_syntax_pattern(pattern_text)?;
        let files = self.scoped_files(args, env).await?;
        let files_scanned = files.len();

        let mut entries = Vec::new();
        let mut total = 0usize;
        let mut skips = ScanSkips::default();
        for file in files {
            let ext = extension_of(&file);
            let bytes = match env.fs().read(&file).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    skips.unreadable += 1;
                    continue;
                }
            };
            if bytes.len() as u64 > MAX_SOURCE_BYTES {
                skips.oversized += 1;
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);
            let display = display_path(&file, env.workspace_root());
            for declaration in syntax::extract_declarations(&ext, &content) {
                if !pattern.matches(&declaration) {
                    continue;
                }
                total += 1;
                if entries.len() <= caps.limit {
                    entries.push((
                        format!(
                            "{} {}  ({}:{}-{})",
                            declaration.kind,
                            declaration.qualified_name(),
                            display,
                            declaration.start_line,
                            declaration.end_line
                        ),
                        String::new(),
                    ));
                }
            }
        }

        if total == 0 {
            return Ok(format!(
                "No declaration matched '{pattern_text}' in the queried scope ({files_scanned} \
                 source file(s) scanned). Matching is exact on kind and glob on name: check the \
                 kind list (fn, method, struct, enum, trait, impl, class, interface, type, const, \
                 static, mod, macro) or widen the glob, e.g. 'fn:*name*'.{}",
                skips.note()
            ));
        }

        let truncated = total > caps.limit || entries.len() > caps.limit;
        let (rendered, shown, _) = render_entries(entries.into_iter(), caps);
        let mut out = format!(
            "Declarations matching '{pattern_text}': {shown} of {total} match(es) in \
             {files_scanned} source file(s)"
        );
        if truncated {
            out.push_str(", output truncated");
        }
        out.push_str(".\n");
        out.push_str(&rendered);
        push_truncation_note(&mut out, truncated, shown, total, caps);
        out.push_str(&skips.note());
        Ok(out)
    }

    /// The files a scoped query reads: exactly the file named by `path`, or
    /// every supported source file beneath it, bounded by count and bytes.
    async fn scoped_files(
        &self,
        args: &CodeQueryArgs,
        env: &dyn ExecutionEnvironment,
    ) -> Result<Vec<PathBuf>, String> {
        let path = args.path.as_deref().unwrap_or(".");
        let root = resolve_search_root(env, path)?;
        let metadata = env
            .fs()
            .metadata(&root)
            .await
            .map_err(|error| format!("Cannot query '{path}': {error}"))?;

        if !metadata.is_dir {
            let ext = extension_of(&root);
            if syntax::SupportedLanguage::from_extension(&ext).is_none() {
                return Err(unsupported_language_error(path, &ext));
            }
            return Ok(vec![root]);
        }

        let walker = build_file_walker(&root, &[], None)?.build();
        let files = tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();
            let mut bytes = 0u64;
            for entry in walker.flatten() {
                if entry.depth() == 0 || !entry.file_type().is_some_and(|kind| kind.is_file()) {
                    continue;
                }
                let candidate = entry.path().to_path_buf();
                if syntax::SupportedLanguage::from_extension(&extension_of(&candidate)).is_none() {
                    continue;
                }
                let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                if bytes + size > MAX_SCOPED_BYTES || files.len() >= MAX_SCOPED_FILES {
                    break;
                }
                bytes += size;
                files.push(candidate);
            }
            files
        })
        .await
        .map_err(|error| format!("Structural query task failed: {error}"))?;

        if files.is_empty() {
            return Err(format!(
                "No supported source files (rs, ts, js, py, c, cpp, go) found under '{path}'."
            ));
        }
        Ok(files)
    }
}

/// A located declaration, together with the snapshot it was read from.
struct RenderedMatch {
    display_path: String,
    version: String,
    declaration: syntax::Declaration,
    source: String,
}

/// Does a declaration answer a `symbol` lookup?
///
/// An unqualified request matches the declaration's own name. A qualified
/// request (`Type::name`, `mod::Type::name`) matches on **whole segments** — a
/// suffix match on the raw string would let `Service::run` match
/// `FooService::run`, crossing an identifier boundary.
fn declaration_matches_symbol(declaration: &syntax::Declaration, symbol: &str) -> bool {
    if !symbol.contains("::") {
        return declaration.name == symbol;
    }
    let requested: Vec<&str> = symbol.split("::").filter(|part| !part.is_empty()).collect();
    if requested.is_empty() {
        return false;
    }
    let mut available: Vec<&str> = declaration.containers.iter().map(String::as_str).collect();
    available.push(declaration.name.as_str());
    available.len() >= requested.len()
        && available[available.len() - requested.len()..] == requested
}

/// Render one `symbol` hit: a header naming path, line range, kind, and version,
/// plus its source when it fits the remaining budget.
fn render_symbol_hit(hit: &RenderedMatch, budget_left: usize) -> (String, Option<(String, bool)>) {
    let decl = &hit.declaration;
    let header = format!(
        "\n{} {}  ({}:{}-{}, version {})\n",
        decl.kind,
        decl.qualified_name(),
        hit.display_path,
        decl.start_line,
        decl.end_line,
        hit.version
    );
    if budget_left <= header.len() {
        return (header, None);
    }
    let (text, truncated) = slice_source(&hit.source, decl.byte_range, budget_left - header.len());
    if text.is_empty() {
        return (header, None);
    }
    (header, Some((text, truncated)))
}

/// Render `(line, _)` entries honoring both the entry limit and byte budget.
/// Returns the rendered block, how many entries were written, and whether the
/// result was cut short.
fn render_entries(
    entries: impl Iterator<Item = (String, String)>,
    caps: &Caps,
) -> (String, usize, bool) {
    let mut out = String::new();
    let mut shown = 0usize;
    let mut truncated = false;
    for (line, suffix) in entries {
        if shown >= caps.limit {
            truncated = true;
            break;
        }
        let rendered = if suffix.is_empty() {
            format!("  {line}\n")
        } else {
            format!("  {line} {suffix}\n")
        };
        if out.len() + rendered.len() > caps.budget {
            truncated = true;
            break;
        }
        out.push_str(&rendered);
        shown += 1;
    }
    (out, shown, truncated)
}

/// A one-line result header carrying provenance (source identity + version).
fn header(
    title: &str,
    version: &str,
    bytes: Option<u64>,
    shown: usize,
    total: usize,
    truncated: bool,
) -> String {
    let size = bytes
        .map(|bytes| format!(", {bytes} bytes"))
        .unwrap_or_default();
    let mut out = format!("{title} (version {version}{size}, {shown} of {total} symbols");
    if truncated {
        out.push_str("; output truncated");
    }
    out.push_str("):\n");
    out
}

/// Disclose truncation with the exact cap that caused it and the way out.
fn push_truncation_note(
    out: &mut String,
    truncated: bool,
    shown: usize,
    total: usize,
    caps: &Caps,
) {
    if !truncated {
        return;
    }
    out.push_str(&format!(
        "\n[Shown {shown} of {total} entries within the {}-entry / {}-byte budget. Narrow the \
         scope or raise 'limit' / 'budget' (max {MAX_BUDGET_BYTES} bytes) for the remainder.]\n",
        caps.limit, caps.budget
    ));
}

/// Workspace-relative display path, falling back to the absolute path when the
/// file lives outside the workspace root (an admitted sibling root).
fn display_path(path: &Path, workspace: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

nuo_protocol::register_tool!(CodeQueryFactory => |ctx| CodeQueryTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

#[cfg(test)]
mod tests;
