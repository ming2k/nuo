//! Architecture convergence gates (ADR-0180 through ADR-0193).
//!
//! These checks make the one-time convergence non-optional: one owner per
//! vocabulary, no compatibility facades, no capability linkage in the engine,
//! no storybook parallel implementation, a grouped application core, and hard
//! size ceilings that prevent a new god object or thousand-line function.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::Visit;
use walkdir::WalkDir;

const MAX_FUNCTION_LINES: usize = 400;
const MAX_STRUCT_FIELDS: usize = 80;
const MAX_FILE_LINES: usize = 2_000;

fn workspace_root() -> PathBuf {
    test_support::dev::find_workspace_root()
}

fn tier_crates(root: &Path) -> Vec<PathBuf> {
    let mut crates = Vec::new();
    for tier in ["crates"] {
        let tier_root = root.join(tier);
        let Ok(entries) = fs::read_dir(&tier_root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("Cargo.toml").is_file() {
                crates.push(path);
            }
        }
    }
    crates.sort();
    crates
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "target" && entry.file_name() != ".git")
        .flatten()
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "rs")
        })
        .map(|entry| entry.into_path())
        .collect()
}

fn is_test_source(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.contains("/tests/")
        || path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name == "tests.rs" || name.ends_with("_tests.rs") || name == "snapshot_tests.rs"
        })
}

fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("test") {
                    found = true;
                }
                Ok(())
            });
            found
        }
    })
}

struct SizeVisitor {
    violations: Vec<String>,
}

impl SizeVisitor {
    fn function(
        &mut self,
        name: &str,
        attrs: &[syn::Attribute],
        start: usize,
        end: usize,
        file: &Path,
    ) {
        if has_cfg_test(attrs) || end < start {
            return;
        }
        let lines = end - start + 1;
        if lines > MAX_FUNCTION_LINES {
            self.violations.push(format!(
                "{}:{start}: function `{name}` is {lines} lines (limit {MAX_FUNCTION_LINES})",
                file.display()
            ));
        }
    }
}

impl<'ast> Visit<'ast> for SizeVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let start = node.sig.fn_token.span.start().line;
        let end = node.block.brace_token.span.close().start().line;
        self.function(
            &node.sig.ident.to_string(),
            &node.attrs,
            start,
            end,
            Path::new(""),
        );
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let start = node.sig.fn_token.span.start().line;
        let end = node.block.brace_token.span.close().start().line;
        self.function(
            &node.sig.ident.to_string(),
            &node.attrs,
            start,
            end,
            Path::new(""),
        );
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if !has_cfg_test(&node.attrs) && node.fields.len() > MAX_STRUCT_FIELDS {
            self.violations.push(format!(
                "struct `{}` has {} fields (limit {MAX_STRUCT_FIELDS})",
                node.ident,
                node.fields.len()
            ));
        }
        syn::visit::visit_item_struct(self, node);
    }
}

