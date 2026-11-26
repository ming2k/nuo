//! Migration body, runner, ledger, verifier, and secondary-schema unit tests.
//!
//! These exercise the pieces a normal open cannot: the Rust and
//! SQL-then-Rust bodies, multi-step rollback, FK restoration and integrity
//! gates, pre-migration backup refusal, every ledger/verifier rejection branch,
//! and the `role_memory.db` contract.

use std::path::Path;

use rusqlite::Connection;

use super::apply::execute_body;
use super::catalog::{Body, Migration};
use super::history;
use super::runner::{
    assert_referential_integrity, configure_connection, initialize_db, initialize_schema,
    migrate_schema_with, table_exists,
};
use super::schema::Schema;
use super::verify;

#[test]
fn rust_body_runs_inside_the_migration_transaction() {
    let mut conn = Connection::open_in_memory().unwrap();
    let tx = conn.transaction().unwrap();
    execute_body(
        &tx,
        &Body::Rust(|tx| {
            tx.execute_batch("CREATE TABLE rust_body_probe(id INTEGER) STRICT;")?;
            Ok(())
        }),
    )
    .unwrap();
    tx.commit().unwrap();

    assert!(table_exists(&conn, "rust_body_probe").unwrap());
}

#[test]
fn sql_then_rust_runs_both_in_order() {
    let mut conn = Connection::open_in_memory().unwrap();
    let tx = conn.transaction().unwrap();
    execute_body(
        &tx,
        &Body::SqlThenRust {
            sql: "CREATE TABLE ordered_probe(id INTEGER) STRICT;",
            run: |tx| {
                tx.execute("INSERT INTO ordered_probe(id) VALUES(7)", [])?;
                Ok(())
            },
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let value: i64 = conn
        .query_row("SELECT id FROM ordered_probe", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, 7);
}

#[test]
fn a_failing_body_rolls_back_its_sql() {
    let mut conn = Connection::open_in_memory().unwrap();
    let tx = conn.transaction().unwrap();
    let result = execute_body(
        &tx,
        &Body::SqlThenRust {
            sql: "CREATE TABLE rollback_probe(id INTEGER) STRICT;",
            run: |tx| {
                tx.execute("INSERT INTO missing_table VALUES(1)", [])?;
                Ok(())
            },
        },
    );
    assert!(result.is_err());
    drop(tx);

    assert!(!table_exists(&conn, "rollback_probe").unwrap());
}

#[test]
fn checksum_covers_the_manual_rust_revision() {
    let first = Migration {
        version: 2,
        name: "same-name",
        code_revision: 1,
        body: Body::Rust(|_| Ok(())),
    };
    let second = Migration {
        version: 2,
        name: "same-name",
        code_revision: 2,
        body: Body::Rust(|_| Ok(())),
    };
    assert_ne!(first.checksum(), second.checksum());
}

#[test]
fn checksum_covers_sql_bytes() {
    let first = Migration {
        version: 2,
        name: "sql",
        code_revision: 1,
        body: Body::Sql("CREATE TABLE a(id INTEGER) STRICT;"),
    };
    let second = Migration {
        version: 2,
        name: "sql",
        code_revision: 1,
        body: Body::Sql("CREATE TABLE a(id INTEGER NOT NULL) STRICT;"),
    };
    assert_ne!(first.checksum(), second.checksum());
}

#[test]
fn a_baseline_database_matches_the_expected_schema() {
    let conn = super::runner::open_in_memory_for_tests().unwrap();
    verify::verify(&conn, &super::catalog::NUO_SCHEMA).unwrap();
}

#[test]
fn runner_executes_rust_and_sql_then_rust_migrations_and_verifies_schema() {
    static V1_ONLY: &[Migration] = &[Migration {
        version: 1,
        name: "baseline",
        code_revision: 1,
        body: Body::Sql("CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT) STRICT;"),
    }];
    static V3_FULL: &[Migration] = &[
        Migration {
            version: 1,
            name: "baseline",
            code_revision: 1,
            body: Body::Sql("CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT) STRICT;"),
        },
        Migration {
            version: 2,
            name: "backfill_labels",
            code_revision: 1,
            body: Body::Rust(|tx| {
                tx.execute(
                    "UPDATE items SET label = 'backfilled' WHERE label IS NULL",
                    [],
                )?;
                Ok(())
            }),
        },
        Migration {
            version: 3,
            name: "add_score_and_populate",
            code_revision: 1,
            body: Body::SqlThenRust {
                sql: "ALTER TABLE items ADD COLUMN score INTEGER NOT NULL DEFAULT 0;",
                run: |tx| {
                    tx.execute("UPDATE items SET score = 42", [])?;
                    Ok(())
                },
            },
        },
    ];

    let v1_schema = Schema::new("custom.db", V1_ONLY, 1);
    let v3_schema = Schema::new("custom.db", V3_FULL, 3);

    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    migrate_schema_with(&mut conn, &v1_schema, None).unwrap();
    conn.execute("INSERT INTO items(id, label) VALUES(1, NULL)", [])
        .unwrap();

    migrate_schema_with(&mut conn, &v3_schema, None).unwrap();

    let (label, score): (String, i64) = conn
        .query_row("SELECT label, score FROM items WHERE id = 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(label, "backfilled");
    assert_eq!(score, 42);

    let ledger = history::read_applied(&conn).unwrap();
    assert_eq!(ledger.len(), 3);
    assert_eq!(ledger[1].name, "backfill_labels");
    assert_eq!(ledger[2].name, "add_score_and_populate");
}

#[test]
fn runner_rolls_back_entire_batch_and_restores_foreign_keys_when_a_step_fails() {
    static V1_ONLY: &[Migration] = &[Migration {
        version: 1,
        name: "baseline",
        code_revision: 1,
        body: Body::Sql("CREATE TABLE items(id INTEGER PRIMARY KEY) STRICT;"),
    }];
    static FAILING_V3: &[Migration] = &[
        Migration {
            version: 1,
            name: "baseline",
            code_revision: 1,
            body: Body::Sql("CREATE TABLE items(id INTEGER PRIMARY KEY) STRICT;"),
        },
        Migration {
            version: 2,
            name: "step_two_ok",
            code_revision: 1,
            body: Body::Sql("CREATE TABLE step_two_table(id INTEGER PRIMARY KEY) STRICT;"),
        },
        Migration {
            version: 3,
            name: "step_three_fails",
            code_revision: 1,
            body: Body::SqlThenRust {
                sql: "CREATE TABLE step_three_table(id INTEGER PRIMARY KEY) STRICT;",
                run: |tx| {
                    tx.execute("INSERT INTO does_not_exist VALUES(1)", [])?;
                    Ok(())
                },
            },
        },
    ];

    let v1_schema = Schema::new("rollback.db", V1_ONLY, 1);
    let failing_schema = Schema::new("rollback.db", FAILING_V3, 3);

    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    migrate_schema_with(&mut conn, &v1_schema, None).unwrap();
    conn.execute("INSERT INTO items(id) VALUES(99)", [])
        .unwrap();

    let err = migrate_schema_with(&mut conn, &failing_schema, None).unwrap_err();
    assert!(
        err.to_string().contains("does_not_exist"),
        "unexpected error: {err}"
    );

    // Both step 2 and step 3 must be rolled back completely.
    assert!(!table_exists(&conn, "step_two_table").unwrap());
    assert!(!table_exists(&conn, "step_three_table").unwrap());
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let ledger = history::read_applied(&conn).unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].version, 1);

    // Pre-existing row survives untouched.
    let surviving: i64 = conn
        .query_row("SELECT COUNT(*) FROM items WHERE id = 99", [], |r| r.get(0))
        .unwrap();
    assert_eq!(surviving, 1);

    // Foreign key enforcement must be restored to ON even after failure.
    let fk_enabled: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fk_enabled, 1, "PRAGMA foreign_keys must be restored to ON");
}

