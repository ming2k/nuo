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

    // Additional negative checks: invalid effect, invalid scope_kind, duplicate key.
    assert!(
        conn.execute(
            "INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
             VALUES('ops', 'global', '', 'execute_command', 'rm *', 'deny', 1, 1)",
            []
        )
        .is_err(),
        "invalid effect must be refused by CHECK constraint"
    );
    assert!(
        conn.execute(
            "INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
             VALUES('ops', 'project', '/repo/a', 'execute_command', 'ls *', 'allow', 1, 1)",
            []
        )
        .is_err(),
        "invalid scope_kind must be refused by CHECK constraint"
    );
    assert!(
        conn.execute(
            "INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
             VALUES('ops', 'global', '', 'execute_command', 'systemctl status *', 'allow', 1, 1)",
            []
        )
        .is_err(),
        "duplicate (role_id, scope_kind, workspace_key, tool, scope) must be refused"
    );
}

#[test]
fn v1_stale_derived_columns_are_replaced_by_fact_derived_summary() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_to_for_tests(&mut conn, 1).unwrap();

    // v1 stored stale/divergent derived columns directly on `threads`.
    conn.execute_batch(
        r#"
        INSERT INTO threads(id, title, msg_count, last_user_prompt, created_at_ms, updated_at_ms)
            VALUES('t-stale', 'Stale', 99, 'wrong cached prompt', 1000, 2000);
        INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms)
            VALUES('f1', 't-stale', 1, 'dialogue',
                   '{"type":"message","message":{"role":"User","content":"real fact prompt"}}', 1100);
        "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    let (count, prompt): (i64, Option<String>) = conn
        .query_row(
            "SELECT msg_count, last_user_prompt FROM thread_list_view WHERE id = 't-stale'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1, "stale v1 msg_count=99 must not survive v2->v6");
    assert_eq!(
        prompt.as_deref(),
        Some("real fact prompt"),
        "stale v1 last_user_prompt must not survive v2->v6"
    );
}

