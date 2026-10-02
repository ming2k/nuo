//! Chat-round handlers, extracted verbatim from
//! the agent background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`side`, `agent`, `history`, `session`,
//! `lifecycle`, `resp_tx`, `pursuit_service`, `config`, …) so the body reads
//! exactly as it did inline.

use nuo_harness::orchestration::{RoundInput, round_response};
use nuo_harness::{Agent, RoundLifecycle};
use nuo_contracts::{AgentResponse, QueuedMessage, RoundEvent};
use nuo_persistence::session::SessionStore;
use std::sync::Arc;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::side::SideEnv;
use crate::side::{
    SideRegistry, refuse_if_no_provider, start_active_turn, start_session_turn, target_agent,
};

/// `AgentRequest::Chat` — start an interactive round against whichever session
/// the user is currently composing into (primary or `/btw` side).
pub(crate) async fn chat(
    env: SideEnv<'_>,
    text: String,
    images: Vec<nuo_contracts::ImagePart>,
    sent_at_ms: Option<u64>,
) {
    let SideEnv {
        side,
        agent,
        primary_session: session,
        primary_lifecycle: lifecycle,
        tx: resp_tx,
        config,
    } = env;
    // Refuse up-front when no real provider is configured: the shared holder
    // is parked on the `NoProvider` sentinel (catalog could not resolve a
    // channel at startup or the last `/models` switch). Failing here keeps
    // the user's text out of the transcript and surfaces a single notice
    // instead of letting the request reach a non-functional provider. Use the
    // same refusal contract as every other round-entry path (`RoundEvent::Error`
    // + idle `HarnessState`): the TUI has already optimistically painted
    // "queued" for this send, and a bare top-level `Error` would leave that
    // state stuck on the activity bar forever.
    if refuse_if_no_provider(resp_tx, agent, session, &session.id().await).await {
        return;
    }
    start_active_turn(
        SideEnv {
            side,
            agent,
            primary_session: session,
            primary_lifecycle: lifecycle,
            tx: resp_tx,
            config,
        },
        RoundInput {
            prompt: text,
            hidden: false,
            display_prompt: None,
            sent_at_ms,
            images,
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await;
}

/// Queue a steering input into the exact live round named by `session_id`. Failure is
/// returned as a scoped event so the frontend can retain the text as a paused
/// follow-up item instead of dropping it.
pub async fn steer(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
    input: QueuedMessage,
) {
    let accepted = match target_agent(side, agent, session, &session_id).await {
        Some(target) => target.steer(&session_id, input.clone()),
        None => false,
    };
    if !accepted {
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::SteerUnavailable { input_id: input.id },
        ));
    }
}

/// Cancel a steering message if it has not crossed the agent boundary yet. The agent's
/// queue mutex linearizes this against admission, so the response is final.
pub async fn cancel_steer(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
    input_id: String,
) {
    let cancelled = match target_agent(side, agent, session, &session_id).await {
        Some(target) => target.cancel_steer(&session_id, &input_id).is_some(),
        None => false,
    };
    let event = if cancelled {
        RoundEvent::SteerCancelled { input_id }
    } else {
        RoundEvent::SteerCancelFailed { input_id }
    };
    let _ = resp_tx.send(round_response(&session_id, event));
}

/// Dispatch a paused outbox item into a fresh round without consulting the
/// frontend's current view. If its side session vanished, hand ownership back
/// to the outbox through `SteerUnavailable`.
/// `AgentRequest::FollowUp` — the driver-owned follow-up queue authority
/// (ADR-0197 M4). The daemon decides: an idle, un-paused target starts
/// immediately ([`RoundEvent::FollowUpStarted`]); a running (or paused)
/// target enqueues the message ([`RoundEvent::FollowUpQueued`]) and the
/// driver ships it at the round boundary. The frontend never decides when a
/// follow-up ships.
pub(crate) async fn follow_up(
    env: SideEnv<'_>,
    queue: &mut crate::session_driver::FollowUpQueue,
    wake: &mpsc::Sender<String>,
    session_id: String,
    input: QueuedMessage,
) {
    let SideEnv {
        side,
        agent,
        primary_session: session,
        primary_lifecycle: lifecycle,
        tx: resp_tx,
        config: _,
    } = env;
    let Some(target) =
        crate::side::resolve_turn_target(side, agent, session, lifecycle, &session_id).await
    else {
        // The aside was closed after the message entered the frontend
        // outbox — the item cannot run anywhere.
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::SteerUnavailable { input_id: input.id },
        ));
        return;
    };
    let running = target.lifecycle.is_running().await;
    if running || queue.is_paused(&session_id) {
        let input_id = input.id.clone();
        queue.enqueue(&session_id, input);
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::FollowUpQueued { input_id },
        ));
        emit_queue_snapshot(resp_tx, queue, &session_id);
        if running {
            // Watch this target's round boundary so the queue ships the
            // moment the round ends (unless the round was interrupted —
            // the boundary handler parks the queue then).
            spawn_boundary_watcher(target.lifecycle.clone(), wake.clone(), session_id.clone());
        }
        return;
    }
    start_queued_follow_up(env, session_id, input, queue).await;
}

