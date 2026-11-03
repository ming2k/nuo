//! Repository Documentation & Architectural Governance Engine for Nuo.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod adr_lineage;
pub mod color_purity;
pub mod config;
pub mod doc_links;
pub mod firewall;
pub mod invariant_tags;
pub mod residency;
pub mod wire_census;

pub use config::GovernanceConfig;

use std::path::PathBuf;

/// Locate the workspace root directory.
pub fn find_workspace_root() -> PathBuf {
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut candidate = PathBuf::from(manifest_dir);
        while candidate.pop() {
            let manifest = candidate.join("Cargo.toml");
            if manifest.exists() {
                if let Ok(content) = std::fs::read_to_string(&manifest) {
                    if content.contains("[workspace]") {
                        if let Ok(canonical) = candidate.canonicalize() {
                            return canonical;
                        }
                        return candidate;
                    }
                }
            }
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
