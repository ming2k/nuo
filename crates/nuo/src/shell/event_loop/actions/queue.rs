//! Queue operations and outbox state mutations.
use crate::shell::App;
use crate::shell::input::InputAction;
use crate::shell::surfaces::DialogKind;
use nuo_protocol::AgentRequest;

pub(super) fn handle_queue_action(app: &mut App, viewed_session_id: &str, action: InputAction) {
    match action {
        InputAction::ToggleSendMode => {
            app.composer_send_mode = match app.composer_send_mode {
                crate::shell::app::ComposerSendMode::Steer => {
                    crate::shell::app::ComposerSendMode::FollowUp
                }
                crate::shell::app::ComposerSendMode::FollowUp => {
                    crate::shell::app::ComposerSendMode::Steer
                }
            };
        }
        InputAction::RecallQueuedSelected => {
            // The queue modal's `Enter` recalls the *selected* item
            // (the `↑/↓` highlight, not always the newest) into the
            // composer and closes the modal. Closing resumes the
            // auto-block the modal set on open.
            //
            // The verb depends on who owns the row:
            //
            // - `Steering` is client-owned: the server holds it in the running
            //   round, so `CancelSteer` is the linearizing verb (ADR-0137). A
            //   recall is the one cancel that must NOT suppress the restore.
            // - everything else is a projection of the driver's
            //   `FollowUpQueue`, so the recall must also `QueueRemove` it —
            //   otherwise the item stays queued server-side and ships at the
            //   next round boundary while the user is editing it in the
            //   composer, sending the same prompt twice.
            // Ordering matters: the removal must be queued **before** the
            // recall's synthetic close resumes the server's pause below, or the
            // driver is free to ship the very item being recalled in the window
            // between "resumed" and "removed".
            let idx = app.active_index();
            let item = app.queued_at(viewed_session_id, idx).cloned();
            match item {
                Some(item) if item.state == crate::shell::app::QueuedDispatchState::Steering => {
                    app.send_intent(AgentRequest::CancelSteer {
                        thread_id: viewed_session_id.to_string(),
                        input_id: item.id.clone(),
                    });
                    app.recall_queued_at(viewed_session_id, idx);
                    // SteerCancelled will restore the text to the composer; keep
                    // the local action immediate by pushing it into the draft now
                    // (the async restore is a no-op once the composer is busy).
                    app.adopt_as_draft(
                        item.text,
                        item.images,
                        item.text_pastes,
                        crate::shell::app::DraftAdoption::StashCurrent,
                    );
                }
                Some(item) => {
                    // Take it off the server queue first, then out of the local
                    // projection (the `QueueUpdated` echo is a no-op once the
                    // row is gone).
                    app.send_intent(AgentRequest::QueueRemove {
                        thread_id: viewed_session_id.to_string(),
                        input_id: item.id.clone(),
                    });
                    if let Some(dispatch) = app.recall_queued_at(viewed_session_id, idx) {
                        app.restore_dispatch(dispatch);
                    }
                }
                None => {}
            }
            app.dismiss_active_dialog();
        }
        InputAction::QueueToggleBlock => {
            // `Ctrl+P` inside the queue modal: toggle the pause on the viewed
            // session's outbox. M4: the pause is the *server's* queue flag — the
            // local toggle is the optimistic projection and the verb is
            // authoritative.
            //
            // Either direction hands ownership to the user: after this press the
            // pause in effect is theirs, so the modal's close must not release
            // it (paused) and must not re-pause it (resumed) — the exit hook
            // reads this and stays out of the way (`[INV-OUTBOX-04]`).
            let paused = !app.is_queue_blocked(viewed_session_id);
            app.set_queue_blocked(viewed_session_id, paused);
            if let Some((sid, origin)) = app.queue_exit_session.as_mut()
                && sid == viewed_session_id
            {
                *origin = crate::shell::app::QueuePauseOrigin::UserOwned;
            }
            app.send_intent(AgentRequest::QueuePaused {
                thread_id: viewed_session_id.to_string(),
                paused,
            });
        }
        InputAction::QueueDelete => {
            // `D` in the queue modal: remove the highlighted
            // item outright. The queue is auto-blocked on open, so the
            // index can't drift under us. Clamp the selection to the
            // now-shorter list.
            //
            // A pending steer is removed by cancelling it (`CancelSteer`),
            // and the restore is suppressed: a deletion must not resurrect
            // the text in the composer (ADR-0137). Every other row is a
            // projection of the driver's `FollowUpQueue`, so a local-only
            // removal would leave the message queued server-side — it would
            // ship at the next round boundary and the next authoritative
            // snapshot would put the row straight back. `QueueRemove` is the
            // verb that makes the deletion real.
            if app.active_dialog() == Some(DialogKind::Queue) {
                let idx = app.active_index();
                let target = app.queued_at(viewed_session_id, idx).cloned();
                match target {
                    Some(item)
                        if item.state == crate::shell::app::QueuedDispatchState::Steering =>
                    {
                        app.send_intent(AgentRequest::CancelSteer {
                            thread_id: viewed_session_id.to_string(),
                            input_id: item.id.clone(),
                        });
                        app.suppress_steer_restore
                            .insert((viewed_session_id.to_string(), item.id));
                    }
                    Some(item) => {
                        app.send_intent(AgentRequest::QueueRemove {
                            thread_id: viewed_session_id.to_string(),
                            input_id: item.id,
                        });
                    }
                    None => {}
                }
                let _removed = app.remove_queued_at(viewed_session_id, idx);
                let count = app.pending_count(viewed_session_id);
                if count == 0 {
                    app.set_active_index(0);
                } else if app.active_index() >= count {
                    app.set_active_index(count - 1);
                }
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::QueueDialog>()
                    .follow = true;
            }
        }
        InputAction::QueueMoveItem { delta } => {
            // `K`/`J` in the queue modal: reorder the highlighted item
            // toward the front (next to pop) or the tail. Clamp at the
            // session slice boundaries so it can't escape into another
            // session's items.
            //
            // Only `Waiting` rows are in the driver's `FollowUpQueue`, so only
            // they can be reordered: a `Steering` row lives in the running
            // round's admission window and has no position to change, and a
            // `Dispatching` row is excluded from the addressable set entirely.
            // Sending `QueueReorder` for a steer made the local list move while
            // the server no-oped on an unknown id — a reorder that silently did
            // not happen (ADR-0137 §5).
            if app.active_dialog() == Some(DialogKind::Queue) {
                let idx = app.active_index();
                let movable = app
                    .queued_at(viewed_session_id, idx)
                    .filter(|it| it.state == crate::shell::app::QueuedDispatchState::Waiting)
                    .map(|it| it.id.clone());
                if let Some(input_id) = movable {
                    app.send_intent(AgentRequest::QueueReorder {
                        thread_id: viewed_session_id.to_string(),
                        input_id,
                        delta,
                    });
                    app.move_queued(viewed_session_id, idx, delta);
                    // Follow the moved item if it changed position.
                    let count = app.pending_count(viewed_session_id);
                    if count > 0 {
                        app.set_active_index(
                            (idx as i32 + delta).clamp(0, count as i32 - 1) as usize
                        );
                        app.surfaces
                            .dlg_mut::<crate::shell::surfaces::QueueDialog>()
                            .follow = true;
                    }
                }
            }
        }
        _ => {}
    }
}