#[test]
fn runner_aborts_upgrade_when_pre_migration_backup_fails() {
    static V1_ONLY: &[Migration] = &[Migration {
        version: 1,
        name: "baseline",
        code_revision: 1,
        body: Body::Sql("CREATE TABLE items(id INTEGER PRIMARY KEY) STRICT;"),
    }];
    static V2_UPGRADE: &[Migration] = &[
        Migration {
            version: 1,
            name: "baseline",
            code_revision: 1,
            body: Body::Sql("CREATE TABLE items(id INTEGER PRIMARY KEY) STRICT;"),
        },
        Migration {
            version: 2,
            name: "add_extra",
            code_revision: 1,
            body: Body::Sql("CREATE TABLE extra(id INTEGER PRIMARY KEY) STRICT;"),
        },
    ];

    let v1_schema = Schema::new("backup_fail.db", V1_ONLY, 1);
    let v2_schema = Schema::new("backup_fail.db", V2_UPGRADE, 2);

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("backup_fail.db");
    let mut conn = Connection::open(&db_path).unwrap();
    configure_connection(&mut conn).unwrap();
    migrate_schema_with(&mut conn, &v1_schema, Some(&db_path)).unwrap();
    conn.execute("INSERT INTO items(id) VALUES(1)", []).unwrap();

    // Point the backup destination at a nonexistent directory so `VACUUM INTO`
    // deterministically fails regardless of OS user privileges.
    let unwritable = dir.path().join("missing_parent_dir").join("backup_fail.db");
    let err = migrate_schema_with(&mut conn, &v2_schema, Some(&unwritable))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("could not create pre-migration backup")
            && err.contains("refusing to migrate"),
        "expected pre-migration backup refusal, got: {err}"
    );

    // The database must remain untouched at v1.
    assert!(!table_exists(&conn, "extra").unwrap());
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

