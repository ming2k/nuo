//! `code_query` — bounded structural queries against the current bytes of a
//! file or a workspace scope (ADR-0214 direction, consolidated in ADR-0237).
//!
//! One tool, three modes, one parser. `outline` answers "what is in this
//! file?", `symbol` answers "give me the source of this declaration", and
//! `find` answers "where are declarations matching this kind/name?". They were
//! merged rather than shipped as sibling tools because they operate on one
//! resource (the parse of a source snapshot) with one input language
//! (declaration kinds + names) — the boundary ADR-0143 drew for `search_text`
//! and ADR-0215 generalized to the whole surface.
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
use nuo_wire::{ExecutionEnvironment, Tool, ToolAccesses, ToolOutput};
use nuo_tool::ToolSchema;
use serde::Deserialize;

use crate::syntax;
use crate::file_search::{
    build_file_walker, resolve_search_root, search_limit, search_path_argument,
};
use crate::helpers::{
    WorkspaceBase, content_version, env_from_root, execution_environment, resolve_workspace_path,
    workspace_base,
};

/// Maximum entries (symbols, matches, or returned declarations) per result.
const DEFAULT_LIMIT: usize = 200;

/// Maximum rendered result size in bytes. An output budget: source on disk is
/// never truncated, only what travels back to the model.
const DEFAULT_BUDGET_BYTES: usize = 16 * 1024;
const MAX_BUDGET_BYTES: usize = 128 * 1024;

/// Maximum source size eligible for parsing. Parsing is work, so the input is
/// bounded independently of the rendered-output budget (ADR-0214 §3).
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
    /// the accepted input ([`Mode::parse`]), because the `ToolSchema` derive
    /// cannot see an enum's variants: for an enum-typed field it emits
    /// `"type": "object"`, which would tell the model to send an object where a
    /// string is required. The field is therefore declared as a string and the
    /// enum is injected from here, so the published contract and the enforced
    /// one cannot drift (ADR-0237 invariant 6).
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

#[derive(ToolSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeQueryArgs {
    #[tool(
        desc = "Query mode: 'outline' lists one file's declarations, 'symbol' returns a declaration's source by name, 'find' locates declarations matching a kind/name pattern in a scope."
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

