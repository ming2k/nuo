//! Chat-round handlers for interactive sessions (ADR-0058: sovereign sessions).

use std::sync::Arc;
use tokio::sync::mpsc;

use nuo_harness::orchestration::{
    ContextProjectionSettings, InteractiveRoundContext, RoundInput, round_response,
};
use nuo_harness::{Agent, RoundLifecycle};
use nuo_persistence::config::Config;
use nuo_persistence::session::SessionStore;
use nuo_protocol::{AgentResponse, LoopStatus, QueuedMessage, RoundEvent};

use crate::agent_setup::active_context_window;
use crate::session_driver::send_harness_state_for_session;

/// Bundled round-execution environment for sovereign sessions.
#[derive(Clone, Copy)]
pub(crate) struct ChatEnv<'a> {
    pub agent: &'a Arc<Agent>,
    pub session: &'a Arc<SessionStore>,
    pub lifecycle: &'a Arc<RoundLifecycle>,
    pub tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub config: &'a Config,
}

/// No-provider refusal gate.
pub(crate) async fn refuse_if_no_provider(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    agent: &Agent,
    session: &SessionStore,
    session_id: &str,
) -> bool {
    if !nuo_harness::NoProvider::is(agent.provider.as_ref()) {
        return false;
    }
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::Error(
            "No provider configured. Add one with /connections before sending a message."
                .to_string(),
        ),
    ));
    send_harness_state_for_session(tx, session_id, agent, session, LoopStatus::Idle).await;
    true
}

/// Start an interactive turn on the active sovereign thread.
pub(crate) async fn start_turn(env: ChatEnv<'_>, mut input: RoundInput) {
    let ChatEnv {
        agent,
        session,
        lifecycle,
        tx,
        config,
    } = env;
    let session_id = session.id().await;
    if refuse_if_no_provider(tx, agent, session, &session_id).await {
        return;
    }
    if input.is_retry() {
        let pending = session.retry_pending().await;
        let round_counter = session.round_counter().await;
        let Some(pending) = pending.filter(|point| point.round == round_counter) else {
            let _ = tx.send(round_response(
                &session_id,
                RoundEvent::Error(
                    "Nothing to retry — the last round already completed.".to_string(),
                ),
            ));
            send_harness_state_for_session(tx, &session_id, agent, session, LoopStatus::Idle).await;
            return;
        };
        input = RoundInput::resume(pending);
    }
    let projection = ContextProjectionSettings::from_context_policy(
        &config.context,
        active_context_window(agent),
    );
    let retry_max_attempts = config.connection_retry_max_attempts;
    let retry_base_ms = config.connection_retry_base_ms;
    let retry_max_ms = config.connection_retry_max_ms;

    nuo_harness::orchestration::start_interactive_round(
        InteractiveRoundContext {
            agent: (*agent).clone(),
            tx: (*tx).clone(),
            lifecycle: (*lifecycle).clone(),
            session: (*session).clone(),
            session_id,
            projection,
            retry_max_attempts,
            retry_base_ms,
            retry_max_ms,
        },
        input,
    )
    .await;
}

/// `AgentRequest::Chat` — start an interactive round against the thread session.
pub(crate) async fn chat(
    env: ChatEnv<'_>,
    text: String,
    images: Vec<nuo_protocol::ImagePart>,
    sent_at_ms: Option<u64>,
) {
    start_turn(
        env,
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

/// Queue a steering input into the exact live round named by `session_id`.
pub async fn steer(
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
    input: QueuedMessage,
) {
    let accepted = if session.id().await == session_id {
        agent.steer(&session_id, input.clone())
    } else {
        false
    };
    if !accepted {
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::SteerUnavailable { input_id: input.id },
        ));
    }
}

/// Cancel a steering message if it has not crossed the agent boundary yet.
pub async fn cancel_steer(
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
    input_id: String,
) {
    let cancelled = if session.id().await == session_id {
        agent.cancel_steer(&session_id, &input_id).is_some()
    } else {
        false
    };
    let event = if cancelled {
        RoundEvent::SteerCancelled { input_id }
    } else {
        RoundEvent::SteerCancelFailed { input_id }
    };
    let _ = resp_tx.send(round_response(&session_id, event));
}

/// `AgentRequest::FollowUp` — the driver-owned follow-up queue authority.
pub(crate) async fn follow_up(
    env: ChatEnv<'_>,
    queue: &mut crate::session_driver::FollowUpQueue,
    wake: &mpsc::Sender<String>,
    session_id: String,
    input: QueuedMessage,
) {
    if env.session.id().await != session_id {
        let _ = env.tx.send(round_response(
            &session_id,
            RoundEvent::SteerUnavailable { input_id: input.id },
        ));
        return;
    }
    let running = env.lifecycle.is_running().await;
    if running || queue.is_paused(&session_id) {
        let input_id = input.id.clone();
        queue.enqueue(&session_id, input);
        let _ = env.tx.send(round_response(
            &session_id,
            RoundEvent::FollowUpQueued { input_id },
        ));
        emit_queue_snapshot(env.tx, queue, &session_id);
        if running {
            spawn_boundary_watcher(env.lifecycle.clone(), wake.clone(), session_id.clone());
        }
        return;
    }
    start_queued_follow_up(env, session_id, input, queue).await;
}

/// Start one dequeued follow-up.
pub(crate) async fn start_queued_follow_up(
    env: ChatEnv<'_>,
    session_id: String,
    input: QueuedMessage,
    queue: &mut crate::session_driver::FollowUpQueue,
) {
    let current_id = env.session.id().await;
    if current_id != session_id {
        let _ = env.tx.send(round_response(
            &session_id,
            RoundEvent::SteerUnavailable { input_id: input.id },
        ));
    } else {
        let _ = env.tx.send(round_response(
            &session_id,
            RoundEvent::FollowUpStarted(input.clone()),
        ));
        start_turn(
            env,
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
    }
    emit_queue_snapshot(env.tx, queue, &session_id);
}

/// The authoritative queue snapshot for one session.
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

/// Spawn a one-shot boundary watcher: wake the driver's follow-up queue when round ends.
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

/// `AgentRequest::QueueRemove`.
pub(crate) async fn queue_remove(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    queue: &mut crate::session_driver::FollowUpQueue,
    session_id: String,
    input_id: String,
) {
    queue.remove(&session_id, &input_id);
    emit_queue_snapshot(tx, queue, &session_id);
}

/// `AgentRequest::QueueClear`.
pub(crate) async fn queue_clear(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    queue: &mut crate::session_driver::FollowUpQueue,
    session_id: String,
) {
    queue.clear(&session_id);
    emit_queue_snapshot(tx, queue, &session_id);
}

/// `AgentRequest::QueueReorder`.
pub(crate) async fn queue_reorder(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    queue: &mut crate::session_driver::FollowUpQueue,
    session_id: String,
    input_id: String,
    delta: i32,
) {
    queue.reorder(&session_id, &input_id, delta);
    emit_queue_snapshot(tx, queue, &session_id);
}

/// `AgentRequest::QueuePaused`.
pub(crate) async fn queue_paused(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    queue: &mut crate::session_driver::FollowUpQueue,
    session_id: String,
    paused: bool,
) {
    queue.set_paused(&session_id, paused);
    emit_queue_snapshot(tx, queue, &session_id);
}