#[test]
fn post_migration_foreign_key_check_rejects_dangling_references() {
    const V1_SQL: &str = "CREATE TABLE parents(id INTEGER PRIMARY KEY) STRICT;
        CREATE TABLE children(
            id INTEGER PRIMARY KEY,
            parent_id INTEGER NOT NULL REFERENCES parents(id)
        ) STRICT;";
    static V1_ONLY: &[Migration] = &[Migration {
        version: 1,
        name: "baseline",
        code_revision: 1,
        body: Body::Sql(V1_SQL),
    }];
    static V2_BROKEN_FK: &[Migration] = &[
        Migration {
            version: 1,
            name: "baseline",
            code_revision: 1,
            body: Body::Sql(V1_SQL),
        },
        Migration {
            version: 2,
            name: "insert_orphan_while_fk_off",
            code_revision: 1,
            body: Body::Sql("INSERT INTO children(id, parent_id) VALUES(1, 999);"),
        },
    ];

    let v1_schema = Schema::new("fk_gate.db", V1_ONLY, 1);
    let v2_schema = Schema::new("fk_gate.db", V2_BROKEN_FK, 2);

    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    migrate_schema_with(&mut conn, &v1_schema, None).unwrap();

    let err = migrate_schema_with(&mut conn, &v2_schema, None)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("post-migration referential integrity check failed")
            && err.contains("children")
            && err.contains("parents"),
        "expected FK check refusal, got: {err}"
    );
    assert!(assert_referential_integrity(&conn).is_err());
}

#[test]
fn history_validate_rejects_every_ledger_anomaly() {
    // 1. Ledger has more rows than the binary's catalog.
    {
        let mut conn = super::runner::open_in_memory_for_tests().unwrap();
        conn.execute(
            "INSERT INTO schema_migrations(version, name, checksum, applied_at_ms, execution_ms)
             VALUES(99, 'future_migration', 'deadbeef', 1, 0)",
            [],
        )
        .unwrap();
        let err = history::validate(
            &conn,
            super::catalog::CURRENT_DB_VERSION,
            &super::catalog::NUO_SCHEMA,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("knows only"), "{err}");
        let _ = &mut conn;
    }

    // 2. Ledger row version skips/disagrees with catalog version.
    {
        let conn = super::runner::open_in_memory_for_tests().unwrap();
        conn.execute(
            "UPDATE schema_migrations SET version = 42 WHERE version = 2",
            [],
        )
        .unwrap();
        let err = history::validate(
            &conn,
            super::catalog::CURRENT_DB_VERSION,
            &super::catalog::NUO_SCHEMA,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("catalog expects v2") && err.contains("different migration history"),
            "{err}"
        );
    }

    // 3. Ledger row name disagrees with catalog name.
    {
        let conn = super::runner::open_in_memory_for_tests().unwrap();
        conn.execute(
            "UPDATE schema_migrations SET name = 'renamed_migration' WHERE version = 2",
            [],
        )
        .unwrap();
        let err = history::validate(
            &conn,
            super::catalog::CURRENT_DB_VERSION,
            &super::catalog::NUO_SCHEMA,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("renamed_migration") && err.contains("drop_derived_thread_columns"),
            "{err}"
        );
    }

    // 4. PRAGMA user_version disagrees with ledger's last version (while <= current_version).
    {
        let conn = super::runner::open_in_memory_for_tests().unwrap();
        let err = history::validate(
            &conn,
            super::catalog::CURRENT_DB_VERSION - 1,
            &super::catalog::NUO_SCHEMA,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("internally inconsistent"), "{err}");
    }
}