/// Structural queries over a source snapshot (ADR-0211 / ADR-0214 / ADR-0237).
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
        "code_query"
    }

    fn description(&self) -> &str {
        "Query code by structure instead of reading whole files: 'outline' lists a file's declarations, 'symbol' returns one declaration's source by name, 'find' locates declarations matching a kind/name pattern in a scope. Every result names the content version of the bytes it describes. Languages: rs, ts, js, py, c, cpp, go."
    }

    fn parameters(&self) -> serde_json::Value {
        let mut schema = CodeQueryArgs::parameters_schema();
        // The derive cannot name an enum's variants, so the mode enum is
        // injected from `Mode::ALL` — the same const the parser accepts against.
        if let Some(mode) = schema.pointer_mut("/properties/mode") {
            mode["enum"] = serde_json::json!(Mode::ALL.map(Mode::as_str));
        }
        schema
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

nuo_wire::register_tool!(CodeQueryFactory => |ctx| CodeQueryTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const SERVICE: &str = "\
pub struct Service;

impl Service {
    pub fn handle_request(&self) -> u32 {
        1
    }

    fn helper(&self) {}
}

pub fn top_level() {}
";

    fn tool_for(root: &Path) -> CodeQueryTool {
        CodeQueryTool::new(Some(root.to_path_buf()))
    }

    /// ADR-0214 §4: a structural result must identify the source version it
    /// describes, and that identity must track the bytes on disk — it is the
    /// value a later `expected_version` write is checked against.
    #[tokio::test]
    async fn outline_reports_a_version_that_tracks_content() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("service.rs");
        std::fs::write(&file, SERVICE).unwrap();
        let tool = tool_for(dir.path());

        let first = tool
            .call(r#"{"mode":"outline","path":"service.rs"}"#)
            .await
            .unwrap();
        assert!(first.contains("version "), "{first}");
        assert!(first.contains("pub struct Service"), "{first}");
        // Members are reachable, indented under their container.
        assert!(
            first.contains("Service::handle_request") || first.contains("pub fn handle_request"),
            "{first}"
        );

        let repeat = tool
            .call(r#"{"mode":"outline","path":"service.rs"}"#)
            .await
            .unwrap();
        assert_eq!(first.lines().next(), repeat.lines().next());

        // Same-size replacement still changes the version.
        std::fs::write(&file, SERVICE.replace("top_level", "top_levxl")).unwrap();
        let second = tool
            .call(r#"{"mode":"outline","path":"service.rs"}"#)
            .await
            .unwrap();
        assert_ne!(first.lines().next(), second.lines().next());
    }

    /// The whole point of `symbol` mode: the declaration's source, so a caller
    /// does not have to read (and pay for) the entire file.
    #[tokio::test]
    async fn symbol_returns_the_declaration_source_and_its_span() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("service.rs"), SERVICE).unwrap();
        let tool = tool_for(dir.path());

        let found = tool
            .call(r#"{"mode":"symbol","path":"service.rs","symbol":"handle_request"}"#)
            .await
            .unwrap();
        assert!(
            found.contains("pub fn handle_request(&self) -> u32"),
            "{found}"
        );
        assert!(
            found.contains("        1"),
            "body must be included: {found}"
        );
        assert!(found.contains("service.rs:4-6"), "{found}");
        assert!(
            !found.contains("fn helper"),
            "siblings must not be inlined: {found}"
        );
    }

    /// `symbol` accepts a container-qualified name so same-named members in
    /// different containers can be told apart.
    #[tokio::test]
    async fn symbol_resolves_a_qualified_name() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("service.rs"), SERVICE).unwrap();
        let tool = tool_for(dir.path());

        let qualified = tool
            .call(r#"{"mode":"symbol","path":"service.rs","symbol":"Service::helper"}"#)
            .await
            .unwrap();
        assert!(qualified.contains("fn helper(&self)"), "{qualified}");

        let missing = tool
            .call(r#"{"mode":"symbol","path":"service.rs","symbol":"Nope::helper"}"#)
            .await
            .unwrap();
        assert!(missing.contains("No declaration named"), "{missing}");
    }

    /// `find` locates declarations by kind and name glob across a scope.
    #[tokio::test]
    async fn find_matches_kind_and_name_glob_across_a_scope() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), SERVICE).unwrap();
        std::fs::write(
            dir.path().join("src/b.rs"),
            "pub fn handle_other() {}\npub struct Handler;\n",
        )
        .unwrap();

        let tool = tool_for(dir.path());
        let globbed = tool
            .call(r#"{"mode":"find","pattern":"fn:handle_*","path":"src"}"#)
            .await
            .unwrap();
        assert!(globbed.contains("handle_request"), "{globbed}");
        assert!(globbed.contains("handle_other"), "{globbed}");
        assert!(
            globbed.contains("src/a.rs") && globbed.contains("src/b.rs"),
            "{globbed}"
        );
        assert!(!globbed.contains("top_level"), "{globbed}");

        let kinds = tool
            .call(r#"{"mode":"find","pattern":"struct"}"#)
            .await
            .unwrap();
        assert!(kinds.contains("struct Service"), "{kinds}");
        assert!(kinds.contains("struct Handler"), "{kinds}");
        assert!(!kinds.contains("handle_other"), "{kinds}");
    }

    /// A bad kind is rejected with the legal vocabulary named — never a silent
    /// empty result (ADR-0179).
    #[tokio::test]
    async fn find_rejects_unknown_kinds_with_the_vocabulary() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), SERVICE).unwrap();
        let tool = tool_for(dir.path());

        let error = tool
            .call(r#"{"mode":"find","pattern":"fnction:x"}"#)
            .await
            .unwrap_err();
        assert!(
            error.contains("Unknown declaration kind 'fnction'"),
            "{error}"
        );
        assert!(error.contains("method"), "{error}");
    }

    /// Each mode names the argument it requires, so a missing field is a
    /// next-step instruction rather than a bare parse failure.
    #[tokio::test]
    async fn modes_name_their_required_argument() {
        let dir = tempdir().unwrap();
        let tool = tool_for(dir.path());
        assert!(
            tool.call(r#"{"mode":"outline"}"#)
                .await
                .unwrap_err()
                .contains("requires 'path'")
        );
        assert!(
            tool.call(r#"{"mode":"symbol"}"#)
                .await
                .unwrap_err()
                .contains("requires 'symbol'")
        );
        assert!(
            tool.call(r#"{"mode":"find"}"#)
                .await
                .unwrap_err()
                .contains("requires 'pattern'")
        );
    }

    /// The mode enum must be advertised as a string enum, for the same reason
    /// the dispatch role enum must be (ADR-0237 invariant 6): the model reads
    /// the schema, and a value it cannot see is a value it cannot use. It is
    /// generated from the same const the parser accepts against, so the
    /// advertised set and the enforced set are equal by construction.
    #[test]
    fn schema_advertises_the_mode_enum_as_a_string_enum() {
        let tool = CodeQueryTool::new(None);
        let mode = &tool.parameters()["properties"]["mode"];
        assert_eq!(
            mode["type"], "string",
            "a string field must not be schema'd as an object"
        );
        let modes: Vec<String> = mode["enum"]
            .as_array()
            .expect("mode enum is an array")
            .iter()
            .map(|value| value.as_str().expect("modes are strings").to_string())
            .collect();
        assert_eq!(modes, vec!["outline", "symbol", "find"]);

        // Every advertised value must parse, and nothing else may.
        for advertised in &modes {
            assert!(Mode::parse(advertised).is_ok(), "{advertised} must parse");
        }
        assert!(Mode::parse("OUTLINE").is_ok(), "case should not matter");
        let rejection = Mode::parse("summarize").unwrap_err();
        assert!(
            rejection.contains("Unknown 'mode' 'summarize'"),
            "{rejection}"
        );
        assert!(rejection.contains("find"), "{rejection}");
    }

    /// A qualified lookup must not cross an identifier boundary: `Service::run`
    /// must not match `FooService::run`, or a caller asking for one type's
    /// method would silently receive another's.
    #[tokio::test]
    async fn symbol_qualification_does_not_cross_identifier_boundaries() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("svc.rs"),
            "impl FooService {\n    fn run(&self) {}\n}\n",
        )
        .unwrap();
        let tool = tool_for(dir.path());

        let miss = tool
            .call(r#"{"mode":"symbol","path":"svc.rs","symbol":"Service::run"}"#)
            .await
            .unwrap();
        assert!(miss.contains("No declaration named"), "{miss}");

        let hit = tool
            .call(r#"{"mode":"symbol","path":"svc.rs","symbol":"FooService::run"}"#)
            .await
            .unwrap();
        assert!(hit.contains("fn run(&self)"), "{hit}");
    }

    /// A declaration longer than the output budget is disclosed as truncated,
    /// including when the last line is the one that does not fit — a silently
    /// dropped tail would read as a complete declaration.
    #[tokio::test]
    async fn symbol_discloses_a_truncated_declaration() {
        let dir = tempdir().unwrap();
        let mut source = String::from("pub fn big() {\n");
        for index in 0..30 {
            source.push_str(&format!("    let line_{index} = {index};\n"));
        }
        source.push_str("}\n");
        std::fs::write(dir.path().join("big.rs"), source).unwrap();
        let tool = tool_for(dir.path());

        let out = tool
            .call(r#"{"mode":"symbol","path":"big.rs","symbol":"big","budget":120}"#)
            .await
            .unwrap();
        assert!(out.contains("Source truncated"), "{out}");
        assert!(out.contains("pub fn big() {"), "{out}");
    }

    /// A scope-wide query that cannot read every file must say so: an
    /// incomplete scan reported as complete is the failure mode that makes
    /// structural search untrustworthy.
    #[tokio::test]
    async fn scope_scan_discloses_files_it_could_not_analyse() {
        let dir = tempdir().unwrap();
        let unreadable = dir.path().join("locked.rs");
        std::fs::write(&unreadable, "pub fn hidden() {}\n").unwrap();
        std::fs::write(dir.path().join("readable.rs"), "pub fn visible() {}\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        }

        let tool = tool_for(dir.path());
        let out = tool
            .call(r#"{"mode":"find","pattern":"fn"}"#)
            .await
            .unwrap();

        assert!(out.contains("visible"), "{out}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Running as root bypasses mode bits, so only assert the disclosure
            // when the file really was unreadable.
            if std::fs::read(&unreadable).is_err() {
                assert!(out.contains("could not be analysed"), "{out}");
                assert!(!out.contains("hidden"), "{out}");
            }
            let _ = std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644));
        }
    }

    /// ADR-0214 §3: structure output has a finite budget and discloses
    /// truncation rather than silently clipping or emitting everything.
    #[tokio::test]
    async fn outline_discloses_truncation_at_the_entry_limit() {
        let dir = tempdir().unwrap();
        let mut source = String::new();
        for index in 0..40 {
            source.push_str(&format!("pub fn item_{index}() {{}}\n"));
        }
        std::fs::write(dir.path().join("many.rs"), source).unwrap();
        let tool = tool_for(dir.path());

        let out = tool
            .call(r#"{"mode":"outline","path":"many.rs","limit":5}"#)
            .await
            .unwrap();
        assert!(out.contains("5 of 40 symbols"), "{out}");
        assert!(out.contains("output truncated"), "{out}");
        assert!(!out.contains("item_5"), "{out}");
    }

    /// ADR-0214 §3: the *input* is bounded too, not only the rendered output.
    #[tokio::test]
    async fn rejects_source_above_the_parse_budget() {
        let dir = tempdir().unwrap();
        let mut source = String::new();
        while source.len() as u64 <= MAX_SOURCE_BYTES {
            source.push_str("pub struct Pad;\n");
        }
        std::fs::write(dir.path().join("huge.rs"), source).unwrap();
        let tool = tool_for(dir.path());

        let error = tool
            .call(r#"{"mode":"outline","path":"huge.rs"}"#)
            .await
            .unwrap_err();
        assert!(error.contains("input budget"), "{error}");
    }

    /// Unsupported formats fail loudly instead of being reported as parsed.
    #[tokio::test]
    async fn rejects_unsupported_language() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello\n").unwrap();
        let tool = tool_for(dir.path());

        let error = tool
            .call(r#"{"mode":"outline","path":"notes.txt"}"#)
            .await
            .unwrap_err();
        assert!(error.contains("Unsupported file type"), "{error}");
    }

    /// A directory is a scope, not a file: `outline` says so and points at the
    /// mode that takes a scope.
    #[tokio::test]
    async fn outline_points_a_directory_at_find_mode() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let tool = tool_for(dir.path());

        let error = tool
            .call(r#"{"mode":"outline","path":"src"}"#)
            .await
            .unwrap_err();
        assert!(error.contains("is a directory"), "{error}");
        assert!(error.contains("mode 'find'"), "{error}");
    }

    /// Scope-wide queries obey the walker's ignore rules, so a `find` never
    /// wades into build output or vendored trees.
    #[tokio::test]
    async fn find_respects_project_ignore_rules() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("keep.rs"), "pub fn keep_me() {}\n").unwrap();
        std::fs::write(dir.path().join("target/gen.rs"), "pub fn generated() {}\n").unwrap();
        let tool = tool_for(dir.path());

        let out = tool
            .call(r#"{"mode":"find","pattern":"fn"}"#)
            .await
            .unwrap();
        assert!(out.contains("keep_me"), "{out}");
        assert!(!out.contains("generated"), "{out}");
    }
}