#[test]
fn v3_to_v4_drops_populated_request_projections_and_preserves_threads() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_to_for_tests(&mut conn, 3).unwrap();

    conn.execute_batch(
        r#"
        INSERT INTO threads(id, title, created_at_ms, updated_at_ms)
            VALUES('t-proj', 'With Projections', 1000, 2000);
        INSERT INTO request_projections(thread_id, round, turn, created_at_ms, payload)
            VALUES('t-proj', 0, 0, 1100, '{"messages":[]}'),
                  ('t-proj', 1, 0, 1200, '{"messages":[{"role":"user"}]}');
        INSERT INTO permission_rules(role_id, scope_kind, workspace_key, tool, scope, effect, created_at_ms, updated_at_ms)
            VALUES('developer', 'workspace', '/tmp/proj', 'read_file', '*', 'allow', 1000, 1000);
        "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    assert!(!table_exists(&conn, "request_projections").unwrap());
    let index_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_request_projections_recency'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_exists, 0);

    let thread_title: String = conn
        .query_row("SELECT title FROM threads WHERE id = 't-proj'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(thread_title, "With Projections");
    let perm_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM permission_rules", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(perm_count, 1);
    assert_referential_integrity(&conn).unwrap();
}

#[test]
fn v4_to_v5_adds_projection_watermarks_and_enforces_constraints() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_to_for_tests(&mut conn, 4).unwrap();

    assert!(!table_exists(&conn, "projection_watermarks").unwrap());
    conn.execute(
        "INSERT INTO threads(id, created_at_ms, updated_at_ms) VALUES('t-wm', 1000, 1000)",
        [],
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    assert!(table_exists(&conn, "projection_watermarks").unwrap());
    let strict: i64 = conn
        .query_row(
            "SELECT strict FROM pragma_table_list WHERE name = 'projection_watermarks'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(strict, 1, "projection_watermarks must be STRICT");

    // Valid write and upsert.
    conn.execute(
        "INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms) VALUES('t-wm', 0, 1000)",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE projection_watermarks SET seq = 12, updated_at_ms = 2000 WHERE thread_id = 't-wm'",
        [],
    )
    .unwrap();

    // Negative checks: negative seq, negative updated_at_ms, non-integer STRICT violation.
    assert!(
        conn.execute(
            "INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms) VALUES('t-neg-seq', -1, 1000)",
            []
        )
        .is_err(),
        "negative seq must be refused"
    );
    assert!(
        conn.execute(
            "INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms) VALUES('t-neg-ts', 1, -5)",
            []
        )
        .is_err(),
        "negative updated_at_ms must be refused"
    );
    assert!(
        conn.execute(
            "INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms) VALUES('t-strict', 'not-an-int', 1000)",
            []
        )
        .is_err(),
        "STRICT table must refuse text in INTEGER column"
    );
}

#[test]
fn v5_to_v6_backfills_catalog_summary_across_all_edge_cases() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_to_for_tests(&mut conn, 5).unwrap();

    conn.execute_batch(
        r#"
        -- Thread 1: mixed kinds & roles; latest user prompt is seq=4 (lowercase "user"),
        -- while seq=5 is assistant and seq=6 is compaction (excluded from msg_count).
        INSERT INTO threads(id, title, status, origin, created_at_ms, updated_at_ms)
            VALUES('t-mixed', 'Mixed', 'idle', 'trunk', 1000, 6000);
        INSERT INTO thread_policies(thread_id, workspace_root, system_role, updated_at_ms)
            VALUES('t-mixed', '/repo/a', 'developer', 6000);
        INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms) VALUES
            ('m1', 't-mixed', 1, 'dialogue',      '{"type":"message","message":{"role":"User","content":"first user"}}', 1100),
            ('m2', 't-mixed', 2, 'dialogue',      '{"type":"message","message":{"role":"Assistant","content":"reply 1"}}', 1200),
            ('m3', 't-mixed', 3, 'observation',   '{"type":"observation","output":"tool ok"}', 1300),
            ('m4', 't-mixed', 4, 'dialogue',      '{"type":"message","message":{"role":"user","content":"second user"}}', 1400),
            ('m5', 't-mixed', 5, 'dialogue',      '{"type":"message","message":{"role":"assistant","content":"reply 2"}}', 1500),
            ('m6', 't-mixed', 6, 'compaction',    '{"type":"compaction","summary":"compact"}', 1600),
            ('m7', 't-mixed', 7, 'system_notice', '{"type":"notice","text":"hi"}', 1700);

        -- Thread 2: completely empty thread -> msg_count=0, last_user_prompt=NULL.
        INSERT INTO threads(id, title, created_at_ms, updated_at_ms)
            VALUES('t-empty', '', 2000, 2000);

        -- Thread 3: only assistant + observation, zero user dialogue -> msg_count=2, last_user_prompt=NULL.
        INSERT INTO threads(id, title, parent_thread_id, origin, created_at_ms, updated_at_ms)
            VALUES('t-no-user', 'Subagent', 't-mixed', 'subagent', 3000, 3500);
        INSERT INTO causal_nodes(id, thread_id, seq, kind, payload_json, timestamp_ms) VALUES
            ('s1', 't-no-user', 1, 'dialogue',    '{"type":"message","message":{"role":"Assistant","content":"only assistant"}}', 3100),
            ('s2', 't-no-user', 2, 'observation', '{"type":"observation","output":"obs"}', 3200);

        -- Preserve existing v5 watermark row across v5->v6 upgrade.
        INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms)
            VALUES('t-mixed', 7, 6000);
        "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    let query = |id: &str| -> (i64, Option<String>) {
        conn.query_row(
            "SELECT msg_count, last_user_prompt FROM thread_list_view WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };

    // 4 dialogues + 1 observation = 5 (compaction and system_notice excluded).
    assert_eq!(query("t-mixed"), (5, Some("second user".to_string())));
    assert_eq!(query("t-empty"), (0, None));
    assert_eq!(query("t-no-user"), (2, None));

    // Also verify the materialized columns directly on `threads`, not just via the view.
    let (direct_count, direct_prompt): (i64, Option<String>) = conn
        .query_row(
            "SELECT msg_count, last_user_prompt FROM threads WHERE id = 't-mixed'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(direct_count, 5);
    assert_eq!(direct_prompt.as_deref(), Some("second user"));

    // Watermark survives v5->v6 untouched.
    let watermark: i64 = conn
        .query_row(
            "SELECT seq FROM projection_watermarks WHERE thread_id = 't-mixed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(watermark, 7);
}