#[test]
fn source_size_ceilings_are_enforced() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for crate_dir in tier_crates(&root) {
        for path in rust_files(&crate_dir.join("src")) {
            if is_test_source(&path) {
                continue;
            }
            // Migrations are immutable history: editing a shipped migration is
            // itself the defect. The size gate applies to live code only.
            if path.to_string_lossy().contains("/db/migrations/") {
                continue;
            }
            let source = fs::read_to_string(&path).unwrap_or_default();
            let file_lines = source.lines().count();
            if file_lines > MAX_FILE_LINES {
                violations.push(format!(
                    "{}: {file_lines} lines (limit {MAX_FILE_LINES})",
                    path.strip_prefix(&root).unwrap_or(&path).display()
                ));
            }
            let Ok(ast) = syn::parse_file(&source) else {
                violations.push(format!(
                    "{}: could not be parsed for the size gate",
                    path.strip_prefix(&root).unwrap_or(&path).display()
                ));
                continue;
            };
            let mut visitor = SizeVisitor {
                violations: Vec::new(),
            };
            visitor.visit_file(&ast);
            for violation in visitor.violations {
                violations.push(format!(
                    "{}: {violation}",
                    path.strip_prefix(&root).unwrap_or(&path).display()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "source size ceilings ([INV-SIZE-01], ADR-0186): {violations:#?}"
    );
}

#[test]
fn no_retired_engine_alias() {
    let root = workspace_root();
    let mut hits = Vec::new();
    for path in rust_files(&root.join("crates"))
        .into_iter()
        .chain(rust_files(&root.join("tests")))
        .filter(|path| !path.ends_with("architecture_convergence.rs"))
    {
        let source = fs::read_to_string(&path).unwrap_or_default();
        if source.contains("nuo_harness") {
            hits.push(path.strip_prefix(&root).unwrap_or(&path).to_path_buf());
        }
    }
    assert!(
        hits.is_empty(),
        "`nuo_harness` is retired; import `engine` ([INV-ENGINE-01], ADR-0181): {hits:?}"
    );
}

#[test]
fn filesystem_tools_have_no_document_parser_edge() {
    let root = workspace_root();
    for manifest in ["crates/tool-fs/Cargo.toml", "crates/tool/Cargo.toml"] {
        let source = fs::read_to_string(root.join(manifest)).expect("read manifest");
        assert!(
            !source.contains("pdf-extract") && !source.contains("lopdf"),
            "{manifest} must not link a document parser; the host owns extraction \
             ([INV-PDF-05], ADR-0193)"
        );
    }
}

#[test]
fn guardrails_has_no_persistence_edge() {
    let manifest = fs::read_to_string(workspace_root().join("crates/guardrails/Cargo.toml"))
        .expect("read guardrails manifest");
    assert!(
        !manifest.contains("persistence"),
        "guardrails must not depend on persistence ([INV-GUARD-01], ADR-0188)"
    );
}

#[test]
fn engine_has_no_capability_leaf_edge() {
    let manifest = fs::read_to_string(workspace_root().join("crates/engine/Cargo.toml"))
        .expect("read engine manifest");
    let manifest: toml::Table = toml::from_str(&manifest).expect("parse engine manifest");
    let deps = manifest
        .get("dependencies")
        .and_then(|value| value.as_table())
        .expect("engine [dependencies]");
    for forbidden in ["tool-fs", "tool-exec", "tool-web", "tool-ast"] {
        assert!(
            !deps.contains_key(forbidden),
            "engine must not link {forbidden}; the composition root owns capability linkage \
             ([INV-ARCH-TOOL-LINK-01], ADR-0183)"
        );
    }
}

#[test]
fn storybook_is_retired() {
    let root = workspace_root();
    assert!(
        !root.join("crates/nuo/src/shell/declarative").exists(),
        "the declarative storybook suite is retired ([INV-TUI-SINGLE-RENDER-01], ADR-0185)"
    );
    let cli = fs::read_to_string(root.join("crates/nuo/src/cli.rs")).expect("read cli");
    assert!(
        !cli.contains("DevAction") && !cli.contains("Storybook"),
        "the `nuo dev storybook` command is retired ([INV-TUI-SINGLE-RENDER-02], ADR-0185)"
    );
    let mut extra_status = Vec::new();
    for path in rust_files(&root.join("crates")) {
        if path.ends_with("view/src/tools/mod.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap_or_default();
        if source.contains("enum ToolStatus") || source.contains("struct ToolStatus") {
            extra_status.push(path.strip_prefix(&root).unwrap_or(&path).to_path_buf());
        }
    }
    assert!(
        extra_status.is_empty(),
        "one `ToolStatus` lives in view::tools ([INV-TUI-SINGLE-RENDER-01], ADR-0185): \
         {extra_status:?}"
    );
}

#[test]
fn server_root_is_grouped() {
    let root = workspace_root().join("crates/server/src");
    let mut root_files = Vec::new();
    for entry in fs::read_dir(&root).expect("read server src") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            root_files.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    root_files.sort();
    assert_eq!(
        root_files,
        vec!["api.rs".to_string(), "lib.rs".to_string()],
        "every server module must live in composition/, runtime/, handlers/, transport/, or \
         product/ ([INV-SERVER-TOPOLOGY-01], ADR-0184)"
    );
}

#[test]
fn no_cross_crate_reexport_modules() {
    let root = workspace_root();
    let crates = tier_crates(&root);
    let workspace_idents: Vec<String> = crates
        .iter()
        .filter_map(|dir| dir.file_name().and_then(|n| n.to_str()))
        .map(|name| name.replace('-', "_"))
        .collect();
    let mut violations = Vec::new();
    for crate_dir in &crates {
        let own_ident = crate_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .replace('-', "_");
        for path in rust_files(&crate_dir.join("src")) {
            if is_test_source(&path) {
                continue;
            }
            let source = fs::read_to_string(&path).unwrap_or_default();
            // Strip any trailing `#[cfg(test)]` block so a pure re-export module
            // with an inline unit test cannot slip past the gate.
            let prod_source = source.split("#[cfg(test)]").next().unwrap_or(&source);
            let code: Vec<&str> = prod_source
                .lines()
                .map(str::trim)
                .filter(|line| {
                    !line.is_empty() && !line.starts_with("//") && !line.starts_with('#')
                })
                .collect();

            let is_cross_crate_reexport = |line: &str| -> bool {
                let Some(rest) = line.strip_prefix("pub use ") else {
                    return false;
                };
                if rest.starts_with("nuo_") {
                    return true;
                }
                let target = rest
                    .trim_start_matches("::")
                    .split("::")
                    .next()
                    .unwrap_or("");
                target != own_ident && workspace_idents.iter().any(|id| id == target)
            };

            // [INV-VOCAB-02]: no crate may `pub use <other_workspace_crate>::*;`
            // wholesale anywhere in production source.
            for line in &code {
                if is_cross_crate_reexport(line) && line.ends_with("::*;") {
                    violations.push(format!(
                        "{}: wildcard cross-crate re-export `{line}`",
                        path.strip_prefix(&root).unwrap_or(&path).display()
                    ));
                }
            }

            // [INV-VOCAB-03]: a standalone module whose only production code is
            // re-exporting another workspace crate is a compatibility facade.
            if !code.is_empty()
                && code.len() <= 5
                && code.iter().all(|line| {
                    is_cross_crate_reexport(line) || line.ends_with(',') || line.ends_with("};")
                })
                && code.iter().any(|line| is_cross_crate_reexport(line))
            {
                violations.push(format!(
                    "{}: pure cross-crate re-export facade",
                    path.strip_prefix(&root).unwrap_or(&path).display()
                ));
            }
        }
    }
    violations.sort();
    violations.dedup();
    assert!(
        violations.is_empty(),
        "compatibility re-export modules and cross-crate globs are retired; import the owner \
         directly ([INV-VOCAB-02], [INV-VOCAB-03], ADR-0180): {violations:#?}"
    );
}

/// `[INV-MCP-03]` (ADR-0230): Nuo does not ship an MCP server whose only
/// consumer is its own tests. The retired `crates/mcp` export is gone; it must
/// not reappear under a new name.
#[test]
fn inv_mcp_03_no_unserved_mcp_export_crate() {
    let root = workspace_root();
    assert!(
        !root.join("crates/mcp").exists(),
        "crates/mcp exported an MCP server with no production consumer (ADR-0230/ADR-0234)"
    );
}

#[test]
fn adr_registry_has_unique_identity_and_valid_frontmatter() {
    let root = workspace_root();
    let adr_dir = root.join("docs/adr");
    let mut ids = std::collections::BTreeMap::new();
    let mut violations = Vec::new();
    for entry in fs::read_dir(&adr_dir).expect("read adr dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(prefix) = name.get(0..4) else {
            continue;
        };
        if !prefix.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap_or_default();
        let id = source
            .lines()
            .find_map(|line| line.strip_prefix("id: ADR-"))
            .map(|id| id.trim().to_string());
        let Some(id) = id else {
            violations.push(format!("{name}: missing `id: ADR-NNNN` frontmatter"));
            continue;
        };
        if id != prefix {
            violations.push(format!(
                "{name}: id ADR-{id} does not match filename prefix {prefix}"
            ));
        }
        if let Some(previous) = ids.insert(id.clone(), name.clone()) {
            violations.push(format!("duplicate ADR-{id}: {previous} and {name}"));
        }
        for field in [
            "title:",
            "status:",
            "date:",
            "scope:",
            "superseded_by:",
            "negative_knowledge:",
        ] {
            if !source.lines().any(|line| line.starts_with(field)) {
                violations.push(format!("{name}: missing frontmatter field `{field}`"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "ADR registry identity/frontmatter ([INV-DOC-01], ADR-0187): {violations:#?}"
    );
}

/// `[INV-DOC-03]` (ADR-0187): a living document names a crate that exists.
///
/// The scope is every living product document, not four hand-picked files: a
/// drift that lands in `docs/reference/` is as misleading as one in the README.
/// ADRs and RFCs are historical records and are out of scope by definition.
///
/// Two things are always current and are removed before the scan:
///
/// - `nuo-tests`, the root dev-only test host (the one package that keeps the
///   `nuo-` prefix);
/// - `target/nuo-dev`, the debug sandbox directory, which is a path and not a
///   crate.
///
/// A retired name is allowed on a line that explicitly marks it as history
/// (`former`, `retired`, `renamed`, `consolidated from`, `predecessor`,
/// `replaced`, `deleted`, `pre-consolidation`). That is what keeps a genuinely
/// historical sentence legitimate while a second crate named `nuo-tui` cannot
/// reappear as if it were current.
#[test]
fn living_docs_use_current_crate_names() {
    let root = workspace_root();
    let mut living = vec![root.join("README.md"), root.join("README-zh.md")];
    for dir in [
        "docs/dev",
        "docs/explanation",
        "docs/how-to",
        "docs/reference",
        "docs/rfc",
        "docs/tutorials",
    ] {
        for entry in WalkDir::new(root.join(dir))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.into_path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "md" | "yml" | "yaml"))
            {
                living.push(path);
            }
        }
    }
    const ALWAYS_CURRENT: &[&str] = &["nuo-tests", "target/nuo-dev"];
    const HISTORY_MARKERS: &[&str] = &[
        "former",
        "formerly",
        "retired",
        "removed",
        "renamed",
        "consolidated from",
        "predecessor",
        "replaced",
        "deleted",
        "pre-consolidation",
    ];
    let retired = [
        "nuotc",
        "crates/mcp",
        // Crate roles that no longer exist under any name.
        "nuo-harness",
        "nuo-tui",
        "nuo-client",
        "nuo-model-codec",
        "nuo-catalog",
        "nuo-mcp",
        "nuo-provider-adapters",
        "nuo-provider-transport",
        "nuo-provider-opencode",
        "nuo-egress",
        "nuo-oauth",
        "nuo-contracts",
        "nuo-agent",
        "nous-model-wire",
        "codebuddy-auth",
        "provider-siliconflow",
        "providers/nuo-provider",
        // The workspace removed the `nuo-` prefix from every library crate
        // (ADR-0181); naming the old form is naming a crate that is not there.
        "nuo-acp",
        "nuo-engine",
        "nuo-guardrails",
        "nuo-host",
        "nuo-inference-wire",
        "nuo-model-routing",
        "nuo-persistence",
        "nuo-protocol",
        "nuo-provider",
        "nuo-server",
        "nuo-tool",
        "nuo-ui-primitives",
        "nuo-view",
    ];
    // Historical references inside ADR link targets are legitimate; the rule is
    // about prose that names a retired crate as current.
    static LINK_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\]\([^)]*\)").expect("link regex"));
    let mut violations = Vec::new();
    let mut checked = 0;
    for path in living {
        let file = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let source = fs::read_to_string(&path).unwrap_or_default();
        let source = LINK_RE.replace_all(&source, "]");
        checked += 1;
        for (index, raw) in source.lines().enumerate() {
            let mut line = raw.to_owned();
            for current in ALWAYS_CURRENT {
                line = line.replace(current, "");
            }
            let lower = line.to_lowercase();
            if HISTORY_MARKERS.iter().any(|marker| lower.contains(marker)) {
                continue;
            }
            for name in retired {
                if line.contains(name) {
                    violations.push(format!(
                        "{file}:{}: names retired crate `{name}` as current",
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        checked >= 20,
        "the living-doc scan read only {checked} files; a scan that sees nothing certifies nothing"
    );
    assert!(
        violations.is_empty(),
        "living docs must use current crate names ([INV-DOC-03], ADR-0187): {violations:#?}"
    );
}

#[test]
fn rust_doc_comments_use_current_crate_names() {
    let root = workspace_root();
    const HISTORY_MARKERS: &[&str] = &[
        "former",
        "formerly",
        "retired",
        "removed",
        "renamed",
        "consolidated from",
        "predecessor",
        "replaced",
        "deleted",
        "superseded",
        "legacy",
        "used to",
        "was ",
        "were ",
    ];
    let retired = [
        "nuo-harness",
        "nuo-tui",
        "nuo-client",
        "nuo-model-codec",
        "nuo-catalog",
        "nuo-mcp",
        "nuo-provider-adapters",
        "nuo-provider-transport",
        "nuo-provider-opencode",
        "nuo-egress",
        "nuo-oauth",
        "nuo-contracts",
        "nuo-agent",
        "nuo-tool-",
        "nuo-view",
        "nuo-ui",
        "nuo-present",
        "nuo-shell",
    ];
    let mut violations = Vec::new();
    let mut checked = 0;
    for crate_dir in tier_crates(&root) {
        for path in rust_files(&crate_dir.join("src")) {
            if is_test_source(&path) {
                continue;
            }
            let file = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let source = fs::read_to_string(&path).unwrap_or_default();
            checked += 1;
            for (index, raw) in source.lines().enumerate() {
                let trimmed = raw.trim_start();
                if !(trimmed.starts_with("//!") || trimmed.starts_with("///")) {
                    continue;
                }
                let lower = trimmed.to_lowercase();
                if HISTORY_MARKERS.iter().any(|marker| lower.contains(marker)) {
                    continue;
                }
                for name in retired {
                    if trimmed.contains(name) {
                        violations.push(format!(
                            "{file}:{}: rustdoc names retired crate `{name}` as current",
                            index + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(
        checked >= 200,
        "rustdoc scan read only {checked} files; a scan that sees nothing certifies nothing"
    );
    assert!(
        violations.is_empty(),
        "rustdoc comments must use current crate names ([INV-DOC-03], ADR-0187): {violations:#?}"
    );
}

#[test]
fn markdown_relative_links_resolve() {
    let root = workspace_root();
    let mut violations = Vec::new();
    let mut files = Vec::new();
    for dir in [root.join("docs")] {
        for path in walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        {
            files.push(path);
        }
    }
    files.push(root.join("README.md"));
    files.push(root.join("README-zh.md"));
    for path in files {
        let source = fs::read_to_string(&path).unwrap_or_default();
        for cap in markdown_link_regex().captures_iter(&source) {
            let target = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with('#')
            {
                continue;
            }
            let target = target.split('#').next().unwrap_or("");
            if target.is_empty() || !target.ends_with(".md") {
                continue;
            }
            let resolved = path.parent().unwrap_or(&root).join(target);
            if !resolved.exists() {
                violations.push(format!(
                    "{}: link target `{target}` does not exist",
                    path.strip_prefix(&root).unwrap_or(&path).display()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "relative Markdown links must resolve ([INV-DOC-02], ADR-0187): {violations:#?}"
    );
}

fn markdown_link_regex() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\]\(([^)\s]+)\)").expect("link regex"));
    &RE
}

/// Frontmatter values are values, not just field names.
///
/// The registry gate used to check that `status:` and `date:` existed. A
/// misspelled status or `2026-13-40` passed. This validates the value and the
/// supersession link, not only the label.
#[test]
fn adr_frontmatter_values_are_well_formed() {
    let root = workspace_root();
    let adr_dir = root.join("docs/adr");
    let ids: std::collections::BTreeSet<String> = fs::read_dir(&adr_dir)
        .expect("read adr dir")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.ends_with(".md") && name.len() > 4)
        .map(|name| name[..4].to_owned())
        .collect();
    let mut violations = Vec::new();
    let mut checked = 0;
    for entry in fs::read_dir(&adr_dir).expect("read adr dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let source = fs::read_to_string(&path).unwrap_or_default();
        checked += 1;
        let field = |key: &str| {
            source
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key}:")))
                .map(str::trim)
                .unwrap_or("")
        };
        let status = field("status");
        if !matches!(
            status,
            "draft" | "accepted" | "superseded" | "deprecated" | "rejected"
        ) {
            violations.push(format!("{name}: invalid status `{status}`"));
        }
        let date = field("date");
        if !valid_iso_date(date) {
            violations.push(format!(
                "{name}: invalid date `{date}` (expected YYYY-MM-DD)"
            ));
        }
        let negative = field("negative_knowledge");
        if !matches!(negative, "true" | "false") {
            violations.push(format!("{name}: negative_knowledge must be true or false"));
        }
        if field("scope").is_empty() {
            violations.push(format!("{name}: empty scope"));
        }
        let successor = field("superseded_by");
        if status == "superseded" && successor == "null" {
            violations.push(format!("{name}: superseded without a successor"));
        }
        // `deprecated` means the decision was removed with no direct successor
        // (docs/dev/standards/adr.md §2). Naming one makes it `superseded`.
        if status == "deprecated" && successor != "null" {
            violations.push(format!(
                "{name}: deprecated names successor `{successor}`; use `superseded`"
            ));
        }
        if successor != "null" {
            let Some(id) = successor.strip_prefix("ADR-") else {
                violations.push(format!("{name}: malformed successor `{successor}`"));
                continue;
            };
            if !ids.contains(id) {
                violations.push(format!("{name}: successor ADR-{id} does not exist"));
            }
            // A partial supersession (`Superseded in part by`) keeps
            // `status: accepted` and still names the successor. ADR-0130 is
            // the canonical example: its `[model_guidance]` carrier is gone,
            // while its provider-purity decision stands.
        }
    }
    assert!(
        checked >= 200,
        "the frontmatter scan read only {checked} ADRs; a scan that sees nothing certifies nothing"
    );
    assert!(
        violations.is_empty(),
        "ADR frontmatter values must be well formed ([INV-DOC-01], ADR-0187): {violations:#?}"
    );
}

fn valid_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Ok(year) = value[0..4].parse::<i32>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<u32>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<u32>() else {
        return false;
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

/// `[INV-DOC-04]` (ADR-0187): an invariant tag has one definition site.
///
/// `docs/dev/standards/invariants.md` §5 makes a tag a citation target. Two
/// definitions make every citation ambiguous. A restatement, revision,
/// reaffirmation, or revocation is **not** a second definition: it carries one
/// of those words in the bullet, and this scan tells that apart from "here is
/// the rule".
#[test]
fn adr_invariant_tags_have_one_definition_site() {
    use std::collections::{BTreeMap, BTreeSet};

    let root = workspace_root();
    let adr_dir = root.join("docs/adr");
    // One ADR may state a rule in its Decision section and restate it in its
    // Invariants summary; that is one definition site, not two. A tag defined
    // in two different ADRs is the collision this gate exists to catch.
    let mut sites: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in fs::read_dir(&adr_dir).expect("read adr dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let source = fs::read_to_string(&path).unwrap_or_default();
        for line in source.lines() {
            let Some(tag) = invariant_definition_tag(line) else {
                continue;
            };
            sites.entry(tag).or_default().insert(name.clone());
        }
    }
    let definition_count = sites.len();
    assert!(
        definition_count >= 400,
        "the invariant scan found only {definition_count} definition sites; a scan that sees \
         nothing certifies nothing"
    );
    let violations: Vec<String> = sites
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(tag, files)| {
            format!(
                "`[{tag}]` is defined in {} ADRs: {}",
                files.len(),
                files.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect();
    assert!(
        violations.is_empty(),
        "an invariant tag has one definition site ([INV-DOC-04], ADR-0187): {violations:#?}"
    );
}

#[test]
fn invariant_definition_forms_are_recognized() {
    for line in [
        "- **`[INV-EXAMPLE-01]` A rule** — statement",
        "- **`[INV-EXAMPLE-02]` A rule**: statement",
        "- **[INV-EXAMPLE-03] A rule**: statement",
        "- **INV-EXAMPLE-04** — statement",
        "### 1. `[INV-EXAMPLE-05]` A rule",
        "#### `[INV-EXAMPLE-06]` A rule",
    ] {
        assert!(
            invariant_definition_tag(line).is_some(),
            "a definition form was not recognized: {line}"
        );
    }
    for line in [
        // A citation or amendment reference is not a definition.
        "### 4. `[INV-EXAMPLE-01]`'s exception list is now empty",
        "# 0006. Amendment of [INV-EXAMPLE-01]",
        "- See [INV-EXAMPLE-01] for the rule.",
        "**A rule ([INV-EXAMPLE-01])**",
        "- **[INV-EXAMPLE-01] (restates ADR-0187) A rule**: statement",
    ] {
        assert!(
            invariant_definition_tag(line).is_none(),
            "a citation was counted as a definition: {line}"
        );
    }
}

/// A definition is a line that *introduces* a rule with the tag as its lead:
/// a bullet (`- **[INV-X] Title**: …`, brackets optional) or a heading
/// (`### 1. `[INV-X]` Title`). A citation (`**Rule ([INV-X])**`) starts with a
/// title, not the tag, and is not a definition. A restatement / revision /
/// reaffirmation / revocation keeps the tag but is marked, and is not a second
/// definition.
fn invariant_definition_tag(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    if ["restates", "revises", "reaffirms", "revokes", "superseded"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return None;
    }

    let (rest, is_heading) = if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        (rest.trim_start(), false)
    } else {
        let rest = trimmed.strip_prefix('#')?;
        let rest = rest.trim_start_matches('#').trim_start();
        // Optional section number: `### 1. [INV-X] Title`.
        let rest = match rest.find(". ") {
            Some(dot) if rest[..dot].trim().chars().all(|c| c.is_ascii_digit()) => &rest[dot + 2..],
            _ => rest,
        };
        (rest.trim_start(), true)
    };

    let rest = rest.strip_prefix("**").unwrap_or(rest);
    let rest = rest.strip_prefix('`').unwrap_or(rest);
    let bracketed = rest.starts_with('[');
    let rest = rest.strip_prefix('[').unwrap_or(rest);

    let (tag, after) = if bracketed {
        let end = rest.find(']')?;
        (
            &rest[..end],
            rest[end + 1..].trim_start_matches('`').trim_start(),
        )
    } else {
        // The `**INV-X** — …` bullet form omits the brackets.
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '*')
            .unwrap_or(rest.len());
        let after = rest[end..].trim_start_matches('*').trim_start();
        (&rest[..end], after)
    };

    if !tag.starts_with("INV-") || tag == "INV-" {
        return None;
    }
    if after.starts_with("'s") {
        return None;
    }
    if is_heading {
        // A heading must carry more than the tag: `### [INV-X]` with no rule
        // title is a label, not a definition.
        if after
            .trim_matches(|c: char| c == '`' || c == '*' || c.is_whitespace())
            .is_empty()
        {
            return None;
        }
    } else if !(after.contains(':') || after.contains('—') || after.contains("**")) {
        return None;
    }
    Some(tag.to_owned())
}

/// `[INV-DOC-05]` (ADR-0187): `negative_knowledge: true` names a section.
///
/// The flag is only useful if it is falsifiable. An ADR that records no rejected
/// alternative sets it to `false`; an ADR that claims negative knowledge must
/// carry the section that contains it.
#[test]
fn adr_negative_knowledge_flag_is_truthful() {
    let root = workspace_root();
    let adr_dir = root.join("docs/adr");
    let mut violations = Vec::new();
    let mut checked = 0;
    for entry in fs::read_dir(&adr_dir).expect("read adr dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let source = fs::read_to_string(&path).unwrap_or_default();
        let flag = source
            .lines()
            .find_map(|line| line.strip_prefix("negative_knowledge:"))
            .map(str::trim)
            .unwrap_or("");
        if flag != "true" {
            continue;
        }
        checked += 1;
        if !has_negative_knowledge_section(&source) {
            violations.push(format!(
                "{name}: negative_knowledge is true but no alternatives/negative-knowledge section exists"
            ));
        }
    }
    assert!(
        checked >= 100,
        "the negative-knowledge scan saw only {checked} true flags; a scan that sees nothing \
         certifies nothing"
    );
    assert!(
        violations.is_empty(),
        "negative_knowledge must name a section ([INV-DOC-05], ADR-0187): {violations:#?}"
    );
}

fn has_negative_knowledge_section(source: &str) -> bool {
    source.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("## ")
            && (lower.contains("alternative")
                || lower.contains("negative knowledge")
                || lower.contains("considered options")
                || lower.contains("rejected"))
    })
}

/// Every `ADR-NNNN` reference resolves to a file. ADR-0187 §2 made this a rule;
/// only Markdown *links* were checked before, so a naked citation could point at
/// a phantom number for as long as nobody clicked it.
#[test]
fn adr_references_resolve() {
    let root = workspace_root();
    let adr_ids: std::collections::BTreeSet<String> = fs::read_dir(root.join("docs/adr"))
        .expect("read adr dir")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.len() > 4 && name.ends_with(".md"))
        .map(|name| name[..4].to_owned())
        .collect();
    let reference = regex::Regex::new(r"ADR-(\d{4})").expect("ADR reference regex");
    let mut violations = Vec::new();
    let mut references = 0;
    let mut files = Vec::new();
    for dir in ["docs", "crates", "tests"] {
        for entry in WalkDir::new(root.join(dir))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.into_path();
            if path
                .extension()
                .is_some_and(|ext| ext == "md" || ext == "rs" || ext == "toml")
            {
                files.push(path);
            }
        }
    }
    for name in ["README.md", "README-zh.md", "Cargo.toml"] {
        files.push(root.join(name));
    }
    for path in files {
        let source = fs::read_to_string(&path).unwrap_or_default();
        for capture in reference.captures_iter(&source) {
            let id = capture.get(1).map(|m| m.as_str()).unwrap_or("");
            references += 1;
            if !adr_ids.contains(id) {
                violations.push(format!(
                    "{}: phantom ADR-{id}",
                    path.strip_prefix(&root).unwrap_or(&path).display()
                ));
            }
        }
    }
    violations.sort();
    violations.dedup();
    assert!(
        references >= 100,
        "the ADR reference scan found only {references} citations; a scan that sees nothing \
         certifies nothing"
    );
    assert!(
        violations.is_empty(),
        "ADR references must resolve ([INV-DOC-02], ADR-0187): {violations:#?}"
    );
}

/// The parser is not a substring search: a citation and a marked revision are
/// not definitions, while both definition shapes are.
#[test]
fn the_invariant_definition_parser_distinguishes_a_definition_from_a_citation() {
    assert_eq!(
        invariant_definition_tag("- **`[INV-X-01]` A rule**: it holds"),
        Some("INV-X-01".to_owned())
    );
    assert_eq!(
        invariant_definition_tag("- `[INV-X-01]` **A rule**: it holds"),
        Some("INV-X-01".to_owned())
    );
    assert_eq!(
        invariant_definition_tag("- **`[INV-X-01]` (restates ADR-0001) A rule**: it holds"),
        None
    );
    assert_eq!(
        invariant_definition_tag("- **A rule (`[INV-X-01]`)**: it holds"),
        None
    );
    assert_eq!(
        invariant_definition_tag("- The `[INV-X-01]` test could not see it."),
        None
    );
}

/// `[INV-PRODUCT-02]` (ADR-0234): the product overview states the capability
/// boundary. A README that re-advertises an MCP export server or a plugin ABI
/// is the same drift ADR-0230 and ADR-0231 deleted, one release earlier.
#[test]
fn product_docs_do_not_claim_an_mcp_export_server() {
    let root = workspace_root();
    let forbidden = [
        "MCP client/server",
        "MCP server integration",
        "MCP client and server",
        "导出 MCP server",
        "MCP server 集成",
    ];
    let mut violations = Vec::new();
    for file in [
        "README.md",
        "README-zh.md",
        "docs/dev/subsystems.md",
        "docs/explanation/architecture-overview.md",
    ] {
        let source = fs::read_to_string(root.join(file)).unwrap_or_default();
        for needle in forbidden {
            if source.contains(needle) {
                violations.push(format!("{file}: advertises `{needle}`"));
            }
        }
    }
    let readme = fs::read_to_string(root.join("README.md")).unwrap_or_default();
    assert!(
        readme.contains("MCP") && readme.to_lowercase().contains("client"),
        "README.md must name the MCP client boundary ([INV-PRODUCT-02], ADR-0234)"
    );
    assert!(
        violations.is_empty(),
        "product docs must not claim an MCP export server ([INV-PRODUCT-02], ADR-0234): \
         {violations:?}"
    );
}

/// `[INV-INSTANCE-02]` / `[INV-INSTANCE-04]` / `[INV-VERB-02]` (ADR-0252, ADR-0253):
/// The retired duplicate state bags, manual reconciliation functions, `SessionGate`,
/// and `switch_to_new` teardown path must never return to `crates/nuo/src/shell/`.
#[test]
fn inv_instance_02_no_retired_reconciliation_or_teardown_symbols() {
    let shell_dir = workspace_root().join("crates/nuo/src/shell");
    let forbidden = [
        "struct ThreadWorkspace",
        "struct TabContextState",
        "struct TranscriptState",
        "struct SessionGate",
        "fn park_active_workspace",
        "fn install_workspace",
        "fn checkpoint_active_tab_context",
        "fn restore_active_tab_context",
        "switch_to_new",
    ];
    let mut violations = Vec::new();
    for path in rust_files(&shell_dir) {
        let source = fs::read_to_string(&path).unwrap_or_default();
        let rel = path
            .strip_prefix(workspace_root())
            .unwrap_or(&path)
            .display()
            .to_string();
        for needle in forbidden {
            if source.contains(needle) {
                violations.push(format!("{rel}: contains retired `{needle}`"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "retired duplicate state / reconciliation / teardown symbols returned \
         ([INV-INSTANCE-02], [INV-VERB-02]): {violations:#?}"
    );
}

/// Every curated preset in `view::providers::PROVIDER_PRESETS` must agree 1:1
/// with its authoritative `ModelProviderSpec` in `server::composition::provider_registry`
/// on provider id, default protocol, resolved endpoint URL, user-agent, and seed models.
/// Prevents silent drift between the UI template catalog and the runtime spec table.
#[test]
fn provider_presets_match_authoritative_model_provider_specs() {
    use server::composition::provider_registry::MODEL_PROVIDER_SPECS;
    use std::collections::{BTreeMap, BTreeSet};

    let specs: BTreeMap<&str, &provider::ModelProviderSpec> = MODEL_PROVIDER_SPECS
        .iter()
        .map(|spec| (spec.id.as_ref(), spec))
        .collect();
    let preset_ids: BTreeSet<&str> = view::providers::PROVIDER_PRESETS
        .iter()
        .map(|preset| preset.id)
        .collect();

    let mut violations = Vec::new();

    for spec in MODEL_PROVIDER_SPECS {
        if !preset_ids.contains(spec.id.as_ref()) {
            violations.push(format!(
                "spec `{}` in MODEL_PROVIDER_SPECS has no entry in view::providers::PROVIDER_PRESETS",
                spec.id
            ));
        }
    }

    for preset in view::providers::PROVIDER_PRESETS {
        let Some(spec) = specs.get(preset.id) else {
            violations.push(format!(
                "preset `{}` in PROVIDER_PRESETS does not exist in MODEL_PROVIDER_SPECS",
                preset.id
            ));
            continue;
        };

        if preset.protocol != spec.protocol {
            violations.push(format!(
                "preset `{}` protocol ({:?}) != spec protocol ({:?})",
                preset.id, preset.protocol, spec.protocol
            ));
        }

        match spec.endpoint(spec.protocol) {
            Ok(expected_url) => {
                if preset.default_url != Some(expected_url.as_str()) {
                    violations.push(format!(
                        "preset `{}` default_url ({:?}) != spec.endpoint({:?}) ({:?})",
                        preset.id, preset.default_url, spec.protocol, expected_url
                    ));
                }
            }
            Err(err) => {
                violations.push(format!(
                    "spec `{}` failed to derive endpoint for {:?}: {err}",
                    spec.id, spec.protocol
                ));
            }
        }

        if preset.user_agent != spec.user_agent.as_deref() {
            violations.push(format!(
                "preset `{}` user_agent ({:?}) != spec user_agent ({:?})",
                preset.id, preset.user_agent, spec.user_agent
            ));
        }

        if preset.models != spec.models {
            violations.push(format!(
                "preset `{}` models ({:?}) != spec models ({:?})",
                preset.id, preset.models, spec.models
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "view::providers::PROVIDER_PRESETS drifted from MODEL_PROVIDER_SPECS:\n{}",
        violations.join("\n")
    );
}

/// `[INV-SHARD-01]` (ADR-0255): content shards must be versioned through
/// `db/migrations/content_shard.rs` and `migrations/content_shard/`; ad-hoc
/// `CREATE TABLE IF NOT EXISTS` DDL in `db/shards.rs` is prohibited.
#[test]
fn content_shards_have_no_inline_ddl() {
    let root = workspace_root();
    let shards_rs = root.join("crates/persistence/src/db/shards.rs");
    let source = fs::read_to_string(&shards_rs).expect("shards.rs exists");
    for forbidden in ["CREATE TABLE", "CREATE VIRTUAL TABLE", "CREATE TRIGGER"] {
        assert!(
            !source.contains(forbidden),
            "crates/persistence/src/db/shards.rs must not embed inline DDL `{forbidden}` ([INV-SHARD-01], ADR-0255); put schema in migrations/content_shard/"
        );
    }
}

/// `[INV-SESSSTORE-01]` (ADR-0256): the retired parallel `ThreadStore` /
/// `SqliteThreadStore` persistence surface must not reappear in `crates/`.
#[test]
fn retired_parallel_thread_store_surface_is_absent() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for crate_dir in tier_crates(&root) {
        for path in rust_files(&crate_dir.join("src")) {
            let source = fs::read_to_string(&path).unwrap_or_default();
            for (idx, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                for forbidden in [
                    "struct SqliteThreadStore",
                    "struct SqliteBlobStore",
                    "struct MemoryThreadStore",
                    "struct MemoryBlobStore",
                    "trait ThreadStore",
                    "enum ThreadQuery",
                    "struct TranscriptView",
                    "struct ThreadPolicy",
                ] {
                    if line.contains(forbidden) {
                        violations.push(format!(
                            "{}:{}: `{forbidden}`",
                            path.strip_prefix(&root).unwrap_or(&path).display(),
                            idx + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "retired ThreadStore surface reappeared ([INV-SESSSTORE-01], ADR-0256): {violations:#?}"
    );
}
