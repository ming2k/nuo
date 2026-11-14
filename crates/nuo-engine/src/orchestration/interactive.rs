//! Interactive round admission and driver supervision.

use super::harness_state::send_harness_state_for_session;
use super::*;

#[derive(Clone)]
pub struct InteractiveRoundContext {
    pub agent: Arc<Agent>,
    pub tx: mpsc::UnboundedSender<AgentResponse>,
    pub lifecycle: Arc<RoundLifecycle>,
    pub session: Arc<ThreadSession>,
    /// Session id this round belongs to. Tags every emitted
    /// [`RoundEvent`].
    pub session_id: String,
    pub projection: ContextProjectionSettings,
    pub retry_max_attempts: usize,
    /// Budget for attempts that failed after visible output. See
    /// [`RoundContext::stream_restart_max_attempts`].
    pub stream_restart_max_attempts: usize,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
}

pub async fn start_interactive_round(context: InteractiveRoundContext, input: RoundInput) {
    // Snapshot the counter *before* `begin`: this is the number the round
    // this task is about to run will be admitted under (fresh rounds bump in
    // `execute_round`; a `/retry` resume keeps the stopped round's frozen
    // number, which is also what `round_count` returns until the resume
    // re-admits it). Capturing it here keeps the superseded predecessor's
    // tail — which runs concurrently with this round's own bump — from
    // misattributing its interrupt record to whichever round owns the live
    // counter at tail time.
    let round_at_admission = match &input.driver {
        RoundDriver::Resume { point } => point.round,
        RoundDriver::Fresh => context.agent.round_count().saturating_add(1),
    };
    let RoundBegin {
        token,
        generation,
        previous,
    } = context.lifecycle.begin().await;
    if let Some(previous) = previous {
        // A newer round is replacing the still-live predecessor: park the
        // superseded reason *before* cancelling so the predecessor's tail can
        // label its own unwind (C11). Without this the stale round's
        // generation-guarded cleanup is silent and leaves no trace.
        //
        // The park is stamped with the *superseding input's* send time when
        // the user authored one: the stop the marker describes is anchored to
        // that send (the predecessor was still running when it left the
        // composer), and the resume seam-merge places the marker before the
        // first user message sent later than `at_ms` — a tail-time or
        // park-time clock read can land a few milliseconds *after* the send,
        // dropping the marker below the newer round's answer where it reads
        // as an interrupt of a round that completed normally. A hidden or
        // clock-less input falls back to the park moment.
        context.lifecycle.record_interrupt_at(
            nuo_protocol::RoundInterruptReason::Superseded,
            input.sent_at_ms,
        );
        context.agent.reject_pending_permissions();
        context.agent.reject_pending_user_questions();
        context.agent.reject_pending_inputs();
        let _ = context.tx.send(AgentResponse::PermissionsCleared);
        previous.cancel();
    }
    let (stale_steer, stale_follow_up) = context
        .agent
        .begin_session_queues(context.session_id.clone(), generation);
    for stale in stale_steer.into_iter().chain(stale_follow_up) {
        let _ = context.tx.send(round_response(
            &context.session_id,
            RoundEvent::SteerUnavailable { input_id: stale.id },
        ));
    }
    let _ = context.tx.send(round_response(
        &context.session_id,
        RoundEvent::Activity("starting request".to_string()),
    ));
    // The spawned tail records the round-interrupt into its own store handle
    // (C11); `RoundContext` consumes `context.session`, so keep an extra Arc.
    let session_for_tail = Arc::clone(&context.session);

    tokio::spawn(async move {
        // Supervised round task: the tail below (close_user_input_round →
        // terminal event → lifecycle.finish → Idle) must run even when the
        // round body panics. Before this wrapper, a panic skipped all of it:
        // `RoundLifecycle::is_running()` stayed true forever (monitor rows
        // stuck on "Running") and parked user-input
        // requests were never resolved. The panic is converted into an
        // ordinary `HarnessError::Other` so the existing error mapping emits
        // a visible `RoundEvent::Error` instead of silence.
        send_harness_state_running(&context.tx, &context.session_id, &context.agent);
        let result = {
            let round_fut = execute_round(
                RoundContext {
                    agent: context.agent.clone(),
                    tx: context.tx.clone(),
                    token: token.clone(),
                    session: context.session,
                    session_id: context.session_id.clone(),
                    projection: context.projection,
                    retry_max_attempts: context.retry_max_attempts,
                    stream_restart_max_attempts: context.stream_restart_max_attempts,
                    retry_base_ms: context.retry_base_ms,
                    retry_max_ms: context.retry_max_ms,
                    emit_round_completed: true,
                },
                input,
            );
            match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(round_fut)).await {
                Ok(result) => result,
                Err(payload) => {
                    let detail = payload
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string payload".to_string());
                    tracing::error!(panic = %detail, "agent round panicked");
                    Err(HarnessError::Other(format!(
                        "internal error: agent round panicked: {detail}"
                    )))
                }
            }
        };
        let (pending_steer, pending_follow_up) = context.agent.close_session_queues(generation);
        for pending in pending_steer.into_iter().chain(pending_follow_up) {
            let _ = context.tx.send(round_response(
                &context.session_id,
                RoundEvent::SteerUnavailable {
                    input_id: pending.id,
                },
            ));
        }
        let is_current = context.lifecycle.is_current(generation);
        // Consume the reason parked by whichever stop site cancelled this
        // round (C11). Taken before the match so every interrupted arm —
        // including the generation-suppressed one below — sees it. But a
        // parked reason alone does not mean the round *stopped*: a stop site
        // parks unconditionally (even while idle), and a late Esc Esc can
        // land after the round already passed its last cancellation
        // checkpoint. Only an actually-stopped round keeps its record —
        // a natural completion (`Ok(Completed)`) and a hook-denied prompt
        // (`Ok(NotStarted)`) are successes, not interrupts, and must not be
        // projected back as `▲ interrupted | <reason>` on resume.
        let stopped = match &result {
            Ok(RoundCompletion::Completed) | Ok(RoundCompletion::NotStarted) => false,
            Err(_) => true,
        };
        let interrupt_record = if stopped {
            // Attribution: this round's own admitted number, not the live
            // agent counter — by the time a superseded round's tail runs
            // here, the superseding round has already bumped that counter,
            // and stamping `round N+1` over round N's stop read as an
            // interrupt of the wrong (normally completed) round. The counter
            // is only read for error outcomes: a round that never got far
            // enough to know its own number (and never will — it is dead)
            // has no honest number to claim.
            context
                .lifecycle
                .take_interrupt()
                .map(|parked| nuo_protocol::RoundInterrupt {
                    reason: parked.reason,
                    at_ms: parked.at_ms,
                    round: result.as_ref().err().map(|_| round_at_admission),
                    detail: None,
                })
                .or_else(|| {
                    if let Err(error) = &result {
                        let at_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        Some(nuo_protocol::RoundInterrupt {
                            reason: nuo_protocol::RoundInterruptReason::Error,
                            at_ms,
                            round: Some(round_at_admission),
                            detail: Some(error.to_string()),
                        })
                    } else {
                        None
                    }
                })
        } else {
            // Success: drop whatever was parked so it cannot leak into a
            // later round either (defense in depth behind `begin`'s clear).
            context.lifecycle.take_interrupt();
            None
        };
        match result {
            Ok(_) => {}
            Err(HarnessError::Interrupted) => {}
            Err(error) if is_current => {
                let _ = context.tx.send(round_response(
                    &context.session_id,
                    RoundEvent::Error(error.to_string()),
                ));
            }
            Err(_) => {}
        }
        if let Some(record) = interrupt_record {
            // One record + one live event per *stopped* round. The visible
            // `[Interrupted]` arm above, the generation-suppressed supersede
            // arm (the silent `Err(_) => {}` above — previously no trace at
            // all), and the phase-1 unsend (which returns
            // `Ok(RoundCompletion::Unsent)` after emitting `UnsentInput`).
            // A round that completed naturally is deliberately excluded: its
            // history committed and `RoundCompleted` already told the story
            // — an interrupt record there would project a false
            // "▲ interrupted" marker into the resumed transcript. The record
            // is durable projection state; the live event lets every attached
            // frontend render the stop with its reason immediately.
            if let Err(error) = session_for_tail
                .record_round_interrupt(record.clone())
                .await
            {
                tracing::warn!(?error, "could not persist round interrupt record");
            }
            if record.reason != nuo_protocol::RoundInterruptReason::Error {
                let _ = context.tx.send(round_response(
                    &context.session_id,
                    RoundEvent::RoundInterrupted(record),
                ));
            }
        }
        if context.lifecycle.finish(generation).await {
            // Idle snapshot with the session in hand: this is the exact
            // moment a just-failed round's `/retry` point (armed inside
            // `execute_round`'s error path, above) becomes visible to the
            // frontends, and equally the moment a just-completed round's
            // stale point stops being offered.
            send_harness_state_for_session(
                &context.tx,
                &context.session_id,
                &context.agent,
                &session_for_tail,
                LoopStatus::Idle,
            )
            .await;
            // Proactive session ledger snapshot upon round finish/interruption.
            if let Some(ledger) = context.agent.token_ledger() {
                let report = ledger.snapshot_for_session(&context.session_id);
                let _ = context.tx.send(AgentResponse::TokenUsageReport {
                    thread_id: context.session_id.clone(),
                    report,
                });
            }
        }
    });
}
