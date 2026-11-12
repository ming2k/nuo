//! Derived FTS rows, fork-title stripping, and fact mirrors.

use super::*;

/// End-to-end shape of the browser's view (ADR-0120): a real migrated database,
/// real forks through the one primitive, and the picker's own partition query.
///
/// This is the regression that would have caught root cause 4 — a fork landing
/// outside its parent's partition and so vanishing from every query that
/// filters by workspace — because it asserts on the query the TUI actually
/// runs. It also builds a depth-2 chain, the shape the old one-level projection
/// could not represent at all.

#[tokio::test]
async fn fork_appears_in_the_pickers_partition_query_with_its_lineage() {
    use nuo_protocol::{ForkPoint, Message, Role, ThreadForkKind};

    let dir = std::env::temp_dir().join(format!("nuo-lineage-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::session::SessionStore::for_path(dir.join("session.json"));

    store
        .replace_messages(vec![
            Message::new(Role::User, "round 1 user"),
            Message::new(Role::Assistant, "round 1 answer"),
            Message::new(Role::User, "round 2 user"),
            Message::new(Role::Assistant, "round 2 answer"),
        ])
        .await
        .unwrap();
    store.set_round_counter(2).await.unwrap();
    let parent_id = store.id().await;

    // Depth 1: fork the trunk at its settled round.
    let first = store
        .fork_at(crate::session::ForkTarget::Round(2))
        .await
        .unwrap();

    // Depth 2: fork the *branch*. `/fork` accepts a branch as readily as a
    // trunk, so the browser must be able to represent what the command creates.
    // Re-open the child as its own store — the same way a client attaches to it.
    let branch =
        crate::session::SessionStore::for_path(dir.join(format!("{}.json", first.child_id)));
    let second = branch
        .fork_at(crate::session::ForkTarget::Round(1))
        .await
        .unwrap();

    let listed = store.list().await.unwrap();
    let by_id = |id: &str| {
        listed
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("{id} missing from the picker snapshot"))
    };
    let ids: Vec<&str> = listed.iter().map(|s| s.id.as_str()).collect();
    assert!(
        ids.contains(&parent_id.as_str()),
        "the attached thread stays: {ids:?}"
    );

    // Depth 1 branch: in the snapshot, with its address and origin.
    let first_row = by_id(&first.child_id);
    assert_eq!(first_row.fork_point, Some(ForkPoint::at_round(2)));
    assert_eq!(first_row.fork_kind, ThreadForkKind::Fork);
    assert_eq!(first_row.parent_id.as_deref(), Some(parent_id.as_str()));

    // Depth 2 branch: present too — this is the row the old projection dropped.
    let second_row = by_id(&second.child_id);
    assert_eq!(second_row.fork_point, Some(ForkPoint::at_round(1)));
    assert_eq!(
        second_row.parent_id.as_deref(),
        Some(first.child_id.as_str())
    );
    // Both branches came back from a partition-filtered query, which is the
    // assertion that matters: a fork whose partition binding was erased lists
    // nowhere at all.
    assert_eq!(
        listed.len(),
        3,
        "trunk + two branches, one partition: {ids:?}"
    );

    // The parent is untouched: still a trunk, still without provenance.
    let parent_row = by_id(&parent_id);
    assert_eq!(parent_row.fork_point, None);
    assert_eq!(parent_row.fork_kind, ThreadForkKind::Trunk);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Migration 25 purges the provenance the retired fork primitive smuggled into
/// titles (`[INV-LINEAGE-04]`), and only that: a title the user actually typed
/// is never touched.
#[test]
fn migration_25_strips_only_derived_fork_titles() {
    let engine = DatabaseEngine::open_in_memory().unwrap();

    // The exact shapes the retired `fork_at` produced.
    engine
        .conn
        .execute_batch(
            r#"
            INSERT INTO threads (id, title, fork_kind, parent_thread_id, created_at_ms, updated_at_ms)
            VALUES
                ('parent',      'Paged Storage Rewrite', 'trunk', NULL,     1000, 1000),
                ('derived',     'Paged Storage Rewrite (fork)', 'fork', 'parent', 2000, 2000),
                ('untitled-par','',     'trunk', NULL, 1000, 1000),
                ('bare',        'fork', 'fork', 'untitled-par', 2000, 2000),
                -- Authored names that merely contain the marker: keep them.
                ('authored',    'my (fork) experiment', 'fork', 'parent', 2000, 2000),
                ('only-fork',   'fork', 'fork', 'parent', 2000, 2000);
            "#,
        )
        .unwrap();

    {
        // The migration runs inside the caller's transaction and only becomes
        // durable on commit — commit it, exactly as the migrator does.
        let tx = engine.conn.unchecked_transaction().unwrap();
        apply_thread_lineage_graph_schema(&tx).unwrap();
        tx.commit().unwrap();
    }

    let title = |id: &str| -> String {
        engine
            .conn
            .query_row("SELECT title FROM threads WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    };

    assert_eq!(
        title("parent"),
        "Paged Storage Rewrite",
        "a trunk keeps its name"
    );
    assert_eq!(title("derived"), "", "derived provenance is retired");
    assert_eq!(
        title("untitled-par"),
        "",
        "an untitled trunk stays untitled"
    );
    assert_eq!(
        title("authored"),
        "my (fork) experiment",
        "a user-authored title is never rewritten"
    );
    // `fork` is ambiguous: it is derived when the parent is untitled, and the
    // generated value when the parent is titled. This one's parent is titled,
    // so the string was authored.
    assert_eq!(title("only-fork"), "fork");

    // The typed record replaces what the title used to carry.
    let cols = table_columns(&engine.conn, "threads").unwrap();
    assert!(cols.contains("fork_round") && cols.contains("fork_turn"));
    assert!(
        !cols.contains("fork_fact_id"),
        "the never-written column is gone"
    );
}

/// A full rewrite replaces the mirrored facts, not just the transcript rows.
///
/// A rebuilt transcript restarts its sequence at zero, so an append-only mirror
/// collides on `UNIQUE(session_id, seq)`: the save fails and the turn is lost.
/// The graph mirrors the transcript, so "replaced" has to mean "replace" on both
/// sides — keeping the old facts would also duplicate the conversation.
#[test]
fn a_full_rewrite_replaces_the_mirrored_facts() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "first"),
    ));
    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::Assistant, "second"),
    ));
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        &data,
        true,
        &[],
        &CommitGuard::default(),
    )
    .unwrap();

    let mirrored = |conn: &Connection, id: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM causal_nodes WHERE session_id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    };
    // Derived work is queued by the save and produced by the drain.
    assert_eq!(
        mirrored(&engine.conn, &data.id),
        0,
        "the commit derives nothing inline"
    );
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();
    assert_eq!(
        mirrored(&engine.conn, &data.id),
        2,
        "both messages mirrored"
    );

    // Rebuild with one message: the sequence restarts at zero.
    data.transcript = crate::session::rebuild_for_test(&[Message::new(Role::User, "rebuilt")]);
    data.generation = uuid::Uuid::new_v4().to_string();
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        &data,
        false,
        &[],
        &CommitGuard::default(),
    )
    .expect("a rebuilt transcript must replace the facts, not collide with them");
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();
    assert_eq!(
        mirrored(&engine.conn, &data.id),
        1,
        "only the rebuilt message's fact remains"
    );
}
