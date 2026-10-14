//! Live-network end-to-end checks for the web tools.
//!
//! These are `#[ignore]`d by default (they hit the real network and depend on
//! direct network access). Run with:
//! `cargo test -p nuo-harness --test webtool_e2e -- --ignored`.
//!
//! Together they verify the two-stage research pipeline end to end:
//! `websearch` (breadth, via the configured search provider chain) finds
//! URLs, `read_url` (depth, via the configured reader) reads one of them.

// Failure paths are the interesting part of an E2E test, so panicking with
// the message beats propagating errors here (same rationale as the other
// integration tests).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nuo_harness::tools::{WebReaderTool, WebSearchTool};
use nuo_wire::{Tool, WebConfig, WebReaderProvider, WebRuntimeConfig};

/// Shape a config mirroring the developer workstation: direct access, Exa
/// one selected search provider plus the independently selected Jina reader.
fn proxied_config() -> WebRuntimeConfig {
    WebRuntimeConfig {
        behavior: WebConfig {
            timeout_secs: 30,
            reader: WebReaderProvider::Jina,
            ..WebConfig::default()
        },
        search_credential: None,
        reader_credential: None,
    }
}

#[tokio::test]
#[ignore = "live network"]
async fn jina_reader_works() {
    let cfg = proxied_config();
    let jina = WebReaderTool::with_config(cfg);
    let out = jina
        .call(r#"{"url":"https://example.com"}"#)
        .await
        .expect("jina read");
    assert!(
        out.to_lowercase().contains("example domain"),
        "got: {out:.200}"
    );
}

#[tokio::test]
#[ignore = "live network"]
async fn search_then_reader_pipeline_works() {
    let cfg = proxied_config();
    let search = WebSearchTool::with_config(cfg.clone());
    let results = search
        .call(r#"{"query":"rust async traits"}"#)
        .await
        .expect("websearch");
    assert!(results.contains("Search results"), "got: {results:.200}");
    assert!(results.contains("http"), "results should carry URLs");

    // Depth stage: read one of the returned documents through the reader.
    // Handles both result shapes: the blob backends (Exa/Parallel) emit
    // `URL: https://...` lines; the structured backends emit a numbered list
    // with the bare URL on its own indented line.
    let url = results
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            let candidate = trimmed
                .strip_prefix("URL:")
                .map(str::trim)
                .unwrap_or(trimmed);
            candidate.starts_with("https://").then(|| {
                candidate
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
        })
        .expect("a https URL in the search results");
    let reader = WebReaderTool::with_config(cfg);
    let page = reader
        .call(&format!(r#"{{"url":"{url}"}}"#))
        .await
        .expect("read_url of a search hit");
    assert!(!page.trim().is_empty(), "page body should not be empty");
}
