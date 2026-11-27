//! End-to-end SQLite migration tests against the freshly-built `nuo` binary.
//!
//! Each test prepares a real historical `nuo.db` inside an isolated `Sandbox`,
//! drives the shipped CLI/server processes against it, and verifies on-disk
//! schema version, pre-migration backup snapshots, data survival, and fail-fast
//! refusals.

use std::path::Path;
use std::time::Duration;

use nuo_persistence::db::CURRENT_DB_VERSION;
use nuo_persistence::db::migrations::test_fixtures::{
    create_nuo_db_at_version, create_pre_baseline_nuo_db, find_pre_migration_backups, inspect_db,
    query_scalar_i64, query_thread_list_rows, verify_db_integrity,
};
use nuo_test_support::{Cli, MockProvider, Sandbox};

fn sql_escape(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

#[test]
fn live_binary_upgrades_v1_database_preserves_history_and_persists_new_turn()
-> nuo_test_support::Result<()> {
    let mock = MockProvider::with_response("migrated-pong")?;
    let sandbox = Sandbox::new()?;
    sandbox.configure_openai_mock("mock-provider", "mock", "mock-1", &mock.base_url())?;
    let bin = nuo_test_support::binary_path()?;
    let _cleanup = sandbox.server_cleanup(&bin);

    let db_path = sandbox.data_dir().join("nuo.db");
    let ws = sql_escape(sandbox.workspace());
    let seed_v1 = format!(
        r#"
        INSERT INTO threads(id, title, status, origin, msg_count, last_user_prompt, created_at_ms, updated_at_ms)
            VALUES('t-legacy-v1', 'Legacy V1 Thread', 'idle', 'trunk', 99, 'stale cached prompt', 1700000000000, 1700000005000);
        INSERT INTO thread_policies(thread_id, workspace_root, system_role, updated_at_ms)
            VALUES('t-legacy-v1', '{ws}', 'developer', 1700000005000);
        INSERT INTO thread_branches(thread_id, branch_id, name, created_at_ms, updated_at_ms)
            VALUES('t-legacy-v1', 'main', 'main', 1700000000000, 1700000005000);
        INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms) VALUES
            ('v1-f1', 't-legacy-v1', 1, 'dialogue',    '{{"type":"message","message":{{"role":"User","content":"legacy v1 first"}}}}', 1700000001000),
            ('v1-f2', 't-legacy-v1', 2, 'observation', '{{"type":"observation","output":"ok"}}', 1700000002000),
            ('v1-f3', 't-legacy-v1', 3, 'dialogue',    '{{"type":"message","message":{{"role":"user","content":"legacy v1 latest"}}}}', 1700000003000);
        INSERT INTO request_projections(thread_id, round, turn, created_at_ms, payload)
            VALUES('t-legacy-v1', 0, 0, 1700000003000, '{{"legacy":true}}');
        INSERT INTO kv_store(key, value, updated_at)
            VALUES('e2e:v1_marker', 'survived', 1700000003000);
        "#
    );
    create_nuo_db_at_version(&db_path, 1, Some(&seed_v1))?;

    // Drive a real headless turn through the shipped binary. This spawns the
    // server, opens `nuo.db`, creates the pre-v2 snapshot, runs v2..=v6, and
    // persists a new conversation turn.
    let output = Cli::new(&bin, &sandbox)
        .args(["--headless", "-p", "after v1 upgrade", "--json"])
        .timeout(Duration::from_secs(120))
        .output()?;
    assert!(
        output.status.success(),
        "headless turn against v1 DB failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout)?;
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "text_delta" && event["delta"] == "migrated-pong"),
        "expected streamed response after v1 migration: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "round_completed"),
        "expected completed round after v1 migration: {events:?}"
    );

    // Both the migrated legacy thread and the newly created turn must appear in `thread ls`.
    let threads = Cli::new(&bin, &sandbox)
        .args(["thread", "ls", "--json"])
        .success()?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&threads.stdout)?;
    assert!(
        rows.iter().any(|row| row["id"] == "t-legacy-v1"),
        "migrated v1 thread must be discoverable via `nuo thread ls`: {rows:?}"
    );
    assert!(
        rows.len() >= 2,
        "expected both legacy thread and newly persisted thread: {rows:?}"
    );

    // Stop server cleanly so WAL flushes back into `nuo.db` before read-only inspection.
    sandbox.stop_server(&bin)?;

    // Verify pre-v2 backup snapshot exists and preserves untouched v1 schema & data.
    let backups = find_pre_migration_backups(&db_path, 2)?;
    assert_eq!(
        backups.len(),
        1,
        "expected one pre-v2 backup after real v1->v6 upgrade: {backups:?}"
    );
    let backup_info = inspect_db(&backups[0])?;
    assert_eq!(backup_info.user_version, 1);
    assert!(
        backup_info
            .tables
            .iter()
            .any(|t| t == "request_projections")
    );
    assert_eq!(
        query_scalar_i64(&backups[0], "SELECT COUNT(*) FROM request_projections")?,
        1
    );
    verify_db_integrity(&backups[0])?;

    // Verify live upgraded database shape, integrity, and backfilled summary.
    let after = inspect_db(&db_path)?;
    assert_eq!(after.user_version, CURRENT_DB_VERSION);
    assert_eq!(after.ledger.len(), CURRENT_DB_VERSION as usize);
    assert!(!after.tables.iter().any(|t| t == "request_projections"));
    assert!(after.tables.iter().any(|t| t == "permission_rules"));
    assert!(after.tables.iter().any(|t| t == "projection_watermarks"));
    verify_db_integrity(&db_path)?;

    let summaries = query_thread_list_rows(&db_path)?;
    let legacy = summaries
        .iter()
        .find(|row| row.id == "t-legacy-v1")
        .ok_or("t-legacy-v1 missing from thread_list_view after upgrade")?;
    assert_eq!(
        legacy.msg_count, 3,
        "v1 stale count=99 must be replaced by fact count=3"
    );
    assert_eq!(
        legacy.last_user_prompt.as_deref(),
        Some("legacy v1 latest"),
        "v1 stale prompt must be replaced by latest user dialogue"
    );
    assert_eq!(
        query_scalar_i64(
            &db_path,
            "SELECT COUNT(*) FROM kv_store WHERE key = 'e2e:v1_marker' AND value = 'survived'"
        )?,
        1
    );

    // Reopen through a fresh server lifecycle and confirm both the migrated
    // thread and a second post-restart turn persist without creating a second
    // pre-migration backup.
    let second_turn = Cli::new(&bin, &sandbox)
        .args(["--headless", "-p", "after restart", "--json"])
        .timeout(Duration::from_secs(120))
        .success()?;
    assert!(
        String::from_utf8_lossy(&second_turn.stdout).contains("round_completed"),
        "post-restart headless turn did not complete"
    );
    let threads_after_restart = Cli::new(&bin, &sandbox)
        .args(["thread", "ls", "--json"])
        .success()?;
    let restarted_rows: Vec<serde_json::Value> =
        serde_json::from_slice(&threads_after_restart.stdout)?;
    assert!(
        restarted_rows.iter().any(|row| row["id"] == "t-legacy-v1"),
        "migrated thread must persist across server restart: {restarted_rows:?}"
    );
    assert!(
        restarted_rows.len() >= 3,
        "expected legacy thread plus two post-upgrade turns: {restarted_rows:?}"
    );
    sandbox.stop_server(&bin)?;
    assert_eq!(
        find_pre_migration_backups(&db_path, 2)?.len(),
        1,
        "restarting against an already-migrated DB must not create another backup"
    );

    Ok(())
}

