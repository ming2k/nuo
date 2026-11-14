//! Schema baseline, ledger, and clean-break tests.

use super::*;

#[test]
fn fresh_db_migrates_to_baseline() {
    let conn = initialize_in_memory_db().unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    assert_eq!(CURRENT_DB_VERSION, 2);

    let (name, checksum): (String, String) = conn
        .query_row(
            "SELECT name, checksum FROM schema_migrations WHERE version = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(name, "baseline");
    assert_eq!(checksum, crate::db::migrations::catalog::BASELINE_CHECKSUM);

    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ledger_rows, 2);
}

#[test]
fn migration_catalog_is_contiguous() {
    let versions: Vec<u32> = MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .collect();
    assert_eq!(
        versions,
        (1..=CURRENT_DB_VERSION).collect::<Vec<_>>(),
        "migration versions must be contiguous and ordered"
    );
}

/// The baseline is immutable: editing it would silently stamp fresh databases
/// with a schema that no existing database can reproduce.
#[test]
fn baseline_checksum_is_stable() {
    assert_eq!(
        MIGRATIONS[0].checksum(),
        crate::db::migrations::catalog::BASELINE_CHECKSUM,
        "the baseline SQL changed; ship the change as a new numbered migration"
    );
}

#[test]
fn reopening_a_baseline_database_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nuo.db");

    drop(initialize_db(&path).unwrap());
    let conn = initialize_db(&path).unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    assert_eq!(ledger_rows, 2);
}

#[test]
fn upgrade_fixture_builder_can_start_before_the_baseline() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_to_for_tests(&mut conn, 0).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ledger_rows, 0);

    migrate_schema(&mut conn).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
}

#[test]
fn concurrent_openers_apply_the_baseline_once() {
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nuo.db");
    let barrier = Arc::new(Barrier::new(2));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            initialize_db(&path).map(|_| ())
        }));
    }
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let conn = initialize_db(&path).unwrap();
    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ledger_rows, 2, "the catalog must be applied exactly once");
}

#[test]
fn migration_2_removes_derived_columns_and_keeps_the_list_projection() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_to_for_tests(&mut conn, 1).unwrap();

    conn.execute_batch(
        r#"
        INSERT INTO threads(id, created_at_ms, updated_at_ms) VALUES('t1', 1, 1);
        INSERT INTO thread_policies(thread_id, workspace_root, updated_at_ms)
            VALUES('t1', '/tmp', 1);
        INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms)
            VALUES('f1', 't1', 1, 'dialogue',
                   '{"type":"message","message":{"role":"User","content":"hello"}}', 1);
        "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('threads')")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    assert!(!columns.iter().any(|column| column == "msg_count"));
    assert!(!columns.iter().any(|column| column == "last_user_prompt"));

    let (count, prompt): (i64, Option<String>) = conn
        .query_row(
            "SELECT msg_count, last_user_prompt FROM thread_list_view WHERE id = 't1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(prompt.as_deref(), Some("hello"));
}

#[test]
fn pending_migration_creates_a_pre_migration_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nuo.db");
    let mut conn = Connection::open(&path).unwrap();
    migrate_to_for_tests(&mut conn, 1).unwrap();
    conn.execute(
        "INSERT INTO threads(id, created_at_ms, updated_at_ms) VALUES('t1', 1, 1)",
        [],
    )
    .unwrap();

    crate::db::migrations::runner::migrate_schema_at(&mut conn, Some(&path)).unwrap();

    let backups: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().contains(".pre-v2."))
        .collect();
    assert_eq!(backups.len(), 1, "expected one pre-v2 backup: {backups:?}");

    let backup = Connection::open(&backups[0]).unwrap();
    let version: u32 = backup
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1, "the backup must be the pre-migration state");
    let threads: i64 = backup
        .query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))
        .unwrap();
    assert_eq!(threads, 1);
}

#[test]
fn retired_pre_baseline_database_is_refused() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY);
         PRAGMA user_version = 38;",
    )
    .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(error.contains("migration ledger is missing"), "{error}");
    assert!(error.contains("ADR-0189"), "{error}");
}

#[test]
fn unversioned_tables_are_refused() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY);")
        .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(error.contains("tables but no migration ledger"), "{error}");
}

#[test]
fn checksum_drift_is_refused() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.execute(
        "UPDATE schema_migrations SET checksum = 'tampered' WHERE version = 1",
        [],
    )
    .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(error.contains("checksum mismatch"), "{error}");
    assert!(error.contains("edited after it shipped"), "{error}");
}

#[test]
fn schema_drift_is_refused() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.execute_batch("CREATE TABLE unexpected_object (id TEXT PRIMARY KEY);")
        .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(
        error.contains("unexpected_object"),
        "the verifier must name the object that is not in the catalog: {error}"
    );
}

#[test]
fn newer_database_is_refused() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.pragma_update(None, "user_version", CURRENT_DB_VERSION + 1)
        .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(
        error.contains("newer than this binary"),
        "a newer database must be refused: {error}"
    );
}
