//! Transcript save, delta watermarks, lineage, and commit receipts.

use super::blobs::engine_blob_round_trip;
use super::*;

/// Regression (reported incident): the v5 `entries` CHECK coupled `origin`
/// and `hidden`, rejecting the legitimate *visible* harness injections the
/// durable transcript carries (UserSteer/SubagentSteer, CommandEcho "/cmd"
/// & "!cmd", ToolImage). A mid-round save of such a message then failed with
/// `CHECK constraint failed: origin IS NULL OR hidden = 1`. The corrected
/// schema (migration 10) accepts them without weakening the still-real
/// invariants.
#[test]

fn visible_harness_injections_persist_after_origin_hidden_split() {
    use nuo_protocol::{InjectionKind, Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();

    // The exact repro: a user steering insert and a command echo are both
    // visible (`hidden = false`) yet legitimately carry an `origin`.
    let steer = Message::new(Role::User, "pls reconsider point 3")
        .with_origin(nuo_protocol::InjectionOrigin::new(InjectionKind::UserSteer));
    let echo = Message::command_echo("/session list");
    let image = Message::new(Role::User, "Image from screenshot")
        .with_images(vec![nuo_protocol::ImagePart {
            mime: "image/png".into(),
            data: "bytes".into(),
        }])
        .with_origin(nuo_protocol::InjectionOrigin::new(InjectionKind::ToolImage));

    assert!(!steer.hidden && steer.origin.is_some());
    assert!(!echo.hidden && echo.origin.is_some());
    assert!(!image.hidden && image.origin.is_some());

    data.transcript
        .push(TranscriptEntry::from_message(0, &steer));
    data.transcript
        .push(TranscriptEntry::from_message(1, &echo));
    data.transcript
        .push(TranscriptEntry::from_message(2, &image));
    // Was the reported failure: `CHECK constraint failed: origin IS NULL OR hidden = 1`.
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();

    let reloaded = session_store::load_session_full(&engine.conn, &data.id)
        .unwrap()
        .unwrap();
    let messages: Vec<_> = reloaded
        .transcript
        .entries
        .iter()
        .filter_map(|e| e.to_message())
        .collect();
    assert_eq!(messages.len(), 3);
    for m in messages {
        assert!(m.origin.is_some(), "provenance must survive the round-trip");
        assert!(!m.hidden || m.origin.is_some());
    }
}

/// The corrected schema must still reject silent corruption: a *visible*
/// envelope origin that can never be legitimate. A checkpoint is an
/// elided-range stand-in and is hidden by construction (`transcript.rs`);
/// a visible `Role::User` row tagged `origin = 'checkpoint'` is thus
/// impossible and must fail on save.
#[test]
fn visible_checkpoint_origin_is_still_rejected() {
    use nuo_protocol::{Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    let mut message = Message::new(Role::User, "bogus visible checkpoint");
    message.hidden = false; // checkpoint is never visible dialogue
    message.origin = Some(nuo_protocol::InjectionOrigin::new(
        nuo_protocol::InjectionKind::CompactionCheckpoint,
    ));
    data.transcript
        .push(TranscriptEntry::from_message(0, &message));

    let err = session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data)
        .expect_err("constraint must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("constraint failed") || msg.contains("CHECK"),
        "expected a CHECK/constraint failure, got: {msg}"
    );
}

#[test]
fn delta_save_appends_only_rows_above_the_watermark() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();

    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        &data,
        false,
        &[],
        &CommitGuard::default(),
    )
    .unwrap();
    let reloaded = session_store::load_session_full(&engine.conn, &data.id)
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 1);

    // Second delta appends only the new row; the durable watermark moves.
    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "two"),
    ));
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        &data,
        false,
        &[],
        &CommitGuard::default(),
    )
    .unwrap();
    let reloaded = session_store::load_session_full(&engine.conn, &data.id)
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 2);
    assert_eq!(reloaded.transcript.entries[1].seq, 1);
}

