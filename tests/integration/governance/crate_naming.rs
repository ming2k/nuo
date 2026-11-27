//! Crate identity gate (`[INV-NAME-01]`, ADR-0269).
//!
//! Every package in the `crates/` tier is `nuo` (the application) or `nuo-*` (a
//! library). The name is the crate's only identity: its directory, its library
//! target, and every dependency edge on it use the same name, with no alias.
//! The checks read the manifests with a TOML parser rather than matching
//! substrings, so a crate named `nuo-ui-primitives` cannot be mistaken for a
//! crate named `nuo-ui`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use walkdir::WalkDir;

/// The application package; the one crate in the tier without a `-` suffix.
const APPLICATION: &str = "nuo";
/// The prefix every library package carries.
const PREFIX: &str = "nuo-";

fn workspace_root() -> PathBuf {
    crate::common::workspace_root()
}

fn read_manifest(path: &Path) -> toml::Table {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    toml::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// `(directory name, parsed manifest)` for every package in the `crates/` tier.
fn tier_packages(root: &Path) -> Vec<(String, toml::Table)> {
    let mut packages: Vec<(String, toml::Table)> = fs::read_dir(root.join("crates"))
        .expect("read crates/")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("Cargo.toml").is_file())
        .map(|entry| {
            let dir = entry.file_name().to_string_lossy().into_owned();
            (dir, read_manifest(&entry.path().join("Cargo.toml")))
        })
        .collect();
    packages.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        packages.len() >= 30,
        "the crate-identity scan found only {} packages; a scan that sees nothing certifies nothing",
        packages.len()
    );
    packages
}

fn package_name(manifest: &toml::Table) -> &str {
    manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(|name| name.as_str())
        .expect("[package].name")
}

/// Every dependency table in a manifest: `[dependencies]`, `[dev-dependencies]`,
/// `[build-dependencies]`, their `[target.'cfg(..)'.*]` forms, and
/// `[workspace.dependencies]`.
fn dependency_tables(manifest: &toml::Table) -> Vec<(String, &toml::Table)> {
    const KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut tables = Vec::new();
    for kind in KINDS {
        if let Some(table) = manifest.get(kind).and_then(|value| value.as_table()) {
            tables.push((format!("[{kind}]"), table));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(|value| value.as_table()) {
        for (cfg, target) in targets {
            for kind in KINDS {
                if let Some(table) = target.get(kind).and_then(|value| value.as_table()) {
                    tables.push((format!("[target.{cfg}.{kind}]"), table));
                }
            }
        }
    }
    if let Some(table) = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(|value| value.as_table())
    {
        tables.push(("[workspace.dependencies]".to_string(), table));
    }
    tables
}

/// `[INV-NAME-01]`: a tier package is `nuo` or `nuo-*`, lives in
/// `crates/<package name>`, and keeps the library target name Cargo derives
/// from the package name.
#[test]
fn tier_packages_carry_the_nuo_prefix_and_match_their_directory() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for (dir, manifest) in tier_packages(&root) {
        let name = package_name(&manifest);
        if name != APPLICATION && !name.starts_with(PREFIX) {
            violations.push(format!(
                "crates/{dir}: package `{name}` lacks the `{PREFIX}` prefix"
            ));
        }
        if name != dir {
            violations.push(format!(
                "crates/{dir}: package `{name}` must live in `crates/{name}`"
            ));
        }
        let derived = name.replace('-', "_");
        if let Some(lib) = manifest
            .get("lib")
            .and_then(|lib| lib.get("name"))
            .and_then(|lib| lib.as_str())
            && lib != derived
        {
            violations.push(format!(
                "crates/{dir}: `[lib] name = \"{lib}\"` gives `{name}` a second identity; \
                 the library is `{derived}`"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "tier packages must be `nuo` or `nuo-*` and match their directory \
         ([INV-NAME-01], ADR-0269): {violations:#?}"
    );
}

/// `[INV-NAME-01]`: a dependency on a workspace crate is spelled with that
/// crate's own name. `foo = { package = "nuo-foo" }` would bring back the
/// unprefixed identifier in code, which is the ambiguity the prefix removes.
#[test]
fn workspace_crates_are_never_aliased_in_dependency_tables() {
    let root = workspace_root();
    let packages = tier_packages(&root);
    let workspace: BTreeSet<String> = packages
        .iter()
        .map(|(_, manifest)| package_name(manifest).to_string())
        .collect();

    let mut manifests = vec![(
        "Cargo.toml".to_string(),
        read_manifest(&root.join("Cargo.toml")),
    )];
    manifests.extend(
        packages
            .into_iter()
            .map(|(dir, manifest)| (format!("crates/{dir}/Cargo.toml"), manifest)),
    );

    let mut violations = Vec::new();
    let mut workspace_edges = 0usize;
    for (file, manifest) in &manifests {
        for (section, table) in dependency_tables(manifest) {
            for (key, spec) in table {
                let package = spec
                    .get("package")
                    .and_then(|package| package.as_str())
                    .unwrap_or(key);
                if !workspace.contains(package) {
                    continue;
                }
                workspace_edges += 1;
                if package != key {
                    violations.push(format!(
                        "{file} {section}: `{key}` aliases workspace crate `{package}`"
                    ));
                }
                if section == "[workspace.dependencies]" {
                    let path = spec.get("path").and_then(|path| path.as_str());
                    let expected = format!("crates/{package}");
                    if path != Some(expected.as_str()) {
                        violations.push(format!(
                            "{file} {section}: `{key}` must point at `{expected}`, found {path:?}"
                        ));
                    }
                } else if spec.get("path").is_some() {
                    violations.push(format!(
                        "{file} {section}: `{key}` declares its own `path`; in-house edges use \
                         `{{ workspace = true }}` so the root manifest is the one registry"
                    ));
                }
            }
        }
    }
    assert!(
        workspace_edges >= 100,
        "the alias scan saw only {workspace_edges} in-house dependency edges; a scan that sees \
         nothing certifies nothing"
    );
    assert!(
        violations.is_empty(),
        "workspace crates are referenced by their own name only \
         ([INV-NAME-01], ADR-0269): {violations:#?}"
    );
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect()
}

/// Every name a crate's `src/` could expose as the first segment after the
/// crate root: module files and directories, item and macro definitions, and
/// the leaf names of its `use` / `pub use` re-exports. Visibility is ignored on
/// purpose; the gate asks "does this name exist in that crate at all".
fn names_defined_in(src: &Path) -> BTreeSet<String> {
    static ITEM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"\b(?:mod|fn|struct|enum|trait|type|const|static|union|macro_rules!)\s+(?:r#)?([A-Za-z_]\w*)",
        )
        .expect("item regex")
    });
    static REEXPORT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)\buse\s+([^;]+);").expect("use regex"));
    static IDENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[A-Za-z_]\w*").expect("ident regex"));
    let mut names = BTreeSet::new();
    for path in rust_sources(src) {
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            names.insert(stem.to_string());
        }
        if let Some(parent) = path.parent().and_then(|parent| parent.file_name()) {
            names.insert(parent.to_string_lossy().into_owned());
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        names.extend(ITEM.captures_iter(&text).map(|c| c[1].to_string()));
        for tree in REEXPORT.captures_iter(&text) {
            names.extend(IDENT.find_iter(&tree[1]).map(|m| m.as_str().to_string()));
        }
    }
    names
}

