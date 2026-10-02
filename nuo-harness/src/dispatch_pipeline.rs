//! Four-stage tool-call dispatch pipeline.
//!
//! The native tool-call path of [`Agent::dispatch_tool_calls`] is split into
//! four named stages, each owning a precise slice of the historical
//! 250-line function so the whole can be reasoned about stage by stage:
//!
//! | stage | owns |
//! |---|---|
//! | **preflight** ([`Agent::dispatch_preflight`], per turn) | turn classification (`consecutive_readonly_turns`), checkpoint-replay scan + `ProviderRetry` notice, doom-guard `check_doom_ahead` (signature masking + `NudgeInjected` notice + nudge capture), dispatch-id generation, the up-front `AgentEvent::ToolCall` events (all of them, before any `ToolResult`), and the short-circuits: checkpoint-replay and guard-blocked calls get their terminal `ToolResult(duration_ms = 0)` here and their result slot is filled without execution. |
//! | **prepare** (per call, in-task) | the gate sequence inside [`Agent::execute_tool`]: tool resolution (builtin → user → mcp), the full [`PermissionChain`](crate::permission_policy::PermissionChain) evaluation (folding in hook/disabled/schema/scope/bash/broker), interaction-only handling, and the bash stdin policy. It deliberately runs *inside* each scheduled task, not as a separate serialised phase: PreToolUse hooks and permission parks keep their historical concurrency. |
//! | **schedule** ([`Agent::schedule_tool_calls`], the batch) | the concurrent fan-out through [`ToolScheduler`]: per-call declared [`ToolAccesses`](nuo_contracts::ToolAccesses) arbitrate which calls run concurrently (a write serializes against any other access to the same path; non-conflicting reads parallelize). A shared `mpsc` channel forwards `Subagent`/`ToolStream`/`PermissionRequest` events in real time; each task emits its terminal `ToolResult` the instant it finishes; a turn interrupt runs the two-tier cancel (cooperative drain with `SUBAGENT_DRAIN_GRACE`, then forced abort) and pairs every unproduced call with a terminal `AgentEvent::ToolCancelled`. |
//! | **finalize** ([`Agent::dispatch_finalize`], per call, input order) | recovered results folded back into the input-ordered slots, `remember_completed_tool`, [`Agent::record_tool_result`] (token accounting, `TodosUpdated`, `Message::tool_result` with subagent children/meta, image peel-out), post-tool hooks unless replay, turn-level doom-nudge injection, `Ok(!denied)`. On interruption it records only the drained results and returns `Err(HarnessError::Interrupted)` — no hooks, no nudge, no `remember`. |
//!
//! The text-fallback path (one call per turn) stays inside
//! `dispatch_tool_calls`, driving [`Agent::execute_tool_evented`], which
//! mirrors the same cancellation contract for a single call.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, ConcurrentOutcome, RoundState};
use crate::tool_scheduler::{ToolCallTask, ToolScheduler};
use crate::{
    AgentEvent, AgentNotice, HarnessError, InjectionKind, Message, NoticeKind, NoticeSeverity,
    NoticeSource, NoticeSurface, SUBAGENT_DRAIN_GRACE, ToolCall, ToolOutput,
};

/// The product of [`Agent::dispatch_preflight`]: everything `schedule` and
/// `finalize` need to finish the batch.
pub(crate) struct PreparedDispatch {
    /// Dispatch-generated ids, one per call in input order. The matching
    /// `ToolCall` events have all been emitted already.
    pub(crate) call_ids: Vec<String>,
    /// Per-call checkpoint-replay flags (finalize skips post-tool hooks and
    /// the nested-usage token accounting for these; the call never entered
    /// the exec set).
    pub(crate) checkpoint_replays: Vec<bool>,
    /// Indices (into the batch) of the calls that must actually execute, in
    /// input order — scheduler submission order derives from this.
    pub(crate) exec_indices: Vec<usize>,
    /// Per-call result slots, input order. Short-circuited calls (replay /
    /// guard-blocked) are already filled with their `(output, 0)` terminal
    /// result; executable slots stay `None` until `schedule` recovers them.
    pub(crate) results: Vec<Option<(ToolOutput, u64)>>,
    /// Turn-level signals for finalize.
    pub(crate) signals: TurnSignals,
}

