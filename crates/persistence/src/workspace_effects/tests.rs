//! The workspace effect journal's contracts, exercised as SQL facts.
//!
//! Every test here asserts an *invariant*, not an implementation detail: the
//! journal is evidence, so the properties that make it trustworthy (write-once
//! observations, write-ahead ordering, one-shot completion and reversal, ordered
//! reads) are what must hold. A test that only re-stated the SQL would pass while
//! the invariant it was supposed to protect rotted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::db::open_in_memory_for_tests;

const THREAD: &str = "thread-1";
const OTHER_THREAD: &str = "thread-2";

/// Insert a thread row so the journal's foreign key is satisfiable.
fn seed_thread(conn: &Connection, thread_id: &str) {
    conn.execute(
        "INSERT INTO threads (id, title, created_at_ms, updated_at_ms) VALUES (?1, '', 0, 0)",
        params![thread_id],
    )
    .unwrap();
}

/// A write-ahead effect: pre-image known, post-image not yet observed.
fn new_edit<'a>(seq: i64, path: &'a str, before: &'a str) -> NewWorkspaceEffect<'a> {
    NewWorkspaceEffect {
        thread_id: THREAD,
        round_id: "r1",
        turn_id: "t1",
        call_id: "call-1",
        seq,
        tool_name: "edit_text",
        path: Some(path),
        op: JournalOp::Edit,
        effect_class: "file_bytes",
        before_hash: Some(before),
        after_hash: None,
        irreversible_reason: None,
        created_at_ms: 1_000,
    }
}

/// Append a write-ahead effect and immediately complete it — the normal
/// successful path, in one line.
fn append_completed(conn: &Connection, effect: &NewWorkspaceEffect<'_>, after: &str) -> i64 {
    let id = append_effect(conn, effect).unwrap();
    assert!(complete_effect(conn, id, Some(after), 2_000).unwrap());
    id
}

#[test]
fn append_and_read_back_preserves_every_field() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);

    let id = append_completed(&conn, &new_edit(1, "/ws/src/lib.rs", "aaa"), "bbb");

    let read = list_effects(&conn, THREAD).unwrap();
    assert_eq!(read.len(), 1);
    let row = &read[0];
    assert_eq!(row.id, id);
    assert_eq!(row.thread_id, THREAD);
    assert_eq!(row.round_id, "r1");
    assert_eq!(row.turn_id, "t1");
    assert_eq!(row.call_id, "call-1");
    assert_eq!(row.seq, 1);
    assert_eq!(row.tool_name, "edit_text");
    assert_eq!(row.path.as_deref(), Some("/ws/src/lib.rs"));
    assert_eq!(row.op, JournalOp::Edit);
    assert_eq!(row.effect_class, "file_bytes");
    assert_eq!(row.before_hash.as_deref(), Some("aaa"));
    assert_eq!(row.after_hash.as_deref(), Some("bbb"));
    assert_eq!(row.completed_at_ms, Some(2_000));
    assert_eq!(row.reversed_at_ms, None);
    assert!(row.is_completed());
    assert!(row.is_undoable());
}

/// `INV-EFFECT-03`: the row exists *before* the mutation is observable.
///
/// This is the whole reason the journal is retained; the test states the
/// property directly so a future refactor that appends after the write fails
/// here rather than in production.
#[test]
fn write_ahead_effect_is_recorded_before_its_post_image_exists() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);

    let id = append_effect(&conn, &new_edit(1, "/ws/a.rs", "before")).unwrap();
    let row = &list_effects(&conn, THREAD).unwrap()[0];

    assert_eq!(row.id, id);
    assert_eq!(row.before_hash.as_deref(), Some("before"));
    assert_eq!(
        row.after_hash, None,
        "the post-image cannot be known before the tool runs"
    );
    assert!(!row.is_completed());
    assert!(
        !row.is_undoable(),
        "an unconfirmed effect must never be offered for reversal"
    );
}

