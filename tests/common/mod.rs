//! Shared fixtures for the root integration and E2E entries.
//!
//! This module is compiled into both entry binaries. Keep it dependency-light:
//! process-wide sandboxes and path resolution belong in `nuo-test-support`;
//! this file is for repository-shape helpers that tests share across crates.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The workspace root. The root `nuo-tests` package lives beside it, so
/// `CARGO_MANIFEST_DIR` is exactly the repository root.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A crate directory under `crates/`.
pub fn crate_dir(name: &str) -> PathBuf {
    workspace_root().join("crates").join(name)
}

/// Read a repository-relative text file.
pub fn read_repo_file(relative: impl AsRef<Path>) -> String {
    let path = workspace_root().join(relative.as_ref());
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The path of a repository-relative file.
pub fn repo_path(relative: impl AsRef<Path>) -> PathBuf {
    workspace_root().join(relative.as_ref())
}

/// Every package a repository-relative `Cargo.toml` depends on, by exact
/// package name.
///
/// Reads `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, and
/// their `[target.'cfg(..)'.*]` forms with a TOML parser. A renamed edge
/// (`alias = { package = "real" }`) reports the real package. Comments,
/// descriptions, and the package's own name are never matched, which is what
/// separates this from a substring search over the manifest text.
pub fn manifest_dependencies(relative: impl AsRef<Path>) -> BTreeSet<String> {
    const KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let relative = relative.as_ref();
    let manifest: toml::Table = toml::from_str(&read_repo_file(relative))
        .unwrap_or_else(|error| panic!("parse {}: {error}", relative.display()));
    let mut tables: Vec<&toml::Table> = KINDS
        .iter()
        .filter_map(|kind| manifest.get(*kind).and_then(|value| value.as_table()))
        .collect();
    if let Some(targets) = manifest.get("target").and_then(|value| value.as_table()) {
        for target in targets.values() {
            tables.extend(
                KINDS
                    .iter()
                    .filter_map(|kind| target.get(*kind).and_then(|value| value.as_table())),
            );
        }
    }
    tables
        .into_iter()
        .flat_map(|table| table.iter())
        .map(|(key, spec)| {
            spec.get("package")
                .and_then(|package| package.as_str())
                .unwrap_or(key)
                .to_string()
        })
        .collect()
}
