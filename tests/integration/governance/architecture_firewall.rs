//! Integration test target for presentation firewall and architectural topology.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn test_inv_arch_firewall_and_topology() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::firewall::check_presentation_firewall(&root) {
        panic!("Presentation firewall violations:\n{e}");
    }
}

#[test]
fn test_inv_arch_cli_lean_link() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::firewall::check_lean_cli_link(&root) {
        panic!("Lean CLI link violation:\n{e}");
    }
}

#[test]
fn test_inv_wire_census_matches_protocol_version() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::wire_census::check_wire_census(&root) {
        panic!("Wire surface census violation:\n{e}");
    }
}

/// `[INV-EVENT-01]` (ADR-0168): a turn-scoped event carries its own context.
/// The shell must not reintroduce a per-session position or timestamp map; a
/// missing `TurnStarted` would silently make every later row unaddressable.
#[test]
fn test_inv_event_01_turn_context_is_the_only_position_source() {
    let root = nuo::dev::find_workspace_root();
    let path = root.join("crates/nuo/src/shell/mod.rs");
    let source = std::fs::read_to_string(&path).expect("read the shell translator");

    for forbidden in ["positions_by_session", "turn_sent_at_by_session"] {
        assert!(
            !source.contains(forbidden),
            "ADR-0168 forbids the per-session `{forbidden}` side channel;              turn position and send time must ride `RoundScope::Turn`"
        );
    }
}

#[test]
fn test_inv_compaction_is_transcript_owned() {
    // [INV-CMP-04] A compaction is one durable `ThreadFact::Compaction`; the
    // branch horizon moves in the same commit. The path must not grow a second
    // durable write through the retired `SessionIR` APIs.
    let root = nuo::dev::find_workspace_root();
    let path = root.join("crates/engine/src/orchestration/compaction.rs");
    let source = std::fs::read_to_string(&path).expect("read orchestration source");
    let start = source
        .find("pub async fn compact_round_history_with_mode")
        .expect("compaction entry point is present");
    let end = source[start..]
        .find("\n}\n")
        .map(|offset| start + offset)
        .unwrap_or(source.len());
    let body = &source[start..end];
    assert!(
        !body.contains("commit_session_ir") && !body.contains("commit_session_delta"),
        "compaction wrote an IR delta; the checkpoint entry must be the only durable fact"
    );
    assert!(
        body.contains("compact_thread") && body.contains("commit_thread"),
        "compaction no longer commits the thread checkpoint"
    );
}

/// Rewrites `crates/nuo/src/dev/wire-census.txt` from the tree.
///
/// Ignored by default because it mutates the repository: run it deliberately, in the same
/// commit as a `PROTOCOL_VERSION` bump, after the wire surface has settled.
#[test]
#[ignore = "writes wire-census.txt; run deliberately after a protocol bump"]
fn bless_wire_census() {
    let root = nuo::dev::find_workspace_root();
    let path = nuo::dev::wire_census::bless_census(&root).expect("bless wire census");
    eprintln!("blessed {}", path.display());
}

#[test]
fn test_inv_arch_source_residency() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::residency::check_source_residency(&root) {
        panic!("Source residency violations:\n{e}");
    }
}

#[test]
fn test_inv_text_overflow_and_column_budget_firewall() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::text_overflow::check_text_overflow(&root) {
        panic!("Text overflow / column budget firewall violation:\n{e}");
    }
}

#[test]
fn test_inv_color_purity_firewall() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::color_purity::check_color_purity(&root) {
        panic!("Color purity firewall violation:\n{e}");
    }
}

/// `[INV-WS-01]` (ADR-0004, release-and-versioning.md §5): every external
/// package in `Cargo.lock` must resolve to an explicit registry or git source,
/// never a source-less local `[patch]` path override that would fail on a clean
/// CI runner without sibling directories.
#[test]
fn test_inv_ws_01_lockfile_has_no_unpinned_external_path_overrides() {
    let root = nuo::dev::find_workspace_root();
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("read Cargo.lock");
    let parsed: toml::Table = toml::from_str(&lock).expect("parse Cargo.lock");
    let packages = parsed
        .get("package")
        .and_then(|v| v.as_array())
        .expect("Cargo.lock [[package]] array");

    let mut workspace_names: std::collections::HashSet<String> =
        std::iter::once("nuo-tests".to_string()).collect();
    for entry in std::fs::read_dir(root.join("crates")).expect("read crates/") {
        let entry = entry.expect("dir entry");
        let manifest = entry.path().join("Cargo.toml");
        if manifest.is_file() {
            let text = std::fs::read_to_string(&manifest).expect("read crate manifest");
            let tbl: toml::Table = toml::from_str(&text).expect("parse crate manifest");
            if let Some(name) = tbl
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            {
                workspace_names.insert(name.to_string());
            }
        }
    }

    let mut unpinned = Vec::new();
    for pkg in packages {
        let Some(tbl) = pkg.as_table() else { continue };
        let Some(name) = tbl.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if workspace_names.contains(name) {
            continue;
        }
        let source = tbl.get("source").and_then(|v| v.as_str());
        if source.is_none() {
            unpinned.push(name.to_string());
        }
    }
    assert!(
        unpinned.is_empty(),
        "external crates in Cargo.lock resolved to local path overrides without a `source` \
         (comment out `[patch]` in `.cargo/config.toml` and pin the upstream tag/version before committing) \
         ([INV-WS-01]): {unpinned:?}"
    );
}
