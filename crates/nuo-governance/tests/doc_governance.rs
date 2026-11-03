//! Integration test target for documentation and invariant governance.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn test_doc_links() {
    let root = nuo_governance::find_workspace_root();
    if let Err(e) = nuo_governance::doc_links::check_doc_links(&root) {
        panic!("Doc link violations:\n{e}");
    }
}

#[test]
fn test_adr_lineage() {
    let root = nuo_governance::find_workspace_root();
    if let Err(e) = nuo_governance::adr_lineage::check_adr_lineage(&root) {
        panic!("ADR lineage violations:\n{e}");
    }
}

#[test]
fn test_invariant_tags() {
    let root = nuo_governance::find_workspace_root();
    if let Err(e) = nuo_governance::invariant_tags::check_invariant_tags(&root) {
        panic!("Invariant tag violations:\n{e}");
    }
}
