//! Permission / interruption handlers, extracted verbatim from the agent
//! background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`agent`, `session`, `resp_tx`,
//! `lifecycle`, `side`, `subagent_registry`, …) so the body reads exactly as
//! it did inline.

use crate::session_driver::send_harness_state_for_session;
use nuo_harness::{Agent, RoundLifecycle, SubagentRegistry};
use nuo_contracts::{AgentResponse, LoopStatus, PermissionDecision};
use nuo_persistence::session::SessionStore;
use std::sync::Arc;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::side::SideRegistry;

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
    lifecycle.record_interrupt(nuo_contracts::RoundInterruptReason::User);
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

/// `AgentRequest::PermissionReply` — full-duplex routing (ADR-0029): a reply
/// tagged with a `parent_call_id` targets a subagent's parked oneshot via the
/// registry handle; `None` keeps the legacy top-level (/btw side) path. A late
/// reply after the child finished finds no handle and falls through to the
/// "no longer pending" error.
pub async fn reply(
    agent: &Agent,
    subagent_registry: &Arc<SubagentRegistry>,
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    request_id: String,
    decision: PermissionDecision,
    parent_call_id: Option<String>,
) {
    // Three-level routing, mirroring `reply_question` / `reply_input`: a
    // `parent_call_id` targets a subagent; otherwise the primary, then a
    // `/btw` side agent. (The side fallback was missing here — a side
    // agent's permission banner reply used to fall through to "no longer
    // pending" and park forever. ADR-0141 makes all three reply handlers
    // uniform.)
    let resolved = if let Some(parent) = &parent_call_id {
        subagent_registry
            .get(parent)
            .is_some_and(|handle| handle.reply_permission(&request_id, decision))
    } else if agent.reply_permission(&request_id, decision) {
        true
    } else {
        let mut routed = false;
        for s in side.read().await.iter() {
            if s.agent.reply_permission(&request_id, decision) {
                routed = true;
                break;
            }
        }
        routed
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Permission request is no longer pending.".to_string(),
        ));
    }
}

/// Bundled reply environment: the agent, subagent registry, side registry,
/// and response channel shared by the permission/question/input reply
/// handlers. Request-specific fields stay positional.
pub(crate) struct ReplyEnv<'a> {
    pub agent: &'a Agent,
    pub subagent_registry: &'a Arc<SubagentRegistry>,
    pub side: &'a Arc<AsyncRwLock<SideRegistry>>,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
}

/// `AgentRequest::UserQuestionReply` — mirror the permission arm: a
/// `parent_call_id` targets the subagent; otherwise try the primary, then a
/// `/btw` side agent (ADR-0017).
pub(crate) async fn reply_question(
    ReplyEnv {
        agent,
        subagent_registry,
        side,
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
    } else if agent.reply_user_question(&request_id, answers.clone()) {
        true
    } else {
        let mut routed = false;
        for s in side.read().await.iter() {
            if s.agent.reply_user_question(&request_id, answers.clone()) {
                routed = true;
                break;
            }
        }
        routed
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Question request is no longer pending.".to_string(),
        ));
    }
}

/// `AgentRequest::InputReply` (L3.5 β) — mirrors [`reply_question`]: a
/// `parent_call_id` targets the subagent; otherwise try the primary, then a
/// `/btw` side agent.
pub(crate) async fn reply_input(
    ReplyEnv {
        agent,
        subagent_registry,
        side,
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
    } else if agent.reply_input(&request_id, text.clone()) {
        true
    } else {
        let mut routed = false;
        for s in side.read().await.iter() {
            if s.agent.reply_input(&request_id, text.clone()) {
                routed = true;
                break;
            }
        }
        routed
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Input request is no longer pending.".to_string(),
        ));
    }
}
