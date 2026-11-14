//! Legacy schema transition scenarios.

use super::*;

/// The clean-break destination: one durable fact table per thread.
#[test]
fn a_fresh_database_has_one_conversation_shape() {
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let tables: Vec<String> = {
        let mut stmt = engine
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    assert!(tables.iter().any(|name| name == "causal_nodes"));
    assert!(tables.iter().any(|name| name == "thread_branches"));
    assert!(tables.iter().any(|name| name == "thread_blob_refs"));
    assert!(tables.iter().any(|name| name == "fts_thread_facts"));
    assert!(!tables.iter().any(|name| name == "entries"));
    assert!(!tables.iter().any(|name| name == "entry_memberships"));
}