/// `INV-EFFECT-01`: an observation cannot be rewritten after the fact.
///
/// This is what makes `/undo` safe to trust: the pre-image hash it reads is the
/// hash captured at execution time, not one a later bug (or a later round's
/// "helpful" repair) could have substituted.
#[test]
fn observations_are_write_once() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let id = append_completed(&conn, &new_edit(1, "/ws/a.rs", "before"), "after");

    for (column, value) in [
        ("path", "'/ws/elsewhere.rs'"),
        ("before_hash", "'forged'"),
        ("effect_class", "'none'"),
        ("tool_name", "'write_file'"),
        ("round_id", "'r9'"),
        ("seq", "99"),
        ("irreversible_reason", "'fabricated'"),
    ] {
        let err = conn
            .execute(
                &format!("UPDATE workspace_effects SET {column} = {value} WHERE id = ?1"),
                params![id],
            )
            .expect_err(&format!("mutating {column} must be refused"));
        let message = err.to_string();
        assert!(
            message.contains("INV-EFFECT-01"),
            "expected the write-once trigger to name the invariant, got: {message}"
        );
    }

    // The sanctioned transitions still work, and the original observation
    // survives beside them.
    assert!(mark_reversed(&conn, id, 5_000, Some("restored")).unwrap());
    let row = &list_effects(&conn, THREAD).unwrap()[0];
    assert_eq!(row.reversed_at_ms, Some(5_000));
    assert_eq!(row.reversal_note.as_deref(), Some("restored"));
    assert_eq!(row.before_hash.as_deref(), Some("before"));
    assert_eq!(row.after_hash.as_deref(), Some("after"));
    assert_eq!(row.path.as_deref(), Some("/ws/a.rs"));
}

/// `INV-EFFECT-03`: completion happens at most once and is then frozen.
#[test]
fn completion_is_one_shot_and_the_post_image_cannot_be_rewritten() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let id = append_effect(&conn, &new_edit(1, "/ws/a.rs", "before")).unwrap();

    assert!(complete_effect(&conn, id, Some("after"), 2_000).unwrap());
    // A second completion is reported as lost work, not silently applied.
    assert!(
        !complete_effect(&conn, id, Some("other"), 3_000).unwrap(),
        "a second completion of the same row must be refused"
    );

    // Directly rewriting either completion column is refused by trigger.
    for sql in [
        "UPDATE workspace_effects SET completed_at_ms = NULL WHERE id = ?1",
        "UPDATE workspace_effects SET completed_at_ms = 9000 WHERE id = ?1",
        "UPDATE workspace_effects SET after_hash = 'rewritten' WHERE id = ?1",
        "UPDATE workspace_effects SET after_hash = NULL WHERE id = ?1",
    ] {
        let err = conn.execute(sql, params![id]).expect_err("must be refused");
        let message = err.to_string();
        assert!(
            message.contains("INV-EFFECT-03"),
            "expected the monotonicity trigger for `{sql}`, got: {message}"
        );
    }

    let row = &list_effects(&conn, THREAD).unwrap()[0];
    assert_eq!(row.after_hash.as_deref(), Some("after"));
    assert_eq!(row.completed_at_ms, Some(2_000));
}

/// `INV-EFFECT-04`: an unconfirmed effect is not something the journal may act
/// on. Reversing it would write the pre-image back only if this call was what
/// changed the file — exactly what an incomplete record cannot tell us.
#[test]
fn unconfirmed_effect_cannot_be_reversed() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let id = append_effect(&conn, &new_edit(1, "/ws/a.rs", "before")).unwrap();

    let err = conn
        .execute(
            "UPDATE workspace_effects SET reversed_at_ms = 5000 WHERE id = ?1",
            params![id],
        )
        .expect_err("reversing an unconfirmed effect must be refused");
    assert!(err.to_string().contains("INV-EFFECT-04"), "got: {err}");

    // The helper surfaces the trigger's refusal rather than reporting success:
    // a caller that believes it reversed something it did not is the bug this
    // invariant exists to prevent.
    let err = mark_reversed(&conn, id, 5_000, None)
        .expect_err("mark_reversed must propagate the refusal, not return success");
    assert!(err.to_string().contains("INV-EFFECT-04"), "got: {err}");

    let row = &list_effects(&conn, THREAD).unwrap()[0];
    assert_eq!(row.reversed_at_ms, None);
}

/// `INV-EFFECT-02`: reversal is one-shot and monotonic, so replaying `/undo`
/// cannot write stale bytes back over content a later round produced.
#[test]
fn reversal_is_one_shot_and_cannot_be_cleared() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let id = append_completed(&conn, &new_edit(1, "/ws/a.rs", "before"), "after");

    assert!(mark_reversed(&conn, id, 5_000, None).unwrap());
    assert!(
        !mark_reversed(&conn, id, 6_000, None).unwrap(),
        "a second reversal of the same row must be refused"
    );
    for sql in [
        "UPDATE workspace_effects SET reversed_at_ms = NULL WHERE id = ?1",
        "UPDATE workspace_effects SET reversed_at_ms = 7000 WHERE id = ?1",
    ] {
        let err = conn.execute(sql, params![id]).expect_err("must be refused");
        assert!(
            err.to_string().contains("INV-EFFECT-02"),
            "expected the monotonicity trigger, got: {err}"
        );
    }
}

