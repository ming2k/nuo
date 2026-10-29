//! Retention and CAS-liveness contracts for the workspace journal.
//!
//! These tests exist because of one specific defect that would have been invisible
//! until a user lost an undo: the collector's live set was derived from `blob_refs`
//! only, so the first maintenance pass after a pre-image was written would delete
//! it, and `/undo` would report "the recorded pre-image is no longer available" for
//! a row that still looked perfectly undoable. A pre-image that GC reclaims is not
//! a degraded undo — it is a broken promise with no signal.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::db::garbage_collection;
use crate::db::open_in_memory_for_tests;

const THREAD: &str = "thread-1";

fn seed_thread(conn: &Connection, thread_id: &str) {
    conn.execute(
        "INSERT INTO threads (id, title, created_at_ms, updated_at_ms) VALUES (?1, '', 0, 0)",
        params![thread_id],
    )
    .unwrap();
}

/// A completed, reversible row with both images recorded.
fn completed_row(conn: &Connection, before: &str, after: &str, created_at_ms: u64) -> i64 {
    let effect = NewWorkspaceEffect {
        thread_id: THREAD,
        round_id: "r1",
        turn_id: "t1",
        call_id: "c1",
        seq: 1,
        tool_name: "edit_text",
        path: Some("/ws/a.rs"),
        op: JournalOp::Edit,
        effect_class: "file_bytes",
        before_hash: Some(before),
        after_hash: Some(after),
        irreversible_reason: None,
        created_at_ms,
    };
    let id = append_effect(conn, &effect).unwrap();
    assert!(complete_effect(conn, id, Some(after), created_at_ms).unwrap());
    id
}

/// The regression: a journal pre-image is live for the collector.
///
/// Without this, `retain_only(live)` deletes the pre-image and every undo of a
/// file write fails at the last moment.
#[test]
fn journal_preimages_are_live_for_the_blob_collector() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    completed_row(&conn, "preimage-hash", "postimage-hash", 1_000);

    let live = garbage_collection::live_blob_hashes(&conn).unwrap();
    assert!(
        live.contains("preimage-hash"),
        "a reversible pre-image must be live, or /undo is broken"
    );
    assert!(
        live.contains("postimage-hash"),
        "the post-image is live: /diff compares against it"
    );
}

/// A transcript body and a pre-image with identical content are both live, and
/// pruning one does not clear the other.
///
/// This is why liveness is a UNION over two owners rather than a shared
/// `(session_id, hash)` ledger row: two owners sharing one row means either one's
/// removal silently disowns the other's bytes.
#[test]
fn shared_content_is_live_for_both_owners() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    // A transcript entry references the same hash as the pre-image.
    conn.execute(
        "INSERT INTO blob_refs (session_id, hash) VALUES (?1, 'shared')",
        params![THREAD],
    )
    .unwrap();
    completed_row(&conn, "shared", "after", 1_000);

    // Removing the transcript's ledger row must not make the pre-image dead.
    conn.execute("DELETE FROM blob_refs WHERE hash = 'shared'", [])
        .unwrap();
    let live = garbage_collection::live_blob_hashes(&conn).unwrap();
    assert!(
        live.contains("shared"),
        "the journal still owns this content"
    );
}

/// Expiry makes the bytes reclaimable while the record survives.
///
/// The distinction is the whole point of `expired_at_ms`: the change *happened*,
/// so the journal keeps describing it; only the ability to restore it lapses.
#[test]
fn expiry_reclaims_bytes_but_keeps_the_record() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let id = completed_row(&conn, "old-preimage", "old-postimage", 1_000);

    // Nothing expires inside the window.
    assert_eq!(expire_preimages(&conn, 500, 10_000).unwrap(), 0);

    // The row is older than the cutoff: it expires.
    assert_eq!(expire_preimages(&conn, 5_000, 10_000).unwrap(), 1);

    // The bytes leave the live set...
    let live = garbage_collection::live_blob_hashes(&conn).unwrap();
    assert!(!live.contains("old-preimage"), "bytes are reclaimable now");
    assert!(!live.contains("old-postimage"));

    // ...but the observation is intact and readable.
    let row = &list_effects(&conn, THREAD).unwrap()[0];
    assert_eq!(row.id, id);
    assert_eq!(row.path.as_deref(), Some("/ws/a.rs"));
    assert_eq!(row.before_hash.as_deref(), Some("old-preimage"));
    assert_eq!(row.expired_at_ms, Some(10_000));
    assert!(
        !row.is_undoable(),
        "an expired row must not be offered for reversal"
    );
}

