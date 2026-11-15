//! Schema shape scenarios for the baseline database.

use super::*;

/// One durable conversation shape: facts plus branch cursors, no retired
/// transcript tables and no migration machinery from the pre-baseline chain.
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
    for expected in [
        "schema_migrations",
        "threads",
        "thread_policies",
        "thread_branches",
        "causal_nodes",
        "thread_blob_refs",
        "thread_commands",
        "request_projections",
        "workspace_effects",
        "fts_thread_facts",
    ] {
        assert!(
            tables.iter().any(|name| name == expected),
            "missing baseline table {expected}: {tables:?}"
        );
    }
    for retired in [
        "sessions",
        "entries",
        "entry_memberships",
        "projections",
        "thread_revisions",
        "asset_attestations",
    ] {
        assert!(
            !tables.iter().any(|name| name == retired),
            "retired table {retired} leaked into the baseline"
        );
    }
}

#[test]
fn baseline_facts_are_thread_scoped_and_strict() {
    let conn = open_in_memory_for_tests().unwrap();

    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('causal_nodes')")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    assert!(columns.iter().any(|column| column == "thread_id"));
    assert!(!columns.iter().any(|column| column == "session_id"));

    let strict: i64 = conn
        .query_row(
            "SELECT strict FROM pragma_table_list WHERE name = 'threads'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(strict, 1, "baseline tables must be STRICT");
}

/// A thread delete must not leave per-thread rows behind; the baseline declares
/// the cascade instead of relying on every deletion path to remember each table.
#[test]
fn thread_delete_cascades_per_thread_state() {
    let conn = open_in_memory_for_tests().unwrap();
    conn.execute(
        "INSERT INTO threads(id, created_at_ms, updated_at_ms) VALUES('t1', 1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO thread_policies(thread_id, workspace_root, updated_at_ms)
         VALUES('t1', '/tmp', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms)
         VALUES('f1', 't1', 1, 'dialogue', '{}', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO thread_commands(thread_id, command_id, ack_json, created_at_ms)
         VALUES('t1', 'c1', '{}', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO request_projections(thread_id, round, turn, created_at_ms, payload)
         VALUES('t1', 0, 0, 1, '{}')",
        [],
    )
    .unwrap();

    conn.execute("DELETE FROM threads WHERE id = 't1'", [])
        .unwrap();

    for table in [
        "thread_policies",
        "causal_nodes",
        "thread_commands",
        "request_projections",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table} did not cascade with the thread");
    }
}