/// Per-turn signals computed by preflight and consumed by finalize.
#[derive(Debug, Default)]
pub(crate) struct TurnSignals {
    /// The hidden `LoopReviewNudge` to inject after the batch, if the trajectory
    /// guard blocked anything this turn.
    pub(crate) trajectory_nudge: Option<String>,
}

/// Forward one live event from a scheduled task to the dispatch callback,
/// filling the call's result slot when the event is its terminal
/// `ToolResult`. Slots are filled from the events themselves — the event
/// stream is the single source of truth for "this call produced a result",
/// which keeps the slot and the UI event atomic: a task aborted between
/// emitting its `ToolResult` and reporting back to the scheduler still
/// counts as produced (the event is already visible), and is never doubly
/// terminated with a `ToolCancelled`.
fn forward_scheduled_event<F>(
    event: AgentEvent,
    slot_index: &std::collections::HashMap<String, usize>,
    slots: &mut [Option<(ToolOutput, u64)>],
    on_event: &mut F,
) where
    F: FnMut(AgentEvent) + Send,
{
    if let AgentEvent::ToolResult {
        id,
        structured,
        duration_ms,
        ..
    } = &event
        && let Some(&i) = slot_index.get(id)
    {
        slots[i] = Some((structured.clone(), *duration_ms));
    }
    on_event(event);
}