/// A save enqueues derived work; it never mirrors or indexes inline.
///
/// This is the contract that keeps a turn off the derived-index path: the
/// commit writes the transcript and a pending row, and a drain produces the
/// graph and the FTS rows.
#[test]
fn a_save_enqueues_derived_work_and_a_flush_produces_it() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();

    assert_eq!(
        count_causal_nodes(&engine.conn),
        0,
        "the save derives nothing"
    );
    let fts: i64 = engine
        .conn
        .query_row("SELECT COUNT(*) FROM fts_entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fts, 0, "the save indexes nothing");

    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();
    assert_eq!(count_causal_nodes(&engine.conn), 1);
    let fts: i64 = engine
        .conn
        .query_row("SELECT COUNT(*) FROM fts_entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fts, 1);

    // A later delta is enqueued again and drained to the transcript's end.
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "two"),
    ));
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        &data,
        false,
        &[],
        &CommitGuard::default(),
    )
    .unwrap();
    assert_eq!(count_causal_nodes(&engine.conn), 1, "still queued");
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();
    assert_eq!(count_causal_nodes(&engine.conn), 2);
}

/// A full rewrite purges exactly its own FTS rows, not the whole table.
///
/// `fts_entries.session_id` is `UNINDEXED`, so the old
/// `DELETE FROM fts_entries WHERE session_id = ?` scanned every row in the
/// database. The derived reset now enumerates the session's membership rowids
/// (`[INV-DERIVED-06]`) while they still exist. This pins the correctness the
/// fast path depends on: another session's rows survive untouched, and the
/// rewritten session is re-indexed by the normal drain.
#[test]
fn a_full_rewrite_purges_only_its_own_fts_rows() {
    use nuo_protocol::{Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut other = crate::session::SessionData {
        id: "other-session".to_string(),
        ..Default::default()
    };
    other.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "other dialogue"),
    ));
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &other).unwrap();
    crate::db::derived::flush_session(&engine.conn, &other.id).unwrap();

    let mut rewritten = crate::session::SessionData {
        id: "rewritten-session".to_string(),
        ..Default::default()
    };
    rewritten.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "old one"),
    ));
    rewritten.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::Assistant, "old two"),
    ));
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &rewritten).unwrap();
    crate::db::derived::flush_session(&engine.conn, &rewritten.id).unwrap();

    let fts_for = |id: &str| -> i64 {
        engine
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts_entries WHERE session_id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(fts_for(&other.id), 1);
    assert_eq!(fts_for(&rewritten.id), 2);

    // Replace the transcript (the shape a compaction fallback / resume takes).
    rewritten.transcript = nuo_protocol::Transcript::new();
    rewritten.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "new one"),
    ));
    rewritten.generation = uuid::Uuid::new_v4().to_string();
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &rewritten).unwrap();

    assert_eq!(fts_for(&other.id), 1, "another session was collateral");
    assert_eq!(fts_for(&rewritten.id), 0, "the rewrite's old rows must go");
    crate::db::derived::flush_session(&engine.conn, &rewritten.id).unwrap();
    assert_eq!(
        fts_for(&rewritten.id),
        1,
        "the new transcript is re-indexed"
    );
}

