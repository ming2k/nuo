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
        "workspace_effects",
        "fts_thread_facts",
        "permission_rules",
        "projection_watermarks",
    ] {
        assert!(
            tables.iter().any(|name| name == expected),
            "missing baseline table {expected}: {tables:?}"
        );
    }
    for retired in [
        "request_projections",
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

    conn.execute("DELETE FROM threads WHERE id = 't1'", [])
        .unwrap();

    for table in ["thread_policies", "causal_nodes", "thread_commands"] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table} did not cascade with the thread");
    }
}

#[test]
fn permission_rules_migration_upgrades_from_v2_and_enforces_scope_constraints() {
    use crate::db::permissions::{
        PermissionRuleEffect, PermissionScopeKind, PermissionScopeTarget, clear_permission_rules,
        load_permission_rules, touch_permission_rule_last_used, upsert_permission_rule,
    };

    let mut conn = Connection::open_in_memory().unwrap();
    migrate_to_for_tests(&mut conn, 2).unwrap();

    // Migrate v2 -> v3.
    migrate_schema(&mut conn).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);

    // Negative constraint checks: global must have empty workspace_key, workspace must be non-empty.
    assert!(
        conn.execute(
            "INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
             VALUES('ops', 'global', '/tmp', 'execute_command', 'ls *', 'allow', 1, 1)",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
             VALUES('developer', 'workspace', '', 'execute_command', 'ls *', 'allow', 1, 1)",
            []
        )
        .is_err()
    );

    // Role + workspace vs role + global isolation and merge.
    let ops_scope = PermissionScopeTarget::from_role_and_workspace("ops", None);
    let dev_ws =
        PermissionScopeTarget::from_role_and_workspace("developer", Some(Path::new("/repo/a")));
    let dev_global = PermissionScopeTarget::from_role_and_workspace("developer", None);

    upsert_permission_rule(
        &conn,
        &ops_scope,
        "execute_command",
        "systemctl status *",
        PermissionRuleEffect::Allow,
        10,
    )
    .unwrap();
    upsert_permission_rule(
        &conn,
        &dev_global,
        "execute_command",
        "git status *",
        PermissionRuleEffect::Allow,
        11,
    )
    .unwrap();
    upsert_permission_rule(
        &conn,
        &dev_ws,
        "execute_command",
        "cargo test *",
        PermissionRuleEffect::Allow,
        12,
    )
    .unwrap();

    let ops_rules = load_permission_rules(&conn, &ops_scope).unwrap();
    assert_eq!(ops_rules.len(), 1);
    assert_eq!(ops_rules[0].scope, "systemctl status *");
    assert_eq!(ops_rules[0].scope_kind, PermissionScopeKind::Global);

    let dev_rules = load_permission_rules(&conn, &dev_ws).unwrap();
    assert_eq!(dev_rules.len(), 2);
    assert_eq!(dev_rules[0].scope, "git status *");
    assert_eq!(dev_rules[1].scope, "cargo test *");

    touch_permission_rule_last_used(&conn, &dev_ws, "execute_command", "git status *", 99).unwrap();
    touch_permission_rule_last_used(&conn, &dev_ws, "execute_command", "cargo test *", 88).unwrap();
    let git_last_used: Option<i64> = conn
        .query_row(
            "SELECT last_used_ms FROM permission_rules WHERE role_id = 'developer' AND scope = 'git status *'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let cargo_last_used: Option<i64> = conn
        .query_row(
            "SELECT last_used_ms FROM permission_rules WHERE role_id = 'developer' AND scope = 'cargo test *'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(git_last_used, Some(99));
    assert_eq!(cargo_last_used, Some(88));

    clear_permission_rules(
        &conn,
        &dev_ws,
        &[("execute_command".into(), "cargo test *".into())],
        20,
    )
    .unwrap();
    let after_clear = load_permission_rules(&conn, &dev_ws).unwrap();
    assert!(
        after_clear
            .iter()
            .any(|r| { r.scope == "cargo test *" && r.effect == PermissionRuleEffect::Revoked })
    );
}
