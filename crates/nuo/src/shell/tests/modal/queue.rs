use super::*;

#[test]
fn toggle_queue_block_flips_state_and_blocks_dispatch() {
    // `F3` / queue-modal block is the hard "send nothing" override. While a
    // session is blocked, `begin_next_round_dispatch` must yield nothing —
    // even though the item is `Waiting`. The event loop relies on
    // `is_queue_blocked` (and the app-side gate is its mirror) so a blocked
    // outbox can't slip through.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "first"));
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("2", "session-a", "second"));

    // Not blocked initially.
    assert!(!app.is_queue_blocked("session-a"));
    assert_eq!(app.pending_count("session-a"), 2);

    // Toggle on (the local flag is the optimistic projection of the
    // server's `QueuePaused` verb — M4).
    app.set_queue_blocked("session-a", true);
    assert!(app.is_queue_blocked("session-a"));

    // The block is persistent and session-scoped: another session is
    // unaffected.
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("3", "session-b", "other"));
    assert!(!app.is_queue_blocked("session-b"));

    // Toggle off.
    app.set_queue_blocked("session-a", false);
    assert!(!app.is_queue_blocked("session-a"));
}

#[test]
fn block_and_resume_helpers_are_idempotent() {
    // `block_queue` forces the block on; `resume_queue` forces it off. Both
    // must be safe to call repeatedly. The queue modal's open/close path
    // relies on this: open always blocks, close always resumes.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "x"));

    app.block_queue("session-a");
    assert!(app.is_queue_blocked("session-a"));
    app.block_queue("session-a"); // idempotent
    assert!(app.is_queue_blocked("session-a"));

    app.resume_queue("session-a");
    assert!(!app.is_queue_blocked("session-a"));
    app.resume_queue("session-a"); // idempotent
    assert!(!app.is_queue_blocked("session-a"));
}

#[test]
fn remove_queued_at_deletes_by_index_and_clamps() {
    // `D` in the queue modal deletes the highlighted item. The event loop
    // clamps `modal_index` after a delete; here we verify the core removal is
    // index-keyed and session-scoped.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "one"));
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("b", "session-b", "two"));
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "three"));

    // session-a has two items: idx 0 = "a", idx 1 = "c". Delete idx 1.
    let removed = app
        .remove_queued_at("session-a", 1)
        .expect("should remove idx 1");
    assert_eq!(removed.id, "c");
    assert_eq!(app.pending_count("session-a"), 1);
    assert_eq!(app.pending_count("session-b"), 1, "other session untouched");

    // Out of range → None, nothing removed.
    assert!(app.remove_queued_at("session-a", 5).is_none());
    assert_eq!(app.pending_count("session-a"), 1);
}

#[test]
fn move_queued_swaps_within_session_and_clamps_at_edges() {
    // `J`/`K` in the queue modal reorder the highlighted item. Moving toward
    // the front (delta -1) makes it the next to pop; toward the tail (delta 1)
    // pushes it back. Reorder is clamped to the session slice so it can't
    // escape into another session's items.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "one"));
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("x", "session-b", "intruder"));
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("b", "session-a", "two"));
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "three"));

    // session-a display order: [a, b, c]. Move idx 0 (a) toward the tail by 2:
    // clamped to the last position → order becomes [b, c, a].
    app.move_queued("session-a", 0, 2);
    let order: Vec<&str> = app
        .composer
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["b", "c", "a"]);

    // The intruder session-b item is untouched: reorder never crossed session
    // boundaries (session-b still has exactly one item, the same one).
    let session_b: Vec<&str> = app
        .composer
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-b")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(session_b, vec!["x"]);

    // Move the now-front item (b) toward the front by 5: clamped to 0 →
    // stays put. Order unchanged.
    app.move_queued("session-a", 0, -5);
    let order: Vec<&str> = app
        .composer
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["b", "c", "a"]);

    // Move idx 1 (c) toward the front by 1: swaps with b → [c, b, a].
    app.move_queued("session-a", 1, -1);
    let order: Vec<&str> = app
        .composer
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["c", "b", "a"]);
}

#[test]
fn steer_ending_paths_consume_only_the_steer_row() {
    // ADR-0137 replaced the old "inserts are transcript-owned" contract. A
    // busy-Enter steer waits in the outbox as a `Steering` row, and the two
    // ways it can end (`SteerAdmitted` / `SteerCancelled`) remove exactly that
    // row. What must hold is that those paths never consume a `Waiting`
    // follow-up, because a `Waiting` row is a projection of the driver's queue
    // and still has a life of its own.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("w1", "session-a", "waiting one"));

    let mut staged = queued_dispatch("steer-1", "session-a", "steer this");
    staged.state = QueuedDispatchState::Steering;
    app.composer.pending_dispatch.push_back(staged);

    // An id that names no steer row is a no-op: the take is state-scoped, so a
    // `Waiting` row can never be mistaken for the steer it shares no id with.
    assert!(app.take_steer_row("session-a", "w1").is_none());
    assert_eq!(
        app.pending_count("session-a"),
        2,
        "a Waiting row must survive a steer-ending path"
    );

    // The steer row yields its full payload (text + attachments), which is what
    // the restore arms put back into the composer.
    let taken = app
        .take_steer_row("session-a", "steer-1")
        .expect("the steer row is consumed");
    assert_eq!(taken.text, "steer this");
    assert_eq!(taken.state, QueuedDispatchState::Steering);
    let ids: Vec<&str> = app
        .composer
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["w1"],
        "only the steer row left; the follow-up is untouched"
    );
    assert_eq!(app.pending_count("session-a"), 1);
}

