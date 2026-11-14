//! Round, stream and steer event handling.

use nuo_protocol::RoundEvent;
use nuo_tool::Role;
use nuo_view::model::document::{TranscriptMessage, UserMessageOrigin};
use nuo_view::phase::Phase;

use crate::shell::{event_loop, stream_discard_edit, transcript_turn};

use super::{E, M, TranslatorState, TurnCtx, now_ms};

impl TranslatorState {
    /// ADR-0166: **the buffer holds exactly one session.**
    /// `ConversationCleared` / `ConversationReplaced` replace it, and a
    /// superseded round keeps unwinding afterwards — `cancel_current` signals
    /// a token, it does not join the task — so its terminal frames can land
    /// *after* the switch. Applying them would append stale rows to the new
    /// transcript (a result with no matching row becomes a fallback insert).
    /// Drop every frame addressed to a session this buffer has switched away
    /// from.
    pub(super) async fn handle_round(
        &mut self,
        session_id: String,
        scope: nuo_protocol::RoundScope,
    ) {
        if !self.session_gate.accepts(&session_id) {
            return;
        }
        let (event, turn_context) = match scope {
            nuo_protocol::RoundScope::Turn { context, event } => (event, Some(context)),
            nuo_protocol::RoundScope::Session { event } => (event, None),
        };
        // A turn-scoped event always carries its context; a
        // session-scoped event cannot claim one. There is no
        // per-session position map left to consult (ADR-0168).
        let turn_position =
            turn_context.map(|context| (context.round, transcript_turn(context.turn)));
        let turn_sent_at = turn_context.map(|context| context.sent_at_ms);
        let ctx = TurnCtx {
            turn_context,
            turn_position,
            turn_sent_at,
        };
        self.handle_round_event(session_id, event, ctx).await;
    }

    /// Route one round event to its family handler.
    pub(super) async fn handle_round_event(
        &mut self,
        session_id: String,
        event: RoundEvent,
        ctx: TurnCtx,
    ) {
        match event {
            RoundEvent::ContextTokens(_)
            | RoundEvent::TurnPerformance(_)
            | RoundEvent::RoundCompleted(_)
            | RoundEvent::RoundInterrupted(_)
            | RoundEvent::Text(_)
            | RoundEvent::Activity(_)
            | RoundEvent::TurnStarted
            | RoundEvent::StreamStart
            | RoundEvent::StreamDelta(_)
            | RoundEvent::StreamEnd(_)
            | RoundEvent::StreamDiscard
            | RoundEvent::UnsentInput { .. }
            | RoundEvent::StreamReasoningDelta(_)
            | RoundEvent::StreamReasoningEnd(_)
            | RoundEvent::Compacted { .. } => {
                self.handle_stream_round_event(session_id, event, ctx).await
            }
            RoundEvent::SteerAdmitted(_)
            | RoundEvent::SteerUnavailable { .. }
            | RoundEvent::FollowUpQueued { .. }
            | RoundEvent::QueueUpdated { .. }
            | RoundEvent::SteerCancelled { .. }
            | RoundEvent::SteerCancelFailed { .. }
            | RoundEvent::FollowUpStarted(_) => {
                self.handle_steer_event(session_id, event, ctx).await
            }
            RoundEvent::ToolCallStarted { .. }
            | RoundEvent::ToolInputProgress { .. }
            | RoundEvent::ToolCallsRetracted { .. }
            | RoundEvent::ToolCall { .. }
            | RoundEvent::ToolPreview { .. }
            | RoundEvent::ToolResult { .. }
            | RoundEvent::ToolCancelled { .. }
            | RoundEvent::ToolStream { .. } => self.handle_tool_event(session_id, event, ctx).await,
            RoundEvent::SubagentStep { .. }
            | RoundEvent::PermissionRequest(_)
            | RoundEvent::UserQuestionRequest(_)
            | RoundEvent::StdinRequest(_) => self.handle_interaction_event(event).await,
            RoundEvent::CommandResult { .. } => self.handle_command_event(event).await,
            RoundEvent::Notice(_) | RoundEvent::Error(_) => {
                self.handle_notice_event(session_id, event).await
            }
            RoundEvent::HarnessState(_)
            | RoundEvent::TodosUpdated(_)
            | RoundEvent::UnattendedChanged(_)
            | RoundEvent::ConfinementChanged(_) => {
                self.handle_harness_event(session_id, event).await
            }
            RoundEvent::RetryScheduled { .. } => self.handle_retry_event(event).await,
            RoundEvent::BackgroundJobStarted(_)
            | RoundEvent::BackgroundJobProgress { .. }
            | RoundEvent::BackgroundJobReady { .. }
            | RoundEvent::BackgroundJobCompleted(_) => {
                self.handle_background_job_event(event).await
            }
        }
    }