/// A thread's role and workspace binding are write-once.
///
/// `ThreadScope` is the thread's durable identity (ADR-0170); `/role` switches
/// to a thread of the target role instead of rebinding the current one. The
/// database trigger makes that true for every writer, so a bug cannot silently
/// move a conversation into another role's history bucket.
#[test]
fn a_thread_scope_cannot_be_rebound() {
    use nuo_protocol::Message;

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData {
        id: "immutable-scope".to_string(),
        role: Some("developer".to_string()),
        workspace: Some(nuo_protocol::WorkspaceBinding::new("/repo/a")),
        ..Default::default()
    };
    data.transcript
        .push(nuo_protocol::TranscriptEntry::from_message(
            0,
            &Message::new(nuo_protocol::Role::User, "hello"),
        ));
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();

    // The same scope is always writable.
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();

    let mut rebound_role = data.clone();
    rebound_role.role = Some("ops".to_string());
    let error =
        session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &rebound_role)
            .expect_err("a role change must be refused");
    assert!(
        error.to_string().contains("immutable"),
        "expected the immutability guard, got: {error}"
    );

    let mut rebound_workspace = data.clone();
    rebound_workspace.workspace = Some(nuo_protocol::WorkspaceBinding::new("/repo/b"));
    let error = session_store::save_session_full(
        &engine.conn,
        engine.blob_store.as_ref(),
        &rebound_workspace,
    )
    .expect_err("a workspace change must be refused");
    assert!(
        error.to_string().contains("immutable"),
        "expected the immutability guard, got: {error}"
    );

    // The durable scope is still the original one.
    let reloaded = session_store::load_session_full(&engine.conn, &data.id)
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.role.as_deref(), Some("developer"));
    assert_eq!(
        reloaded.workspace.as_ref().map(|w| w.root.as_path()),
        Some(std::path::Path::new("/repo/a"))
    );
}

