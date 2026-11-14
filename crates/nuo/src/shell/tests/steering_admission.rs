//! ADR-0137 steering admission: the invariants that make the outbox/thread
//! boundary honest.
//!
//! These tests exist to *fail first*. Each one pins a property that a naive
//! implementation violates, so the ordering guarantee cannot silently regress
//! into "the steer landed somewhere plausible".

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::shell::app::QueuedDispatchState;

fn steer_dispatch(id: &str, session_id: &str, text: &str) -> QueuedDispatch {
    QueuedDispatch {
        id: id.to_string(),
        session_id: session_id.to_string(),
        state: QueuedDispatchState::Steering,
        text: text.to_string(),
        queued_at_ms: Some(0),
        images: Vec::new(),
        text_pastes: Vec::new(),
    }
}

/// A long thread: three finished rounds, so `messages` carries real history.
/// The steer must land at the seam after the LAST message, never at the head.
fn long_thread() -> Vec<TranscriptMessage> {
    let mut messages = Vec::new();
    for round in 1..=3u64 {
        messages.push(
            TranscriptMessage::new(Role::User, format!("prompt {round}"))
                .with_round(round)
                .with_sent_at_ms(round * 1_000),
        );
        messages.push(
            TranscriptMessage::new(Role::Assistant, format!("answer {round}"))
                .with_round(round)
                .with_turn(1)
                .with_sent_at_ms(round * 1_000 + 1),
        );
    }
    messages
}

/// `[INV-STEER-02]` — an admitted steer lands at the **turn seam**, which is the
/// end of the buffer at admission time (the harness persisted it after the last
/// turn's output and before the next turn's request), NEVER at index 0.
///
/// The previous implementation inserted at index 0, which put a round-4 steer
/// above round 1 for any thread with history. That bug was invisible to a
/// single-message test, so this one loads a real thread.
#[test]
fn admitted_steer_lands_at_the_seam_after_existing_history() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.messages = long_thread();
    let before = app.messages.len();

    let admitted = TranscriptMessage::new(Role::User, "pivot please")
        .with_origin(nuo_view::model::document::UserMessageOrigin::Steer)
        .with_round(3);
    assert!(crate::shell::event_loop::apply::apply_transcript(
        &mut app,
        crate::shell::event_loop::mutations::TranscriptEdit::InsertAdmittedSeam {
            message: admitted,
        },
    ));

    assert_eq!(
        app.messages.len(),
        before + 1,
        "admission appends exactly one fact"
    );
    assert_eq!(
        app.messages.last().unwrap().raw,
        "pivot please",
        "the admitted steer must sit at the seam (the buffer tail at admission), \
         not at the head of the thread"
    );
    assert_eq!(
        app.messages[0].raw, "prompt 1",
        "existing history must keep its order: round 1 stays first"
    );
}

/// `[INV-STEER-05]` — the queue modal's visible set, the addressed set, and the
/// mutation set are the SAME set.
///
/// `queued_at` used to filter by session only while `remove_queued_at` /
/// `recall_queued_at` / `move_queued` filtered by `Waiting`, so with a mixed
/// outbox the highlighted row and the mutated row were different items — `D`
/// deleted a follow-up while the user was looking at a steer.
#[test]
fn outbox_display_index_and_mutation_index_address_the_same_item() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("f-up", "s", "a follow-up"));
    app.pending_dispatch
        .push_back(steer_dispatch("st-1", "s", "a steer"));

    // Every addressing helper must agree on what row 1 is.
    let displayed = app.queued_at("s", 1).expect("row 1 exists");
    assert_eq!(
        (displayed.id.as_str(), displayed.state),
        ("st-1", QueuedDispatchState::Steering),
        "the displayed set includes pending steers"
    );

    // Deleting row 1 must delete the STEER, not the follow-up.
    let removed = app.remove_queued_at("s", 1).expect("row 1 removable");
    assert_eq!(
        removed.id, "st-1",
        "delete must address the same item the modal displayed"
    );
    let survivors: Vec<&str> = app.pending_dispatch.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(
        survivors,
        vec!["f-up"],
        "the follow-up must survive a delete aimed at the steer"
    );
}

/// Recall has the same consistency requirement: recalling row 0 (a follow-up)
/// after a steer exists must yield the follow-up, and leave the steer staged.
#[test]
fn recall_index_matches_the_displayed_row() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(steer_dispatch("st-1", "s", "steer first"));
    app.pending_dispatch
        .push_back(queued_dispatch("f-up", "s", "follow-up second"));

    let recalled = app.recall_queued_at("s", 0).expect("row 0");
    let dispatch = recalled;
    assert_eq!(
        dispatch.id, "st-1",
        "recall row 0 must yield the item the modal shows at row 0"
    );
    assert_eq!(app.pending_count("s"), 1);
    assert_eq!(
        app.pending_dispatch.front().unwrap().id,
        "f-up",
        "the untouched follow-up remains"
    );
}

