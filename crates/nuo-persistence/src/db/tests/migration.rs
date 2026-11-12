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
                fork_kind        TEXT NOT NULL DEFAULT 'trunk',
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

/// Databases already at the final version-5 schema (no interim stamp) pass
/// through the repair as a no-op.
#[test]
fn final_v5_schema_passes_through_migration_six_unchanged() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.execute_batch("PRAGMA user_version = 5;").unwrap();
    migrate_schema(&mut conn).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
}

/// Regression (2026-09-08 incident): migration 10 rebuilds `entries`, the
/// parent table of `entry_memberships`' foreign key. With foreign-key
/// enforcement left ON across the migration transaction, the
/// drop-and-rename swap incremented SQLite's deferred-constraint counter
/// and COMMIT failed with `FOREIGN KEY constraint failed` even though the
/// rebuilt state was perfectly referentially consistent. The transaction
/// rolled back, `user_version` never advanced, and every subsequent
/// open retried and re-failed the rebuild — wedging workspace-trust
/// persistence (the TUI stuck on "Trusting workspace...") until the state
/// file was recreated. The subagent must disable enforcement before `BEGIN`
/// (a no-op inside a transaction), restore it after, and gate the result
/// on `PRAGMA foreign_key_check`.
#[test]
fn migration_ten_rebuilds_parent_entries_with_membership_rows_present() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch(
        r#"
            -- Final version-5 shape with live transcript rows that
            -- satisfy the pre-migration CHECKs. Stamped v5 so the v6-v9 hooks
            -- build the real pre-v10 state on the way up (column repairs,
            -- membership/projection rebuilds, FTS backfill), exactly like a
            -- production database that never survived migration 10.
            CREATE TABLE sessions (
                id               TEXT PRIMARY KEY,
                parent_id        TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                fork_kind        TEXT NOT NULL DEFAULT 'trunk',
                title            TEXT,
                created_at_ms    INTEGER NOT NULL,
                updated_at_ms    INTEGER NOT NULL,
                project_root     TEXT NOT NULL,
                msg_count        INTEGER NOT NULL DEFAULT 0,
                last_user_prompt TEXT,
                digest           TEXT,
                data             TEXT,
                title_manual     BOOLEAN NOT NULL DEFAULT 0
            );
            INSERT INTO sessions (id, created_at_ms, updated_at_ms, project_root)
                VALUES ('s1', 1, 1, '/tmp');

            CREATE TABLE entries (
                id            TEXT PRIMARY KEY,
                kind          TEXT NOT NULL CHECK (kind IN ('message','state')),
                role          TEXT CHECK (role IN ('user','assistant','system','tool')),
                content       TEXT,
                origin        TEXT CHECK (origin IS NULL OR origin IN ('harness','checkpoint')),
                hidden        INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER NOT NULL,
                payload       TEXT NOT NULL,
                CHECK ( origin IS NULL OR hidden = 1 ),
                CHECK ( kind <> 'message' OR role IS NOT NULL )
            );
            INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
                VALUES ('e1', 'message', 'user', 'hello', NULL, 0, 1, '{"content":"hello"}'),
                       ('e2', 'message', 'assistant', 'hi', 'harness', 1, 2, '{"content":"hi"}'),
                       ('e3', 'message', 'user', 'ckpt', 'checkpoint', 1, 3, '{"content":"ckpt"}');

            CREATE TABLE entry_memberships (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                entry_id   TEXT NOT NULL REFERENCES entries(id),
                added_by   INTEGER NOT NULL,
                PRIMARY KEY (session_id, seq)
            );
            INSERT INTO entry_memberships (session_id, seq, entry_id, added_by)
                VALUES ('s1', 1, 'e1', 0), ('s1', 2, 'e2', 0), ('s1', 3, 'e3', 0);

            CREATE TABLE projections (
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq           INTEGER NOT NULL,
                kind          TEXT NOT NULL CHECK (kind IN ('prune','compact','freeze')),
                up_to_seq     INTEGER NOT NULL,
                payload       TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY (session_id, seq)
            );
            INSERT INTO projections (session_id, seq, kind, up_to_seq, payload, created_at_ms)
                VALUES ('s1', 1, 'freeze', 2, '{}', 3);

            CREATE VIRTUAL TABLE fts_entries USING fts5(
                entry_id UNINDEXED, session_id UNINDEXED, role UNINDEXED, content,
                tokenize = 'porter unicode61'
            );

            PRAGMA user_version = 5;
            "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);

    // Enforcement is a per-connection invariant installed by
    // `configure_connection`; the subagent must hand the connection back
    // with it restored.
    let fk_enforced: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk_enforced, 1);

    // The rebuild preserved every row byte-for-byte, including the
    // membership references into the swapped table.
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT COUNT(*) FROM entries"), 3);
    assert_eq!(count("SELECT COUNT(*) FROM entry_memberships"), 3);
    assert_eq!(count("SELECT COUNT(*) FROM projections"), 1);
    // FTS backfill rode along through the v7 hook.
    assert_eq!(count("SELECT COUNT(*) FROM fts_entries"), 3);

    // Referential integrity holds with enforcement back ON.
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);

    // The migration-10 CHECKs accept the visible harness provenance the
    // v5 schema rejected (the integrity fix the rebuild ships).
    conn.execute(
        "INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
             VALUES ('e4', 'message', 'user', 'steer', 'harness', 0, 4, '{}')",
        [],
    )
    .unwrap();
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

/// The migration-31 lock: no trigger on the transcript may write the derived
/// FTS table.
///
/// Migration 28 dropped four names, but migration 24 had re-created the
/// membership pair as `trg_entries_membership_*`. They survived, so every
/// commit indexed FTS inline and a full rewrite scanned the whole FTS table
/// through the `UNINDEXED` session column — the seconds-long `saving request`
/// stall. The sweep also covers a name the list does not know.
#[test]
fn migration_31_drops_every_legacy_fts_write_trigger() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.execute_batch(
        r#"
        CREATE TRIGGER trg_entries_membership_ai AFTER INSERT ON entry_memberships BEGIN
            INSERT INTO fts_entries(rowid, entry_id, session_id, role, content)
            SELECT new.rowid, new.entry_id, new.session_id, e.role, e.content
            FROM entries e WHERE e.id = new.entry_id;
        END;
        CREATE TRIGGER trg_entries_membership_ad AFTER DELETE ON entry_memberships BEGIN
            DELETE FROM fts_entries WHERE rowid = old.rowid;
        END;
        -- A trigger spelling the named list cannot anticipate.
        CREATE TRIGGER trg_unexpected_fts_write AFTER INSERT ON entries BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            VALUES (new.id, '', '', '');
        END;
        PRAGMA user_version = 30;
        "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    let leftover: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
              WHERE type = 'trigger'
                AND tbl_name IN ('entries', 'entry_memberships')
                AND lower(sql) LIKE '%fts_entries%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0, "a transcript trigger still writes FTS");
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