/// `[INV-NAME-01]`: a comment or rustdoc line that cites `nuo_<crate>::<name>`
/// cites something that exists in that crate.
///
/// rustdoc resolves intra-doc links only on documented public items, so a path
/// in backticks — or a link on a private item — is never checked by
/// `cargo doc`. A crate rename that also prefixes a *local* module's name
/// (`host::run` meaning `crate::runtime::host::run`, not the `nuo-host` crate)
/// would otherwise leave confident, wrong pointers behind. Only the first
/// segment after the crate root is checked: that is where a mis-attributed
/// crate shows.
#[test]
fn comments_cite_items_that_exist_in_the_named_crate() {
    static CITATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[^A-Za-z0-9_:])(nuo_[a-z0-9_]+)::(?:r#)?([A-Za-z_]\w*)")
            .expect("citation regex")
    });
    let root = workspace_root();
    let crates: BTreeMap<String, BTreeSet<String>> = tier_packages(&root)
        .into_iter()
        .filter(|(dir, _)| dir != APPLICATION)
        .map(|(dir, _)| {
            let names = names_defined_in(&root.join("crates").join(&dir).join("src"));
            (dir.replace('-', "_"), names)
        })
        .collect();

    let mut sources = rust_sources(&root.join("crates"));
    sources.extend(rust_sources(&root.join("tests")));
    let mut violations = Vec::new();
    let mut citations = 0usize;
    for path in sources {
        let text = fs::read_to_string(&path).unwrap_or_default();
        let file = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (index, line) in text.lines().enumerate() {
            let Some(comment) = line.trim_start().strip_prefix("//") else {
                continue;
            };
            for capture in CITATION.captures_iter(comment) {
                let (krate, segment) = (&capture[1], &capture[2]);
                let Some(names) = crates.get(krate) else {
                    continue;
                };
                citations += 1;
                if !names.contains(segment) {
                    violations.push(format!(
                        "{file}:{}: `{krate}::{segment}` — `{krate}` defines no `{segment}`",
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        citations >= 300,
        "the citation scan saw only {citations} `nuo_*::` references in comments; a scan that \
         sees nothing certifies nothing"
    );
    assert!(
        violations.is_empty(),
        "comments must cite items that exist in the crate they name \
         ([INV-NAME-01], ADR-0269): {violations:#?}"
    );
}
