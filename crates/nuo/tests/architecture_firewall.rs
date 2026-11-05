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

#[test]
fn test_inv_arch_source_residency() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::residency::check_source_residency(&root) {
        panic!("Source residency violations:\n{e}");
    }
}

#[test]
fn test_inv_color_purity_firewall() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::color_purity::check_color_purity(&root) {
        panic!("Color purity firewall violation:\n{e}");
    }
}