#[test]
fn live_binary_upgrades_v5_database_and_backfills_catalog_summary() -> nuo_test_support::Result<()>
{
    let mock = MockProvider::with_response("v6-ready")?;
    let sandbox = Sandbox::new()?;
    sandbox.configure_openai_mock("mock-provider", "mock", "mock-1", &mock.base_url())?;
    let bin = nuo_test_support::binary_path()?;
    let _cleanup = sandbox.server_cleanup(&bin);

    let db_path = sandbox.data_dir().join("nuo.db");
    let ws = sql_escape(sandbox.workspace());
    let seed_v5 = format!(
        r#"
        INSERT INTO threads(id, title, status, origin, created_at_ms, updated_at_ms)
            VALUES('t-v5-active', 'V5 Active', 'idle', 'trunk', 1700000010000, 1700000020000),
                  ('t-v5-empty',  'V5 Empty',  'idle', 'trunk', 1700000010000, 1700000010000);
        INSERT INTO thread_policies(thread_id, workspace_root, system_role, updated_at_ms)
            VALUES('t-v5-active', '{ws}', 'developer', 1700000020000),
                  ('t-v5-empty',  '{ws}', 'developer', 1700000010000);
        INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms) VALUES
            ('v5-n1', 't-v5-active', 1, 'dialogue',   '{{"type":"message","message":{{"role":"User","content":"first v5"}}}}', 1700000011000),
            ('v5-n2', 't-v5-active', 2, 'dialogue',   '{{"type":"message","message":{{"role":"Assistant","content":"ans v5"}}}}', 1700000012000),
            ('v5-n3', 't-v5-active', 3, 'dialogue',   '{{"type":"message","message":{{"role":"user","content":"final v5 user"}}}}', 1700000013000),
            ('v5-n4', 't-v5-active', 4, 'compaction', '{{"type":"compaction","summary":"compacted"}}', 1700000014000);
        INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
            VALUES('developer', 'workspace', '{ws}', 'execute_command', 'git status *', 'allow', 1700000010000, 1700000010000);
        INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms)
            VALUES('t-v5-active', 4, 1700000020000);
        "#
    );
    create_nuo_db_at_version(&db_path, 5, Some(&seed_v5))?;

    let output = Cli::new(&bin, &sandbox)
        .args(["--headless", "-p", "v5 to v6 probe", "--json"])
        .timeout(Duration::from_secs(120))
        .output()?;
    assert!(
        output.status.success(),
        "headless turn against v5 DB failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let threads = Cli::new(&bin, &sandbox)
        .args(["thread", "ls", "--json"])
        .success()?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&threads.stdout)?;
    assert!(
        rows.iter().any(|row| row["id"] == "t-v5-active"),
        "v5 thread must be listed after upgrade: {rows:?}"
    );

    sandbox.stop_server(&bin)?;

    // Backup must be stamped pre-v6 and remain at v5.
    let backups = find_pre_migration_backups(&db_path, 6)?;
    assert_eq!(backups.len(), 1, "expected one pre-v6 backup: {backups:?}");
    let backup_info = inspect_db(&backups[0])?;
    assert_eq!(backup_info.user_version, 5);
    verify_db_integrity(&backups[0])?;

    // Live DB must be at v6 with accurate backfill and preserved v3/v5 tables.
    let after = inspect_db(&db_path)?;
    assert_eq!(after.user_version, CURRENT_DB_VERSION);
    verify_db_integrity(&db_path)?;

    let summaries = query_thread_list_rows(&db_path)?;
    let active = summaries
        .iter()
        .find(|row| row.id == "t-v5-active")
        .ok_or("t-v5-active missing")?;
    assert_eq!(active.msg_count, 3);
    assert_eq!(active.last_user_prompt.as_deref(), Some("final v5 user"));

    let empty = summaries
        .iter()
        .find(|row| row.id == "t-v5-empty")
        .ok_or("t-v5-empty missing")?;
    assert_eq!(empty.msg_count, 0);
    assert_eq!(empty.last_user_prompt, None);

    assert_eq!(
        query_scalar_i64(&db_path, "SELECT COUNT(*) FROM permission_rules")?,
        1
    );
    assert_eq!(
        query_scalar_i64(
            &db_path,
            "SELECT seq FROM projection_watermarks WHERE thread_id = 't-v5-active'"
        )?,
        4
    );

    Ok(())
}

