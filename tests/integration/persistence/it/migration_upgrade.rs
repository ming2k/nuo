//! Integration tests for on-disk SQLite schema migrations through the real
//! persistence front door (`PersistenceHandle`, `DbReader`, `RoleMemoryStore`).

use std::path::Path;

use persistence::blobs::BlobStore;
use persistence::db::migrations::test_fixtures::{
    create_nuo_db_at_version, create_pre_baseline_nuo_db, create_pre_baseline_role_memory_db,
    execute_raw_sql, find_pre_migration_backups, inspect_db, query_scalar_i64, set_user_version,
    tamper_migration_checksum, verify_db_integrity,
};
use persistence::db::permissions::{PermissionRuleEffect, PermissionScopeTarget};
use persistence::db::{CURRENT_DB_VERSION, PersistenceHandle, WriterHealth};
use persistence::role_memory::RoleMemoryStore;

fn seed_sql_for_version(version: u32, workspace: &Path) -> String {
    let ws = workspace.display().to_string().replace('\'', "''");
    let mut sql = format!(
        r#"
        INSERT INTO threads(id, title, status, origin, created_at_ms, updated_at_ms)
            VALUES('t-hist', 'Historical Thread', 'idle', 'trunk', 1000, 4000);
        INSERT INTO thread_policies(thread_id, workspace_root, system_role, updated_at_ms)
            VALUES('t-hist', '{ws}', 'developer', 4000);
        INSERT INTO thread_branches(thread_id, branch_id, name, created_at_ms, updated_at_ms)
            VALUES('t-hist', 'main', 'main', 1000, 4000);
        INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms) VALUES
            ('n1', 't-hist', 1, 'dialogue',    '{{"type":"message","message":{{"role":"User","content":"initial question"}}}}', 1100),
            ('n2', 't-hist', 2, 'observation', '{{"type":"observation","output":"tool result"}}', 1200),
            ('n3', 't-hist', 3, 'dialogue',    '{{"type":"message","message":{{"role":"user","content":"latest question"}}}}', 1300),
            ('n4', 't-hist', 4, 'compaction',  '{{"type":"compaction","summary":"trimmed"}}', 1400);
        INSERT INTO thread_commands(thread_id, command_id, ack_json, created_at_ms)
            VALUES('t-hist', 'cmd-1', '{{"ok":true}}', 1300);
        INSERT INTO input_history(text, session_id, workspace, created_at_ms)
            VALUES('latest question', 't-hist', '{ws}', 1300);
        INSERT INTO kv_store(key, value, updated_at)
            VALUES('it:legacy_key', 'preserved-value', 1300);
        INSERT INTO workspace_effects(thread_id, round_id, turn_id, call_id, seq, tool_name, path, op, effect_class, created_at_ms)
            VALUES('t-hist', 'r1', 'turn1', 'c1', 1, 'write_file', '{ws}/src.rs', 'create', 'file_bytes', 1300);
        "#
    );

    if (1..=3).contains(&version) {
        sql.push_str(
            "INSERT INTO request_projections(thread_id, round, turn, created_at_ms, payload)
             VALUES('t-hist', 0, 0, 1300, '{\"messages\":[]}');",
        );
    }
    if version >= 3 {
        sql.push_str(&format!(
            "INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
             VALUES('developer', 'workspace', '{ws}', 'execute_command', 'cargo test *', 'allow', 1300, 1300);"
        ));
    }
    if version >= 5 {
        sql.push_str(
            "INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms)
             VALUES('t-hist', 4, 1400);",
        );
    }
    if version >= 6 {
        // v6 already materialized catalog summary columns on `threads`;
        // upgrading from v6 -> v7 preserves them without re-running v6's backfill.
        sql.push_str(
            "UPDATE threads SET msg_count = 3, last_user_prompt = 'latest question' WHERE id = 't-hist';",
        );
    }
    sql
}

