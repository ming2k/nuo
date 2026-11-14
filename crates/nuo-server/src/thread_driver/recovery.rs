//! Crash residue detection and recovery.

use super::*;

pub(super) fn request_may_mutate_context(req: &AgentRequest) -> bool {
    match req {
        AgentRequest::SlashCommand(cmd) => {
            let name = cmd.split_whitespace().next().unwrap_or("");
            matches!(
                name,
                "/compact"
                    | "/new"
                    | "/clear"
                    | "/resume"
                    | "/session"
                    | "/fork"
                    | "/retry"
                    | "/unsend"
                    | "/undo"
                    | "/skills"
                    | "/tools"
                    | "/mcp"
            )
        }
        _ => false,
    }
}

/// Whether `req` owns the round lifecycle and therefore resolves the TUI's
/// optimistic "queued" activity state on its own, via the round task's
/// terminal `HarnessState(Idle)`.
///
/// - Chat-family requests start (or feed) a round; the round task emits the
///   closing idle snapshot when it finishes, errors, or is interrupted.
///
/// Everything else is a control-plane operation (slash command, provider/
/// session/tool/mcp toggle, query, layout update, …) that runs inline in the
/// driver loop and emits no lifecycle event of its own. The driver
/// reconciles those after dispatch (see [`ThreadDriver::run`]) by
/// re-publishing the authoritative harness state.
pub(super) fn round_owned_request(req: &AgentRequest) -> bool {
    matches!(
        req,
        AgentRequest::Prompt { .. }
            | AgentRequest::FollowUp { .. }
            | AgentRequest::Steer { .. }
            | AgentRequest::CancelSteer { .. }
    )
}

/// Ship the next queued follow-up for every idle, un-paused target.
///
/// Extracted from the wake branch so the *scheduler* is not the owner of the
/// invariant: `[INV-OUTBOX-07]` requires this to be evaluated on every driver
/// iteration, because a wake is a lossy hint (`Notify` stores one permit, and a
/// boundary watcher drops its send when the channel is full). Making shipment
/// depend on receiving a hint left the queue parked forever after a lost
/// notification — and the user's only symptom was a message that never ran.
pub(super) async fn ship_queued_follow_ups(
    agent: &Arc<Agent>,
    session: &Arc<nuo_persistence::thread::ThreadSession>,
    lifecycle: &Arc<RoundLifecycle>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    shared_config: &crate::SharedConfig,
    followup_queue: &mut crate::thread_driver::FollowUpQueue,
    wake_tx: &mpsc::Sender<String>,
) {
    // Order-preserving distinct scan of the queue's targets.
    let mut targets: Vec<String> = Vec::new();
    for item in &followup_queue.items {
        if !targets.contains(&item.session_id) {
            targets.push(item.session_id.clone());
        }
    }
    for target_id in targets {
        if !followup_queue.has_items_for(&target_id) {
            continue;
        }
        if session.id().await != target_id {
            // The queue's target is not this driver's session: the items can
            // never ship from here, so drop them and say so.
            followup_queue.clear(&target_id);
            crate::handlers_chat::emit_queue_snapshot(resp_tx, followup_queue, &target_id);
            continue;
        }
        if lifecycle.is_running().await || followup_queue.is_paused(&target_id) {
            continue;
        }
        if let Some(message) = followup_queue.dequeue_front(&target_id) {
            let config = shared_config.read().await;
            crate::handlers_chat::start_queued_follow_up(
                crate::handlers_chat::ChatEnv {
                    agent,
                    session,
                    lifecycle,
                    tx: resp_tx,
                    config: &config,
                },
                target_id.clone(),
                message,
                followup_queue,
            )
            .await;
            // Watch this round's boundary so the *next* queued item ships when
            // it ends. The watch is an optimisation now, not the mechanism: the
            // next iteration re-evaluates regardless.
            crate::handlers_chat::spawn_boundary_watcher(
                lifecycle.clone(),
                wake_tx.clone(),
                target_id.clone(),
            );
        }
    }
}

/// Whether dispatching `req` will start (or feed) a round, in which case the
/// pre-dispatch ship pass must stand aside: starting a second round on top of
/// one this request is about to open would race the lifecycle.
pub(super) fn request_starts_a_round(req: &AgentRequest) -> bool {
    matches!(
        req,
        AgentRequest::Prompt { .. } | AgentRequest::FollowUp { .. }
    )
}

/// Whether the driver must reconcile the TUI's optimistic activity state after
/// dispatching `req`: true for every control-plane (non-round) request when no
/// round is live. When a round is running the reconcile is deliberately a
/// no-op — the round's own events own the display, and re-emitting a running
/// snapshot would reset the TUI's round timer/turn counters.
pub(super) async fn needs_activity_reconcile(
    req: &AgentRequest,
    lifecycle: &RoundLifecycle,
) -> bool {
    !matches!(req, AgentRequest::CompleteComposer { .. })
        && !round_owned_request(req)
        && !lifecycle.is_running().await
}

