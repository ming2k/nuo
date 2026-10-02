//! Guard: the web client's hand-maintained protocol mirror (ADR-0134) must
//! equal the Rust `PROTOCOL_VERSION`.
//!
//! ts-rs cannot export a `const`, so `apps/web/src/lib/stores/daemon.svelte.ts`
//! restates the wire number by hand — the one manual mirror of the value. This
//! test fails the moment the two drift apart, replacing the retired
//! `scripts/check-wire-compat.sh` mirror check with a durable in-tree guard.

use std::path::PathBuf;

/// The web client's `PROTOCOL_VERSION` constant, parsed from its source.
fn web_protocol_version() -> u32 {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../web/src/lib/stores/daemon.svelte.ts");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    // `const PROTOCOL_VERSION = 13;` — the declaration, not the doc-comment
    // mention (which reads "`PROTOCOL_VERSION` in `…/wire.rs`", never
    // "const PROTOCOL_VERSION").
    const ANCHOR: &str = "const PROTOCOL_VERSION";
    let after = source
        .split(ANCHOR)
        .nth(1)
        .unwrap_or_else(|| panic!("`{ANCHOR}` not found in {}", path.display()));
    let digits = after
        .split('=')
        .nth(1)
        .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("could not parse the value of `{ANCHOR}`"));
    digits
        .parse()
        .unwrap_or_else(|e| panic!("`{ANCHOR} = {digits}` is not a u32: {e}"))
}

#[test]
fn web_protocol_version_matches_rust() {
    assert_eq!(
        web_protocol_version(),
        nuo_contracts::PROTOCOL_VERSION,
        "apps/web/src/lib/stores/daemon.svelte.ts PROTOCOL_VERSION must equal \
         muta-contracts PROTOCOL_VERSION (ADR-0134)"
    );
}