#[test]
fn live_binary_refuses_pre_baseline_database_without_corrupting_it() -> nuo_test_support::Result<()>
{
    let mock = MockProvider::with_response("unreachable")?;
    let sandbox = Sandbox::new()?;
    sandbox.configure_openai_mock("mock-provider", "mock", "mock-1", &mock.base_url())?;
    let bin = nuo_test_support::binary_path()?;
    let _cleanup = sandbox.server_cleanup(&bin);

    let db_path = sandbox.data_dir().join("nuo.db");
    create_pre_baseline_nuo_db(&db_path, 38)?;

    let output = Cli::new(&bin, &sandbox)
        .args(["--headless", "-p", "should fail fast", "--json"])
        .timeout(Duration::from_secs(60))
        .output()?;

    assert!(
        !output.status.success(),
        "pre-baseline v38 DB must fail fast, but exited with success:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("migration ledger is missing")
            || combined.contains("ADR-0189")
            || combined.contains("nuo.db"),
        "expected actionable migration refusal in output, got:\n{combined}"
    );

    // Stop server if spawned and verify the user's original database file was
    // never deleted, renamed, or overwritten.
    let _ = sandbox.stop_server(&bin);
    let after = inspect_db(&db_path)?;
    assert_eq!(
        after.user_version, 38,
        "refused database must stay at its original user_version"
    );
    assert_eq!(
        query_scalar_i64(
            &db_path,
            "SELECT COUNT(*) FROM threads WHERE id = 'legacy-t1'"
        )?,
        1,
        "refused database rows must remain intact on disk"
    );

    Ok(())
}