#[test]
fn a_dispatching_row_is_not_addressable_outbox_work() {
    // The in-flight `Dispatching` row is not counted as outbox work: the strip,
    // the modal and the addressable set exclude it (one population,
    // `[INV-STEER-05]`). The row still has to survive a `QueueSnapshot` (its
    // ack has not landed yet), so the snapshot's preservation and the strip's
    // exclusion are two different rules that must both hold.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let mut dispatching = queued_dispatch("in-flight", "session-a", "dispatching");
    dispatching.state = QueuedDispatchState::Dispatching;
    app.composer.pending_dispatch.push_back(dispatching);

    assert_eq!(
        app.pending_count("session-a"),
        0,
        "a Dispatching row is not addressable outbox work"
    );
    assert!(
        app.queued_at("session-a", 0).is_none(),
        "and it is not a row the modal can address"
    );
}

/// The queue modal's verbs must reach the **server**, not just the projection.
///
/// A `Waiting` outbox row is the client's view of the driver's
/// `FollowUpQueue`. `D` (delete) and `Enter` (recall-to-composer) used to mutate
/// only the local list, so the message stayed queued server-side: the driver
/// shipped it at the next round boundary while the next `QueueUpdated` snapshot
/// put the row back. Deleting was therefore both ineffective (the prompt still
/// ran) and unstable (the row returned).
#[tokio::test]
async fn queue_modal_delete_and_recall_are_mirrored_to_the_server() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.link.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "s1";
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("q1", sid, "first"));
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("q2", sid, "second"));

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenQueue,
        sid,
    )
    .await;
    // Opening auto-pauses the server's queue so nothing ships mid-edit.
    assert!(matches!(
        rx.try_recv(),
        Ok(AgentRequest::QueuePaused { paused: true, .. })
    ));

    // `D` on row 0 deletes q1 remotely as well as locally.
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::QueueDelete,
        sid,
    )
    .await;
    match rx.try_recv() {
        Ok(AgentRequest::QueueRemove {
            thread_id,
            input_id,
        }) => {
            assert_eq!(thread_id, sid);
            assert_eq!(input_id, "q1", "the deleted row is the one addressed");
        }
        other => panic!("delete must send QueueRemove, got {other:?}"),
    }
    assert_eq!(app.pending_count(sid), 1);

    // `Enter` recalls the remaining row: remove remotely, restore locally.
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::RecallQueuedSelected,
        sid,
    )
    .await;
    match rx.try_recv() {
        Ok(AgentRequest::QueueRemove { input_id, .. }) => assert_eq!(input_id, "q2"),
        other => panic!("recall must send QueueRemove, got {other:?}"),
    }
    assert_eq!(
        app.composer.input, "second",
        "the prompt returns to the composer"
    );
    assert_eq!(app.pending_count(sid), 0);

    // Closing the modal releases the pause it set — the symmetric half.
    // (`RecallQueuedSelected` already dismissed the modal, so the release is
    // the next thing on the wire.)
    assert!(
        matches!(
            rx.try_recv(),
            Ok(AgentRequest::QueuePaused { paused: false, .. })
        ),
        "every close path must resume the server's queue"
    );
}

/// `Ctrl+P` inside the modal and `/queue block` are the same verb.
#[tokio::test]
async fn queue_block_toggles_are_mirrored_to_the_server() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.link.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "s1";
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("q1", sid, "first"));

    // The slash command path (`/queue block`) must not be a local-only lie.
    crate::shell::event_loop::actions::dispatch_command_for_test(
        &mut app,
        &runtime,
        crate::shell::keymap::CommandId::ToggleQueueBlock,
        sid,
    )
    .await;
    assert!(app.is_queue_blocked(sid));
    assert!(
        matches!(
            rx.try_recv(),
            Ok(AgentRequest::QueuePaused { paused: true, .. })
        ),
        "/queue block must pause the driver, not just the projection"
    );

    crate::shell::event_loop::actions::dispatch_command_for_test(
        &mut app,
        &runtime,
        crate::shell::keymap::CommandId::ToggleQueueBlock,
        sid,
    )
    .await;
    assert!(!app.is_queue_blocked(sid));
    assert!(matches!(
        rx.try_recv(),
        Ok(AgentRequest::QueuePaused { paused: false, .. })
    ));
}

