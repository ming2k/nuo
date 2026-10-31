//! Developer and repository governance tools.
//!
//! Replaces legacy `scripts/` Python checkers with native Rust implementations
//! (ADR-0111).

pub mod adr_lineage;
pub mod doc_links;
pub mod firewall;
pub mod residency;
pub mod wire_census;

use std::path::PathBuf;

/// Locate the workspace root directory.
pub fn find_workspace_root() -> PathBuf {
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let candidate = PathBuf::from(manifest_dir).join("../..");
        if candidate.join("Cargo.toml").exists() {
            if let Ok(canonical) = candidate.canonicalize() {
                return canonical;
            }
            return candidate;
        }
    }

    let mut current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        let manifest = current.join("Cargo.toml");
        if manifest.exists() {
            if let Ok(content) = std::fs::read_to_string(&manifest) {
                if content.contains("[workspace]") {
                    return current;
                }
            }
        }
        if !current.pop() {
            break;
        }
    }

    PathBuf::from(".")
}
