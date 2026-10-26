//! Permission / interruption handlers, extracted verbatim from the agent
//! background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`agent`, `session`, `resp_tx`,
//! `lifecycle`, `side`, `subagent_registry`, …) so the body reads exactly as
//! it did inline.
use crate::session_driver::send_harness_state_for_session;
use nuo_harness::{Agent, RoundLifecycle, SubagentRegistry};
use nuo_persistence::session::SessionStore;
use nuo_wire::{AgentResponse, LoopStatus, PermissionDecision};
use std::sync::Arc;
use tokio::sync::mpsc;

/// `AgentRequest::Interrupt` — reject every pending permission, question, and
/// interactive-input waiter; flip the harness to idle eagerly (before the
/// in-flight round's own terminal
/// idle snapshot, which is gated behind persistence fsyncs), then cancel the
/// live token. The generation is deliberately NOT bumped
/// ([`RoundLifecycle::cancel_current`], not `supersede`) so the stale round
/// still emits its own "... \[Interrupted\]" cleanup.
pub async fn interrupt(
    agent: &Agent,
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    lifecycle: &Arc<RoundLifecycle>,
) {
    // Park the reason before anything else so the unwinding round's tail can
    // label its own terminal event + durable record (C11): this stop is the
    // user's explicit Esc Esc (or the control-plane Interrupt equivalent).
    lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
    agent.reject_pending_permissions();
    agent.reject_pending_user_questions();
    agent.reject_pending_inputs();
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);

    // Flip the harness to idle the instant interrupt is requested — BEFORE
    // the in-flight turn unwinds. The work itself stops the moment the token
    // is cancelled below, but the round task's own terminal "idle" snapshot is
    // only sent at the very end of its cleanup, which is gated behind
    // persistence fsyncs (`session.replace_messages` inside `execute_round`,
    // then `set_checkpoint` in `start_pursuit`). Without this eager snapshot
    // the activity bar keeps showing the stale "pursue"/"running" loop_status
    // — and a climbing elapsed timer — for the whole disk-write window, which
    // reads as "still working" when the work is already stopped.
    //
    // This is idempotent with the stale task's later idle send: if no new
    // round starts, both snapshots are "idle"; if one does, it bumps
    // generation itself and its "running" snapshot supersedes, while the
    // stale task's generation-guarded idle send is skipped
    // (`orchestration.rs` start_pursuit / start_interactive_round).
    send_harness_state_for_session(
        resp_tx,
        &session.id().await,
        agent,
        session,
        LoopStatus::Idle,
    )
    .await;

    lifecycle.cancel_current().await;
}

/// `AgentRequest::InterruptSubagent` — interrupt one in-flight subagent spawned
/// by this session, addressed by the parent tool-call id (scene scope).
///
/// Esc Esc inside the Subagent scene resolves here instead of the primary
/// [`interrupt`]: the chord is scoped to the viewed child, so stopping it must
/// **not** cancel the enclosing primary round (which the primary `Interrupt`
/// would, propagating the cancellation down into every in-flight child via
/// `Tool::request_cancel`). The registry handle → `SubagentHandle::interrupt`
/// routes the stop into that one child; its round loop observes it at the next
/// safe boundary and returns its partial transcript, which the parent records
/// as an interrupted (resumable) result.
///
/// Unlike the primary chat path this emits **no** session-level activity
/// flip or `PermissionsCleared`: the primary chrome is untouched, and the
/// child's own `SubagentEvent`s (its interrupted tool result) repaint the
/// nested view. An unknown or already-finished `call_id` degrades to a no-op —
/// the child already settled, so there is nothing to stop.
pub fn interrupt_subagent(subagent_registry: &Arc<SubagentRegistry>, call_id: &str) {
    subagent_registry.interrupt(call_id);
}

/// `AgentRequest::PermissionReply` — full-duplex routing (ADR-0029): a reply
/// tagged with a `parent_call_id` targets a subagent's parked oneshot via the
/// registry handle; `None` targets the active thread agent.
pub async fn reply(
    agent: &Agent,
    subagent_registry: &Arc<SubagentRegistry>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    request_id: String,
    decision: PermissionDecision,
    parent_call_id: Option<String>,
) {
    let resolved = if let Some(parent) = &parent_call_id {
        subagent_registry
            .get(parent)
            .is_some_and(|handle| handle.reply_permission(&request_id, decision))
    } else {
        agent.reply_permission(&request_id, decision)
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Permission request is no longer pending.".to_string(),
        ));
    }
}

/// Bundled reply environment: the agent, subagent registry,
/// and response channel shared by the permission/question/input reply
/// handlers. Request-specific fields stay positional.
pub(crate) struct ReplyEnv<'a> {
    pub agent: &'a Agent,
    pub subagent_registry: &'a Arc<SubagentRegistry>,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
}

/// `AgentRequest::UserQuestionReply` — mirror the permission arm: a
/// `parent_call_id` targets the subagent; otherwise the active thread agent.
pub(crate) async fn reply_question(
    ReplyEnv {
        agent,
        subagent_registry,
        resp_tx,
    }: ReplyEnv<'_>,
    request_id: String,
    answers: Vec<Vec<String>>,
    parent_call_id: Option<String>,
) {
    let resolved = if let Some(parent) = &parent_call_id {
        subagent_registry
            .get(parent)
            .is_some_and(|handle| handle.reply_user_question(&request_id, answers.clone()))
    } else {
        agent.reply_user_question(&request_id, answers.clone())
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Question request is no longer pending.".to_string(),
        ));
    }
}

/// `AgentRequest::InputReply` (L3.5 β) — mirrors [`reply_question`]: a
/// `parent_call_id` targets the subagent; otherwise the active thread agent.
pub(crate) async fn reply_input(
    ReplyEnv {
        agent,
        subagent_registry,
        resp_tx,
    }: ReplyEnv<'_>,
    request_id: String,
    text: String,
    parent_call_id: Option<String>,
) {
    let resolved = if let Some(parent) = &parent_call_id {
        subagent_registry
            .get(parent)
            .is_some_and(|handle| handle.reply_input(&request_id, text.clone()))
    } else {
        agent.reply_input(&request_id, text.clone())
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Input request is no longer pending.".to_string(),
        ));
    }
}
