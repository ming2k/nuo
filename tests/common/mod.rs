//! Shared fixtures for the root integration and E2E entries.
//!
//! This module is compiled into both entry binaries. Keep it dependency-light:
//! process-wide sandboxes and path resolution belong in `nuo-test-support`;
//! this file is for repository-shape helpers that tests share across crates.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(dead_code)]

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

/// A crate directory under `providers/` or `tools/`.
pub fn tier_crate_dir(tier: &str, name: &str) -> PathBuf {
    workspace_root().join(tier).join(name)
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