/// `/queue clear` must also cancel a live steer.
///
/// A `Steering` row is not in the driver's follow-up queue, so `QueueClear`
/// cannot reach it: clearing only the list would let the steer land *after* the
/// user emptied the queue — the one outcome "clear" promises not to have.
#[tokio::test]
async fn clear_queue_command_cancels_a_live_steer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.link.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("a", "s1", "mine"));
    let mut steer = queued_dispatch("st1", "s1", "steer this");
    steer.state = QueuedDispatchState::Steering;
    app.composer.pending_dispatch.push_back(steer);

    crate::shell::event_loop::actions::dispatch_command_for_test(
        &mut app,
        &runtime,
        crate::shell::keymap::CommandId::ClearQueue,
        "s1",
    )
    .await;

    let mut sent = Vec::new();
    while let Ok(req) = rx.try_recv() {
        sent.push(req);
    }
    assert!(
        sent.iter().any(|req| matches!(
            req,
            AgentRequest::QueueClear { thread_id } if thread_id == "s1"
        )),
        "the follow-up queue is cleared: {sent:?}"
    );
    assert!(
        sent.iter().any(|req| matches!(
            req,
            AgentRequest::CancelSteer { input_id, .. } if input_id == "st1"
        )),
        "and the live steer is cancelled, or it lands after the clear: {sent:?}"
    );
    assert_eq!(app.pending_count("s1"), 0);
    assert!(
        app.composer
            .suppress_steer_restore
            .contains(&("s1".to_string(), "st1".to_string())),
        "a clear consumes the draft; it must not hand the text back"
    );
}

/// `/queue clear` is session-scoped and mirrored.
#[tokio::test]
async fn clear_queue_command_is_session_scoped_and_mirrored() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.link.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("a", "s1", "mine"));
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("b", "s2", "someone else's"));

    crate::shell::event_loop::actions::dispatch_command_for_test(
        &mut app,
        &runtime,
        crate::shell::keymap::CommandId::ClearQueue,
        "s1",
    )
    .await;

    match rx.try_recv() {
        Ok(AgentRequest::QueueClear { thread_id }) => assert_eq!(thread_id, "s1"),
        other => panic!("clear must send QueueClear, got {other:?}"),
    }
    assert_eq!(app.pending_count("s1"), 0);
    assert_eq!(
        app.pending_count("s2"),
        1,
        "clearing one session's queue must not touch another's"
    );
}

/// `[INV-OUTBOX-04]` — the close hook must not stomp a pause the user set, and
/// must not *release* one they asked for.
///
/// `Ctrl+P` pauses from inside the modal, but the pending-exit marker stays
/// armed for the close, which then resumes unconditionally: the user's pause is
/// silently undone the moment they close the panel. The exit hook owns "release
/// what this open set", not "force resume".
#[tokio::test]
async fn closing_the_queue_panel_does_not_undo_an_explicit_pause() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.link.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "s1";
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("q1", sid, "first"));

    // Open (auto-pauses), then pause *deliberately* is a no-op — it is already
    // paused — so the interesting sequence is: open, resume, pause.
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenQueue,
        sid,
    )
    .await;
    let _ = rx.try_recv();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::QueueToggleBlock,
        sid,
    )
    .await;
    assert!(!app.is_queue_blocked(sid), "Ctrl+P resumed it");
    let _ = rx.try_recv();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::QueueToggleBlock,
        sid,
    )
    .await;
    assert!(app.is_queue_blocked(sid), "Ctrl+P paused it again");
    let _ = rx.try_recv();

    // Now close. The user's pause must SURVIVE the close.
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseModal,
        sid,
    )
    .await;
    assert!(
        app.is_queue_blocked(sid),
        "closing the panel must not resume a queue the user explicitly paused"
    );
    assert!(
        !matches!(
            rx.try_recv(),
            Ok(AgentRequest::QueuePaused { paused: false, .. })
        ),
        "and it must not tell the server to resume either"
    );
}

/// A steer that is admitted while the queue modal is open leaves the list, so
/// the close hook must not reuse the captured index against the new list.
#[tokio::test]
async fn closing_the_queue_panel_after_a_steer_leaves_the_list() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.link.tx = tx;
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let sid = "s1";
    app.composer
        .pending_dispatch
        .push_back(queued_dispatch("q1", sid, "first"));
    let mut steer = queued_dispatch("st1", sid, "steer this");
    steer.state = QueuedDispatchState::Steering;
    app.composer.pending_dispatch.push_back(steer);

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::OpenQueue,
        sid,
    )
    .await;
    let _ = rx.try_recv();

    // The steer is admitted: its row leaves and the list shrinks to one.
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::DispatchRemoved {
            session_id: sid.to_string(),
            input_id: "st1".to_string(),
        },
    );
    assert_eq!(app.pending_count(sid), 1);

    // Close: the cursor must land inside the surviving list, not past its end.
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseModal,
        sid,
    )
    .await;
    let idx = app
        .surfaces
        .surfaces
        .dlg::<crate::shell::surfaces::QueueDialog>()
        .index;
    assert!(
        idx < app.pending_count(sid).max(1),
        "the retained cursor must be clamped to the list that still exists (got {idx})"
    );
}
