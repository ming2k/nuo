//! `[INV-ROUTING-01]` enforcement (ADR-0082, ADR-0102).
//!
//! "The model-routing pipeline exists in exactly one crate (`nuo-model-routing`);
//! consumers import it directly and MUST NOT host a duplicate module or re-export
//! it as a local path shim."
//!
//! A tagged System Invariant with no mechanism is only an aspiration, so this
//! test makes it a mechanism: it scans every workspace crate's sources and
//! asserts the canonical pipeline entrypoint is defined in exactly one crate. A
//! re-introduced duplicate fails the build.

// Tests panic on assertion failure by design; the workspace's unwrap/expect
// warnings are meant for production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// The canonical pipeline entrypoint (ADR-0102 §3).
const CANONICAL_ENTRYPOINT: &str = "fn build_routes";

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn pipeline_is_defined_in_exactly_one_crate() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");

    let mut definers: Vec<String> = Vec::new();
    for entry in fs::read_dir(&crates).unwrap().flatten() {
        let src = entry.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        if files.iter().any(|file| {
            fs::read_to_string(file)
                .map(|text| text.contains(CANONICAL_ENTRYPOINT))
                .unwrap_or(false)
        }) {
            definers.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    definers.sort();

    assert_eq!(
        definers,
        vec!["nuo-model-routing".to_string()],
        "the routing pipeline (`{CANONICAL_ENTRYPOINT}`) must be defined in exactly one crate [INV-ROUTING-01]; found in: {definers:?}"
    );
}