/// Deleting a thread must not cascade into the append-only workspace journal.
///
/// The original `workspace_effects.thread_id` declared `ON DELETE CASCADE`
/// while `trg_workspace_effects_no_delete` correctly refuses every row delete,
/// so a thread that had journaled a file mutation could never be removed. The
/// journal keeps its observation (and thread id) as evidence; only the
/// pre-image bytes are expired so the CAS collector can reclaim them.
#[test]
fn deleting_a_thread_keeps_its_workspace_journal_rows() {
    use nuo_protocol::{Message, Role};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData {
        id: "thread-with-effects".to_string(),
        role: Some("developer".to_string()),
        workspace: Some(nuo_protocol::WorkspaceBinding::new("/repo/a")),
        ..Default::default()
    };
    data.transcript
        .push(nuo_protocol::TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, "touch a file"),
        ));
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();

    engine
        .conn
        .execute(
            "INSERT INTO workspace_effects (
                thread_id, round_id, turn_id, call_id, seq, tool_name, path, op,
                effect_class, before_hash, after_hash, completed_at_ms, created_at_ms
             ) VALUES (?1, 'r1', 't1', 'call1', 0, 'edit_text', '/repo/a/file',
                       'edit', 'file_bytes', 'before', 'after', 1000, 1000)",
            rusqlite::params![data.id],
        )
        .unwrap();

    assert!(session_store::delete_session(&engine.conn, &data.id).unwrap());
    assert!(
        session_store::load_session_full(&engine.conn, &data.id)
            .unwrap()
            .is_none(),
        "the thread row must be gone"
    );
    let (rows, expired): (i64, i64) = engine
        .conn
        .query_row(
            "SELECT COUNT(*), COUNT(expired_at_ms) FROM workspace_effects WHERE thread_id = ?1",
            rusqlite::params![data.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(rows, 1, "the journal observation is append-only evidence");
    assert_eq!(
        expired, 1,
        "its pre-image bytes are released for collection"
    );
}

fn dialogue_chain_len(conn: &Connection, session_id: &str) -> usize {
    let mut current = crate::db::ir_mirror::newest_position(conn, session_id)
        .unwrap()
        .map(|(id, _)| id);
    let mut seen = std::collections::HashSet::new();
    let mut count = 0;
    while let Some(id) = current {
        assert!(seen.insert(id.clone()), "lineage is cyclic at {id}");
        count += 1;
        current = conn
            .query_row(
                "SELECT parent_id FROM causal_nodes WHERE session_id = ?1 AND id = ?2",
                rusqlite::params![session_id, id],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap();
    }
    count
}

fn causal_cursor(conn: &Connection, session_id: &str) -> i64 {
    conn.query_row(
        "SELECT causal_seq FROM derived_cursors WHERE session_id = ?1",
        rusqlite::params![session_id],
        |row| row.get(0),
    )
    .unwrap()
}

/// A compaction checkpoint is a transcript fact; the graph is a derive.
///
/// The entry alone must reproduce the horizon after any amount of derived-state
/// loss: one compaction node, parented to the split parent, injected by the
/// horizon walk, with the same window as the transcript projection.
#[test]
fn a_compaction_entry_derives_the_horizon_and_survives_a_graph_loss() {
    use nuo_protocol::{
        CompactionMeta, DirectiveKind, DirectivePayload, EntryPayload, Message,
        ProjectionDirective, Role, TranscriptEntry,
    };
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    for i in 0..10u64 {
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, format!("turn {i}")),
        ));
    }
    let split_parent = data.transcript.entries[5].id.clone();
    let first_kept = data.transcript.entries[6].id.clone();
    let folded_through = data.transcript.entries[5].seq;

    let mut checkpoint =
        TranscriptEntry::from_message(0, &nuo_protocol::checkpoint_message("SUMMARY"));
    checkpoint.id = "cp-horizon".into();
    checkpoint.content = Some("SUMMARY".into());
    checkpoint.payload = EntryPayload::Compaction(Box::new(CompactionMeta {
        first_kept_node_id: first_kept.clone(),
        split_parent_node_id: Some(split_parent.clone()),
        folded_through_seq: folded_through,
        tokens_before: 123,
        read_files: vec!["src/a.rs".into()],
        modified_files: vec!["src/b.rs".into()],
    }));
    data.transcript.push(checkpoint);
    let checkpoint_seq = data.transcript.entries.last().unwrap().seq;
    data.transcript.push_directive(ProjectionDirective {
        seq: 0,
        kind: DirectiveKind::Compact,
        up_to_seq: folded_through,
        payload: DirectivePayload::Compact { checkpoint_seq },
    });
    // A fact appended after the fold must attach to the kept tail, not to the
    // checkpoint (which is last in sequence order).
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::Assistant, "after fold"),
    ));
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();
    assert_eq!(
        count_causal_nodes(&engine.conn),
        0,
        "a save derives nothing"
    );
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();

    let (kind, parent, seq): (String, Option<String>, i64) = engine
        .conn
        .query_row(
            "SELECT kind, parent_id, seq FROM causal_nodes WHERE session_id = ?1 AND id = 'cp-horizon'",
            rusqlite::params![data.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(kind, "compaction");
    assert_eq!(parent.as_deref(), Some(split_parent.as_str()));
    assert_eq!(seq as u64, checkpoint_seq);

    let active = |conn: &Connection| {
        let ir = crate::db::session_ir::load_session_ir(conn, &data.id)
            .unwrap()
            .unwrap();
        assert_eq!(ir.state.compaction_horizon.as_deref(), Some("cp-horizon"));
        ir.resolve_active_messages()
    };
    let window = active(&engine.conn);
    assert!(window.iter().any(|m| m.content.contains("SUMMARY")));
    assert!(window.iter().any(|m| m.content.contains("turn 6")));
    assert!(window.iter().any(|m| m.content.contains("after fold")));
    assert!(
        !window.iter().any(|m| m.content.contains("turn 0")),
        "the folded head entered the active window: {window:#?}"
    );
    let projected = data.transcript.project_messages();
    assert_eq!(projected.len(), window.len());

    // Lose the derived graph but keep the cursor: the coverage probe must
    // rebuild the horizon from the transcript alone.
    engine.conn.execute("DELETE FROM causal_nodes", []).unwrap();
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();
    assert_eq!(
        count_causal_nodes(&engine.conn),
        12,
        "11 dialogue facts + one derived horizon"
    );
    let window = active(&engine.conn);
    assert!(window.iter().any(|m| m.content.contains("turn 6")));
    assert!(!window.iter().any(|m| m.content.contains("turn 0")));
}

/// The online scrub repairs a damaged session without any reader touching it.
///
/// This is residual C: a corrupt graph is inert in the read path but still a
/// lie in the index. The writer probes a bounded slice while idle, using the
/// exact repair a reader would run.
#[test]
fn the_idle_scrub_repairs_a_corrupt_session() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    for i in 0..5u64 {
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, format!("message {i}")),
        ));
    }
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();

    let head: String = engine
        .conn
        .query_row(
            "SELECT id FROM causal_nodes WHERE session_id = ?1 AND seq = 4",
            rusqlite::params![data.id],
            |row| row.get(0),
        )
        .unwrap();
    engine
        .conn
        .execute(
            "UPDATE causal_nodes SET parent_id = ?2 WHERE session_id = ?1 AND seq = 0",
            rusqlite::params![data.id, head],
        )
        .unwrap();
    assert!(crate::db::ir_mirror::has_corrupt_lineage(&engine.conn, &data.id).unwrap());

    let scanned =
        crate::db::derived::scrub_batch(&engine.conn, crate::db::derived::SCRUB_BATCH).unwrap();
    assert!(scanned >= 1);
    assert!(
        !crate::db::ir_mirror::has_corrupt_lineage(&engine.conn, &data.id).unwrap(),
        "the scrub left the graph damaged"
    );
    assert_eq!(dialogue_chain_len(&engine.conn, &data.id), 5);
}