/// Start one dequeued follow-up (the shared ship path for direct dispatch
/// and round-boundary shipping).
pub(crate) async fn start_queued_follow_up(
    env: SideEnv<'_>,
    session_id: String,
    input: QueuedMessage,
    queue: &mut crate::session_driver::FollowUpQueue,
) {
    let SideEnv {
        side,
        agent,
        primary_session: session,
        primary_lifecycle: lifecycle,
        tx: resp_tx,
        config,
    } = env;
    let started = start_session_turn(
        &session_id,
        SideEnv {
            side,
            agent,
            primary_session: session,
            primary_lifecycle: lifecycle,
            tx: resp_tx,
            config,
        },
        RoundInput {
            prompt: input.text.clone(),
            hidden: false,
            display_prompt: input.display_text.clone(),
            sent_at_ms: input.sent_at_ms,
            images: input.images.clone(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await;
    if !started {
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::SteerUnavailable { input_id: input.id },
        ));
    } else {
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::FollowUpStarted(input),
        ));
    }
    emit_queue_snapshot(resp_tx, queue, &session_id);
}

/// The authoritative queue snapshot for one session, after every queue
/// change (ADR-0197 M4).
pub(crate) fn emit_queue_snapshot(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    queue: &crate::session_driver::FollowUpQueue,
    session_id: &str,
) {
    let (items, paused) = queue.snapshot(session_id);
    let _ = resp_tx.send(round_response(
        session_id,
        RoundEvent::QueueUpdated { items, paused },
    ));
}

/// Spawn a one-shot boundary watcher: wake the driver's follow-up queue the
/// moment this target's round ends (ADR-0197 M4).
pub(crate) fn spawn_boundary_watcher(
    lifecycle: Arc<RoundLifecycle>,
    wake: mpsc::Sender<String>,
    session_id: String,
) {
    tokio::spawn(async move {
        loop {
            if !lifecycle.is_running().await {
                break;
            }
            lifecycle.finished().notified().await;
        }
        let _ = wake.send(session_id).await;
    });
}

/// `AgentRequest::QueueRemove` — the queue modal's delete / destructive
/// recall-to-composer. Idempotent.
pub(crate) async fn queue_remove(
    env: SideEnv<'_>,
    queue: &mut crate::session_driver::FollowUpQueue,
    session_id: String,
    input_id: String,
) {
    let SideEnv { tx: resp_tx, .. } = env;
    queue.remove(&session_id, &input_id);
    emit_queue_snapshot(resp_tx, queue, &session_id);
}

/// `AgentRequest::QueueClear` — the queue modal's clear.
pub(crate) async fn queue_clear(
    env: SideEnv<'_>,
    queue: &mut crate::session_driver::FollowUpQueue,
    session_id: String,
) {
    let SideEnv { tx: resp_tx, .. } = env;
    queue.clear(&session_id);
    emit_queue_snapshot(resp_tx, queue, &session_id);
}

/// `AgentRequest::QueueReorder` — the queue modal's `K`/`J` reorder.
pub(crate) async fn queue_reorder(
    env: SideEnv<'_>,
    queue: &mut crate::session_driver::FollowUpQueue,
    session_id: String,
    input_id: String,
    delta: i32,
) {
    let SideEnv { tx: resp_tx, .. } = env;
    queue.reorder(&session_id, &input_id, delta);
    emit_queue_snapshot(resp_tx, queue, &session_id);
}

/// `AgentRequest::QueuePaused` — the queue modal's block control
/// (`Ctrl+P`). Pausing gates only the round-boundary auto-ship.
pub(crate) async fn queue_paused(
    env: SideEnv<'_>,
    queue: &mut crate::session_driver::FollowUpQueue,
    session_id: String,
    paused: bool,
) {
    let SideEnv { tx: resp_tx, .. } = env;
    queue.set_paused(&session_id, paused);
    emit_queue_snapshot(resp_tx, queue, &session_id);
}
