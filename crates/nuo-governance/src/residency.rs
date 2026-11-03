//! Rust source residency: every `.rs` file must be reachable from a crate root.
//!
//! ## The failure mode this closes
//!
//! Rust only compiles a file that some `mod` declaration names. A file that no
//! declaration reaches is **invisible**: no compile error, no warning, no test
//! failure — it simply contributes nothing while still looking like source. The
//! repository has already paid for this twice:
//!
//! - `crates/nuo/src/shell/snapshot_tests.rs` (2469 lines, 56 test functions,
//!   24 insta baselines) was moved by a crate-restructuring commit
//!   (`29cb306`, ADR-0109/0111) but its `mod snapshot_tests;` line was not
//!   moved with it. It sat unreachable for months, silently taking the only
//!   visual regression coverage of the tool-step geometry with it — which is
//!   why the turn-band `HH:MM` regression had no test to fail.
//!
//! A reviewer cannot catch this by reading a diff: the file is present, the
//! tests look real, `cargo test` is green. Only a whole-tree reachability check
//! can. That check is this module.
//!
//! ## The rule
//!
//! Every `.rs` file under a tier root (`crates/*`, `providers/*`, `tools/*`)
//! must be reachable by following `mod` declarations from one of that crate's
//! **entry points**:
//!
//! - `src/lib.rs` and `src/main.rs` (library and binary roots), both
//!   unconditionally — a crate compiles its tests against the union.
//! - Every `[[bin]]`, `[[test]]`, `[[bench]]`, `[[example]]` target declared in
//!   the manifest, plus the conventional target directories
//!   (`tests/*.rs`, `benches/*.rs`, `examples/*.rs`) that Cargo auto-discovers
//!   when the manifest does not declare them explicitly.
//! - For a target root at `tests/foo.rs` (or `benches/`/`examples/`), Cargo also
//!   treats `tests/foo/` as that target's module directory, so
//!   `tests/foo/bar.rs` is reachable as `mod bar;` inside `tests/foo.rs`.
//!
//! A file reached this way is compiled; anything else is a defect.
//!
//! ## Nested module forests
//!
//! `mod x;` resolves to `x.rs` **or** `x/mod.rs` beside the declaring file, and
//! — per RFC 2126, stable since Rust 2018 — children of `x.rs` may live in the
//! sibling directory `x/`. Both shapes are followed, so the check never reports
//! a false orphan for a supported layout.
//!
//! ## `#[path]`
//!
//! An explicit `#[path = "…"]` attribute overrides the derived name. It is
//! honoured (resolved relative to the declaring file's directory, or for an
//! inline `mod x { … }` tree relative to that inline module's directory), so
//! the two legitimate `#[path]` uses in the workspace
//! (`nuo-view/src/components/notice.rs`, `nuo-view/src/model/document.rs`) are
//! followed rather than flagged.
//!
//! ## Non-goals
//!
//! This is a **residency** check, not a link check. `cfg(test)` / `cfg(feature)`
//! gating is deliberately ignored: a file behind a `#[cfg(test)] mod` is still
//! reachable, and this module asserts only that some declaration names it. A
//! file whose module is declared but never referenced is a dead-code question,
//! answered by the compiler, not here.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// Tier roots that hold crates (`[INV-ARCH-TIER-01]`, ADR-0109/0111).
const TIERS: &[&str] = &["crates", "providers", "tools"];

/// One `mod` declaration found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModDecl {
    /// The module name as written.
    name: String,
    /// `Some(…)` for an external declaration (`mod x;`), whose file must be
    /// located. `None` for an inline declaration (`mod x { … }`), which owns no
    /// file of its own but may declare external children.
    external: bool,
    /// Explicit `#[path = "…"]` override, when present.
    path_attr: Option<String>,
}