impl Agent {
    /// **Stage 1 — preflight** (per turn). Classifies the turn, scans for
    /// checkpoint replays, runs the pre-dispatch trajectory-guard check, generates
    /// the dispatch ids, emits every `ToolCall` event up front, and resolves
    /// the short-circuits (replay / guard-blocked) by emitting their terminal
    /// `ToolResult(duration_ms = 0)` and filling their slots. Returns the
    /// prepared batch; the calls listed in [`PreparedDispatch::exec_indices`]
    /// still need execution.
    pub(crate) async fn dispatch_preflight<F>(
        &self,
        tool_calls: &[ToolCall],
        messages: &[Message],
        state: &mut RoundState,
        on_event: &mut F,
    ) -> PreparedDispatch
    where
        F: FnMut(AgentEvent) + Send,
    {
        // Classify this turn once, for two consumers: the turn-hook axis
        // (consecutive read-only streak, surfaced to user hooks) and the
        // round-scoped guard registry (checked at the turn boundary). Any call
        // whose target is a real Path/Command (i.e. not Unspecified) makes
        // the turn "progress", resetting both.
        let all_read = tool_calls
            .iter()
            .all(|c| self.tool_target_is_unspecified(&c.name, &c.arguments));
        if all_read {
            state.consecutive_readonly_turns = state.consecutive_readonly_turns.saturating_add(1);
        } else {
            state.consecutive_readonly_turns = 0;
        }

        // A provider retry may produce the same tool request again even
        // though its terminal result is already in the checkpointed
        // history. Treat exact matches as idempotency replays regardless
        // of the optional trajectory-loop setting.
        let checkpoint_replays: Vec<bool> = tool_calls
            .iter()
            .map(|call| state.is_checkpoint_replay(call))
            .collect();
        if checkpoint_replays.iter().any(|replayed| *replayed) {
            on_event(AgentEvent::Notice(
                AgentNotice::new(
                    NoticeKind::ProviderRetry,
                    NoticeSeverity::Warning,
                    "Completed tool call not repeated",
                    NoticeSource::Harness,
                )
                .with_body(
                    "The retried model request repeated a tool call that already completed. \
                     The checkpointed result remains authoritative; the tool was not run again.",
                )
                .with_surface(NoticeSurface::Toast),
            ));
        }

        // Pre-dispatch trajectory-loop check (ADR-0247). Before
        // any tool runs this turn, evaluate trajectory against repeating ruts.
        let trajectory_calls: Vec<(&str, &str)> = tool_calls
            .iter()
            .zip(&checkpoint_replays)
            .filter(|(_, replayed)| !**replayed)
            .map(|(call, _)| (call.name.as_str(), call.arguments.as_str()))
            .collect();
        let preceding_context = {
            let mut ctx = String::new();
            for m in messages.iter().rev().take(6) {
                if !m.content.is_empty() {
                    ctx.push_str(&format!(
                        "{:?}: {}\n",
                        m.role,
                        m.content.chars().take(500).collect::<String>()
                    ));
                }
            }
            ctx
        };
        let trajectory_action = if let Some(input) = state
            .guards
            .check_trajectory_candidate(&trajectory_calls, preceding_context)
        {
            tracing::info!(
                signature = %input.signature,
                tier = input.threshold_tier,
                "L1 trajectory detector flagged repetition candidate; invoking Steward cognitive arbiter (ADR-0247)"
            );
            let verdict = self.cognitive().review_trajectory_loop(input.clone()).await;
            if verdict.is_loop() {
                tracing::warn!(
                    signature = %input.signature,
                    "Cognitive arbiter confirmed trajectory loop; applying surgical block"
                );
                state.guards.block_trajectory(&input.signature)
            } else {
                tracing::info!(
                    signature = %input.signature,
                    tier = input.threshold_tier,
                    "Cognitive arbiter acquitted candidate; escalating backoff ladder (ADR-0247)"
                );
                state
                    .guards
                    .acquit_and_backoff(&input.signature, &trajectory_calls);
                crate::guard::GuardAction::Continue
            }
        } else {
            state.guards.check_trajectory_ahead(&trajectory_calls)
        };
        let trajectory_nudge: Option<String> = match &trajectory_action {
            crate::guard::GuardAction::Block { message, .. } => {
                tracing::warn!(
                    blocked = ?state.guards.blocked_summary(),
                    "trajectory guard blocked a repeating tool call before execution"
                );
                on_event(AgentEvent::Notice(
                    AgentNotice::new(
                        NoticeKind::NudgeInjected,
                        NoticeSeverity::Warning,
                        "Repeating tool call blocked",
                        NoticeSource::TurnGuard,
                    )
                    .with_body(
                        "The agent tried to re-run a tool call it already issued this round. \
                         The call was blocked before it ran — the result it already has is \
                         unchanged, so re-running it cannot help. The agent must change \
                         approach (or call `abort`).",
                    )
                    .with_surface(NoticeSurface::Toast),
                ));
                Some(message.clone())
            }
            _ => None,
        };

        // Emit all ToolCall events up front — before any ToolResult.
        let call_ids: Vec<String> = tool_calls
            .iter()
            .map(|_| format!("call_{}", uuid::Uuid::new_v4()))
            .collect();
        tracing::info!(count = tool_calls.len(), "dispatching native tool calls");
        for (call, id) in tool_calls.iter().zip(&call_ids) {
            tracing::debug!(tool = %call.name, "tool call");
            on_event(AgentEvent::ToolCall {
                id: id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });
        }
        // Signature-level trajectory guard (ADR-0247): a call whose canonical
        // signature is in the per-round block mask is short-circuited here, before execution. The model gets an
        // explanatory error instead of the content/side-effect, so it is
        // physically unable to re-enter the loop. Blocked calls are split
        // out so the rest run concurrently exactly as before. Each blocked
        // call is given the same ToolResult event an executed call would
        // get, so the UI never sees an orphaned running step.
        let blocked_output = |name: &str| {
            ToolOutput::Text(format!(
                "[loop guard] This call ({name}) is blocked for the rest of the turn \
                 because it was a repeat of one already issued this round. Re-running it \
                 cannot help: the result is already in context above. Act on it now \
                 (use what you already have, try a *different* command/file/query), or, \
                 if you cannot proceed, say so explicitly or call `abort`."
            ))
        };
        let checkpoint_output = |name: &str| {
            ToolOutput::Text(format!(
                "[retry checkpoint] This exact {name} call already completed before the \
                 provider retry. Its result is present earlier in the conversation and \
                 remains authoritative. The tool was not executed again."
            ))
        };
        let mut results: Vec<Option<(ToolOutput, u64)>> =
            (0..tool_calls.len()).map(|_| None).collect();
        let exec_indices: Vec<usize> = tool_calls
            .iter()
            .enumerate()
            .filter(|(idx, c)| {
                if checkpoint_replays[*idx] {
                    tracing::warn!(
                        tool = %c.name,
                        args = %c.arguments,
                        "provider retry repeated a completed tool call"
                    );
                    let output = checkpoint_output(&c.name);
                    let id = &call_ids[*idx];
                    on_event(AgentEvent::ToolResult {
                        id: id.clone(),
                        name: c.name.clone(),
                        output: output.to_text(),
                        structured: output.clone(),
                        duration_ms: 0,
                    });
                    results[*idx] = Some((output, 0));
                    false
                } else if state.guards.is_blocked(&c.name, &c.arguments) {
                    tracing::warn!(
                        tool = %c.name,
                        args = %c.arguments,
                        "tool call blocked by turn-loop guard signature mask"
                    );
                    let output = blocked_output(&c.name);
                    let id = &call_ids[*idx];
                    // Emit the ToolResult the executed path would have, so
                    // the UI pairs this call's ToolCall with a terminal
                    // result instead of leaving it "running".
                    on_event(AgentEvent::Notice(
                        AgentNotice::new(
                            NoticeKind::NudgeInjected,
                            NoticeSeverity::Warning,
                            "Blocked repeating tool call",
                            NoticeSource::TurnGuard,
                        )
                        .with_body(format!(
                            "A tool call ({}) was blocked by the loop guard — it is a \
                             repeat of a call already issued this round. Use the result \
                             already in context, or try a different call.",
                            c.name,
                        ))
                        .with_surface(NoticeSurface::Toast),
                    ));
                    on_event(AgentEvent::ToolResult {
                        id: id.clone(),
                        name: c.name.clone(),
                        output: output.to_text(),
                        structured: output.clone(),
                        duration_ms: 0,
                    });
                    results[*idx] = Some((output, 0));
                    false // do not execute
                } else {
                    true // execute
                }
            })
            .map(|(idx, _)| idx)
            .collect();
        PreparedDispatch {
            call_ids,
            checkpoint_replays,
            exec_indices,
            results,
            signals: TurnSignals { trajectory_nudge },
        }
    }

