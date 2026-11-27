//! Schema baseline, ledger, and clean-break tests.

use super::*;

#[test]
fn fresh_db_migrates_to_baseline() {
    let conn = initialize_in_memory_db().unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    assert_eq!(CURRENT_DB_VERSION, 9);

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
    assert_eq!(ledger_rows, 9);
}

#[test]
fn content_shard_migration_catalog_is_contiguous_and_checksum_stable() {
    use crate::db::migrations::content_shard::{
        BASELINE_CHECKSUM, CURRENT_CONTENT_SHARD_VERSION, MIGRATIONS as SHARD_MIGRATIONS,
        SHIPPED_CHECKSUMS,
    };
    let versions: Vec<u32> = SHARD_MIGRATIONS.iter().map(|m| m.version).collect();
    assert_eq!(
        versions,
        (1..=CURRENT_CONTENT_SHARD_VERSION).collect::<Vec<_>>()
    );
    assert_eq!(SHARD_MIGRATIONS[0].checksum(), BASELINE_CHECKSUM);
    assert_eq!(SHARD_MIGRATIONS.len(), SHIPPED_CHECKSUMS.len());
    for (migration, (version, name, checksum)) in
        SHARD_MIGRATIONS.iter().zip(SHIPPED_CHECKSUMS.iter())
    {
        assert_eq!(migration.version, *version);
        assert_eq!(migration.name, *name);
        assert_eq!(migration.checksum(), *checksum);
    }
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

/// Every shipped migration (v1..=CURRENT_DB_VERSION) is immutable. Editing any
/// already-shipped SQL file breaks existing user databases with a checksum
/// mismatch on the next open (ADR-0189).
#[test]
fn all_shipped_migration_checksums_are_stable() {
    let expected = crate::db::migrations::catalog::SHIPPED_CHECKSUMS;
    assert_eq!(
        MIGRATIONS.len(),
        expected.len(),
        "update SHIPPED_CHECKSUMS when appending a new migration"
    );
    for (migration, (version, name, checksum)) in MIGRATIONS.iter().zip(expected.iter()) {
        assert_eq!(migration.version, *version);
        assert_eq!(migration.name, *name);
        assert_eq!(
            migration.checksum(),
            *checksum,
            "shipped migration v{} ({}) changed; ship a new numbered migration instead",
            migration.version,
            migration.name
        );
    }
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
    assert_eq!(ledger_rows, MIGRATIONS.len() as i64);
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
    assert_eq!(
        ledger_rows,
        MIGRATIONS.len() as i64,
        "the catalog must be applied exactly once"
    );
}

#[test]
fn catalog_summary_is_materialized_and_backfilled() {
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
    // ADR-0244 re-materialises the catalog summary after migration 0002 dropped
    // it; the migration backfills from the fact log while both live in one DB.
    assert!(columns.iter().any(|column| column == "msg_count"));
    assert!(columns.iter().any(|column| column == "last_user_prompt"));

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

    initialize_schema(&mut conn, &NUO_SCHEMA, Some(&path)).unwrap();

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

#[test]
fn every_historical_version_upgrades_cleanly_to_current() {
    for from_version in 0..=CURRENT_DB_VERSION {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_to_for_tests(&mut conn, from_version).unwrap();

        if (1..=8).contains(&from_version) {
            conn.execute_batch(
                r#"
                INSERT INTO threads(id, title, status, origin, created_at_ms, updated_at_ms)
                    VALUES('t-matrix', 'Matrix Thread', 'idle', 'trunk', 1000, 2000);
                "#,
            )
            .unwrap();
        } else if from_version >= 9 {
            conn.execute_batch(
                r#"
                INSERT INTO threads(id, title, origin, created_at_ms, updated_at_ms)
                    VALUES('t-matrix', 'Matrix Thread', 'trunk', 1000, 2000);
                "#,
            )
            .unwrap();
        }
        if from_version >= 1 {
            conn.execute_batch(
                r#"
                INSERT INTO thread_policies(thread_id, workspace_root, system_role, updated_at_ms)
                    VALUES('t-matrix', '/repo/matrix', 'developer', 2000);
                INSERT INTO input_history(text, session_id, workspace, created_at_ms)
                    VALUES('latest prompt', 't-matrix', '/repo/matrix', 1300);
                INSERT INTO kv_store(key, value, updated_at)
                    VALUES('matrix:key', 'matrix:val', 1300);
                INSERT INTO usage_records(session_id, actor_id, round, turn, attempt, payload, day, recorded_at_ms, project, revision)
                    VALUES('t-matrix', 'main', 1, 1, 1, '{"status":"completed"}', '2026-01-01', 1300, 'matrix', 1);
                "#,
            )
            .unwrap();
        }
        if (1..=8).contains(&from_version) {
            conn.execute_batch(
                r#"
                INSERT INTO thread_branches(thread_id, branch_id, name, created_at_ms, updated_at_ms)
                    VALUES('t-matrix', 'main', 'main', 1000, 2000);
                INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms)
                    VALUES('f-1', 't-matrix', 1, 'dialogue',
                           '{"type":"message","message":{"role":"User","content":"first prompt"}}', 1100);
                INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms)
                    VALUES('f-2', 't-matrix', 2, 'observation',
                           '{"type":"observation","output":"ok"}', 1200);
                INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms)
                    VALUES('f-3', 't-matrix', 3, 'dialogue',
                           '{"type":"message","message":{"role":"user","content":"latest prompt"}}', 1300);
                INSERT INTO thread_commands(thread_id, command_id, ack_json, created_at_ms)
                    VALUES('t-matrix', 'cmd-1', '{"ok":true}', 1300);
                INSERT INTO workspace_effects(thread_id, round_id, turn_id, call_id, seq, tool_name, path, op, effect_class, created_at_ms)
                    VALUES('t-matrix', 'r1', 'turn1', 'c1', 1, 'write_file', '/repo/matrix/a.rs', 'create', 'file_bytes', 1300);
                "#,
            )
            .unwrap();
        }
        if (1..=3).contains(&from_version) {
            conn.execute(
                "INSERT INTO request_projections(thread_id, round, turn, created_at_ms, payload)
                 VALUES('t-matrix', 1, 1, 1300, '{\"prompt\":\"legacy\"}')",
                [],
            )
            .unwrap();
        }
        if from_version >= 3 {
            conn.execute(
                "INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
                 VALUES('developer', 'workspace', '/repo/matrix', 'execute_command', 'cargo test *', 'allow', 1300, 1300)",
                [],
            )
            .unwrap();
        }
        if from_version >= 5 {
            conn.execute(
                "INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms)
                 VALUES('t-matrix', 3, 1300)",
                [],
            )
            .unwrap();
        }

        migrate_schema(&mut conn).unwrap_or_else(|err| {
            panic!("upgrade from v{from_version} to v{CURRENT_DB_VERSION} failed: {err}")
        });

        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_DB_VERSION, "from_version={from_version}");

        let ledger_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            ledger_rows,
            MIGRATIONS.len() as i64,
            "from_version={from_version}"
        );

        for retired in [
            "request_projections",
            "causal_nodes",
            "thread_branches",
            "thread_blob_refs",
            "command_acts",
            "workspace_effects",
            "fts_thread_facts",
            "thread_commands",
            "thread_leases",
            "thread_lease_counters",
            "thread_blobs",
        ] {
            assert!(
                !table_exists(&conn, retired).unwrap(),
                "{retired} must be retired from nuo.db after upgrade from v{from_version} to v{CURRENT_DB_VERSION}"
            );
        }
        assert!(table_exists(&conn, "permission_rules").unwrap());
        assert!(table_exists(&conn, "projection_watermarks").unwrap());
        assert_referential_integrity(&conn).unwrap();

        if from_version >= 1 {
            let (msg_count, last_prompt): (i64, Option<String>) = conn
                .query_row(
                    "SELECT msg_count, last_user_prompt FROM thread_list_view WHERE id = 't-matrix'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            if from_version < 6 {
                // v6 backfill materializes count & prompt from causal_nodes before v9 drops it.
                assert_eq!(msg_count, 3, "from_version={from_version}");
                assert_eq!(
                    last_prompt.as_deref(),
                    Some("latest prompt"),
                    "from_version={from_version}"
                );
            }
            let kv: String = conn
                .query_row(
                    "SELECT value FROM kv_store WHERE key = 'matrix:key'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(kv, "matrix:val");
            let usage: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM usage_records WHERE session_id = 't-matrix'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(usage, 1);
        }
        if from_version >= 3 {
            let perms: i64 = conn
                .query_row("SELECT COUNT(*) FROM permission_rules", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(perms, 1);
        }
        if from_version >= 5 {
            let watermark: i64 = conn
                .query_row(
                    "SELECT seq FROM projection_watermarks WHERE thread_id = 't-matrix'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(watermark, 3);
        }
    }
}

#[test]
fn concurrent_openers_upgrade_an_existing_database_cleanly() {
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nuo.db");
    {
        let mut conn = Connection::open(&path).unwrap();
        migrate_to_for_tests(&mut conn, 1).unwrap();
        conn.execute_batch(
            "INSERT INTO threads(id, created_at_ms, updated_at_ms) VALUES('t-conc', 1, 1);
             INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms)
                 VALUES('f-conc', 't-conc', 1, 'dialogue',
                        '{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"race\"}}', 1);",
        )
        .unwrap();
    }

    let workers = 4;
    let barrier = Arc::new(Barrier::new(workers));
    let mut handles = Vec::new();
    for _ in 0..workers {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            initialize_db(&path).map(|_| ())
        }));
    }

    for handle in handles {
        handle
            .join()
            .unwrap()
            .expect("concurrent upgrade must succeed without backup-path or lock collisions");
    }

    let conn = initialize_db(&path).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ledger_rows, MIGRATIONS.len() as i64);
    let (count, prompt): (i64, Option<String>) = conn
        .query_row(
            "SELECT msg_count, last_user_prompt FROM thread_list_view WHERE id = 't-conc'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(prompt.as_deref(), Some("race"));
}
