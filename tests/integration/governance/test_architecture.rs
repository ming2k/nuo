//! Mechanical governance for the layered test architecture (ADR-0178).
//!
//! The repository has exactly two external entry points:
//!
//! - `tests/integration/main.rs` (L2, one binary for every crate suite)
//! - `tests/e2e/main.rs` (L3, one binary for process-level flows)
//!
//! Every other test is an inline `#[cfg(test)] mod tests` or a sibling
//! `<module>/tests.rs` owned by the module under test. These checks make that
//! shape non-optional: a new per-crate `tests/` directory, an unexplained
//! ignored test, a shipped test-support dependency, or a missing E2E entry
//! fails the gate.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

const TIERS: &[&str] = &["crates", "providers", "tools"];

/// Crates admitted without any test, with the reason they are exempt.
///
/// Keep this empty unless a crate is genuinely unexecutable test data.
const EXEMPT_CRATES: &[(&str, &str)] = &[];

fn workspace_root() -> PathBuf {
    nuo::dev::find_workspace_root()
}

fn workspace_crates(root: &Path) -> Vec<PathBuf> {
    let mut crates = Vec::new();
    for tier in TIERS {
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
    if !dir.exists() {
        return Vec::new();
    }
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

fn test_count(crate_dir: &Path) -> usize {
    let mut count = 0;
    for path in rust_files(crate_dir) {
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        count += source
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                line.starts_with("#[test]")
                    || line.starts_with("#[test(")
                    || line.starts_with("#[tokio::test")
                    || line.starts_with("#[test_case")
            })
            .count();
    }
    count
}

#[test]
fn every_workspace_crate_owns_at_least_one_unit_test() {
    let root = workspace_root();
    let mut missing = Vec::new();
    for crate_dir in workspace_crates(&root) {
        let name = crate_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if EXEMPT_CRATES
            .iter()
            .any(|(exempt, _)| *exempt == name.as_str())
        {
            continue;
        }
        if test_count(&crate_dir) == 0 {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "every workspace crate must own at least one inline or sibling test \
         ([INV-TEST-01], ADR-0178). Missing: {missing:?}. External tests are \
         centralized under `tests/`; add a focused test beside the behaviour, or \
         record a real exemption in EXEMPT_CRATES."
    );
}

#[test]
fn every_crate_uses_inline_or_sibling_unit_tests_only() {
    let root = workspace_root();
    let mut stray = Vec::new();
    for crate_dir in workspace_crates(&root) {
        let tests = crate_dir.join("tests");
        if tests.exists() {
            stray.push(tests.strip_prefix(&root).unwrap_or(&tests).to_path_buf());
        }
    }
    assert!(
        stray.is_empty(),
        "per-crate `tests/` directories are retired; external suites live under \
         the root `tests/integration/` entry and unit tests stay beside the \
         module (`src/foo.rs` + `src/foo/tests.rs`) ([INV-TEST-04], ADR-0178): \
         {stray:?}"
    );
}

#[test]
fn large_inline_test_modules_are_externalized() {
    let root = workspace_root();
    let mut bloated = Vec::new();
    for crate_dir in workspace_crates(&root) {
        for path in rust_files(&crate_dir.join("src")) {
            let Ok(source) = fs::read_to_string(&path) else {
                continue;
            };
            let lines: Vec<&str> = source.lines().collect();
            let mut index = 0;
            while index < lines.len() {
                if lines[index].trim() != "#[cfg(test)]" {
                    index += 1;
                    continue;
                }
                let mut body = index + 1;
                while body < lines.len() && lines[body].trim().is_empty() {
                    body += 1;
                }
                if body >= lines.len() || !lines[body].trim_start().starts_with("mod tests {") {
                    index += 1;
                    continue;
                }
                let mut end = body;
                while end < lines.len() && lines[end] != "}" {
                    end += 1;
                }
                let span = end.saturating_sub(index) + 1;
                if span >= 300 {
                    bloated.push((
                        path.strip_prefix(&root).unwrap_or(&path).to_path_buf(),
                        index + 1,
                        span,
                    ));
                }
                index = end.saturating_add(1);
            }
        }
    }
    assert!(
        bloated.is_empty(),
        "inline test modules of 300+ lines are test-code bloat. Move each body \
         to the module's sibling `<module>/tests.rs` and keep only \
         `#[cfg(test)] mod tests;` ([INV-TEST-07]): {bloated:?}"
    );
}

#[test]
fn ignored_tests_declare_a_reason() {
    let root = workspace_root();
    let mut bare = Vec::new();
    for path in rust_files(&root) {
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        if source
            .lines()
            .any(|line| line.trim() == "#[ignore]" || line.trim() == "#[ignore =]")
        {
            bare.push(path.strip_prefix(&root).unwrap_or(&path).to_path_buf());
        }
    }
    assert!(
        bare.is_empty(),
        "`#[ignore]` must carry a reason (`#[ignore = \"live network\"]`) so \
         reviewers can tell a quarantine from a forgotten test ([INV-TEST-05], \
         ADR-0178): {bare:?}"
    );
}

#[test]
fn external_tests_have_exactly_two_entries() {
    let root = workspace_root();
    let integration = root.join("tests/integration/main.rs");
    let e2e = root.join("tests/e2e/main.rs");
    let common = root.join("tests/common/mod.rs");
    assert!(integration.is_file(), "missing `tests/integration/main.rs`");
    assert!(e2e.is_file(), "missing `tests/e2e/main.rs`");
    assert!(common.is_file(), "missing `tests/common/mod.rs`");

    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read root manifest");
    assert!(
        manifest.contains("name = \"nuo-tests\"")
            && manifest.contains("autotests = false")
            && manifest.contains("name = \"integration\"")
            && manifest.contains("name = \"e2e\""),
        "the root `nuo-tests` package must declare exactly the `integration` and \
         `e2e` entries with `autotests = false`"
    );

    // The common module is shared by both entries through an explicit path.
    let e2e_source = fs::read_to_string(&e2e).expect("read e2e entry");
    assert!(
        e2e_source.contains("#![cfg(unix)]"),
        "the process-level E2E entry is Unix-gated"
    );
    for path in rust_files(&root.join("tests/integration")) {
        if path == root.join("tests/integration/governance/test_architecture.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !source.contains("NUO_TEST_BIN") && !source.contains("CARGO_BIN_EXE_nuo"),
            "{} spawns the shipped binary from the integration tier; move it to \
             `tests/e2e/` ([INV-TEST-02], ADR-0178)",
            path.display()
        );
    }
    for path in rust_files(&root.join("tests/e2e")) {
        let source = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !source.contains("std::env::set_var") && !source.contains("env::set_var"),
            "{} mutates process-global environment; E2E tests run concurrently. \
             Pass the sandbox environment to the child process instead \
             ([INV-TEST-02], ADR-0178)",
            path.display()
        );
    }

    let headless = root.join("tests/e2e/headless_roundtrip.rs");
    let headless = fs::read_to_string(&headless).expect("read headless E2E module");
    assert!(
        headless.contains("#[test]")
            && headless.contains("binary_path")
            && headless.contains("MockProvider"),
        "the headless E2E test must run the fresh binary (`nuo_test_support::binary_path`) \
         against the hermetic mock provider; an E2E tier with no tests is not a tier"
    );
}