    /// Round/stream lifecycle: phase facts, stream deltas, and round
    /// outcomes.
    pub(super) async fn handle_stream_round_event(
        &mut self,
        session_id: String,
        event: RoundEvent,
        ctx: TurnCtx,
    ) {
        match event {
            RoundEvent::ContextTokens(snapshot) => {
                self.mutations
                    .send(M::ContextTokens {
                        session_id: session_id.clone(),
                        snapshot,
                    })
                    .await;
            }
            RoundEvent::TurnPerformance(performance) => {
                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::TurnPerformance(Box::new(performance)),
                )
                .await;
            }
            RoundEvent::RoundCompleted(_summary) => {
                // The retry episode is over; retire any transient state.
                self.retry_progress = None;
                // ADR-0166 `[INV-TOOL-ID-05]`: the round is over,
                // so no step it announced can still be ingesting
                // arguments. A round outcome is the boundary
                // signal; a `HarnessState` snapshot is a *state*
                // sample and must not be used as an edge.
                self.transcript(E::RetractPendingToolSteps).await;
            }
            RoundEvent::RoundInterrupted(record) => {
                // C11: the durable twin of the live stop.
                //
                // If the round died while backing off, the last
                // fault is the *reason*, and the anchor's
                // one-line detail is where it belongs — one
                // line, not a pretty-printed HTTP body in the
                // timeline. The full payload is already in the
                // notice log (each scheduled attempt wrote its
                // receipt) and in the client log file.
                let last_fault = self
                    .retry_progress
                    .as_ref()
                    .and_then(|progress| progress.faults().last().cloned());
                let record = match (record.detail, last_fault) {
                    (None, Some(fault)) => nuo_protocol::RoundInterrupt {
                        detail: Some(nuo_view::model::notice_text::display_text(&fault)),
                        ..record
                    },
                    (detail, _) => nuo_protocol::RoundInterrupt { detail, ..record },
                };
                self.retry_progress = None;
                self.chrome(&session_id, event_loop::mutations::ChromeEdit::RoundEnded)
                    .await;

                self.mutations.send(M::SetPhase(None)).await;
                self.mutations.send(M::SetResponding(false)).await;

                // ADR-0166: a stopped round is a boundary for the
                // same reason a completed one is.
                self.transcript(E::RetractPendingToolSteps).await;
                self.transcript(E::Interrupted { record }).await;
            }
            RoundEvent::Text(t) => {
                self.retry_progress = None;
                let (provider, model) = self.attribution();
                let effort = self.picker_effort();
                let mut message = TranscriptMessage::new(Role::Assistant, t)
                    .with_attribution(provider, model)
                    .with_effort(effort)
                    .with_sent_at_ms(ctx.turn_sent_at.unwrap_or_else(now_ms));
                if let Some((round, turn)) = ctx.turn_position {
                    message.round = Some(round);
                    message.turn = Some(turn);
                }
                self.transcript(E::Append {
                    class: nuo_protocol::EventClass::Dialogue,
                    message,
                })
                .await;
                // A one-shot text payload is the degenerate case
                // of the delta stream: it *is* visible model
                // output, so it moves the session's phase
                // exactly as a delta does. Otherwise the bar
                // keeps reading "waiting for model" after the
                // model has answered — and, since the setback
                // clause is retired when the phase leaves
                // `AwaitingModel`, the countdown for
                // the attempt that just landed rides on.
                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::PhaseOnly(Some(Phase::Answering)),
                )
                .await;

                if self.harness.loop_status.is_idle() {
                    self.mutations.send(M::SetResponding(false)).await;
                    self.mutations.send(M::SetPhase(None)).await;
                } else {
                    self.mutations
                        .send(M::SetPhase(Some(Phase::Answering)))
                        .await;
                }
            }
            RoundEvent::Activity(status) => {
                // View-scoped chrome: record this session's own
                // phase regardless of focus. Stale in-flight
                // activity events must not revive the bar if the
                // harness is already idle (e.g. after interrupt).
                if !self.harness.loop_status.is_idle() {
                    let folded = Phase::classify(&status);
                    self.chrome(
                        &session_id,
                        event_loop::mutations::ChromeEdit::ActivityFolded(folded.clone()),
                    )
                    .await;

                    self.mutations.send(M::SetPhase(Some(folded))).await;
                    self.mutations.send(M::SetResponding(true)).await;
                }
            }
            RoundEvent::TurnStarted => {
                let Some(context) = ctx.turn_context else {
                    return;
                };
                let round = context.round;
                // 1-indexed for display: turn 0 is the first
                // model request, shown as `turn 1`.
                let turn = transcript_turn(context.turn);
                self.transcript(E::StampTurnPrompt { round }).await;

                self.mutations.send(M::SetRoundCount(round)).await;
                self.mutations.send(M::SetCurrentTurn(turn)).await;

                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::TurnStarted { round, turn },
                )
                .await;

