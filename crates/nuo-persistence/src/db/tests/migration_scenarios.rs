//! Legacy schema transition scenarios.

use super::*;

#[test]
fn test_v16_to_v17_clean_break_migration() {
    let engine = DatabaseEngine::open_in_memory().unwrap();

    // 1. Insert a legacy session row with old columns (tree, commands, round_interrupts)
    engine
        .conn
        .execute_batch(
            r#"
            INSERT OR IGNORE INTO threads (id, created_at_ms, updated_at_ms) VALUES ('legacy-s1', 100000, 200000);
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY, parent_id TEXT, fork_kind TEXT, title TEXT, created_at_s INTEGER, updated_at_s INTEGER,
                workspace_root TEXT, additional_roots TEXT, persona TEXT, msg_count INTEGER, last_user_prompt TEXT,
                digest TEXT, digest_anchor INTEGER, tree TEXT, transcript_generation TEXT, provider_connection TEXT,
                round_counter INTEGER, unattended INTEGER, disabled_tools TEXT, commands TEXT, round_interrupts TEXT,
                retry_resolutions TEXT, retry_pending TEXT, checksum INTEGER, schema_version INTEGER
            );
            INSERT INTO sessions (
                id, parent_id, fork_kind, title, created_at_s, updated_at_s,
                workspace_root, additional_roots, persona, msg_count, last_user_prompt,
                digest, digest_anchor, tree, transcript_generation, provider_connection,
                round_counter, unattended, disabled_tools, commands, round_interrupts,
                retry_resolutions, retry_pending, checksum, schema_version
            ) VALUES (
                'legacy-s1', NULL, 'aside', 'Legacy Aside', 100, 200,
                '/workspace', '[]', 'architect', 1, 'Fix bug',
                NULL, NULL, '{"entries":{},"active_leaf_id":"leaf-99"}', 'gen-1', 'openai',
                3, 1, '["bash"]', '[{"name":"help","args":""}]', '[]',
                '[]', NULL, 1234, 16
            );
            "#,
        )
        .unwrap();

    // Insert legacy entry and membership
    engine
        .conn
        .execute_batch(
            r#"
            INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
            VALUES ('e1', 'message', 'user', 'Legacy question', NULL, 0, 100000, '{}');
            INSERT INTO entry_memberships (session_id, seq, entry_id)
            VALUES ('legacy-s1', 1, 'e1');
            "#,
        )
        .unwrap();

    // 2. Run Migration 17 on the database
    let tx = engine.conn.unchecked_transaction().unwrap();
    migrations::apply_session_ir_clean_break_schema(&tx).unwrap();
    tx.commit().unwrap();

    // 3. Verify sessions_v2 was populated and purged of old fields
    let (active_leaf, status, unattended) = engine
        .conn
        .query_row(
            "SELECT active_leaf, status, (SELECT json_extract(guardrails_json, '$.unattended') FROM session_policies WHERE session_id = id) FROM sessions_v2 WHERE id = 'legacy-s1'",
            [],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)),
        )
        .unwrap();

    assert_eq!(active_leaf.as_deref(), Some("leaf-99"));
    assert_eq!(status, "idle");
    assert!(unattended);

    // 4. Verify causal_nodes was populated from entries/entry_memberships
    let (node_id, kind, content) = engine
        .conn
        .query_row(
            "SELECT id, kind, json_extract(payload_json, '$.message.content') FROM causal_nodes WHERE session_id = 'legacy-s1'",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .unwrap();

    assert_eq!(node_id, "e1");
    assert_eq!(kind, "dialogue");
    assert_eq!(content, "Legacy question");

    // 5. Verify session_list_view derives msg_count and last_user_prompt dynamically
    let (msg_count, last_prompt) = engine
        .conn
        .query_row(
            "SELECT msg_count, last_user_prompt FROM session_list_view WHERE id = 'legacy-s1'",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .unwrap();

    assert_eq!(msg_count, 1);
    assert_eq!(last_prompt.as_deref(), Some("Legacy question"));
}