/// Mark which bytes of `src` are **code** (as opposed to comments or string/char
/// literal bodies), so declaration scanning can never be fooled by prose or by a
/// literal that happens to spell a declaration.
///
/// Returns a mask with one entry per byte of `src`, so every offset the scanner
/// computes is an offset into the *original* text. That matters: `#[path]`
/// attribute values are only recoverable from the original text, and comparing
/// those offsets against a stripped copy silently mispairs them.
fn code_mask(src: &str) -> Vec<bool> {
    let bytes = src.as_bytes();
    let mut mask = vec![false; bytes.len()];
    let mut i = 0usize;
    let mut block_depth = 0usize;
    while i < bytes.len() {
        if block_depth > 0 {
            if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                block_depth += 1;
                i += 2;
            } else if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                block_depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // Line comment.
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment start.
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            block_depth = 1;
            i += 2;
            continue;
        }
        // Raw string: r"…", r#"…"#, r##"…"##, and the byte-string form br"…".
        if bytes[i] == b'r' || (bytes[i] == b'b' && i + 1 < bytes.len() && bytes[i + 1] == b'r') {
            let r_pos = if bytes[i] == b'r' { i } else { i + 1 };
            let mut j = r_pos + 1;
            let mut hashes = 0usize;
            while j < bytes.len() && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                // The literal (prefix included) is *not* code: a `mod` spelled
                // inside it must not count. Do not mark anything retroactively —
                // that would resurrect comments already skipped.
                j += 1;
                loop {
                    if j >= bytes.len() {
                        break;
                    }
                    if bytes[j] == b'"' {
                        let mut k = j + 1;
                        let mut seen = 0usize;
                        while seen < hashes && k < bytes.len() && bytes[k] == b'#' {
                            seen += 1;
                            k += 1;
                        }
                        if seen == hashes {
                            j = k;
                            break;
                        }
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        // Normal string literal.
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Char literal (`'x'`, `'\n'`), distinguished from a lifetime.
        if bytes[i] == b'\'' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] != b'\'' {
                    j += 1;
                }
                if j < bytes.len() {
                    i = j + 1;
                    continue;
                }
            } else if i + 2 < bytes.len() && bytes[i + 2] == b'\'' {
                i += 3;
                continue;
            }
        }
        mask[i] = true;
        i += 1;
    }
    mask
}

/// Extract `#[path = "..."]` values by scanning the *original* text, so string
/// literals are available.
fn extract_path_attrs(original: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut search = 0usize;
    while let Some(rel) = original[search..].find("#[path") {
        let at = search + rel;
        let rest = &original[at + 2..];
        if let Some(eq) = rest.find('=') {
            let after_eq = &rest[eq + 1..];
            if let Some(open) = after_eq.find('"') {
                let value_start = open + 1;
                if let Some(close) = after_eq[value_start..].find('"') {
                    let value = &after_eq[value_start..value_start + close];
                    out.push((at, value.to_string()));
                }
            }
        }
        search = at + 5;
    }
    out
}

/// Find every `mod name;` / `mod name { … }` declaration in `src`.
///
/// Scans the original text but only accepts `mod` where [`code_mask`] says the
/// byte is code, so prose and literals cannot forge a declaration while every
/// reported offset stays comparable with `#[path]` attribute offsets.
fn scan_mods(src: &str, mask: &[bool]) -> Vec<ModDecl> {
    let path_attrs = extract_path_attrs(src);
    let bytes = src.as_bytes();
    let mut decls = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if !mask[i] {
            i += 1;
            continue;
        }
        // Word boundary check for `mod`.
        let at_word_start = i == 0
            || !(mask[i - 1] && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'));
        if at_word_start && bytes[i..].starts_with(b"mod") {
            let after = i + 3;
            // Must be followed by whitespace (not `model`, `modify`, …).
            if after < bytes.len() && bytes[after].is_ascii_whitespace() {
                let mut j = after;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                // Module name (ASCII identifier, so byte slicing is safe).
                let name_start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                if j > name_start {
                    let name = String::from_utf8_lossy(&bytes[name_start..j]).to_string();
                    let mut k = j;
                    while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    let external = k < bytes.len() && bytes[k] == b';';
                    // The nearest preceding `#[path = "…"]` is this
                    // declaration's override (attributes precede their item).
                    let path_attr = path_attrs
                        .iter()
                        .rfind(|(at, _)| *at < i)
                        .map(|(_, value)| value.clone());
                    decls.push(ModDecl {
                        name,
                        external,
                        path_attr,
                    });
                    i = k;
                    continue;
                }
            }
        }
        i += 1;
    }
    decls
}