/// `INV-EFFECT-01`: rows are append-only evidence, so there is no delete path.
#[test]
fn journal_rows_cannot_be_deleted() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    let id = append_completed(&conn, &new_edit(1, "/ws/a.rs", "before"), "after");

    let err = conn
        .execute("DELETE FROM workspace_effects WHERE id = ?1", params![id])
        .expect_err("delete must be refused");
    assert!(
        err.to_string().contains("INV-EFFECT-01"),
        "expected the append-only trigger, got: {err}"
    );
}

#[test]
fn reads_are_thread_scoped_and_ordered_by_observation() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    seed_thread(&conn, OTHER_THREAD);

    // Insert out of seq order to prove the read sorts rather than relying on
    // insertion order (AUTOINCREMENT id and seq need not agree across
    // concurrent appenders).
    let mut third = new_edit(3, "/ws/c.rs", "c0");
    third.call_id = "call-3";
    append_completed(&conn, &third, "c1");
    let mut first = new_edit(1, "/ws/a.rs", "a0");
    first.call_id = "call-1";
    append_completed(&conn, &first, "a1");
    let mut second = new_edit(2, "/ws/b.rs", "b0");
    second.call_id = "call-2";
    append_completed(&conn, &second, "b1");

    let mut foreign = new_edit(1, "/ws/other.rs", "x0");
    foreign.thread_id = OTHER_THREAD;
    foreign.call_id = "call-x";
    append_completed(&conn, &foreign, "x1");

    let paths: Vec<String> = list_effects(&conn, THREAD)
        .unwrap()
        .into_iter()
        .map(|effect| effect.path.unwrap())
        .collect();
    assert_eq!(paths, ["/ws/a.rs", "/ws/b.rs", "/ws/c.rs"]);
}

/// The undo candidate set is exactly the reversible, confirmed,
/// not-yet-reversed rows, newest-first so overlapping writes to one path unwind
/// through their own history instead of jumping to the oldest pre-image.
#[test]
fn undoable_set_is_reversible_only_and_newest_first() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);

    let first = append_completed(&conn, &new_edit(1, "/ws/a.rs", "a0"), "a1");
    // A shell-out in the middle: recorded, never undoable, and it *breaks the
    // chain* — the row after it must not be reversed back past it.
    append_completed(
        &conn,
        &NewWorkspaceEffect {
            effect_class: "opaque",
            irreversible_reason: Some("ran with arbitrary execution authority"),
            before_hash: None,
            after_hash: Some("shell-out"),
            op: JournalOp::Edit,
            ..new_edit(2, "/ws/a.rs", "")
        },
        "shell-out",
    );
    append_completed(&conn, &new_edit(3, "/ws/a.rs", "shell"), "a2");

    let undoable = undoable_effects(&conn, THREAD, 10).unwrap();
    assert_eq!(
        undoable.iter().map(|effect| effect.seq).collect::<Vec<_>>(),
        [3, 1],
        "newest first, opaque rows excluded"
    );

    mark_reversed(&conn, first, 9_000, None).unwrap();
    let after = undoable_effects(&conn, THREAD, 10).unwrap();
    assert_eq!(
        after.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [3],
        "a reversed row leaves the undo set"
    );
}

/// An unconfirmed row must not appear in the undo candidate set: offering it
/// would mean `/undo` reported work it then refused to do.
#[test]
fn undoable_set_excludes_unconfirmed_effects() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);

    append_effect(&conn, &new_edit(1, "/ws/a.rs", "a0")).unwrap();
    append_completed(&conn, &new_edit(2, "/ws/b.rs", "b0"), "b1");

    let undoable = undoable_effects(&conn, THREAD, 10).unwrap();
    assert_eq!(
        undoable.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [2],
        "only the confirmed effect is offered"
    );
}

#[test]
fn undoable_set_honours_the_limit() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    for seq in 1..=5 {
        append_completed(&conn, &new_edit(seq, "/ws/a.rs", "x"), "y");
    }
    assert_eq!(undoable_effects(&conn, THREAD, 2).unwrap().len(), 2);
    assert_eq!(undoable_effects(&conn, THREAD, 100).unwrap().len(), 5);
}

