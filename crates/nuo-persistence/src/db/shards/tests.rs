//! Unit tests for versioned fixed-K content shards (ADR-0242, ADR-0255).

use super::*;
use crate::db::migrations::content_shard::{CONTENT_SHARD_SCHEMA, CURRENT_CONTENT_SHARD_VERSION};
use crate::workspace_effects::JournalOp;

fn sample_effect(thread_id: &str, path: &str) -> NewOwnedEffect {
    NewOwnedEffect {
        thread_id: thread_id.to_string(),
        round_id: "r1".to_string(),
        turn_id: "t1".to_string(),
        call_id: "c1".to_string(),
        seq: 1,
        tool_name: "edit_text".to_string(),
        path: Some(path.to_string()),
        op: JournalOp::Edit,
        effect_class: "file_bytes".to_string(),
        before_hash: Some("pre".to_string()),
        after_hash: None,
        irreversible_reason: None,
        created_at_ms: 1_000,
    }
}

#[test]
fn unledgered_legacy_shard_is_ignored_by_reader_and_rebuilt_with_ledger_by_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = shard_path(dir.path(), 0);

    // Seed a pre-ADR-0255 unledgered shard (even if user_version happens to match).
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE workspace_effects (id INTEGER PRIMARY KEY, thread_id TEXT NOT NULL);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", CURRENT_CONTENT_SHARD_VERSION)
            .unwrap();
    }

    // Reader must decline unledgered shards so catch-up replays from journals.
    assert!(open_shard_reader_conn(&path).unwrap().is_none());

    // Writer must discard the stale disposable shard and rebuild with the
    // versioned schema + ledger.
    let writer = open_shard_writer_conn(&path).unwrap();
    let version: u32 = writer
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_CONTENT_SHARD_VERSION);
    let ledger_count: i64 = writer
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ledger_count, CONTENT_SHARD_SCHEMA.migrations.len() as i64);
    drop(writer);

    let reader = open_shard_reader_conn(&path)
        .unwrap()
        .expect("migrated content shard must be readable");
    let reader_version: u32 = reader
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(reader_version, CURRENT_CONTENT_SHARD_VERSION);
}

#[tokio::test]
async fn workspace_effects_composite_pk_isolates_identical_effect_ids_across_threads() {
    let dir = tempfile::tempdir().unwrap();
    // Force both threads onto the single shard (shard_count = 1).
    let shards = ContentShardPool::new(dir.path().to_path_buf(), 1);

    let shared_id: i64 = 42;
    shards
        .append_workspace_effect(shared_id, sample_effect("thread-a", "/ws/a.rs"), Some(1))
        .await
        .unwrap();
    shards
        .append_workspace_effect(shared_id, sample_effect("thread-b", "/ws/b.rs"), Some(1))
        .await
        .unwrap();

    assert!(
        shards
            .complete_workspace_effect("thread-a", shared_id, Some("post-a".into()), 2_000)
            .await
            .unwrap()
    );
    // Completing thread-a must not touch thread-b's row with the same effect_id.
    let a_rows = shards.list_workspace_effects("thread-a").unwrap();
    let b_rows = shards.list_workspace_effects("thread-b").unwrap();
    assert_eq!(a_rows.len(), 1);
    assert_eq!(b_rows.len(), 1);
    assert_eq!(a_rows[0].after_hash.as_deref(), Some("post-a"));
    assert_eq!(b_rows[0].after_hash, None);

    assert!(
        shards
            .complete_workspace_effect("thread-b", shared_id, Some("post-b".into()), 2_500)
            .await
            .unwrap()
    );
    assert!(
        shards
            .mark_workspace_effect_reversed("thread-a", shared_id, 3_000, Some("undo a".into()))
            .await
            .unwrap()
    );
    let a_after = shards.list_workspace_effects("thread-a").unwrap();
    let b_after = shards.list_workspace_effects("thread-b").unwrap();
    assert_eq!(a_after[0].reversed_at_ms, Some(3_000));
    assert_eq!(b_after[0].reversed_at_ms, None);
}

#[tokio::test]
async fn production_content_shard_enforces_inv_effect_04_no_early_reversal() {
    let dir = tempfile::tempdir().unwrap();
    let shards = ContentShardPool::new(dir.path().to_path_buf(), 1);

    let effect_id = 7;
    shards
        .append_workspace_effect(effect_id, sample_effect("thread-1", "/ws/x.rs"), Some(1))
        .await
        .unwrap();

    // Unconfirmed effect must be refused by `trg_workspace_effects_no_early_reversal`.
    let err = shards
        .mark_workspace_effect_reversed("thread-1", effect_id, 5_000, None)
        .await
        .expect_err("production content shard must enforce INV-EFFECT-04");
    assert!(
        err.to_string().contains("INV-EFFECT-04"),
        "expected INV-EFFECT-04 refusal, got: {err}"
    );
}

#[tokio::test]
async fn production_content_shard_fts_uses_porter_unicode61_stemming() {
    let dir = tempfile::tempdir().unwrap();
    let shards = ContentShardPool::new(dir.path().to_path_buf(), 1);

    let mut thread = nuo_protocol::Thread::new(nuo_protocol::ThreadId::new("t-stem"), 1_000);
    thread
        .append(
            nuo_protocol::FactPayload::Message {
                message: nuo_tool::Message::new(
                    nuo_tool::Role::User,
                    "running migrations reliably across shards",
                ),
            },
            nuo_protocol::FactContext::default(),
            1_001,
        )
        .unwrap();

    shards
        .save_content(
            "t-stem".into(),
            Some(thread),
            Vec::new(),
            true,
            None,
            Some(1),
        )
        .await
        .unwrap();

    let catalog = crate::db::open_in_memory_for_tests().unwrap();
    catalog
        .execute(
            "INSERT INTO threads(id, title, created_at_ms, updated_at_ms) VALUES('t-stem', 'Stem', 1000, 1000)",
            [],
        )
        .unwrap();
    catalog
        .execute(
            "INSERT INTO thread_policies(thread_id, workspace_root, system_role, updated_at_ms) VALUES('t-stem', '/ws', 'developer', 1000)",
            [],
        )
        .unwrap();

    // Stemmed query "run migrate" must match "running migrations" via porter tokenizer.
    let hits = shards
        .search_history_fanout(&catalog, "run migrate", None, 10, false)
        .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "porter stemming must match on content shards"
    );
    assert_eq!(hits[0].session_id, "t-stem");
}