/// Candidate files for `mod name;` declared in `declaring_file`.
///
/// The candidate paths `mod <name>;` may resolve to, **in Rust's own priority
/// order**.
///
/// Module resolution for an out-of-line `mod name;` is not a free choice among
/// equally valid paths — Rust picks exactly one, and picking the wrong one here
/// makes the whole check both unsound and useless:
///
/// - For a **directory module** (`foo/mod.rs`), `mod bar;` resolves to
///   `foo/bar.rs` or `foo/bar/mod.rs`. The declaring file's directory is
///   authoritative.
/// - For a **non-`mod.rs` file** (`foo.rs`), Rust 2018+ (RFC 2126) first looks
///   in the sibling directory `foo/` — i.e. `foo/bar.rs` / `foo/bar/mod.rs` —
///   and only then falls back to `foo`'s own directory. A same-named file
///   beside `foo.rs` is **not** a candidate at all when `foo.rs` has children:
///   it would be ambiguous with `foo/bar.rs`, and Rust resolves the ambiguity by
///   never considering the beside-file form.
///
/// Returning the beside-file form *first* for `foo.rs` is the bug this ordering
/// fixes: with both `foo/bar.rs` and `bar.rs` on disk, the checker walked the
/// beside-file, marked it reachable, and reported `foo/bar.rs` — the file Rust
/// actually compiles — as orphaned. Worse, it would have silently blessed a
/// `bar.rs` that the compiler ignores.
fn resolve_mod_file(declaring_file: &Path, decl: &ModDecl) -> Vec<PathBuf> {
    let dir = declaring_file.parent().unwrap_or(Path::new("."));
    if let Some(path_attr) = &decl.path_attr {
        return vec![dir.join(path_attr)];
    }

    let is_dir_module = (declaring_file.file_name().and_then(|n| n.to_str()) == Some("mod.rs"))
        || matches!(
            declaring_file.file_stem().and_then(|s| s.to_str()),
            Some("lib" | "main")
        );

    if is_dir_module {
        // The declaring file's directory is the module root.
        return vec![
            dir.join(format!("{}.rs", decl.name)),
            dir.join(&decl.name).join("mod.rs"),
        ];
    }

    // A non-`mod.rs` file: RFC 2126 puts the module's own children in the
    // sibling directory first, and only then in the declaring file's directory.
    match declaring_file.file_stem().and_then(|s| s.to_str()) {
        Some(stem) if !stem.is_empty() => {
            let sibling = dir.join(stem);
            vec![
                sibling.join(format!("{}.rs", decl.name)),
                sibling.join(&decl.name).join("mod.rs"),
                dir.join(format!("{}.rs", decl.name)),
                dir.join(&decl.name).join("mod.rs"),
            ]
        }
        _ => vec![
            dir.join(format!("{}.rs", decl.name)),
            dir.join(&decl.name).join("mod.rs"),
        ],
    }
}

/// Determine which files a crate's targets make reachable.
fn reachable_files(manifest_dir: &Path) -> BTreeSet<PathBuf> {
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();

    for root in crate_entry_points(manifest_dir) {
        if root.is_file() {
            queue.push_back(root);
        }
    }

    while let Some(file) = queue.pop_front() {
        let canonical = normalize(&file);
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let Ok(source) = fs::read_to_string(&canonical) else {
            continue;
        };
        let mask = code_mask(&source);
        for decl in scan_mods(&source, &mask) {
            if !decl.external {
                // Inline module: its file is the declaring one, already seen.
                // Its external children resolve relative to this file, which
                // the candidates below already express.
                continue;
            }
            for candidate in resolve_mod_file(&canonical, &decl) {
                if candidate.is_file() {
                    queue.push_back(candidate);
                    break;
                }
            }
        }
    }
    seen
}

