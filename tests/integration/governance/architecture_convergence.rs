//! Architecture convergence gates (ADR-0180 through ADR-0188).
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
    nuo::dev::find_workspace_root()
}

fn tier_crates(root: &Path) -> Vec<PathBuf> {
    let mut crates = Vec::new();
    for tier in ["crates", "tools", "providers"] {
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
        .chain(rust_files(&root.join("tools")))
        .chain(rust_files(&root.join("providers")))
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
        "`nuo_harness` is retired; import `nuo_engine` ([INV-ENGINE-01], ADR-0181): {hits:?}"
    );
}

#[test]
fn guardrails_has_no_persistence_edge() {
    let manifest = fs::read_to_string(workspace_root().join("crates/nuo-guardrails/Cargo.toml"))
        .expect("read guardrails manifest");
    assert!(
        !manifest.contains("nuo-persistence"),
        "nuo-guardrails must not depend on nuo-persistence ([INV-GUARD-01], ADR-0188)"
    );
}

#[test]
fn engine_has_no_capability_leaf_edge() {
    let manifest = fs::read_to_string(workspace_root().join("crates/nuo-engine/Cargo.toml"))
        .expect("read engine manifest");
    let manifest: toml::Table = toml::from_str(&manifest).expect("parse engine manifest");
    let deps = manifest
        .get("dependencies")
        .and_then(|value| value.as_table())
        .expect("engine [dependencies]");
    for forbidden in [
        "nuo-tool-fs",
        "nuo-tool-exec",
        "nuo-tool-web",
        "nuo-tool-ast",
    ] {
        assert!(
            !deps.contains_key(forbidden),
            "nuo-engine must not link {forbidden}; the composition root owns capability linkage \
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
        if path.ends_with("nuo-view/src/tools/mod.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap_or_default();
        if source.contains("enum ToolStatus") || source.contains("struct ToolStatus") {
            extra_status.push(path.strip_prefix(&root).unwrap_or(&path).to_path_buf());
        }
    }
    assert!(
        extra_status.is_empty(),
        "one `ToolStatus` lives in nuo-view::tools ([INV-TUI-SINGLE-RENDER-01], ADR-0185): \
         {extra_status:?}"
    );
}

#[test]
fn server_root_is_grouped() {
    let root = workspace_root().join("crates/nuo-server/src");
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
    let mut violations = Vec::new();
    for crate_dir in tier_crates(&root) {
        for path in rust_files(&crate_dir.join("src")) {
            let source = fs::read_to_string(&path).unwrap_or_default();
            let code: Vec<&str> = source
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with("//"))
                .collect();
            if code.is_empty() || code.len() > 3 {
                continue;
            }
            let all_reexport = code
                .iter()
                .all(|line| line.starts_with("pub use nuo_") || line.starts_with("pub use acp::"));
            if all_reexport {
                violations.push(path.strip_prefix(&root).unwrap_or(&path).to_path_buf());
            }
            for line in code {
                if line.starts_with("pub use nuo_") && line.ends_with("::*;") {
                    violations.push(path.strip_prefix(&root).unwrap_or(&path).to_path_buf());
                }
            }
        }
    }
    violations.sort();
    violations.dedup();
    assert!(
        violations.is_empty(),
        "compatibility re-export modules are retired; import the owner directly \
         ([INV-VOCAB-02], ADR-0180): {violations:?}"
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

#[test]
fn living_docs_use_current_crate_names() {
    let root = workspace_root();
    let living = [
        "README.md",
        "README-zh.md",
        "docs/dev/subsystems.md",
        "docs/explanation/architecture-overview.md",
    ];
    let retired = [
        "nuo-harness",
        "nuo-tui",
        "nuo-client",
        "nuo-model-codec",
        "nuo-catalog",
    ];
    let mut violations = Vec::new();
    for file in living {
        let source = fs::read_to_string(root.join(file)).unwrap_or_default();
        // Historical references inside ADR link targets are legitimate; the
        // rule is about prose that names a retired crate as current.
        static LINK_RE: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"\]\([^)]*\)").expect("link regex"));
        let source = LINK_RE.replace_all(&source, "]");
        for name in retired {
            if source.contains(name) {
                violations.push(format!("{file}: names retired crate `{name}`"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "living docs must use current crate names ([INV-DOC-03], ADR-0187): {violations:?}"
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