    /// **Stage 3 — schedule** (the batch). Fan the executable calls out
    /// through a [`ToolScheduler`] in input order (the scheduler's FIFO
    /// queueing preserves it), forwarding interleaved events to the callback
    /// in real time. Returns the outcome slots in input order.
    ///
    /// Cancellation-aware, two-tier: an interrupt first cancels the batch
    /// cooperatively — queued tasks reject immediately, running tasks observe
    /// their child token (a cooperatively-cancellable call, i.e. a subagent,
    /// drains to a terminal result) — within the bounded
    /// [`SUBAGENT_DRAIN_GRACE`]; whatever still has not settled is then aborted.
    /// The outcome reports `interrupted: true` with every drained result
    /// preserved in its slot, and every call that produced nothing is paired
    /// with a terminal [`AgentEvent::ToolCancelled`]. The caller
    /// ([`Agent::dispatch_finalize`]) decides how to end the round.
    pub(crate) async fn schedule_tool_calls<F>(
        self: &Arc<Self>,
        calls: &[ToolCall],
        call_ids: &[String],
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<ConcurrentOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        // One scheduler per batch; the batch-wide token is a child of the
        // turn token, so `cancel_all` reaches queued and running tasks alike
        // without touching the turn token itself.
        let scheduler: ToolScheduler<()> = ToolScheduler::with_token(cancel.child_token());
        let (tx, mut rx) = mpsc::unbounded_channel();

        // Submit in input order. A call whose tool can't be resolved gets
        // `none()` accesses (freely parallel) — it still produces its
        // "not found" error inside `execute_tool`; there's no point
        // serializing an error.
        let mut receivers = Vec::with_capacity(calls.len());
        for (call, call_id) in calls.iter().zip(call_ids) {
            let accesses = self.accesses_for_call(call);
            let agent = Arc::clone(self);
            let call = call.clone();
            let call_id = call_id.clone();
            let tx = tx.clone();
            let task = ToolCallTask::new(accesses, move |token| async move {
                let started = std::time::Instant::now();
                // One execution future, pinned: the cancel arm keeps driving
                // the SAME future to its drained terminal result instead of
                // dropping and re-entering the tool (dropping mid-permission park
                // or mid-subagent and re-running would duplicate side effects).
                let fut = agent.execute_tool(&call, &call_id, &tx);
                tokio::pin!(fut);
                let output = tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        // Cooperative cancel: a tool that can stop at a safe
                        // boundary (a subagent) is signalled and drains; anything
                        // else is dropped here by returning early — its
                        // terminal ToolCancelled is emitted by the driver,
                        // which owns pairing for unproduced calls.
                        let tool = agent.tool_manager().find(&call.name);
                        let cancellable = tool
                            .as_ref()
                            .is_some_and(|sourced| sourced.tool.supports_cooperative_cancel());
                        if !cancellable {
                            return Err(crate::tool_scheduler::SchedulerError::Cancelled);
                        }
                        if let Some(sourced) = &tool {
                            sourced.tool.request_cancel(&call_id);
                        }
                        fut.await
                    }
                    output = &mut fut => output,
                };
                let duration_ms = started.elapsed().as_millis() as u64;
                // Emit ToolResult immediately so the TUI transitions this
                // step Running→Completed without waiting for siblings.
                let _ = tx.send(AgentEvent::ToolResult {
                    id: call_id.clone(),
                    name: call.name.clone(),
                    output: output.to_text(),
                    structured: output.clone(),
                    duration_ms,
                });
                Ok(())
            });
            receivers.push(scheduler.add(task).await);
        }
        // Drop the driver's own sender so `rx` closes once every task has
        // finished; `rx_open` then latches off instead of spinning on `None`.
        drop(tx);
        let mut rx_open = true;
        // Result slots, filled from the terminal ToolResult events (see
        // `forward_scheduled_event`). On interrupt the slots keep whatever
        // drained before the grace deadline, so finalize can record real
        // work even though the round ends.
        let mut slots: Vec<Option<(ToolOutput, u64)>> = vec![None; calls.len()];
        let slot_index: std::collections::HashMap<String, usize> = call_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();
        let receivers = futures::future::join_all(receivers);
        tokio::pin!(receivers);

        let interrupted = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break true,
                event = rx.recv(), if rx_open => {
                    match event {
                        Some(event) => {
                            forward_scheduled_event(event, &slot_index, &mut slots, on_event);
                        }
                        None => rx_open = false,
                    }
                }
                _ = &mut receivers => break false,
            }
        };

        if interrupted {
            // Cooperative tier: reject every queued task immediately and
            // signal the running ones through their child tokens.
            scheduler.cancel_all().await;
            // Bounded grace for cooperative tools (subagents) to drain to a
            // terminal result; events keep flowing while we wait.
            let grace = tokio::time::sleep(SUBAGENT_DRAIN_GRACE);
            tokio::pin!(grace);
            let drained = loop {
                tokio::select! {
                    biased;
                    _ = &mut grace => break false,
                    event = rx.recv(), if rx_open => {
                        match event {
                            Some(event) => {
                                forward_scheduled_event(event, &slot_index, &mut slots, on_event);
                            }
                            None => rx_open = false,
                        }
                    }
                    _ = &mut receivers => break true,
                }
            };
            if !drained {
                // Grace expired: force-stop whatever ignored the token. A
                // receiver dropped by the abort ends in `RecvError` — no
                // result; its slot stays `None` unless its ToolResult event
                // already landed (and filled the slot) before the abort.
                scheduler.abort_all().await;
            }
            while let Ok(event) = rx.try_recv() {
                forward_scheduled_event(event, &slot_index, &mut slots, on_event);
            }
            // Pair every announced-but-unproduced call with its terminal
            // ToolCancelled; produced (drained) calls keep their results.
            for (i, slot) in slots.iter().enumerate() {
                if slot.is_none() {
                    on_event(AgentEvent::ToolCancelled {
                        id: call_ids[i].clone(),
                        name: calls[i].name.clone(),
                    });
                }
            }
            return Ok(ConcurrentOutcome {
                results: slots,
                interrupted: true,
            });
        }

        // Normal completion: drain the terminal events that landed with the
        // final tasks, then flatten in input order. Any slot still None means
        // its task never produced (defensive — every terminal task emits a
        // ToolResult); synthesize the loop-guard placeholder to keep the
        // contract non-panicking.
        while let Ok(event) = rx.try_recv() {
            forward_scheduled_event(event, &slot_index, &mut slots, on_event);
        }
        for slot in slots.iter_mut() {
            let _ = slot
                .get_or_insert_with(|| (ToolOutput::Text("[loop guard] blocked".to_string()), 0));
        }
        Ok(ConcurrentOutcome {
            results: slots,
            interrupted: false,
        })
    }

    /// **Stage 4 — finalize** (per call, input order). Fold the recovered
    /// results back into the input-ordered batch, record every result
    /// (token accounting, todos, transcript messages), run post-tool hooks
    /// for non-replay calls, and inject the trajectory nudge captured by
    /// preflight. On interruption only the drained results are recorded and
    /// the round ends with `Err(HarnessError::Interrupted)` — no hooks, no
    /// nudge, no `remember_completed_tool`, matching the historical contract.
    pub(crate) async fn dispatch_finalize<F>(
        &self,
        tool_calls: &[ToolCall],
        mut prepared: PreparedDispatch,
        outcome: Option<ConcurrentOutcome>,
        messages: &mut Vec<Message>,
        state: &mut RoundState,
        on_event: &mut F,
    ) -> Result<bool, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        if let Some(outcome) = outcome {
            // Destructure so the per-call results can move out by value.
            let ConcurrentOutcome {
                results: recovered,
                interrupted,
            } = outcome;
            if interrupted {
                // The user interrupted this turn mid-flight. Record whatever
                // real work drained (an interrupted subagent's partial
                // transcript), then end the round as interrupted — the caller
                // commits the transcript so the recovered work survives into
                // the session. Dropped calls already received their terminal
                // `ToolCancelled` event.
                for (&idx, recovered) in prepared.exec_indices.iter().zip(recovered) {
                    if let Some((result, duration_ms)) = recovered {
                        self.record_tool_result(
                            crate::agent::ToolResultRecord {
                                call: &tool_calls[idx],
                                call_id: &prepared.call_ids[idx],
                                result: &result,
                                duration_ms,
                                checkpoint_replay: false,
                                emit_event: false,
                            },
                            messages,
                            state,
                            on_event,
                        );
                    }
                }
                return Err(HarnessError::Interrupted);
            }
            for (&idx, recovered) in prepared.exec_indices.iter().zip(recovered) {
                prepared.results[idx] = recovered;
                state.remember_completed_tool(&tool_calls[idx]);
            }
        }
        // Flatten back to a positional Vec, matching tool_calls order.
        let results: Vec<(ToolOutput, u64)> = prepared
            .results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| (ToolOutput::Text("[loop guard] blocked".to_string()), 0)))
            .collect();
        let denied = results
            .iter()
            .any(|(result, _)| matches!(result, ToolOutput::PermissionDenied { .. }));
        for (idx, ((call, id), (result, duration_ms))) in tool_calls
            .iter()
            .zip(&prepared.call_ids)
            .zip(results)
            .enumerate()
        {
            self.record_tool_result(
                crate::agent::ToolResultRecord {
                    call,
                    call_id: id,
                    result: &result,
                    duration_ms,
                    checkpoint_replay: prepared.checkpoint_replays[idx],
                    emit_event: false,
                },
                messages,
                state,
                on_event,
            );
            if !prepared.checkpoint_replays[idx] {
                self.run_post_tool_hooks(call, &result, duration_ms, messages)
                    .await;
            }
        }
        // If the user denied permission for any call, stop the round here
        // instead of feeding the (possibly partial) results back to the
        // model and asking it to continue.
        // If the trajectory guard blocked any repeats this round, deliver its
        // consolidated message as a hidden user note alongside the blocked
        // tool results, so the model learns *why* its call was refused and
        // what to do instead. Non-terminating: the turn continues with the
        // (now masked) signatures hard-blocked for subsequent turns in this round.
        if let Some(message) = prepared.signals.trajectory_nudge {
            messages.push(crate::conversation_context::hidden_user(
                InjectionKind::LoopReviewNudge,
                message,
            ));
        }
        Ok(!denied)
    }
}