/// Every entry point Cargo would compile for this crate.
fn crate_entry_points(manifest_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let src = manifest_dir.join("src");
    for root in ["lib.rs", "main.rs"] {
        let candidate = src.join(root);
        if candidate.is_file() {
            roots.push(candidate);
        }
    }
    // Cargo's auto-discovered integration/bench/example targets: every `.rs`
    // directly inside the target directory is its own crate root. Files in a
    // *subdirectory* of a target directory are that target's module tree
    // (`tests/foo/bar.rs` reachable as `mod bar;` from `tests/foo.rs`), or —
    // when `foo.rs` does not exist — inert assets, which are not compiled.
    for dir in ["tests", "benches", "examples"] {
        let target_dir = manifest_dir.join(dir);
        let Ok(entries) = fs::read_dir(&target_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                roots.push(path);
            }
        }
    }
    roots
}

/// True when `path` sits inside a test-target *asset* tree that Cargo defines as
/// non-source.
///
/// Cargo treats `tests/<target>/…` as source modules only for the `tests/<target>.rs`
/// root that declares them. When no such root exists, the directory is data the
/// tests read (a parser corpus, a fixture, a sample file the code under test
/// ingests) — `.rs` files there are *inputs*, not crate modules. A corpus of
/// fixture sources is exactly what a Rust-parsing tool needs, so it must not be
/// mistaken for an undeclared module.
fn is_target_asset(path: &Path, crate_dir: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(crate_dir) else {
        return false;
    };
    let mut components = rel.components();
    let Some(std::path::Component::Normal(first)) = components.next() else {
        return false;
    };
    let first = first.to_string_lossy();
    if !matches!(first.as_ref(), "tests" | "benches" | "examples") {
        return false;
    }
    let Some(std::path::Component::Normal(target)) = components.next() else {
        // `tests/foo.rs` itself: a real root, not an asset.
        return false;
    };
    // `tests/<target>/…` is source only when `tests/<target>.rs` declares it.
    !crate_dir
        .join(first.as_ref())
        .join(format!("{}.rs", target.to_string_lossy()))
        .is_file()
}

/// Lexically normalize a path (resolve `.`/`..` and make it absolute) without
/// touching the filesystem, so symlinked or missing components cannot confuse
/// the residency comparison.
fn normalize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Crate manifest directories under the tier roots.
fn crate_dirs(root: &Path) -> Vec<PathBuf> {
    let mut crates = Vec::new();
    for tier in TIERS {
        let tier_dir = root.join(tier);
        let Ok(entries) = fs::read_dir(&tier_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if dir.is_dir() && dir.join("Cargo.toml").is_file() {
                crates.push(dir);
            }
        }
    }
    crates.sort();
    crates
}