/// The durable residue of one round the host process abandoned mid-flight.
#[derive(Debug, Default)]
pub(super) struct CrashResidue {
    /// `Terminated` interrupt records to append (C11), one per distinct
    /// in-flight round, so the resumed transcript explains its dangling
    /// round instead of leaving it unexplained.
    pub(super) interrupts: Vec<nuo_protocol::RoundInterrupt>,
    /// A `/retry` resume point for the highest in-flight round, so a session
    /// re-hosted after a crash offers `/retry` instead of answering
    /// "Nothing to retry".
    pub(super) retry_point: Option<nuo_protocol::RetryPoint>,
}

/// Decide, from durable state alone, what a hard process death left dangling
/// (+ C11).
///
/// The crash signal is a request-usage record still `InFlight` **in the
/// session store**: every live settlement path (`RequestAccountingGuard`'s
/// Drop on completion/interrupt/failure) rewrites a terminal status through
/// `set_request_usage_records` before the round ends, and a graceful server
/// kill records a `Terminated` interrupt instead. A store-side `InFlight`
/// record therefore means the process vanished with the request on the wire.
///
/// Guards:
/// - Only the *highest* in-flight round is considered. The handler and
///   `start_resolved_turn` reject a point whose `round` no longer equals the
///   session's counter, so a lower one could never fire anyway.
/// - The point names only the *root* actor's round. Subagent (`task`)
///   agents bill their own requests under `subagent:<call-id>` against the same
///   session; a child's key must not decide the root agent's resume point.
/// - `turns_committed` is recovered from the transcript itself: the round's
///   committed turns are the assistant messages after its opening prompt
///   (the last visible, non-echo user message — the transcript carries no
///   round delimiters), because the in-flight turn was never committed.
/// - No point unless the round is the session's *current* one — the counter
///   below the record means the session moved past it (nothing to resurrect),
///   above it means the counter's durable write never landed.
///
/// A pre-existing terminal interrupt for the round does **not** suppress the
/// point. The two are orthogonal: the record explains the transcript, the
/// point offers recovery — and every stop a *graceful* path can observe
/// (interrupt, failure, completion) settles the usage record to a terminal
/// status, leaving store-side `InFlight` exclusively to process death. A
/// graceful kill therefore leaves the same residue as a crash, and its round
/// is just as resumable; suppressing on the interrupt would also break a
/// crash during a `/retry` resume, which reuses the same round number.
///
/// Performs only read access on the store; the caller applies the
/// interrupts / resume point through the store's normal durable setters.
pub(super) async fn recover_crashed_round(
    session: &Arc<ThreadSession>,
    records: Vec<nuo_protocol::RequestUsageRecord>,
    now_ms: u64,
) -> CrashResidue {
    use nuo_protocol::{RequestUsageStatus, Role};
    let mut residue = CrashResidue::default();
    let Some(latest) = records
        .iter()
        .filter(|record| record.status == RequestUsageStatus::InFlight)
        .max_by_key(|record| record.key.round)
    else {
        return residue;
    };
    let round = latest.key.round;
    residue.interrupts.push(nuo_protocol::RoundInterrupt {
        reason: nuo_protocol::RoundInterruptReason::Terminated,
        at_ms: now_ms,
        round: Some(round),
        detail: None,
    });
    // Counter guard: the point may only name the session's *current* round —
    // the handler (and `start_resolved_turn`) reject anything else, and a
    // round below the counter means the session already moved past it (a
    // resume-then-more-work history), while a round above it means the
    // counter's durable write never landed.
    let round_counter = session.round_counter().await;
    if round != round_counter {
        return residue;
    }
    let window = session.model_window().await;
    // Committed ReAct turns of the crashed round. The transcript carries no
    // round delimiters, so the round's opener is approximated as the last
    // visible, non-echo user message: every turn this round committed follows
    // its opening prompt, and the in-flight one was never committed (its
    // partial stream died with the process). A hidden round input opens its
    // round the same way, while a command echo (`/cmd`, `!cmd`) is
    // non-driving and must not be taken for an opener. Mid-round
    // `InsertUserInput` admissions make this an undercount (the ordinal
    // resumes lower than reality) — cosmetic: the number only labels the
    // transcript band and usage keys, history itself is seeded from the exact
    // `history_watermark`.
    let opener = window
        .iter()
        .rposition(|message| {
            matches!(message.role, Role::User) && !message.hidden && !message.is_command_echo()
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let turns_committed = window[opener..]
        .iter()
        .filter(|message| matches!(message.role, Role::Assistant))
        .count();
    residue.retry_point = Some(nuo_protocol::RetryPoint {
        round,
        turns_committed,
        history_watermark: window.len(),
        paused_ms: 0,
        at_ms: now_ms,
    });
    residue
}