#[tokio::test]
async fn all_historical_on_disk_versions_upgrade_through_persistence_handle_and_support_live_io() {
    for from_version in 1..CURRENT_DB_VERSION {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let db_path = dir.path().join("nuo.db");
        let blobs_dir = dir.path().join("blobs");

        let seed = seed_sql_for_version(from_version, &workspace);
        create_nuo_db_at_version(&db_path, from_version, Some(&seed)).unwrap();

        // Confirm fixture starts at the exact historical version.
        let before = inspect_db(&db_path).unwrap();
        assert_eq!(before.user_version, from_version);
        assert_eq!(before.ledger.len(), from_version as usize);

        // Open through the production single-writer PersistenceHandle.
        let handle =
            PersistenceHandle::spawn(db_path.clone(), Some(BlobStore::new(blobs_dir.clone())));
        handle
            .ensure_ready()
            .unwrap_or_else(|err| panic!("upgrade from v{from_version} failed: {err}"));
        assert_eq!(handle.health(), WriterHealth::Healthy);

        // Pre-migration snapshot must exist beside the DB and preserve the old version.
        let expected_next = from_version + 1;
        let backups = find_pre_migration_backups(&db_path, expected_next).unwrap();
        assert_eq!(
            backups.len(),
            1,
            "expected exactly one pre-v{expected_next} backup when upgrading from v{from_version}: {backups:?}"
        );
        let backup_info = inspect_db(&backups[0]).unwrap();
        assert_eq!(
            backup_info.user_version, from_version,
            "backup must preserve pre-migration user_version"
        );
        verify_db_integrity(&backups[0]).unwrap();
        let backup_threads = query_scalar_i64(&backups[0], "SELECT COUNT(*) FROM threads").unwrap();
        assert_eq!(backup_threads, 1);

        // Read back migrated historical data through the production DbReader.
        let reader = handle.reader().unwrap();
        let thread = reader
            .get_session("t-hist")
            .unwrap()
            .expect("historical thread must survive upgrade");
        assert_eq!(thread.title.as_deref(), Some("Historical Thread"));
        assert_eq!(thread.msg_count, 3, "from_version={from_version}");
        assert_eq!(
            thread.last_user_prompt.as_deref(),
            Some("latest question"),
            "from_version={from_version}"
        );
        assert_eq!(
            reader.get_kv("it:legacy_key").unwrap().as_deref(),
            Some("preserved-value")
        );
        let history = reader.load_input_history(10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].text, "latest question");

        if from_version >= 3 {
            let scope =
                PermissionScopeTarget::from_role_and_workspace("developer", Some(&workspace));
            let rules = reader.load_permission_rules(&scope).unwrap();
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].scope, "cargo test *");
        }
        if from_version >= 5 {
            assert_eq!(
                reader.catalog_projection_watermark("t-hist").unwrap(),
                Some(4)
            );
            // Content shards are not materialized yet for a pure SQLite
            // migration without journal catch-up, so the combined watermark
            // must report `None` so catch-up rebuilds the shard (ADR-0242).
            assert_eq!(reader.projection_watermark("t-hist").unwrap(), None);
        }
        drop(reader);

        // Verify live mutations succeed against the freshly migrated database.
        handle
            .set_kv("it:post_upgrade".to_string(), "ok".to_string())
            .await
            .unwrap();
        let scope = PermissionScopeTarget::from_role_and_workspace("ops", None);
        handle
            .upsert_permission_rule_blocking(
                scope.clone(),
                "execute_command".to_string(),
                "uptime".to_string(),
                PermissionRuleEffect::Allow,
            )
            .unwrap();
        handle
            .rename_session(
                "t-hist".to_string(),
                Some("Renamed After Upgrade".into()),
                true,
            )
            .await
            .unwrap();

        let reader = handle.reader().unwrap();
        assert_eq!(
            reader.get_kv("it:post_upgrade").unwrap().as_deref(),
            Some("ok")
        );
        assert_eq!(
            reader
                .get_session("t-hist")
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("Renamed After Upgrade")
        );
        assert_eq!(reader.load_permission_rules(&scope).unwrap().len(), 1);
        drop(reader);

        // Shut down the actor, then reopen to verify idempotency and no extra backup.
        drop(handle);
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;

        let reopened = PersistenceHandle::spawn(db_path.clone(), Some(BlobStore::new(blobs_dir)));
        reopened.ensure_ready().unwrap();
        assert_eq!(
            find_pre_migration_backups(&db_path, expected_next)
                .unwrap()
                .len(),
            1,
            "reopening an up-to-date DB must not create a second backup"
        );
        drop(reopened);

        let after = inspect_db(&db_path).unwrap();
        assert_eq!(after.user_version, CURRENT_DB_VERSION);
        assert_eq!(after.ledger.len(), CURRENT_DB_VERSION as usize);
        assert!(!after.tables.iter().any(|t| t == "request_projections"));
        assert!(after.tables.iter().any(|t| t == "permission_rules"));
        assert!(after.tables.iter().any(|t| t == "projection_watermarks"));
        verify_db_integrity(&db_path).unwrap();
    }
}