#[test]
fn structural_verifier_rejects_missing_extra_and_changed_schema_objects() {
    // Missing index
    {
        let conn = super::runner::open_in_memory_for_tests().unwrap();
        conn.execute_batch("DROP INDEX idx_permission_rules_lookup;")
            .unwrap();
        let err = verify::verify(&conn, &super::catalog::NUO_SCHEMA)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("missing schema object") && err.contains("idx_permission_rules_lookup"),
            "{err}"
        );
    }

    // Missing trigger on content shard (`CONTENT_SHARD_SCHEMA`)
    {
        let conn = super::runner::open_content_shard_in_memory_for_tests().unwrap();
        conn.execute_batch("DROP TRIGGER trg_causal_nodes_immutable;")
            .unwrap();
        let err = verify::verify(&conn, &super::content_shard::CONTENT_SHARD_SCHEMA)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("missing schema object") && err.contains("trg_causal_nodes_immutable"),
            "{err}"
        );
    }

    // Missing INV-EFFECT-04 trigger on content shard
    {
        let conn = super::runner::open_content_shard_in_memory_for_tests().unwrap();
        conn.execute_batch("DROP TRIGGER trg_workspace_effects_no_early_reversal;")
            .unwrap();
        let err = verify::verify(&conn, &super::content_shard::CONTENT_SHARD_SCHEMA)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("missing schema object")
                && err.contains("trg_workspace_effects_no_early_reversal"),
            "{err}"
        );
    }

    // Changed view SQL
    {
        let conn = super::runner::open_in_memory_for_tests().unwrap();
        conn.execute_batch(
            "DROP VIEW thread_list_view;
             CREATE VIEW thread_list_view AS SELECT id FROM threads;",
        )
        .unwrap();
        let err = verify::verify(&conn, &super::catalog::NUO_SCHEMA)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("thread_list_view") && err.contains("SQL differs from catalog SQL"),
            "{err}"
        );
    }

    // Changed table SQL (constraint dropped)
    {
        let conn = super::runner::open_in_memory_for_tests().unwrap();
        conn.execute_batch(
            "DROP TABLE projection_watermarks;
             CREATE TABLE projection_watermarks (
                 thread_id TEXT PRIMARY KEY,
                 seq INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             ) STRICT;",
        )
        .unwrap();
        let err = verify::verify(&conn, &super::catalog::NUO_SCHEMA)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("projection_watermarks") && err.contains("SQL differs from catalog SQL"),
            "{err}"
        );
    }
}

#[test]
fn file_open_refusal_includes_database_path() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy_nuo.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY);
             PRAGMA user_version = 38;",
        )
        .unwrap();
    }

    let err = initialize_db(&db_path).unwrap_err().to_string();
    assert!(
        err.contains(&db_path.display().to_string()),
        "ADR-0191 requires path-bearing refusal errors; got: {err}"
    );
    assert!(err.contains("migration ledger is missing"), "{err}");
}

#[test]
fn role_memory_uses_the_same_baseline_and_ledger_contract() {
    let mut conn = Connection::open_in_memory().unwrap();
    initialize_schema(&mut conn, &super::role_memory::ROLE_MEMORY_SCHEMA, None).unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, super::role_memory::CURRENT_DB_VERSION);
    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ledger_rows, 1);
    assert_eq!(
        super::role_memory::MIGRATIONS[0].checksum(),
        super::role_memory::BASELINE_CHECKSUM
    );
    let versions: Vec<u32> = super::role_memory::MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .collect();
    assert_eq!(
        versions,
        (1..=super::role_memory::CURRENT_DB_VERSION).collect::<Vec<_>>()
    );
    verify::verify(&conn, &super::role_memory::ROLE_MEMORY_SCHEMA).unwrap();
}