/// Every staged item — steer or follow-up — is counted by the strip and named
/// by the preview. A pending steer must never be invisible while the user is
/// waiting for it to land.
#[test]
fn pending_steer_is_counted_and_previewed_by_the_strip() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(steer_dispatch("st-1", "s", "use rg not find"));
    app.pending_dispatch
        .push_back(steer_dispatch("st-2", "s", "and be terse"));

    assert_eq!(app.pending_count("s"), 2, "steers are outbox items");
}

/// `[INV-STEER-05]` — one outbox population, everywhere.
///
/// The strip's count, the modal's list, the `N queued` badge and the set `D`
/// addresses must be the SAME rows. Before unification: the modal listed every
/// row of the session while `remove_queued_at`/`recall_queued_at`/`move_queued`
/// filtered by `Waiting`, and `pending_count`/`queue_count` counted everything —
/// three different populations under one name.
#[test]
fn every_outbox_surface_reports_the_same_population() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("f-up", "s", "follow-up"));
    app.pending_dispatch
        .push_back(steer_dispatch("st-1", "s", "steer"));
    // A row whose round-start request is in flight: not addressable yet.
    let mut dispatching = queued_dispatch("in-flight", "s", "dispatching");
    dispatching.state = QueuedDispatchState::Dispatching;
    app.pending_dispatch.push_back(dispatching);

    let visible = app.outbox_projection("s").len();
    assert_eq!(visible, 2, "the in-flight row is not an outbox row");
    assert_eq!(
        app.pending_count("s"),
        visible,
        "the strip's count must equal the modal's list"
    );
    assert_eq!(
        app.outbox_total(),
        visible,
        "the global badge count must equal the per-session list (one session here)"
    );
    // …and the addressable set matches: both visible rows can be addressed,
    // the dispatching one must not be.
    assert_eq!(app.queued_at("s", 0).map(|d| d.id.as_str()), Some("f-up"));
    assert_eq!(app.queued_at("s", 1).map(|d| d.id.as_str()), Some("st-1"));
    assert!(app.queued_at("s", 2).is_none(), "no third addressable row");
}

/// `[INV-STEER-07]` — the deletion-intent flag cannot outlive its item.
///
/// `D` records "this removal is a deletion, do not restore the text". If the
/// steer is admitted instead of cancelled (the race the protocol allows), the
/// item leaves without a `SteerCancelled`, and a stale flag would swallow a
/// later recall for the same id. The terminal mutation clears it.
#[test]
fn deletion_intent_is_cleared_when_the_item_leaves() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(steer_dispatch("st-1", "s", "steer"));
    app.suppress_steer_restore
        .insert(("s".to_string(), "st-1".to_string()));

    // The item leaves through the terminal mutation (admitted, not cancelled).
    assert!(crate::shell::event_loop::apply::apply(
        &mut app,
        &crate::shell::event_loop::UiRuntime::minimal_for_test(),
        crate::shell::event_loop::AppMutation::DispatchRemoved {
            session_id: "s".to_string(),
            input_id: "st-1".to_string(),
        },
    ));

    assert!(
        app.suppress_steer_restore.is_empty(),
        "a flag for a departed item is a leak: it would suppress a later \
         recall for a reused id"
    );
}

/// `[INV-STEER-08]` — a `QueueUpdated` snapshot replaces only what the server
/// owns, so a pending steer survives it.
///
/// The driver emits a snapshot after *every* queue change, including the
/// `QueuePaused(true)` echo of merely opening the queue modal. The old merge
/// preserved `Dispatching` rows only, so every snapshot deleted every pending
/// steer: the client lost the row that *is* the message payload, and the later
/// `SteerUnavailable` had nothing to restore. The user's prompt disappeared
/// with no transcript row, no composer draft, and no receipt.
#[test]
fn queue_snapshot_preserves_client_owned_rows() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    app.pending_dispatch
        .push_back(steer_dispatch("st-1", "s", "use rg not find"));
    let mut dispatching = queued_dispatch("in-flight", "s", "follow up");
    dispatching.state = QueuedDispatchState::Dispatching;
    app.pending_dispatch.push_back(dispatching);
    // A `Waiting` row IS the server's — the snapshot is authoritative for it.
    app.pending_dispatch
        .push_back(queued_dispatch("srv-old", "s", "stale projection"));

    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::QueueSnapshot {
            session_id: "s".to_string(),
            items: vec![nuo_protocol::QueuedMessage {
                id: "srv-new".to_string(),
                text: "authoritative".to_string(),
                display_text: None,
                images: Vec::new(),
                sent_at_ms: Some(7),
            }],
            paused: false,
        },
    );

    let ids: Vec<(&str, QueuedDispatchState)> = app
        .pending_dispatch
        .iter()
        .map(|d| (d.id.as_str(), d.state))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("srv-new", QueuedDispatchState::Waiting),
            ("st-1", QueuedDispatchState::Steering),
            ("in-flight", QueuedDispatchState::Dispatching),
        ],
        "the server's list replaces its own rows; client-owned rows survive \
         (a snapshot must not be able to delete a steer the server never saw, \
         and the removed `srv-old` must not linger)"
    );
}