                self.mutations
                    .send(M::SetPhase(Some(Phase::AwaitingModel)))
                    .await;
            }
            RoundEvent::StreamStart => {
                // A stream lifecycle event is not visible
                // transcript content; the first visible delta
                // lazily creates its own typed component. A
                // successful stream does retire any transient
                // provider-retry disclosure.
                self.transcript(E::BeginStream).await;
                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::StreamStarted,
                )
                .await;

                self.mutations.send(M::SetResponding(true)).await;
                self.mutations
                    .send(M::SetPhase(Some(Phase::Answering)))
                    .await;
            }
            RoundEvent::StreamDelta(delta) => {
                // Visible-text deltas outrank the reasoning phase.

                self.mutations
                    .send(M::SetPhase(Some(Phase::Answering)))
                    .await;

                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::PhaseOnly(Some(Phase::Answering)),
                )
                .await;
                let Some(context) = ctx.turn_context else {
                    return;
                };
                let position = Some((context.round, transcript_turn(context.turn)));
                let sent_at_ms = context.sent_at_ms;
                let round = position.map(|(round, _)| round);
                let turn = position.map(|(_, turn)| turn);
                let (provider, model) = self.attribution();
                let effort = self.picker_effort();
                let mut created = TranscriptMessage::new(Role::Assistant, delta.clone())
                    .with_attribution(provider, model)
                    .with_effort(effort)
                    // The turn header's `HH:MM` reads this
                    // stamp. It is the *turn's* send time, so
                    // thinking / text / tools of one turn can
                    // never disagree, live or resumed.
                    .with_sent_at_ms(sent_at_ms);
                if let Some((r, t)) = position {
                    created.round = Some(r);
                    created.turn = Some(t);
                }
                self.transcript(E::StreamTextDelta {
                    round,
                    turn,
                    delta,
                    created: Some(created),
                    clear_retry: false,
                })
                .await;
            }
            RoundEvent::StreamEnd(final_content) => {
                self.mutations.send(M::SetResponding(true)).await;
                self.mutations
                    .send(M::SetPhase(Some(Phase::Finalizing)))
                    .await;

                self.retry_progress = None;
                let Some(context) = ctx.turn_context else {
                    return;
                };
                let position = Some((context.round, transcript_turn(context.turn)));
                let sent_at_ms = context.sent_at_ms;
                let round = position.map(|(round, _)| round);
                let turn = position.map(|(_, turn)| turn);
                // Identity-addressed: the applier
                // resolves the streaming entry by position. The
                // fallback (providers that deliver only a final
                // payload) is built only for non-empty content.
                let created = if !final_content.is_empty() {
                    let (provider, model) = self.attribution();
                    let effort = self.picker_effort();
                    let mut message =
                        TranscriptMessage::new(Role::Assistant, final_content.clone())
                            .with_attribution(provider, model)
                            .with_effort(effort)
                            // Same rule as the delta arm: this
                            // only-visible-bytes path must carry the
                            // turn's send time into the band header.
                            .with_sent_at_ms(sent_at_ms);
                    if let Some((r, t)) = position {
                        message.round = Some(r);
                        message.turn = Some(t);
                    }
                    Some(message)
                } else {
                    None
                };
                self.transcript(E::StreamTextFinalize {
                    round,
                    turn,
                    content: final_content,
                    created,
                })
                .await;
            }
            RoundEvent::StreamDiscard => {
                self.retry_progress = None;
                // The turn context rides the envelope
                // (ADR-0168); no `TurnStarted` observation is
                // required. The wire ordinal is 0-based; the
                // transcript row is 1-based.
                let Some(context) = ctx.turn_context else {
                    return;
                };
                self.transcript(stream_discard_edit(context.round, context.turn))
                    .await;
            }
            RoundEvent::UnsentInput { .. } => {
                // Retraction is removed: entries only grow and
                // update in place; the prompt is marked Cancelled.
                self.retry_progress = None;
                self.transcript(E::CancelLastUserPrompt).await;

                self.mutations.send(M::SetResponding(false)).await;
                self.mutations.send(M::SetPhase(None)).await;
            }
            RoundEvent::StreamReasoningDelta(delta) => {
                // Phase fact before anything else: the reasoning
                // stream is alive. Hidden-chain models also land
                // here (their summary deltas still prove thinking).

                self.mutations
                    .send(M::SetPhase(Some(Phase::Reasoning)))
                    .await;

                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::PhaseOnly(Some(Phase::Reasoning)),
                )
                .await;
                // Surface only a reasoning summary, never their
                // full chain: gate at message creation. Unrecognized
                // ids default to disclosed — only known
                // `ReasoningSummary` models are gated.
                let hidden_chain = !nuo_protocol::model_by_id(&self.current_model)
                    .map(|m| m.thinking.chain_disclosed())
                    .unwrap_or(true);
                if hidden_chain {
                    return;
                }
                let Some(context) = ctx.turn_context else {
                    return;
                };
                let position = Some((context.round, transcript_turn(context.turn)));
                let sent_at_ms = context.sent_at_ms;
                let round = position.map(|(round, _)| round);
                let turn = position.map(|(_, turn)| turn);
                let (provider, model) = self.attribution();
                let effort = self.picker_effort();
                let mut created = TranscriptMessage::reasoning(delta.clone())
                    .with_attribution(provider, model)
                    .with_effort(effort)
                    // A reasoning trace is a turn component, so it
                    // carries the turn's send time: when thinking
                    // is the *first* thing a turn emits (the common
                    // case for a reasoning channel), this entry
                    // owns the band header and its `HH:MM`.
                    .with_sent_at_ms(sent_at_ms);
                if let Some((r, t)) = position {
                    created.round = Some(r);
                    created.turn = Some(t);
                }
                // Disclosure default is applied by the applier
                // (`App::reasoning_default_expanded`).
                self.transcript(E::ReasoningDelta {
                    round,
                    turn,
                    delta,
                    created: Some(created),
                })
                .await;
                self.reasoning_start = Some(std::time::Instant::now());
            }
            RoundEvent::StreamReasoningEnd(content) => {
                let duration_ms = self
                    .reasoning_start
                    .take()
                    .map(|started| started.elapsed().as_millis() as u64);
                let Some(context) = ctx.turn_context else {
                    return;
                };
                let position = Some((context.round, transcript_turn(context.turn)));
                self.transcript(E::ReasoningFinalize {
                    round: position.map(|(round, _)| round),
                    turn: position.map(|(_, turn)| turn),
                    content,
                    duration_ms,
                })
                .await;
            }
            RoundEvent::Compacted {
                archived_messages,
                window_tokens_before,
                window_tokens_after,
                summary,
                tracked_files,
            } => {
                let message = TranscriptMessage::compacted_card(
                    archived_messages,
                    window_tokens_before,
                    window_tokens_after,
                    summary,
                    tracked_files,
                )
                .with_sent_at_ms(now_ms());
                self.transcript(E::Append {
                    class: nuo_protocol::EventClass::TurnOutcome,
                    message,
                })
                .await;
            }
            _ => {}
        }
    }

    /// Steering, follow-up queue and unsent-input events.
    pub(super) async fn handle_steer_event(
        &mut self,
        session_id: String,
        event: RoundEvent,
        ctx: TurnCtx,
    ) {
        match event {
            RoundEvent::SteerUnavailable { input_id } => {
                // Steer is ephemeral to the active round. It MUST NOT silently mutate
                // into a next-round follow-up item. Remove the optimistic entry from transcript
                // and restore the text directly to the composer draft.
                self.mutations
                    .send(M::SteerMissed {
                        session_id,
                        input_id,
                    })
                    .await;
            }
            RoundEvent::SteerAdmitted(input) => {
                let input_id = input.id.clone();
                let visible = input
                    .display_text
                    .clone()
                    .unwrap_or_else(|| input.text.clone());
                // ADR-0137: admission is the materialization
                // point, and the buffer tail is the turn seam
                // (the harness persisted the steer after the
                // finished turn; the next turn's components
                // have not arrived). The message is APPENDED —
                // never head-inserted, which would put a
                // round-N steer above round 1.
                //
                // Position: the steer is *consumed* by the turn
                // that is about to start, and the event carries
                // that turn's context directly. The round is
                // unchanged (a steer never opens a round).
                let mut admitted = TranscriptMessage::new(Role::User, visible);
                admitted.insert_id = Some(input_id.clone());
                admitted.sent_at_ms = input.sent_at_ms;
                admitted.origin = UserMessageOrigin::Steer;
                let Some(context) = ctx.turn_context else {
                    return;
                };
                admitted.round = Some(context.round);
                admitted.turn = Some(transcript_turn(context.turn));
                // In-round provenance: the renderer draws it as
                // a one-body-row interjection (ADR-0137).
                admitted = admitted
                    .with_origin_tags(vec![nuo_view::model::document::SteerOriginTag::InRound]);
                self.transcript(E::InsertAdmittedSeam { message: admitted })
                    .await;
                self.mutations
                    .send(M::DispatchRemoved {
                        session_id,
                        input_id,
                    })
                    .await;
            }
            RoundEvent::SteerCancelled { input_id } => {
                // The pending steer was cancelled before
                // admission (its only legal fate beside
                // SteerAdmitted, per the protocol's
                // linearization contract). Restore the text to
                // the composer and drop the outbox row; the
                // transcript never held it (ADR-0137).
                self.mutations
                    .send(M::SteerCancelledRestored {
                        session_id,
                        input_id,
                    })
                    .await;
            }
            RoundEvent::SteerCancelFailed { input_id } => {
                // Cancellation lost the race with admission (or
                // the id was unknown): the protocol guarantees
                // that `SteerAdmitted` / `SteerUnavailable`
                // still follows and is authoritative.
                //
                // What must be repaired here is the client-side
                // intent recorded by a deletion: the suppress
                // flag exists so a *deleted* steer cannot
                // resurrect into the composer. If the cancel
                // failed, the steer is going to land after all,
                // so keeping the flag would silently swallow a
                // future recall for this id. Clear it and say
                // what happened — a deletion that lost the race
                // must not be a silent surprise (ADR-0137).
                self.mutations
                    .send(M::SteerCancelFailedCleared {
                        session_id,
                        input_id,
                    })
                    .await;
            }
            RoundEvent::FollowUpQueued { input_id } => {
                // The server admitted the follow-up into its
                // queue: the optimistic entry settles back to
                // Waiting (the queue bar keeps showing it).
                self.mutations
                    .send(M::DispatchQueued {
                        session_id: session_id.clone(),
                        input_id,
                    })
                    .await;
            }
            RoundEvent::QueueUpdated { items, paused } => {
                // The authoritative queue snapshot (
                // M4): full-replace projection.
                self.mutations
                    .send(M::QueueSnapshot {
                        session_id: session_id.clone(),
                        items,
                        paused,
                    })
                    .await;
            }
            RoundEvent::FollowUpStarted(input) => {
                let input_id = input.id.clone();
                let visible = input
                    .display_text
                    .clone()
                    .unwrap_or_else(|| input.text.clone());
                // ADR-0137: a follow-up opens a *fresh round*, so
                // its admission is the same materialization
                // point as a steer's — one transcript fact,
                // appended at the seam the harness persisted it
                // at. The former `SettleInserted` path settled an
                // optimistic in-thread row that no longer exists
                // (follow-ups live in the outbox until now), so
                // both admission kinds share one edit.
                let mut admitted = TranscriptMessage::new(Role::User, visible);
                admitted.insert_id = Some(input_id.clone());
                admitted.sent_at_ms = input.sent_at_ms;
                admitted.origin = UserMessageOrigin::FollowUp;
                // A follow-up opens its own round, so it keeps
                // full round-panel weight.
                admitted = admitted.with_origin_tags(vec![
                    nuo_view::model::document::SteerOriginTag::RoundOpening,
                ]);
                self.transcript(E::InsertAdmittedSeam { message: admitted })
                    .await;
                self.mutations
                    .send(M::DispatchRemoved {
                        session_id,
                        input_id,
                    })
                    .await;
            }
            _ => {}
        }
    }
}