#[test]
fn role_memory_on_disk_reopen_and_refusals_follow_adr_0192() {
    let dir = tempfile::tempdir().unwrap();

    // 1. Fresh on-disk role_memory.db opens and reopens cleanly as a no-op.
    let fresh_path = dir.path().join("role_memory.db");
    {
        let mut conn = Connection::open(&fresh_path).unwrap();
        initialize_schema(
            &mut conn,
            &super::role_memory::ROLE_MEMORY_SCHEMA,
            Some(&fresh_path),
        )
        .unwrap();
    }
    {
        let mut conn = Connection::open(&fresh_path).unwrap();
        initialize_schema(
            &mut conn,
            &super::role_memory::ROLE_MEMORY_SCHEMA,
            Some(&fresh_path),
        )
        .unwrap();
        verify::verify(&conn, &super::role_memory::ROLE_MEMORY_SCHEMA).unwrap();
    }

    // 2. Tampered checksum in role_memory.db is refused with path.
    {
        let mut conn = Connection::open(&fresh_path).unwrap();
        conn.execute(
            "UPDATE schema_migrations SET checksum = 'bad' WHERE version = 1",
            [],
        )
        .unwrap();
        let err = initialize_schema(
            &mut conn,
            &super::role_memory::ROLE_MEMORY_SCHEMA,
            Some(&fresh_path),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(err.contains(&fresh_path.display().to_string()), "{err}");
    }

    // 3. Pre-baseline role_memory.db without ledger is refused with path.
    let legacy_path = dir.path().join("legacy_role_memory.db");
    super::test_fixtures::create_pre_baseline_role_memory_db(&legacy_path).unwrap();
    {
        let mut conn = Connection::open(&legacy_path).unwrap();
        let err = initialize_schema(
            &mut conn,
            &super::role_memory::ROLE_MEMORY_SCHEMA,
            Some(Path::new(&legacy_path)),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no migration ledger"), "{err}");
        assert!(err.contains(&legacy_path.display().to_string()), "{err}");
    }
}

#[test]
fn migration_observer_reports_backup_steps_and_verify_and_stays_silent_when_current() {
    use super::{
        CURRENT_DB_VERSION, MIGRATIONS, MigrationObserver, MigrationProgress,
        initialize_db_with_observer, migrate_to_for_tests,
    };
    use std::sync::{Arc, Mutex};

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("nuo.db");

    // Seed an existing database at v1 so opening it at CURRENT_DB_VERSION
    // exercises pre-migration backup, numbered migration steps, and verify.
    {
        let mut conn = Connection::open(&db_path).unwrap();
        migrate_to_for_tests(&mut conn, 1).unwrap();
    }

    let events: Arc<Mutex<Vec<MigrationProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let observer: MigrationObserver = Arc::new(move |progress| {
        captured.lock().unwrap().push(progress);
    });

    let conn = initialize_db_with_observer(&db_path, Some(&observer)).unwrap();
    drop(conn);

    let recorded = events.lock().unwrap().clone();
    let pending_count = (CURRENT_DB_VERSION - 1) as usize;
    assert_eq!(
        recorded.len(),
        1 + pending_count + 1,
        "expected BackingUp + {pending_count} Migrating + Verifying, got: {recorded:?}"
    );
    assert_eq!(
        recorded[0],
        MigrationProgress::BackingUp { target_version: 2 }
    );
    for (i, migration) in MIGRATIONS[1..].iter().enumerate() {
        assert_eq!(
            recorded[1 + i],
            MigrationProgress::Migrating {
                step: (i + 1) as u32,
                total: pending_count as u32,
                name: migration.name,
            }
        );
    }
    assert_eq!(recorded.last(), Some(&MigrationProgress::Verifying));

    // Reopening an already up-to-date database MUST emit zero observer events
    // (ADR-0266 `[INV-SRVPHASE-07]` fast-path purity).
    events.lock().unwrap().clear();
    let conn = initialize_db_with_observer(&db_path, Some(&observer)).unwrap();
    drop(conn);
    assert!(
        events.lock().unwrap().is_empty(),
        "up-to-date reopen must emit zero migration events"
    );
}
