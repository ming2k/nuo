//! Server-side journal adapters, exercised against a real in-memory database.
//!
//! The subject under test is the *seam*: the harness's row vocabulary converted to
//! the storage layer's, written through the single-writer door, and readable back
//! with the invariants intact. The capture and reversal logic is tested where it
//! lives (`nuo-harness`); here the question is only whether the two halves agree.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use nuo_harness::runtime::{JournalOpToken, JournalRow, WorkspaceJournalPort};

/// Build a port over an in-memory engine, seeding the owning thread row.
fn port() -> (PersistenceHandle, tempfile::TempDir) {
    // The single-writer handle needs a file-backed database (it supervises an
    // actor around a real path), so the test uses a temporary directory rather
    // than an in-memory connection.
    let dir = tempfile::tempdir().unwrap();
    let handle = PersistenceHandle::spawn(dir.path().join("state.db"), None);
    handle.ensure_ready().unwrap();
    (handle, dir)
}

fn row(thread_id: &str) -> JournalRow {
    JournalRow {
        thread_id: thread_id.to_string(),
        round_id: "1".to_string(),
        turn_id: "0".to_string(),
        call_id: "call-1".to_string(),
        seq: 42,
        tool_name: "edit_text".to_string(),
        path: Some("/ws/a.rs".to_string()),
        op: JournalOpToken::Edit,
        effect_class: "file_bytes".to_string(),
        before_hash: Some("pre".to_string()),
        irreversible_reason: None,
    }
}

/// Seed a thread so the journal's foreign key is satisfiable through the handle.
fn seed_thread(handle: &PersistenceHandle, thread_id: &str) {
    // The journal table references `threads`, and the single-writer door has no
    // "insert thread" verb, so the row is created through the session upsert
    // path the server already uses.
    handle.try_upsert_session(nuo_persistence::db::ThreadRecord {
        id: thread_id.to_string(),
        parent_id: None,
        fork_kind: "trunk".to_string(),
        fork_point: None,
        title: None,
        created_at_s: 0,
        updated_at_s: 0,
        workspace_root: None,
        persona: None,
        msg_count: 0,
        last_user_prompt: None,
        digest: None,
    });
}

/// The seam round-trips: a harness row becomes a storage row with every field
/// intact, and comes back through the read path.
#[test]
fn harness_row_round_trips_through_the_single_writer_door() {
    let (handle, _dir) = port();
    seed_thread(&handle, "thread-1");
    let blobs = nuo_persistence::blobs::BlobStore::new(_dir.path().join("blobs"));
    let port = SessionJournalPort::new(handle.clone(), blobs);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let id = port.append(row("thread-1")).await.unwrap();
        assert!(id > 0);
        // A write-ahead row is not undoable until completed.
        assert!(
            handle
                .undoable_workspace_effects("thread-1".to_string(), 10)
                .await
                .unwrap()
                .is_empty(),
            "an unconfirmed row must not be offered for undo"
        );

        assert!(
            port.complete(id, Some("post".to_string()), 1_000)
                .await
                .unwrap()
        );

        let effects = handle
            .list_workspace_effects("thread-1".to_string())
            .await
            .unwrap();
        assert_eq!(effects.len(), 1);
        let effect = &effects[0];
        assert_eq!(effect.thread_id, "thread-1");
        assert_eq!(effect.round_id, "1");
        assert_eq!(effect.turn_id, "0");
        assert_eq!(effect.call_id, "call-1");
        assert_eq!(effect.seq, 42);
        assert_eq!(effect.tool_name, "edit_text");
        assert_eq!(effect.path.as_deref(), Some("/ws/a.rs"));
        assert_eq!(
            effect.op,
            nuo_persistence::workspace_effects::JournalOp::Edit
        );
        assert_eq!(effect.effect_class, "file_bytes");
        assert_eq!(effect.before_hash.as_deref(), Some("pre"));
        assert_eq!(effect.after_hash.as_deref(), Some("post"));
        assert!(effect.is_completed());
        assert!(effect.is_undoable());
    });
}