/// A lost cursor over an existing graph triggers a whole-transcript rebuild.
///
/// The rebuild covers the **entire** transcript, so the cursor must jump to its
/// end. Advancing it by one drain budget instead leaves the next batch thinking
/// the tail is unmigrated; re-mirroring those rows then rewrote their parent
/// edges (the graph's head began pointing at its own tail) and `linear_path`
/// walked the cycle until the process ran out of memory.
#[test]
fn a_rebuild_covers_the_whole_transcript_in_one_cursor_step() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    for i in 0..300u64 {
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, format!("message {i}")),
        ));
    }
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();
    assert_eq!(dialogue_chain_len(&engine.conn, &data.id), 300);

    // A session whose cursor row is missing reports -1; the graph is intact.
    engine
        .conn
        .execute(
            "DELETE FROM derived_cursors WHERE session_id = ?1",
            rusqlite::params![data.id],
        )
        .unwrap();
    crate::db::derived::enqueue(&engine.conn, &data.id, 299, unix_ms()).unwrap();

    while crate::db::derived::advance_batch(&engine.conn, crate::db::derived::IDLE_BATCH).unwrap()
        > 0
    {}

    assert_eq!(causal_cursor(&engine.conn, &data.id), 299);
    assert_eq!(
        dialogue_chain_len(&engine.conn, &data.id),
        300,
        "the rebuilt chain is linear, not a loop"
    );
    assert!(
        !crate::db::ir_mirror::has_corrupt_lineage(&engine.conn, &data.id).unwrap(),
        "the rebuild leaves no forward parent edges"
    );
}

/// A graph damaged by an older binary must heal on the next read even though
/// its cursors claim full coverage. Cursor equality says how far the mirror
/// reached, not that the edges it wrote are walkable.
#[test]
fn a_corrupt_lineage_self_heals_on_flush() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    for i in 0..5u64 {
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, format!("message {i}")),
        ));
    }
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();

    // The exact corruption shape found in the live database: the first node's
    // parent is the newest node, so walking from the leaf revisits the head.
    let head: String = engine
        .conn
        .query_row(
            "SELECT id FROM causal_nodes WHERE session_id = ?1 AND seq = 4",
            rusqlite::params![data.id],
            |row| row.get(0),
        )
        .unwrap();
    engine
        .conn
        .execute(
            "UPDATE causal_nodes SET parent_id = ?2 WHERE session_id = ?1 AND seq = 0",
            rusqlite::params![data.id, head],
        )
        .unwrap();
    assert!(
        crate::db::ir_mirror::has_corrupt_lineage(&engine.conn, &data.id).unwrap(),
        "the forward edge is detected"
    );

    // Cursors are already at the end, so only the integrity probe can trigger.
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();

    assert!(
        !crate::db::ir_mirror::has_corrupt_lineage(&engine.conn, &data.id).unwrap(),
        "the flush repaired the lineage"
    );
    assert_eq!(dialogue_chain_len(&engine.conn, &data.id), 5);
    assert_eq!(causal_cursor(&engine.conn, &data.id), 4);
}