/// Verify that every `.rs` file in the workspace is reachable from a crate
/// root by following `mod` declarations.
///
/// Returns every orphan grouped by crate, so one run reports the whole backlog
/// instead of stopping at the first offender.
pub fn check_source_residency(root: &Path) -> Result<(), String> {
    let mut orphans: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut stray: Vec<String> = Vec::new();

    for crate_dir in crate_dirs(root) {
        let crate_name = crate_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| crate_dir.display().to_string());
        let reachable = reachable_files(&crate_dir);

        for entry in WalkDir::new(&crate_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let normalized = normalize(path);
            if reachable.contains(&normalized) {
                continue;
            }
            if is_target_asset(path, &crate_dir) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            orphans.entry(crate_name.clone()).or_default().insert(rel);
        }
    }

    // A `.rs` file outside every crate root (e.g. a stray at the workspace
    // root) can never be compiled by anything: it is a defect by construction.
    for entry in WalkDir::new(root)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("rs") {
            stray.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }

    if orphans.is_empty() && stray.is_empty() {
        return Ok(());
    }

    let mut report = String::from(
        "Source residency violations ([INV-ARCH-RESIDENCY-01], ADR-0124): every `.rs` file \
         must be reachable from a crate root by `mod` declarations.\n\
         An unreachable file is compiled by nothing: no error, no warning, no test failure.\n",
    );
    for (crate_name, files) in &orphans {
        report.push_str(&format!("\n  {crate_name}:\n"));
        for file in files {
            report.push_str(&format!("    {file}\n"));
        }
    }
    for file in &stray {
        report.push_str(&format!(
            "\n  outside any crate (uncompilable by construction):\n    {file}\n"
        ));
    }
    report.push_str(
        "\nFix: declare the module (add the missing `mod <name>;`) or delete the file. \
         Do not leave it in place.\n",
    );
    Err(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every candidate resolution shape for `mod bar;` in `foo.rs`.
    #[test]
    fn resolves_nested_module_forest_shapes() {
        let decl = ModDecl {
            name: "bar".to_string(),
            external: true,
            path_attr: None,
        };
        let candidates = resolve_mod_file(Path::new("/c/src/foo.rs"), &decl);
        let rendered: Vec<String> = candidates
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(
            rendered.contains(&"/c/src/bar.rs".to_string()),
            "flat sibling: {rendered:?}"
        );
        assert!(
            rendered.contains(&"/c/src/bar/mod.rs".to_string()),
            "directory module: {rendered:?}"
        );
        assert!(
            rendered.contains(&"/c/src/foo/bar.rs".to_string()),
            "RFC 2126 nested forest: {rendered:?}"
        );
    }

    /// The **order** of candidates must match Rust's own, because the walker
    /// stops at the first existing candidate.
    ///
    /// For a non-`mod.rs` file `foo.rs`, RFC 2126 resolves `mod bar;` in the
    /// sibling directory `foo/` **before** considering `bar.rs` beside `foo.rs`.
    /// Getting this backwards made the residency check mark the ignored
    /// beside-file as reachable and report the compiled `foo/bar.rs` as
    /// orphaned — a false positive that would also have blessed a file the
    /// compiler never reads.
    #[test]
    fn non_mod_file_prefers_the_sibling_directory_forest() {
        let decl = ModDecl {
            name: "bar".to_string(),
            external: true,
            path_attr: None,
        };
        let resolved: Vec<String> = resolve_mod_file(Path::new("/c/src/foo.rs"), &decl)
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let forest = resolved
            .iter()
            .position(|p| p == "/c/src/foo/bar.rs")
            .expect("the sibling-directory forest must be a candidate");
        let beside = resolved
            .iter()
            .position(|p| p == "/c/src/bar.rs")
            .expect("the beside-file form must remain a fallback");

        assert!(
            forest < beside,
            "RFC 2126 resolves `foo/bar.rs` before `bar.rs`; got {resolved:?}"
        );
    }

    /// A `mod.rs` (or a crate root) has no sibling forest: its children live in
    /// its own directory, so the beside-file form is the only form.
    #[test]
    fn directory_module_children_live_beside_the_mod_rs() {
        let decl = ModDecl {
            name: "bar".to_string(),
            external: true,
            path_attr: None,
        };
        // A `mod.rs` declares children in its own directory (`foo/`).
        let resolved: Vec<String> = resolve_mod_file(Path::new("/c/src/foo/mod.rs"), &decl)
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            resolved,
            vec![
                "/c/src/foo/bar.rs".to_string(),
                "/c/src/foo/bar/mod.rs".to_string()
            ],
            "a mod.rs resolves into its own directory only"
        );

        // A crate root's directory *is* its module root, so the child sits
        // directly in `src/`.
        for declaring in ["/c/src/lib.rs", "/c/src/main.rs"] {
            let resolved: Vec<String> = resolve_mod_file(Path::new(declaring), &decl)
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            assert_eq!(
                resolved,
                vec!["/c/src/bar.rs".to_string(), "/c/src/bar/mod.rs".to_string()],
                "{declaring} resolves children in `src/` only"
            );
        }
    }

    #[test]
    fn resolves_explicit_path_attribute() {
        let decl = ModDecl {
            name: "document_tests".to_string(),
            external: true,
            path_attr: Some("document_tests.rs".to_string()),
        };
        let candidates = resolve_mod_file(Path::new("/c/src/model/document.rs"), &decl);
        assert_eq!(
            candidates,
            vec![PathBuf::from("/c/src/model/document_tests.rs")],
            "a #[path] override names exactly one file, relative to the declaring file"
        );
    }

    /// A `mod` keyword inside prose or a string literal is not a declaration.
    #[test]
    fn ignores_declarations_in_comments_and_literals() {
        let source = r##"
// mod gone_in_line_comment;
/* mod gone_in_block /* nested */ comment; */
/// mod gone_in_doc;
fn f() -> &'static str { "mod gone_in_string;" }
const RAW: &str = r#"mod gone_in_raw_string;"#;
mod real_one;
"##;
        let mask = code_mask(source);
        let decls = scan_mods(source, &mask);
        let names: Vec<String> = decls
            .iter()
            .filter(|d| d.external)
            .map(|d| d.name.clone())
            .collect();
        assert_eq!(
            names,
            vec!["real_one".to_string()],
            "only real declarations count, got {names:?}"
        );
    }

    /// `model`, `modify`, `module` are identifiers, not `mod` declarations.
    #[test]
    fn ignores_mod_prefixed_identifiers() {
        let source = "fn model() {}\nstruct modify;\nlet module = 1;\nmod genuine;\n";
        let mask = code_mask(source);
        let decls = scan_mods(source, &mask);
        let names: Vec<String> = decls.iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["genuine".to_string()], "got {names:?}");
    }

    /// Inline modules own no file, but their external children are still found.
    #[test]
    fn finds_external_children_of_inline_modules() {
        let source = "mod outer { mod inner; }";
        let mask = code_mask(source);
        let decls = scan_mods(source, &mask);
        let rendered: Vec<(String, bool)> =
            decls.iter().map(|d| (d.name.clone(), d.external)).collect();
        assert_eq!(
            rendered,
            vec![("outer".to_string(), false), ("inner".to_string(), true)]
        );
    }

    #[test]
    fn path_attribute_is_recognised_next_to_other_attributes() {
        let source = "#[cfg(test)]\n#[path = \"doc_tests.rs\"]\nmod document_tests;\n";
        let mask = code_mask(source);
        let decls = scan_mods(source, &mask);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].path_attr.as_deref(), Some("doc_tests.rs"));
    }

    /// The residency walk must reach a file through a nested module forest.
    #[test]
    fn walk_reaches_nested_forest_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("alpha")).expect("mkdir");
        fs::write(src.join("lib.rs"), "mod alpha;\n").expect("write lib");
        fs::write(src.join("alpha.rs"), "mod beta;\n").expect("write alpha");
        fs::write(src.join("alpha").join("beta.rs"), "// leaf\n").expect("write beta");

        let reachable = reachable_files(tmp.path());
        assert!(
            reachable.contains(&normalize(&src.join("alpha").join("beta.rs"))),
            "RFC 2126 child must be reachable, got {reachable:?}"
        );
    }

    /// A file no declaration names is reported, not silently skipped.
    #[test]
    fn reports_an_unreachable_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("lib.rs"), "// nothing declared\n").expect("write lib");
        fs::write(src.join("orphan.rs"), "// unreachable\n").expect("write orphan");

        let reachable = reachable_files(tmp.path());
        assert!(
            !reachable.contains(&normalize(&src.join("orphan.rs"))),
            "an undeclared file must not be reachable"
        );
    }
}
