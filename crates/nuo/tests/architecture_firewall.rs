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
    // [INV-CMP-04] A compaction is one transcript fact; the causal node is a
    // derive. The compaction path must not grow a second durable write through
    // the IR delta API — that is exactly the two-step commit this invariant
    // removed.
    let root = nuo::dev::find_workspace_root();
    let path = root.join("crates/nuo-engine/src/orchestration/compaction.rs");
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
        body.contains("commit_compaction"),
        "compaction no longer records the transcript checkpoint"
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
