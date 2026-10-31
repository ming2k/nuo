//! Integration test target: assertions fail by panicking, so the production
//! no-panic lints are meaningless here (the exemption ends at the test boundary).
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

/// `[INV-ARCH-RESIDENCY-01]` (ADR-0124): every `.rs` file must be reachable from
/// a crate root by `mod` declarations.
///
/// This is the check whose absence let `crates/nuo/src/shell/snapshot_tests.rs`
/// — the crate's only painted-grid regression net — sit uncompiled through
/// months of geometry changes.
/// `[INV-WIRE-03]` (ADR-0136): the wire variant census and `PROTOCOL_VERSION`
/// agree, so a wire change cannot silently keep the old number.
///
/// ADR-0134 deleted the `RetryResolved` variant without bumping the protocol.
/// A server built before that commit kept answering protocol 18 and then
/// emitted a frame the client could not model, which the client reported as a
/// dead link and closed on. The receiving side is now resilient, but this is
/// the cure: the number has to move when the wire does.
#[test]
fn test_inv_wire_census_matches_protocol_version() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::wire_census::check_wire_census(&root) {
        panic!("Wire surface census violation:\n{e}");
    }
}

#[test]
fn test_inv_arch_source_residency() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::residency::check_source_residency(&root) {
        panic!("Source residency violations:\n{e}");
    }
}
