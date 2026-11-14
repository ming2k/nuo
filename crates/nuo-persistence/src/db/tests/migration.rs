//! Schema migration and fingerprint tests.

use super::*;

#[test]
fn fresh_db_migrates_to_latest_version() {
    let conn = initialize_in_memory_db().unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
}

/// Regression for the interim version-5 stamp: development builds of the
/// Tranche wrote `sessions` with `scheduled_jobs` and without the
/// final working-state columns, then stamped `user_version = 5`. The
/// migrator must reconcile such databases to the final schema instead of
/// letting runtime INSERTs fail with "no column named ...".
#[test]
fn interim_v5_stamp_is_repaired_to_final_schema() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch(
        r#"
            CREATE TABLE sessions (
                id               TEXT PRIMARY KEY,
                parent_id        TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                fork_kind     TEXT NOT NULL DEFAULT 'trunk',
                title            TEXT,
                created_at_ms    INTEGER NOT NULL,
                updated_at_ms    INTEGER NOT NULL,
                project_root     TEXT NOT NULL,
                msg_count        INTEGER NOT NULL DEFAULT 0,
                last_user_prompt TEXT,
                digest           TEXT,
                scheduled_jobs   TEXT NOT NULL DEFAULT '[]',
                provider_connection TEXT
            );
            INSERT INTO sessions (id, created_at_ms, updated_at_ms, project_root)
                VALUES ('s1', 1, 1, '/tmp');
            PRAGMA user_version = 5;
            "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    assert!(table_exists(&conn, "threads").unwrap());
    assert!(table_exists(&conn, "thread_policies").unwrap());
    assert!(!table_exists(&conn, "sessions").unwrap());
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM threads WHERE id = 's1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

/// The startup guard fails loudly on a database missing runtime columns.
#[test]
fn schema_guard_rejects_missing_columns() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY, persona TEXT NOT NULL);")
        .unwrap();
    let err = verify_sessions_schema(&conn).unwrap_err();
    assert!(err.to_string().contains("missing column(s)"), "{err}");
}

/// Migration 33 detaches the append-only workspace journal from `threads`.
///
/// A foreign key with `ON DELETE CASCADE` asks SQLite to delete the child rows,
/// but the journal's no-delete trigger correctly refuses. The rebuilt table has
/// no foreign key; thread deletion leaves the observations in place.
#[test]
fn migration_33_detaches_the_workspace_journal_from_threads() {
    let conn = initialize_in_memory_db().unwrap();
    let foreign_keys: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_list('workspace_effects')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(foreign_keys, 0, "the journal must outlive its thread");
    let no_delete: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
              WHERE type = 'trigger' AND name = 'trg_workspace_effects_no_delete'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(no_delete, 1, "append-only evidence is still enforced");
}

/// The post-RFC schema carries facts and the derived FTS index, not a
/// transcript table. This is the clean-break assertion: there is no second
/// durable conversation shape.
#[test]
fn migration_36_retires_the_transcript_tables() {
    let conn = initialize_in_memory_db().unwrap();
    assert!(!table_exists(&conn, "entries").unwrap());
    assert!(!table_exists(&conn, "entry_memberships").unwrap());
    assert!(!table_exists(&conn, "projections").unwrap());
    assert!(!table_exists(&conn, "blob_refs").unwrap());
    assert!(table_exists(&conn, "causal_nodes").unwrap());
    assert!(table_exists(&conn, "thread_blob_refs").unwrap());
    assert!(table_exists(&conn, "fts_thread_facts").unwrap());
}

/// Migration immutability lock: once a migration version has shipped in a
/// commit, editing its SQL in place strands databases stamped by the old
/// SQL (the interim-v5 incident). Any change to the catalog — including
/// edits disguised as refactors — must therefore land as a NEW version
/// entry, which changes this fingerprint and fails the test.
#[test]
fn migration_catalog_fingerprint_is_stable() {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    for migration in MIGRATIONS {
        hasher.update(migration.version.to_le_bytes());
        hasher.update(migration.sql.as_bytes());
    }
    let fingerprint = format!("{:x}", hasher.finalize());
    assert_eq!(
        fingerprint,
        MIGRATION_CATALOG_FINGERPRINT,
        "the migration catalog changed; ship the change as version {} \
             with new SQL instead of rewriting an applied migration",
        CURRENT_DB_VERSION + 1
    );
}