#[test]
fn test_support_stays_a_dev_dependency() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in WalkDir::new(&root)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "target" && entry.file_name() != ".git")
        .flatten()
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "Cargo.toml")
        .map(|entry| entry.into_path())
    {
        if path == root.join("Cargo.toml") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let mut section = String::new();
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                section = trimmed.to_string();
                continue;
            }
            let shipped_section =
                section.ends_with("dependencies") && !section.ends_with("dev-dependencies");
            if shipped_section && trimmed.starts_with("nuo-test-support") {
                violations.push(path.clone());
                break;
            }
        }
    }
    assert!(
        violations.is_empty(),
        "`nuo-test-support` is a dev-only harness and must never appear in \
         [dependencies] or [build-dependencies] ([INV-TEST-03], ADR-0178): \
         {violations:?}"
    );

    let root_manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read root manifest");
    assert!(
        root_manifest.contains("nuo-test-support = { workspace = true }"),
        "the root test host must dev-depend on nuo-test-support"
    );
    let kit_manifest = fs::read_to_string(root.join("crates/nuo-test-support/Cargo.toml"))
        .expect("read nuo-test-support manifest");
    assert!(
        kit_manifest.contains("publish = false"),
        "nuo-test-support is a repository-local harness and must never be published"
    );
}

#[test]
fn tier_runner_profiles_are_declared() {
    let root = workspace_root();
    let config = fs::read_to_string(root.join(".config/nextest.toml"))
        .expect("missing `.config/nextest.toml`; tier selection is not mechanical");
    for section in ["[profile.unit]", "[profile.integration]", "[profile.e2e]"] {
        assert!(
            config.contains(section),
            "missing nextest profile `{section}`; the test pyramid must be \
             selectable without hand-maintained package lists ([INV-TEST-04], \
             ADR-0178)"
        );
    }
    assert!(
        config.contains("default-filter") && config.contains("binary(e2e)"),
        "the e2e profile must select the `e2e` test binary explicitly"
    );
    assert!(
        config.contains("scripts.setup.nuo-binary") && config.contains("NUO_TEST_BIN"),
        "the e2e profile must build the host binary and export NUO_TEST_BIN; the \
         root test package cannot receive CARGO_BIN_EXE_nuo from Cargo"
    );
}

#[test]
fn snapshot_drift_is_not_committed() {
    let root = workspace_root();
    let mut stale = Vec::new();
    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "target" && entry.file_name() != ".git")
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name.ends_with(".snap.new") || name.ends_with(".pending-snap") {
            stale.push(
                entry
                    .path()
                    .strip_prefix(&root)
                    .unwrap_or(entry.path())
                    .to_path_buf(),
            );
        }
    }
    assert!(
        stale.is_empty(),
        "unreviewed insta snapshots must not be committed; run \
         `cargo insta review` and commit the accepted snapshot: {stale:?}"
    );
}
