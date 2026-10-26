// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn test_doc_links() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::doc_links::check_doc_links(&root) {
        panic!("Doc link violations:\n{e}");
    }
}

#[test]
fn test_adr_lineage() {
    let root = nuo::dev::find_workspace_root();
    if let Err(e) = nuo::dev::adr_lineage::check_adr_lineage(&root) {
        panic!("ADR lineage violations:\n{e}");
    }
}