/// A non-reversible receipt arrives already complete: there is no recovery point
/// to wait for, and leaving it unconfirmed would make it look like a mutation of
/// unknown outcome.
#[test]
fn opaque_receipt_is_appended_already_completed() {
    let (handle, dir) = port();
    seed_thread(&handle, "thread-1");
    let blobs = nuo_persistence::blobs::BlobStore::new(dir.path().join("blobs"));
    let port = SessionJournalPort::new(handle.clone(), blobs);

    let receipt = JournalRow {
        tool_name: "execute_command".to_string(),
        path: None,
        effect_class: "opaque".to_string(),
        before_hash: None,
        irreversible_reason: Some("ran with arbitrary execution authority".to_string()),
        ..row("thread-1")
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        // `append_receipt` — not `append`. The phase is *declared* by the caller, so a
        // receipt cannot be produced by a nullability coincidence: an earlier revision
        // inferred it inside `append` and thereby stamped a *withheld* pre-image complete
        // before the tool ran. See `withheld_preimage_is_journaled_write_ahead_not_as_a_receipt`.
        port.append_receipt(receipt, 1_000).await.unwrap();
        let effects = handle
            .list_workspace_effects("thread-1".to_string())
            .await
            .unwrap();
        assert_eq!(effects.len(), 1);
        assert!(
            effects[0].is_completed(),
            "a receipt needs no completion step"
        );
        assert_eq!(
            effects[0].finished_at(),
            Some(1_000),
            "the caller-declared completion instant is recorded verbatim"
        );
        assert!(
            !effects[0].is_undoable(),
            "opaque effects are never undoable"
        );
        assert!(effects[0].irreversible_reason.is_some());
        assert!(
            handle
                .undoable_workspace_effects("thread-1".to_string(), 10)
                .await
                .unwrap()
                .is_empty()
        );
    });
}

/// The reversal stamp goes through the same door and is one-shot.
/// `append` is write-ahead, always: it must never produce a completed row, whatever
/// the row's fields look like. This is the assertion the old inference could not make.
#[test]
fn append_never_completes_a_row() {
    let (handle, dir) = port();
    seed_thread(&handle, "thread-1");
    let blobs = nuo_persistence::blobs::BlobStore::new(dir.path().join("blobs"));
    let port = SessionJournalPort::new(handle.clone(), blobs);

    // The shape that used to be mis-read as a receipt: no pre-image, but a reason.
    let withheld = JournalRow {
        tool_name: "write_file".to_string(),
        path: Some("/ws/.env".to_string()),
        effect_class: "file_bytes".to_string(),
        before_hash: None,
        irreversible_reason: Some("this file conventionally holds credentials".to_string()),
        ..row("thread-1")
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let id = port.append(withheld).await.unwrap();
        let effects = handle
            .list_workspace_effects("thread-1".to_string())
            .await
            .unwrap();
        assert!(
            !effects[0].is_completed(),
            "a write-ahead row is incomplete until the caller completes it"
        );
        // And completion lands, rather than losing a race against a row that was
        // already stamped.
        assert!(
            port.complete(id, None, 2_000).await.unwrap(),
            "completion must take effect on a pending row"
        );
    });
}

#[test]
fn reversal_stamp_is_written_through_the_door_and_is_one_shot() {
    let (handle, dir) = port();
    seed_thread(&handle, "thread-1");
    let blobs = nuo_persistence::blobs::BlobStore::new(dir.path().join("blobs"));
    let port = SessionJournalPort::new(handle.clone(), blobs);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let id = port.append(row("thread-1")).await.unwrap();
        port.complete(id, Some("post".to_string()), 1_000)
            .await
            .unwrap();

        assert!(
            handle
                .mark_workspace_effect_reversed(id, 2_000, Some("restored".to_string()))
                .await
                .unwrap()
        );
        assert!(
            !handle
                .mark_workspace_effect_reversed(id, 3_000, None)
                .await
                .unwrap(),
            "a second reversal must lose the race, not overwrite"
        );
    });
}

/// The blob adapter is a straight pass-through: content addressing is the storage
/// layer's contract, and the harness must not re-implement it.
#[test]
fn blob_adapter_is_content_addressed() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = SessionBlobAdapter::new(nuo_persistence::blobs::BlobStore::new(
        dir.path().to_path_buf(),
    ));

    let first = adapter.put(b"same bytes").unwrap();
    let second = adapter.put(b"same bytes").unwrap();
    let different = adapter.put(b"other bytes").unwrap();

    assert_eq!(first, second, "identical content shares a hash");
    assert_ne!(first, different);
    assert_eq!(
        adapter.get(&first).as_deref(),
        Some(b"same bytes".as_slice())
    );
    assert_eq!(adapter.get("no-such-hash"), None);
}