/// An expired row is not in the undo candidate set, so `/undo` never reports work
/// it then has to refuse for a missing blob.
#[test]
fn expired_rows_leave_the_undo_candidate_set() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    completed_row(&conn, "pre", "post", 1_000);
    assert_eq!(undoable_effects(&conn, THREAD, 10).unwrap().len(), 1);

    expire_preimages(&conn, 5_000, 10_000).unwrap();
    assert!(
        undoable_effects(&conn, THREAD, 10).unwrap().is_empty(),
        "an expired pre-image cannot be restored, so it must not be offered"
    );
}

/// `INV-EFFECT-05`: expiry is one-way, so an expired pre-image cannot be
/// resurrected into the live set after the collector has reclaimed it.
#[test]
fn expiry_is_one_way() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let id = completed_row(&conn, "pre", "post", 1_000);
    expire_preimages(&conn, 5_000, 10_000).unwrap();

    for sql in [
        "UPDATE workspace_effects SET expired_at_ms = NULL WHERE id = ?1",
        "UPDATE workspace_effects SET expired_at_ms = 11000 WHERE id = ?1",
    ] {
        let err = conn.execute(sql, params![id]).expect_err("must be refused");
        assert!(
            err.to_string().contains("INV-EFFECT-05"),
            "expected the one-way expiry trigger for `{sql}`, got: {err}"
        );
    }
}

/// An unconfirmed row is never expired: the crash-recovery question "what did this
/// call record?" must still be answerable from its own hashes.
#[test]
fn unconfirmed_rows_are_never_expired() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let effect = NewWorkspaceEffect {
        thread_id: THREAD,
        round_id: "r1",
        turn_id: "t1",
        call_id: "c1",
        seq: 1,
        tool_name: "edit_text",
        path: Some("/ws/a.rs"),
        op: JournalOp::Edit,
        effect_class: "file_bytes",
        before_hash: Some("pre"),
        after_hash: None,
        irreversible_reason: None,
        created_at_ms: 1_000,
    };
    let id = append_effect(&conn, &effect).unwrap();

    assert_eq!(
        expire_preimages(&conn, 5_000, 10_000).unwrap(),
        0,
        "an unconfirmed row keeps its record"
    );
    let row = &list_effects(&conn, THREAD).unwrap()[0];
    assert_eq!(row.id, id);
    assert_eq!(row.expired_at_ms, None);
}

/// A reversed row is not expired: its pre-image already served its purpose, and
/// expiring it would be bookkeeping for no benefit.
#[test]
fn reversed_rows_are_not_expired() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let id = completed_row(&conn, "pre", "post", 1_000);
    mark_reversed(&conn, id, 2_000, Some("restored")).unwrap();

    assert_eq!(expire_preimages(&conn, 5_000, 10_000).unwrap(), 0);
}

/// Expiry touches reversible rows' bytes only; a receipt has no pre-image to
/// reclaim and nothing to expire.
#[test]
fn opaque_receipts_have_nothing_to_expire() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    append_completed_effect(
        &conn,
        &NewWorkspaceEffect {
            thread_id: THREAD,
            round_id: "r1",
            turn_id: "t1",
            call_id: "c1",
            seq: 1,
            tool_name: "execute_command",
            path: None,
            op: JournalOp::Edit,
            effect_class: "opaque",
            before_hash: None,
            after_hash: Some("shell-output"),
            irreversible_reason: Some("arbitrary execution authority"),
            created_at_ms: 1_000,
        },
        1_000,
    )
    .unwrap();

    // The receipt's post-image stays live (it is the record of what ran), and the
    // row itself is not an expiry candidate.
    let (live, stale) = preimage_posture(&conn, 5_000).unwrap();
    assert_eq!((live, stale), (0, 0), "a receipt holds no pre-image");
    assert!(
        garbage_collection::live_blob_hashes(&conn)
            .unwrap()
            .contains("shell-output")
    );
}

/// `preimage_posture` reports what the retention policy is about to do.
#[test]
fn posture_counts_live_and_stale_preimages() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    // Two rows: one old, one recent.
    let old = completed_row(&conn, "old", "old-after", 1_000);
    let recent = completed_row(&conn, "recent", "recent-after", 9_000);
    assert_ne!(old, recent, "distinct journal identities");

    let (live, stale) = preimage_posture(&conn, 5_000).unwrap();
    assert_eq!(live, 2, "both pre-images are still held");
    assert_eq!(stale, 1, "one is past the window");
}