/// A transcript whose graph is missing (a session saved before the live mirror
/// existed, or restored from migration) must be backfilled, not linked onto a
/// missing parent.
///
/// Mirroring only the appended tail onto an empty graph builds an orphaned
/// segment: `linear_path` walks it from the newest fact and stops, so
/// compaction reads a conversation that begins mid-history. The predecessor
/// check turns that into a one-time full backfill.
#[test]
fn a_missing_graph_is_backfilled_instead_of_linked_onto_nothing() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::Assistant, "two"),
    ));
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();
    assert_eq!(
        count_causal_nodes(&engine.conn),
        2,
        "the drain mirrored both entries"
    );

    // The graph predates the live mirror.
    engine.conn.execute("DELETE FROM causal_nodes", []).unwrap();
    assert_eq!(count_causal_nodes(&engine.conn), 0);

    data.transcript.push(TranscriptEntry::from_message(
        2,
        &Message::new(Role::User, "three"),
    ));
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        &data,
        false,
        &[],
        &CommitGuard::default(),
    )
    .unwrap();
    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();

    assert_eq!(
        count_causal_nodes(&engine.conn),
        3,
        "the drain must backfill the missing history, not just the delta"
    );
    let root_parent: Option<String> = engine
        .conn
        .query_row(
            "SELECT parent_id FROM causal_nodes WHERE seq = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        root_parent.is_none(),
        "a backfill starts a fresh chain, never a cycle"
    );
    let parent: String = engine
        .conn
        .query_row(
            "SELECT parent_id FROM causal_nodes WHERE seq = 2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        parent, data.transcript.entries[1].id,
        "the appended node must link to its transcript predecessor"
    );
}

fn count_causal_nodes(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM causal_nodes", [], |row| row.get(0))
        .unwrap()
}

/// Append-only commits must leave one contiguous parent chain. Each save
/// mirrors only its own delta, so linkage *across* saves is the whole
/// contract — a broken link makes `linear_path` see only the newest segment,
/// which is precisely how compaction silently stops engaging.
#[test]
fn a_delta_chain_links_across_saves() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    for round in 0..3u64 {
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, format!("user {round}")),
        ));
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::Assistant, format!("assistant {round}")),
        ));
        session_store::save_session_inner(
            &engine.conn,
            engine.blob_store.as_ref(),
            &data,
            false,
            &[],
            &CommitGuard::default(),
        )
        .unwrap();
        crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();
    }

    let mut stmt = engine
        .conn
        .prepare("SELECT seq, id, parent_id FROM causal_nodes ORDER BY seq")
        .unwrap();
    let rows: Vec<(u64, String, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(rows.len(), 6, "one node per entry");
    assert!(rows[0].2.is_none(), "the chain starts at the root");
    for window in rows.windows(2) {
        assert_eq!(
            window[1].2.as_deref(),
            Some(window[0].1.as_str()),
            "seq {} must link to seq {}",
            window[1].0,
            window[0].0
        );
    }
}

#[test]
fn generation_mismatch_escalates_delta_to_full_rewrite() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "old"),
    ));
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), &data).unwrap();

    // Rebuild: fresh entries, fresh ids, fresh generation. The delta save
    // must notice the generation change and rewrite everything.
    let rebuilt = crate::session::rebuild_for_test(&[Message::new(Role::User, "rebuilt")]);
    data.transcript = rebuilt;
    data.generation = uuid::Uuid::new_v4().to_string();
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        &data,
        false,
        &[],
        &CommitGuard::default(),
    )
    .unwrap();

    let reloaded = session_store::load_session_full(&engine.conn, &data.id)
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 1);
    assert_eq!(
        reloaded.transcript.entries[0].to_message().unwrap().content,
        "rebuilt"
    );
    assert_eq!(reloaded.generation, data.generation);
}