/// The steer payload survives the whole snapshot → `SteerUnavailable` sequence.
///
/// This is the end-to-end shape of the loss above: the row must still be there
/// when the round reports that the steer missed, so the text reaches the
/// composer instead of vanishing.
#[test]
fn steer_text_survives_a_snapshot_then_a_missed_round() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.live_session_id = "s".to_string();
    app.current_session_id = "s".to_string();

    let mut steer = steer_dispatch("st-1", "s", "use rg not find");
    steer.images = vec![nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "base64".to_string(),
    }];
    app.pending_dispatch.push_back(steer);

    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::QueueSnapshot {
            session_id: "s".to_string(),
            items: Vec::new(),
            paused: true,
        },
    );
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::SteerMissed {
            session_id: "s".to_string(),
            input_id: "st-1".to_string(),
        },
    );

    assert_eq!(
        app.input, "use rg not find",
        "the steer text must come back to the composer"
    );
    assert_eq!(
        app.pending_images.len(),
        1,
        "and so must its attachments: the row is the payload"
    );
    assert!(
        app.pending_dispatch.is_empty(),
        "the row is consumed once its content is restored"
    );
}

/// A steer that ends after its row is gone is *disclosed*, never dropped.
///
/// The row is the only copy of the text, so an ending with no row means the
/// message is unrecoverable. Silence would be a prompt vanishing with no
/// transcript row, no draft, and no receipt — the worst possible outcome.
#[test]
fn an_unrecoverable_steer_is_disclosed_in_the_notice_log() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.live_session_id = "s".to_string();
    app.current_session_id = "s".to_string();
    let before = app.notice_log().len();

    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::SteerMissed {
            session_id: "s".to_string(),
            input_id: "never-staged".to_string(),
        },
    );

    assert!(
        app.notice_log().len() > before,
        "an unrecoverable steer must leave a retained disclosure \
         ([INV-NOTICE-05]): a prompt must never disappear silently"
    );
    assert!(app.input.is_empty(), "there was nothing to restore");
}

/// The deletion intent survives a lost race while its item is still staged, and
/// dies with the item.
///
/// Clearing it on `SteerCancelFailed` made "the cancel lost" indistinguishable
/// from "no cancel was requested": a later `SteerCancelled` for the same id would
/// find no flag and restore text the user had deliberately deleted.
#[test]
fn a_lost_cancel_keeps_the_deletion_intent_until_the_item_leaves() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.live_session_id = "s".to_string();
    app.current_session_id = "s".to_string();

    // Case 1: the row is still staged, so the admission that follows will end
    // the id — the flag is still meaningful.
    app.pending_dispatch
        .push_back(steer_dispatch("st-1", "s", "delete me"));
    app.suppress_steer_restore
        .insert(("s".to_string(), "st-1".to_string()));
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::SteerCancelFailedCleared {
            session_id: "s".to_string(),
            input_id: "st-1".to_string(),
        },
    );
    assert!(
        app.suppress_steer_restore
            .contains(&("s".to_string(), "st-1".to_string())),
        "a lost race must not forget that the user asked to delete this steer"
    );

    // The admission ends the item, and the flag goes with it.
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::DispatchRemoved {
            session_id: "s".to_string(),
            input_id: "st-1".to_string(),
        },
    );
    assert!(
        app.suppress_steer_restore.is_empty(),
        "the flag cannot outlive its item"
    );

    // Case 2: the row is already gone (deleted before the race resolved), so
    // nothing will end this id again — the flag must be released here.
    app.suppress_steer_restore
        .insert(("s".to_string(), "st-2".to_string()));
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::SteerCancelFailedCleared {
            session_id: "s".to_string(),
            input_id: "st-2".to_string(),
        },
    );
    assert!(
        app.suppress_steer_restore.is_empty(),
        "an id that can never end again must not leave a flag behind"
    );
}

/// A steer the user deleted must not resurrect its text when its round ends.
///
/// `SteerMissed` removed the deletion-intent flag and then restored
/// unconditionally, so every deletion of a missed steer undid itself: the text
/// came back into the composer with a "restored" notice, and the user's explicit
/// `D` or `/queue clear` was silently reversed.
#[test]
fn a_deleted_steer_is_not_restored_when_its_round_ends() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.live_session_id = "s".to_string();
    app.current_session_id = "s".to_string();

    app.pending_dispatch
        .push_back(steer_dispatch("st-1", "s", "delete me"));
    app.suppress_steer_restore
        .insert(("s".to_string(), "st-1".to_string()));

    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::SteerMissed {
            session_id: "s".to_string(),
            input_id: "st-1".to_string(),
        },
    );

    assert!(
        app.input.is_empty(),
        "a deleted steer must not reappear in the composer (got {:?})",
        app.input
    );
    assert!(
        app.pending_dispatch.is_empty(),
        "and its row is gone either way"
    );
    assert!(
        app.suppress_steer_restore.is_empty(),
        "the flag still cannot outlive the item"
    );
}