#[test]
fn round_scoped_read_starts_at_that_round_boundary() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);

    let mut r1 = new_edit(1, "/ws/a.rs", "a0");
    r1.round_id = "r1";
    append_completed(&conn, &r1, "a1");
    let mut r2a = new_edit(2, "/ws/b.rs", "b0");
    r2a.round_id = "r2";
    append_completed(&conn, &r2a, "b1");
    let mut r2b = new_edit(3, "/ws/c.rs", "c0");
    r2b.round_id = "r2";
    append_completed(&conn, &r2b, "c1");

    let from_r2 = list_effects_from_round(&conn, THREAD, "r2").unwrap();
    assert_eq!(
        from_r2.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [2, 3],
        "a round-scoped read begins at that round and includes later rounds"
    );

    // An unknown round is empty rather than an error: a thread that never
    // wrote files has no journal, which is not a failure.
    assert!(
        list_effects_from_round(&conn, THREAD, "r99")
            .unwrap()
            .is_empty()
    );
}

/// Both boundaries against an unknown operation, proved independently.
///
/// The journal's `op` vocabulary is closed at *two* points, and it matters that
/// each holds on its own:
///
/// 1. **Write side** — a `CHECK` constraint refuses a row whose op this build
///    does not define, so no writer can record an operation with semantics the
///    reader cannot reconstruct.
/// 2. **Read side** — [`JournalOp::parse`] refuses to guess. Guessing `Edit`
///    for an unknown op would reverse by writing content back over a file that
///    should have been removed; the reader must fail instead.
///
/// The read-side guard is defense in depth: a database stamped by a newer
/// binary is already refused at open (`migrate_schema` rejects
/// `user_version > CURRENT_DB_VERSION`), so this path is only reachable if a
/// future build widens the `CHECK` without teaching the parser.
#[test]
fn unknown_op_is_refused_on_both_the_write_and_read_paths() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);

    // Write side: the CHECK constraint closes the vocabulary.
    let err = conn
        .execute(
            "INSERT INTO workspace_effects (
                thread_id, round_id, turn_id, call_id, seq, tool_name, path, op,
                effect_class, created_at_ms
             ) VALUES (?1, 'r1', 't1', 'c1', 1, 'tool', '/ws/a.rs', 'rename', 'file_bytes', 0)",
            params![THREAD],
        )
        .expect_err("the op CHECK must refuse an undefined operation");
    assert!(
        err.to_string().contains("CHECK constraint failed"),
        "got: {err}"
    );

    // Read side: the parser has no fallback variant.
    assert_eq!(JournalOp::parse("rename"), None);
    assert_eq!(JournalOp::parse(""), None);
    // ...and the defined vocabulary still round-trips.
    for op in [JournalOp::Create, JournalOp::Edit, JournalOp::Delete] {
        assert_eq!(JournalOp::parse(op.as_str()), Some(op));
    }
}

#[test]
fn latest_seq_tracks_the_thread_not_the_table() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    seed_thread(&conn, OTHER_THREAD);

    assert_eq!(latest_seq(&conn, THREAD).unwrap(), 0);
    append_completed(&conn, &new_edit(1, "/ws/a.rs", "x"), "y");
    let mut foreign = new_edit(7, "/ws/z.rs", "x");
    foreign.thread_id = OTHER_THREAD;
    append_completed(&conn, &foreign, "y");

    assert_eq!(
        latest_seq(&conn, THREAD).unwrap(),
        1,
        "another thread's higher seq must not leak in"
    );
}

#[test]
fn irreversibility_reason_survives_round_trip() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    append_completed(
        &conn,
        &NewWorkspaceEffect {
            effect_class: "opaque",
            irreversible_reason: Some("it ran with arbitrary execution authority"),
            before_hash: None,
            op: JournalOp::Edit,
            ..new_edit(1, "/ws/a.rs", "")
        },
        "out",
    );

    let row = &list_effects(&conn, THREAD).unwrap()[0];
    assert_eq!(
        row.irreversible_reason.as_deref(),
        Some("it ran with arbitrary execution authority")
    );
    assert!(
        !row.is_undoable(),
        "a row carrying an irreversibility reason must not be offered for undo"
    );
    // Even a *completed* external effect is not undoable: its class is the
    // refusal, not its completion state.
    assert!(row.is_completed());
}

/// A `create` has no pre-image, and its reversal is a deletion rather than
/// writing empty content — the operation, not the hash, decides that.
#[test]
fn create_effect_has_no_pre_image_and_stays_undoable() {
    let conn = open_in_memory_for_tests().unwrap();
    seed_thread(&conn, THREAD);
    append_completed(
        &conn,
        &NewWorkspaceEffect {
            op: JournalOp::Create,
            before_hash: None,
            ..new_edit(1, "/ws/new.rs", "")
        },
        "content",
    );

    let row = &list_effects(&conn, THREAD).unwrap()[0];
    assert_eq!(row.op, JournalOp::Create);
    assert_eq!(row.before_hash, None);
    assert!(row.is_undoable(), "a creation is reversible by removal");
}