pub(super) fn save_session_full(
    engine: &DatabaseEngine,
    data: &crate::session::SessionData,
) -> std::result::Result<(), SaveError> {
    session_store::save_session_full(&engine.conn, engine.blob_store.as_ref(), data)
}

pub(super) fn load_session_full(
    engine: &DatabaseEngine,
    session_id: &str,
) -> Result<Option<crate::session::SessionData>> {
    session_store::load_session_full(&engine.conn, session_id)
}

pub(super) fn guarded_save(
    engine: &DatabaseEngine,
    data: &crate::session::SessionData,
    operation_id: Option<&str>,
    expected_revision: Option<u64>,
) -> std::result::Result<u64, SaveError> {
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        data,
        false,
        &[],
        &CommitGuard {
            operation_id: operation_id.map(str::to_string),
            expected_revision,
        },
    )
}

#[test]
fn commit_revision_advances_and_is_durable() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 0);

    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    assert_eq!(guarded_save(&engine, &data, Some("op-1"), None).unwrap(), 1);
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 1);

    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "two"),
    ));
    assert_eq!(
        guarded_save(&engine, &data, Some("op-2"), Some(1)).unwrap(),
        2
    );
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 2);
}

#[test]
fn replayed_operation_returns_its_original_receipt() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));

    let first = guarded_save(&engine, &data, Some("op-1"), None).unwrap();
    let replay = guarded_save(&engine, &data, Some("op-1"), None).unwrap();
    assert_eq!(first, replay, "a replay returns the original revision");
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), first);
    let receipt = latest_commit_receipt(&engine.conn, &data.id)
        .unwrap()
        .expect("a receipt is durable");
    assert_eq!(receipt.0, "op-1");
    assert_eq!(receipt.2, first);
    let reloaded = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert_eq!(
        reloaded.transcript.entries.len(),
        1,
        "the delta must not be applied twice"
    );
}

#[test]
fn reused_operation_id_for_other_content_is_refused() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    guarded_save(&engine, &data, Some("op-1"), None).unwrap();

    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "two"),
    ));
    let error = guarded_save(&engine, &data, Some("op-1"), None).unwrap_err();
    assert!(
        matches!(error, SaveError::OperationConflict { .. }),
        "expected an operation conflict, got {error:?}"
    );
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 1);
    let reloaded = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 1);
}

#[test]
fn stale_expected_revision_fails_closed() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    guarded_save(&engine, &data, None, None).unwrap();

    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "two"),
    ));
    let error = guarded_save(&engine, &data, None, Some(0)).unwrap_err();
    assert!(
        matches!(
            error,
            SaveError::StaleRevision {
                expected: 0,
                actual: 1
            }
        ),
        "expected a stale-revision refusal, got {error:?}"
    );
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 1);
    let reloaded = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert_eq!(
        reloaded.transcript.entries.len(),
        1,
        "the stale commit must not be applied"
    );
}

#[test]
fn delta_save_offloads_only_newly_inserted_rows() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let blob_store =
        BlobStore::new(std::env::temp_dir().join(format!("nuo-offload-{}", uuid::Uuid::new_v4())));
    let mut data = crate::session::SessionData::default();
    let big = "x".repeat(CAS_THRESHOLD_BYTES * 4);
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, big.clone()),
    ));
    let stored = engine_blob_round_trip(&data, &blob_store);
    let reloaded = load_session_full(&stored, &data.id).unwrap().unwrap();
    let payload = reloaded.transcript.entries[0].as_message().unwrap();
    let hash = payload.content_blob.as_ref().expect("body was offloaded");
    assert_eq!(
        reloaded.transcript.entries[0].content.as_deref(),
        Some(""),
        "the inline body is cleared"
    );
    assert_eq!(blob_store.get(hash).unwrap(), big.as_bytes());
    let _ = std::fs::remove_dir_all(blob_store.root());
}