#[test]
fn persistence_handle_fails_fast_on_incompatible_or_corrupted_databases() {
    // 1. Retired pre-baseline v38 DB without schema_migrations ledger.
    {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("legacy_v38.db");
        create_pre_baseline_nuo_db(&db_path, 38).unwrap();

        let handle = PersistenceHandle::spawn(db_path.clone(), None);
        let err = handle.ensure_ready().unwrap_err();
        assert!(err.contains("migration ledger is missing"), "{err}");
        assert!(err.contains("ADR-0189"), "{err}");
        assert!(
            err.contains(&db_path.display().to_string()),
            "refusal must include DB path: {err}"
        );
        assert!(matches!(handle.health(), WriterHealth::Down { .. }));
        assert!(handle.reader().is_err());

        // File must remain untouched: never auto-deleted or overwritten.
        let info = inspect_db(&db_path).unwrap();
        assert_eq!(info.user_version, 38);
        assert_eq!(
            query_scalar_i64(&db_path, "SELECT COUNT(*) FROM threads").unwrap(),
            1
        );
    }

    // 2. Checksum tampering is refused with path.
    {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("tampered.db");
        create_nuo_db_at_version(&db_path, CURRENT_DB_VERSION, None).unwrap();
        tamper_migration_checksum(&db_path, 2, "0000deadbeef").unwrap();

        let handle = PersistenceHandle::spawn(db_path.clone(), None);
        let err = handle.ensure_ready().unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(err.contains(&db_path.display().to_string()), "{err}");
    }

    // 3. Structural drift (missing index) is refused and names the object + path.
    {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("drift_missing.db");
        create_nuo_db_at_version(&db_path, CURRENT_DB_VERSION, None).unwrap();
        execute_raw_sql(&db_path, "DROP INDEX idx_permission_rules_lookup;").unwrap();

        let handle = PersistenceHandle::spawn(db_path.clone(), None);
        let err = handle.ensure_ready().unwrap_err();
        assert!(err.contains("idx_permission_rules_lookup"), "{err}");
        assert!(err.contains(&db_path.display().to_string()), "{err}");
    }

    // 4. Newer schema version than this binary is refused with upgrade guidance.
    {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("future.db");
        create_nuo_db_at_version(&db_path, CURRENT_DB_VERSION, None).unwrap();
        set_user_version(&db_path, CURRENT_DB_VERSION + 1).unwrap();

        let handle = PersistenceHandle::spawn(db_path.clone(), None);
        let err = handle.ensure_ready().unwrap_err();
        assert!(err.contains("newer than this binary"), "{err}");
        assert!(err.contains(&db_path.display().to_string()), "{err}");
    }
}

#[test]
fn role_memory_store_on_disk_lifecycle_and_clean_break_refusal() {
    let dir = tempfile::tempdir().unwrap();

    // Fresh store opens, persists dialogue, reopens, and recalls it.
    let db_path = dir.path().join("role_memory.db");
    {
        let store = RoleMemoryStore::open(&db_path).unwrap();
        let entry = store
            .record_dialogue(
                "developer",
                Some("s1"),
                "how does schema migration work",
                "immutable baseline plus numbered migrations",
            )
            .unwrap();
        assert!(entry.is_some());
    }
    {
        let store = RoleMemoryStore::open(&db_path).unwrap();
        let recalled = store.recall("developer", "immutable baseline", 5).unwrap();
        assert_eq!(recalled.len(), 1);
        assert!(recalled[0].role_response.contains("numbered migrations"));
    }
    let info = inspect_db(&db_path).unwrap();
    assert_eq!(info.user_version, 1);
    assert_eq!(info.ledger.len(), 1);
    verify_db_integrity(&db_path).unwrap();

    // Legacy pre-baseline role_memory.db is refused without overwriting it.
    let legacy_path = dir.path().join("legacy_role_memory.db");
    create_pre_baseline_role_memory_db(&legacy_path).unwrap();
    let err = match RoleMemoryStore::open(&legacy_path) {
        Ok(_) => panic!("pre-baseline role_memory.db must be refused"),
        Err(error) => error.to_string(),
    };
    assert!(err.contains("no migration ledger"), "{err}");
    assert!(err.contains(&legacy_path.display().to_string()), "{err}");
    assert_eq!(
        query_scalar_i64(&legacy_path, "SELECT COUNT(*) FROM role_memories").unwrap(),
        1,
        "refused role_memory.db must remain intact on disk"
    );
}
