//! Response translator: translates wire AgentResponse stream into AppMutations.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

use crate::shell::{
    event_loop, is_coalescible_stream_update, session_gate, stream_discard_edit,
    transcript::{
        merge_command_rows, merge_round_interrupt_rows, transcript_commands_from_ledger,
        transcript_interrupts_from_records, transcript_messages_from_core,
    },
    transcript_turn,
};
use nuo_protocol::{AgentResponse, LoopStatus, ProviderPickerSnapshot, Role, RoundEvent};
use nuo_view::model::document::{NoticeSeverity, TranscriptMessage, UserMessageOrigin};
use nuo_view::phase::Phase;

pub struct ResponseTranslatorConfig {
    pub rx: mpsc::UnboundedReceiver<AgentResponse>,
    pub runtime: event_loop::UiRuntime,
    pub link_lost: crate::shell::client::LinkLostMarker,
    pub tui_config: Arc<crate::shell::config::TuiConfig>,
    pub initial_provider: String,
    pub initial_model: String,
    pub initial_round_count: u64,
    pub live_session: String,
}

pub(crate) fn spawn_response_translator(config: ResponseTranslatorConfig) {
    let ResponseTranslatorConfig {
        mut rx,
        runtime,
        link_lost,
        tui_config,
        initial_provider,
        initial_model,
        initial_round_count,
        live_session,
    } = config;

    let mutations = runtime.mutations.clone();
    let link_lost_for_reader = link_lost.clone();
    let dirty_clone = runtime.dirty.clone();
    let dirty_notify_clone = runtime.dirty_notify.clone();
    let ir_clone = runtime.is_responding.clone();
    let trust_gate_dismissed_clone = runtime.trust_gate_dismissed.clone();
    let awaiting_oauth_add_clone = runtime.awaiting_oauth_add.clone();
    let viewed_session_id_clone = runtime.viewed_session_id.clone();
    let tui_config_clone = tui_config.clone();
    let live_session_for_translator = live_session.clone();
    let initial_provider_for_translator = initial_provider.clone();
    let initial_model_for_translator = initial_model.clone();

    tokio::spawn(async move {
        // Turn position and send time ride each turn-scoped event
        // (`RoundScope::Turn`); the translator keeps no per-session
        // position or timestamp map (ADR-0168). A mid-stream attach is
        // therefore as correct as a client that observed `TurnStarted`.
        // A session switch replaces the transcript before its authoritative
        // idle HarnessState arrives. Rebase the reconstructed tail exactly
        // once when that snapshot supplies the persisted round counter.
        let mut needs_round_rebase = false;
        // Translator-owned mirrors of the values it itself receives (sent to
        // `App` via mutations): attribution, the provider-picker snapshot
        // (effort derivation), and the harness snapshot (idle gating). No
        // shared cells are read for these.
        let mut current_provider = initial_provider_for_translator.clone();
        let mut current_model = initial_model_for_translator.clone();
        let mut picker = ProviderPickerSnapshot::default();
        let mut harness = nuo_protocol::HarnessSnapshot {
            loop_status: LoopStatus::Idle,
            round_counter: initial_round_count,
            unattended: false,
            confined: true,
            workspace_security: nuo_protocol::WorkspaceSecuritySnapshot::default(),
            retry_pending: false,
            role: None,
            workspace: None,
        };
        // The round's retry episode, as the *typed* state machine
        // (`nuo_view::retry`). One value holds the attempt pair, the
        // backoff instant and the fault history, so the three consumers
        // that need it — the bar clause, the receipt-log line and the
        // settled turn anchor — cannot disagree about any of them.
        //
        // It is the only retry state in the translator: there is no
        // parallel counter and no second copy of the count. `None` means
        // the round in flight has not retried. The `= None` writes below
        // are the round boundaries, and they exist so a later round's
        // error can never be blamed for an earlier round's attempts.
        let mut retry_progress: Option<nuo_view::retry::RetryProgress> = None;
        let mut reasoning_start: Option<std::time::Instant> = None;
        // The live primary session id: the translator updates it on
        // `/new` / `/session open` / `/resume` / `/fork` and scopes
        // session-addressed replies (the tree snapshot) against it.
        let mut live_session = live_session_for_translator.clone();
        // ADR-0166 `[INV-TOOL-ID-06]`: sessions this client has switched
        // away from. A superseded round can still emit terminal frames after
        // the switch (`cancel_current` signals a token; it does not join the
        // task), and those frames must not write into the replacement
        // buffer. See [`session_gate`] for why this is a superseded set and
        // not an equality check against the live id.
        let mut session_gate = session_gate::SessionGate::new();

        // Attribution + effort derivation from translator-owned mirrors
        // (formerly `event_loop::attribution` / `picker_effort` over
        // shared cells).
        macro_rules! attribution {
            () => {
                (current_provider.clone(), current_model.clone())
            };
        }
        macro_rules! picker_effort {
            () => {
                // The picker row already carries the server's resolved
                // depth — no protocol sniffing, no re-derivation
                // (ADR-0126).
                picker
                    .rows
                    .iter()
                    .find(|row| row.id == current_provider)
                    .and_then(|row| row.model_info.iter().find(|m| m.model == current_model))
                    .and_then(|m| m.effort.clone())
            };
        }
        macro_rules! now_ms {
            () => {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            };
        }

        while let Some(resp) = rx.recv().await {
            // Any handled response can change state the loop renders from,
            // so signal a redraw. High-frequency stream deltas deliberately
            // do not wake the loop one-by-one: while responding, its 10fps
            // heartbeat coalesces them into a smooth stream.
            dirty_clone.store(true, Ordering::Release);
            // A side thread can receive stream deltas while the
            // primary activity indicator is idle — no heartbeat then, so
            // retain the immediate wake.
            let defer_stream_wakeup =
                is_coalescible_stream_update(&resp) && ir_clone.load(Ordering::SeqCst);
            if !defer_stream_wakeup {
                dirty_notify_clone.notify_one();
            }
            use event_loop::mutations::AppMutation as M;
            use event_loop::mutations::TranscriptEdit as E;
            match resp {
                // Per-turn events arrive tagged with the session they
                // belong to; there is a single primary transcript buffer now
                // (the aside substrate is retired, ADR-0058), so the
                // translator routes every event to it. Permission and
                // user-question requests stay global so their modals surface
                // regardless of which view is focused.
                AgentResponse::Round {
                    thread_id: session_id,
                    scope,
                } => {
                    // ADR-0166: **the buffer holds exactly one session.**
                    // `ConversationCleared` / `ConversationReplaced` replace
                    // it, and a superseded round keeps unwinding afterwards —
                    // `cancel_current` signals a token, it does not join the
                    // task — so its terminal frames can land *after* the
                    // switch. Applying them would append stale rows to the
                    // new transcript (a result with no matching row becomes a
                    // fallback insert). Drop every frame addressed to a
                    // session this buffer has switched away from.
                    if !session_gate.accepts(&session_id) {
                        continue;
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
                    macro_rules! require_turn {
                        () => {
                            match turn_context {
                                Some(context) => context,
                                None => continue,
                            }
                        };
                    }
                    macro_rules! transcript {
                        ($edit:expr) => {
                            mutations.send(M::Transcript { edit: $edit }).await;
                        };
                    }
                    macro_rules! chrome {
                        ($edit:expr) => {
                            mutations
                                .send(M::ChromeEdit {
                                    session_id: session_id.clone(),
                                    edit: $edit,
                                })
                                .await;
                        };
                    }
                    match event {
                        RoundEvent::ContextTokens(snapshot) => {
                            mutations
                                .send(M::ContextTokens {
                                    session_id: session_id.clone(),
                                    snapshot,
                                })
                                .await;
                        }
                        RoundEvent::TurnPerformance(performance) => {
                            chrome!(event_loop::mutations::ChromeEdit::TurnPerformance(
                                Box::new(performance),
                            ));
                        }
                        RoundEvent::SteerUnavailable { input_id } => {
                            // Steer is ephemeral to the active round. It MUST NOT silently mutate
                            // into a next-round follow-up item. Remove the optimistic entry from transcript
                            // and restore the text directly to the composer draft.
                            mutations
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
                            let context = require_turn!();
                            admitted.round = Some(context.round);
                            admitted.turn = Some(transcript_turn(context.turn));
                            // In-round provenance: the renderer draws it as
                            // a one-body-row interjection (ADR-0137).
                            admitted = admitted.with_origin_tags(vec![
                                nuo_view::model::document::SteerOriginTag::InRound,
                            ]);
                            transcript!(E::InsertAdmittedSeam { message: admitted });
                            mutations
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
                            mutations
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
                            mutations
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
                            mutations
                                .send(M::DispatchQueued {
                                    session_id: session_id.clone(),
                                    input_id,
                                })
                                .await;
                        }
                        RoundEvent::QueueUpdated { items, paused } => {
                            // The authoritative queue snapshot (
                            // M4): full-replace projection.
                            mutations
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
                            transcript!(E::InsertAdmittedSeam { message: admitted });
                            mutations
                                .send(M::DispatchRemoved {
                                    session_id,
                                    input_id,
                                })
                                .await;
                        }
                        RoundEvent::RoundCompleted(_summary) => {
                            // The retry episode is over; retire any transient state.
                            retry_progress = None;
                            // ADR-0166 `[INV-TOOL-ID-05]`: the round is over,
                            // so no step it announced can still be ingesting
                            // arguments. A round outcome is the boundary
                            // signal; a `HarnessState` snapshot is a *state*
                            // sample and must not be used as an edge.
                            transcript!(E::RetractPendingToolSteps);
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
                            let last_fault = retry_progress
                                .as_ref()
                                .and_then(|progress| progress.faults().last().cloned());
                            let record = match (record.detail, last_fault) {
                                (None, Some(fault)) => nuo_protocol::RoundInterrupt {
                                    detail: Some(nuo_view::model::notice_text::display_text(
                                        &fault,
                                    )),
                                    ..record
                                },
                                (detail, _) => nuo_protocol::RoundInterrupt { detail, ..record },
                            };
                            retry_progress = None;
                            chrome!(event_loop::mutations::ChromeEdit::RoundEnded);

                            mutations.send(M::SetPhase(None)).await;
                            mutations.send(M::SetResponding(false)).await;

                            // ADR-0166: a stopped round is a boundary for the
                            // same reason a completed one is.
                            transcript!(E::RetractPendingToolSteps);
                            transcript!(E::Interrupted { record });
                        }
                        RoundEvent::Notice(notice) => {
                            // While the PreAttach trust gate owns the screen, a
                            // trust **failure** is the gate's round-trip outcome
                            // (the server could not persist the grant) — deliver it
                            // to the surface so the failure renders where the
                            // submitting spinner was, instead of burying it in a
                            // transcript the full-screen interstitial hides. The
                            // applier drops it when no gate is mounted, so
                            // in-session failures keep their normal disclosure.
                            //
                            // Only `TrustFailure` is intercepted. A
                            // `TrustChanged` *condition* is what opened the gate in
                            // the first place; feeding it here would render the
                            // workspace's own state as if it were an error.
                            if notice.kind == nuo_protocol::NoticeKind::TrustFailure {
                                mutations
                                    .send(M::PreAttachAttestationError(notice.render_text()))
                                    .await;
                            }
                            // The layer is derived from the surface context,
                            // never chosen by the producer and never the
                            // thread transcript ([INV-LAYER-04]): a notice is
                            // a receipt, not dialogue.
                            //
                            // `SHELL` declares the notice log, so every
                            // disclosure is *retained* there. A fault
                            // additionally raises a bubble that names the log;
                            // an acknowledgement does not. That inversion is
                            // the point: the log is the home, the bubble is a
                            // pointer to it.
                            let layer = nuo_view::model::layer::layer_for(
                                &notice,
                                nuo_view::model::layer::SHELL,
                            );
                            if layer != nuo_view::model::layer::ContentLayer::Absorbed {
                                // `RetryScheduled` owns the retry disclosure;
                                // an absorbed receipt is dropped rather than
                                // written into the conversation. The catalog
                                // projects the wire notice into the funnel
                                // payload (ADR-0138), taking the topic from
                                // the closed wire vocabulary.
                                mutations
                                        .send(M::NoticeDisclosure(
                                            crate::shell::app::notice_catalog::RenderedNotice::from_core(
                                                &notice,
                                            ),
                                        ))
                                        .await;
                            }
                        }
                        RoundEvent::Text(t) => {
                            retry_progress = None;
                            let (provider, model) = attribution!();
                            let effort = picker_effort!();
                            let mut message = TranscriptMessage::new(Role::Assistant, t)
                                .with_attribution(provider, model)
                                .with_effort(effort)
                                .with_sent_at_ms(turn_sent_at.unwrap_or_else(|| now_ms!()));
                            if let Some((round, turn)) = turn_position {
                                message.round = Some(round);
                                message.turn = Some(turn);
                            }
                            transcript!(E::Append {
                                class: nuo_protocol::EventClass::Dialogue,
                                message
                            });
                            // A one-shot text payload is the degenerate case
                            // of the delta stream: it *is* visible model
                            // output, so it moves the session's phase
                            // exactly as a delta does. Otherwise the bar
                            // keeps reading "waiting for model" after the
                            // model has answered — and, since the setback
                            // clause is retired when the phase leaves
                            // `AwaitingModel`, the countdown for
                            // the attempt that just landed rides on.
                            chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                Phase::Answering,
                            )));

                            if harness.loop_status.is_idle() {
                                mutations.send(M::SetResponding(false)).await;
                                mutations.send(M::SetPhase(None)).await;
                            } else {
                                mutations.send(M::SetPhase(Some(Phase::Answering))).await;
                            }
                        }
                        RoundEvent::CommandResult {
                            name,
                            args,
                            result,
                            effect,
                        } => {
                            mutations.send(M::ClearSwitchingSession).await;
                            retry_progress = None;
                            let invocation = if args.is_empty() {
                                format!("/{}", name)
                            } else {
                                format!("/{} {}", name, args)
                            };
                            // The server's declared class decides where the
                            // reply is filed, and nothing else does
                            // (ADR-0140, `[INV-CMD-08]`). An **act** holds a
                            // position in the thread, so it settles the staged
                            // row in place (or materialises the fallback if
                            // the act happened before this view was watching).
                            // A **reading** never touches the thread: its
                            // reply is a receipt in the client's notice log,
                            // beside every other disclosure, because a
                            // measurement of the world is not a thing that was
                            // said.
                            //
                            // Furthermore (ADR-0149, `[INV-CMD-09]`), any failed
                            // command or unknown command is withdrawn from the thread:
                            // errors never deface L0 transcript, but route to the
                            // top-right notice toast and restore the user's draft.
                            let is_failure = matches!(
                                result,
                                nuo_protocol::CommandResult::Error { .. }
                                    | nuo_protocol::CommandResult::UnknownCommand { .. }
                            );
                            if is_failure || effect == nuo_protocol::CommandEffect::Reading {
                                transcript!(E::WithdrawStagedCommand {
                                    invocation: invocation.clone(),
                                    restore_draft: is_failure,
                                });
                                mutations
                                        .send(M::NoticeDisclosure(
                                            crate::shell::app::notice_catalog::ClientNotice::CommandReceipt {
                                                invocation,
                                                body: result.to_text(),
                                                failed: is_failure,
                                            }
                                            .render(),
                                        ))
                                        .await;
                            } else {
                                let fallback = TranscriptMessage::command_result(
                                    name.clone(),
                                    args.clone(),
                                    Some(result.clone()),
                                )
                                .with_sent_at_ms(now_ms!());
                                transcript!(E::SettleCommandResult {
                                    invocation,
                                    result,
                                    fallback: Some(fallback),
                                });
                            }
                            if harness.loop_status.is_idle() {
                                mutations.send(M::SetResponding(false)).await;
                                mutations.send(M::SetPhase(None)).await;
                            }
                        }
                        RoundEvent::Activity(status) => {
                            // View-scoped chrome: record this session's own
                            // phase regardless of focus. Stale in-flight
                            // activity events must not revive the bar if the
                            // harness is already idle (e.g. after interrupt).
                            if !harness.loop_status.is_idle() {
                                let folded = Phase::classify(&status);
                                chrome!(event_loop::mutations::ChromeEdit::ActivityFolded(
                                    folded.clone(),
                                ));

                                mutations.send(M::SetPhase(Some(folded))).await;
                                mutations.send(M::SetResponding(true)).await;
                            }
                        }
                        RoundEvent::TurnStarted => {
                            let Some(context) = turn_context else {
                                continue;
                            };
                            let round = context.round;
                            // 1-indexed for display: turn 0 is the first
                            // model request, shown as `turn 1`.
                            let turn = transcript_turn(context.turn);
                            transcript!(E::StampTurnPrompt { round });

                            mutations.send(M::SetRoundCount(round)).await;
                            mutations.send(M::SetCurrentTurn(turn)).await;

                            chrome!(event_loop::mutations::ChromeEdit::TurnStarted { round, turn });

                            mutations
                                .send(M::SetPhase(Some(Phase::AwaitingModel)))
                                .await;
                        }
                        RoundEvent::StreamStart => {
                            // A stream lifecycle event is not visible
                            // transcript content; the first visible delta
                            // lazily creates its own typed component. A
                            // successful stream does retire any transient
                            // provider-retry disclosure.
                            transcript!(E::BeginStream);
                            chrome!(event_loop::mutations::ChromeEdit::StreamStarted);

                            mutations.send(M::SetResponding(true)).await;
                            mutations.send(M::SetPhase(Some(Phase::Answering))).await;
                        }
                        RoundEvent::StreamDelta(delta) => {
                            // Visible-text deltas outrank the reasoning phase.

                            mutations.send(M::SetPhase(Some(Phase::Answering))).await;

                            chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                Phase::Answering,
                            )));
                            let context = require_turn!();
                            let position = Some((context.round, transcript_turn(context.turn)));
                            let sent_at_ms = context.sent_at_ms;
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            let (provider, model) = attribution!();
                            let effort = picker_effort!();
                            let mut created =
                                TranscriptMessage::new(Role::Assistant, delta.clone())
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
                            transcript!(E::StreamTextDelta {
                                round,
                                turn,
                                delta,
                                created: Some(created),
                                clear_retry: false,
                            });
                        }
                        RoundEvent::StreamEnd(final_content) => {
                            mutations.send(M::SetResponding(true)).await;
                            mutations.send(M::SetPhase(Some(Phase::Finalizing))).await;

                            retry_progress = None;
                            let context = require_turn!();
                            let position = Some((context.round, transcript_turn(context.turn)));
                            let sent_at_ms = context.sent_at_ms;
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            // Identity-addressed: the applier
                            // resolves the streaming entry by position. The
                            // fallback (providers that deliver only a final
                            // payload) is built only for non-empty content.
                            let created = if !final_content.is_empty() {
                                let (provider, model) = attribution!();
                                let effort = picker_effort!();
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
                            transcript!(E::StreamTextFinalize {
                                round,
                                turn,
                                content: final_content,
                                created,
                            });
                        }
                        RoundEvent::StreamDiscard => {
                            retry_progress = None;
                            // The turn context rides the envelope
                            // (ADR-0168); no `TurnStarted` observation is
                            // required. The wire ordinal is 0-based; the
                            // transcript row is 1-based.
                            let Some(context) = turn_context else {
                                continue;
                            };
                            transcript!(stream_discard_edit(context.round, context.turn));
                        }
                        RoundEvent::UnsentInput { .. } => {
                            // Retraction is removed: entries only grow and
                            // update in place; the prompt is marked Cancelled.
                            retry_progress = None;
                            transcript!(E::CancelLastUserPrompt);

                            mutations.send(M::SetResponding(false)).await;
                            mutations.send(M::SetPhase(None)).await;
                        }
                        RoundEvent::StreamReasoningDelta(delta) => {
                            // Phase fact before anything else: the reasoning
                            // stream is alive. Hidden-chain models also land
                            // here (their summary deltas still prove thinking).

                            mutations.send(M::SetPhase(Some(Phase::Reasoning))).await;

                            chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                Phase::Reasoning,
                            )));
                            // Surface only a reasoning summary, never their
                            // full chain: gate at message creation. Unrecognized
                            // ids default to disclosed — only known
                            // `ReasoningSummary` models are gated.
                            let hidden_chain = !nuo_protocol::model_by_id(&current_model)
                                .map(|m| m.thinking.chain_disclosed())
                                .unwrap_or(true);
                            if hidden_chain {
                                continue;
                            }
                            let context = require_turn!();
                            let position = Some((context.round, transcript_turn(context.turn)));
                            let sent_at_ms = context.sent_at_ms;
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            let (provider, model) = attribution!();
                            let effort = picker_effort!();
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
                            transcript!(E::ReasoningDelta {
                                round,
                                turn,
                                delta,
                                created: Some(created),
                            });
                            reasoning_start = Some(std::time::Instant::now());
                        }
                        RoundEvent::StreamReasoningEnd(content) => {
                            let duration_ms = reasoning_start
                                .take()
                                .map(|started| started.elapsed().as_millis() as u64);
                            let context = require_turn!();
                            let position = Some((context.round, transcript_turn(context.turn)));
                            transcript!(E::ReasoningFinalize {
                                round: position.map(|(round, _)| round),
                                turn: position.map(|(_, turn)| turn),
                                content,
                                duration_ms,
                            });
                        }
                        RoundEvent::ToolCallStarted { id, name } => {
                            // ADR-0026 / ADR-0166: the call is named but its
                            // arguments are still streaming. Show a running
                            // step and move the phase to the tool verb now,
                            // instead of leaving the bar on the misleading
                            // `answering` phase for the whole argument stream.
                            // The step carries the call id the later
                            // `ToolCall` will carry, so the applier merges
                            // instead of inserting a second row.
                            mutations
                                .send(M::SetPhase(Some(Phase::Tool(event_loop::tool_verb_for(
                                    &name,
                                )))))
                                .await;
                            mutations.send(M::SetResponding(true)).await;

                            chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                Phase::Tool(event_loop::tool_verb_for(&name)),
                            )));
                            let (provider, model) = attribution!();
                            let effort = picker_effort!();
                            let context = require_turn!();
                            let position = Some((context.round, transcript_turn(context.turn)));
                            let sent_at_ms = context.sent_at_ms;
                            let mut message = TranscriptMessage::tool_step_announced(id, name)
                                .with_attribution(provider, model)
                                .with_effort(effort)
                                // A tool-only turn has no text or thinking to own
                                // the band header: this announced step does. It
                                // carries the turn's send time so the live band
                                // shows `HH:MM` exactly as the resumed one does.
                                .with_sent_at_ms(sent_at_ms);
                            if let Some((round, turn)) = position {
                                message = message.with_round(round).with_turn(turn);
                            }
                            transcript!(E::ToolStepUpsert { message });
                        }
                        RoundEvent::ToolInputProgress {
                            id,
                            bytes,
                            arguments,
                        } => {
                            // ADR-0026 / ADR-0046: progress tick and streaming arguments.
                            mutations.send(M::SetResponding(true)).await;

                            transcript!(E::ToolInputProgress {
                                id,
                                bytes,
                                arguments,
                            });
                        }
                        RoundEvent::ToolCallsRetracted { ids } => {
                            // The attempt that announced these calls died
                            // before dispatch. Drop the provisional rows;
                            // they never described a call, so a terminal
                            // "cancelled" row would be a lie.
                            transcript!(E::ToolCallsRetracted { ids });
                        }
                        RoundEvent::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
                            mutations
                                .send(M::SetPhase(Some(Phase::Tool(event_loop::tool_verb_for(
                                    &name,
                                )))))
                                .await;

                            let (provider, model) = attribution!();
                            let effort = picker_effort!();
                            retry_progress = None;
                            let context = require_turn!();
                            let position = Some((context.round, transcript_turn(context.turn)));
                            let sent_at_ms = context.sent_at_ms;
                            // The turn's time, not this call's: it is the same
                            // value the announced step (and every sibling
                            // component) carries, so advancing an announcement
                            // into a dispatch cannot change the band header.
                            // ADR-0166: the dispatch fact carries the same id
                            // the announcement did. The applier merges it into
                            // that row (or inserts it when the provider never
                            // announced, or the attempt was retracted).
                            let mut message = TranscriptMessage::tool_step(id, name, arguments)
                                .with_attribution(provider, model)
                                .with_effort(effort)
                                .with_sent_at_ms(sent_at_ms);
                            if let Some((round, turn)) = position {
                                message = message.with_round(round).with_turn(turn);
                            }
                            transcript!(E::ToolStepUpsert { message });

                            mutations.send(M::SetResponding(true)).await;
                        }
                        RoundEvent::ToolPreview { id, preview, .. } => {
                            // ADR-0125: the preview arrives strictly after the
                            // `ToolCall` that created this step, so matching by
                            // dispatch id finds it. No phase change and no
                            // responding toggle: nothing is running yet, and the
                            // card simply gains its diff.
                            transcript!(E::ToolPreview { id, preview });
                        }
                        RoundEvent::ToolResult {
                            id,
                            name,
                            output,
                            structured,
                            duration_ms,
                        } => {
                            mutations.send(M::SetPhase(Some(Phase::Preparing))).await;

                            chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                Phase::Preparing,
                            )));
                            let (provider, model) = attribution!();
                            let context = require_turn!();
                            let position = Some((context.round, transcript_turn(context.turn)));
                            // The fallback (no matching in-flight call, e.g. a
                            // turn restored from history) is fully finished by
                            // the translator; the applier applies the
                            // lifecycle-aware default disclosure.
                            let mut fallback =
                                TranscriptMessage::tool_step(id.clone(), name.clone(), "{}")
                                    .with_attribution(provider, model);
                            if let Some((round, turn)) = position {
                                fallback.round = Some(round);
                                fallback.turn = Some(turn);
                            }
                            fallback.finish_tool_step(
                                &id,
                                output.clone(),
                                structured.clone(),
                                duration_ms,
                            );
                            transcript!(E::ToolResult {
                                id,
                                name,
                                output,
                                structured,
                                duration_ms,
                                fallback: Some(fallback),
                            });
                        }
                        RoundEvent::ToolCancelled { id, .. } => {
                            // An in-flight call was aborted: flip it (and any
                            // nested subagent children) to Cancelled.
                            let context = require_turn!();
                            let position = Some((context.round, transcript_turn(context.turn)));
                            let mut fallback =
                                TranscriptMessage::tool_step(id.clone(), "tool", "{}");
                            if let Some((round, turn)) = position {
                                fallback.round = Some(round);
                                fallback.turn = Some(turn);
                            }
                            fallback.cancel_tool_step(&id);
                            fallback.set_tool_step_expanded(false);
                            transcript!(E::ToolCancel {
                                id,
                                fallback: Some(fallback)
                            });
                        }
                        RoundEvent::ToolStream { id, stream } => {
                            transcript!(E::ToolStream { id, stream });
                        }
                        RoundEvent::SubagentStep {
                            parent_call_id,
                            event,
                        } => {
                            // Full-duplex (ADR-0029): a subagent's permission
                            // broker or `ask_user` request bubbles up nested
                            // under this `parent_call_id`; the reply is tagged
                            // for down-routing into the child.
                            match &event {
                                nuo_protocol::SubagentEvent::PermissionRequest(req) => {
                                    mutations
                                        .send(M::QueuePermission {
                                            request: req.clone(),
                                            parent_call_id: Some(parent_call_id.clone()),
                                        })
                                        .await;

                                    mutations.send(M::SetPhase(Some(Phase::AwaitingUser))).await;
                                    mutations.send(M::SetResponding(true)).await;
                                }
                                nuo_protocol::SubagentEvent::UserQuestionRequest(req) => {
                                    mutations
                                        .send(M::QueueQuestion {
                                            request: req.clone(),
                                            parent_call_id: Some(parent_call_id.clone()),
                                        })
                                        .await;

                                    mutations.send(M::SetPhase(Some(Phase::AwaitingUser))).await;
                                    mutations.send(M::SetResponding(true)).await;
                                }
                                _ => {}
                            }
                            transcript!(E::SubagentEvent {
                                parent_call_id,
                                event,
                            });
                        }
                        RoundEvent::PermissionRequest(request) => {
                            // Stays global regardless of session so the modal
                            // always surfaces (ADR-0017 → ADR-0058).
                            mutations
                                .send(M::QueuePermission {
                                    request,
                                    parent_call_id: None,
                                })
                                .await;

                            mutations.send(M::SetPhase(Some(Phase::AwaitingUser))).await;
                            mutations.send(M::SetResponding(true)).await;
                        }
                        RoundEvent::UserQuestionRequest(request) => {
                            mutations
                                .send(M::QueueQuestion {
                                    request,
                                    parent_call_id: None,
                                })
                                .await;

                            mutations.send(M::SetPhase(Some(Phase::AwaitingUser))).await;
                            mutations.send(M::SetResponding(true)).await;
                        }
                        RoundEvent::StdinRequest(request) => {
                            mutations.send(M::QueueInput(request)).await;

                            mutations.send(M::SetPhase(Some(Phase::AwaitingUser))).await;
                            mutations.send(M::SetResponding(true)).await;
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
                            .with_sent_at_ms(now_ms!());
                            transcript!(E::Append {
                                class: nuo_protocol::EventClass::TurnOutcome,
                                message
                            });
                        }
                        RoundEvent::HarnessState(snapshot) => {
                            harness.loop_status = snapshot.loop_status;
                            harness.round_counter = snapshot.round_counter;
                            let running = !snapshot.loop_status.is_idle();
                            // View-scoped chrome: the authoritative
                            // running/idle transition for this session.
                            chrome!(event_loop::mutations::ChromeEdit::RoundLifecycle {
                                round_count: snapshot.round_counter,
                                running,
                                can_retry: snapshot.retry_pending && !running,
                            });

                            // Publish a quarantined snapshot to
                            // the PreAttach mount when the per-run gate
                            // has not already answered. The applier
                            // deduplicates (it mounts only when the
                            // interstitial is absent).
                            {
                                harness.workspace_security = snapshot.workspace_security.clone();
                                let gate_needed = !trust_gate_dismissed_clone
                                    .load(Ordering::SeqCst)
                                    && nuo_view::trust_gate::gate_request(
                                        &harness.workspace_security,
                                    )
                                    .is_some();
                                if gate_needed {
                                    mutations
                                        .send(M::PreAttach(crate::shell::PreAttachSignal {
                                            snapshot: harness.workspace_security.clone(),
                                        }))
                                        .await;
                                }
                            }
                            if running {
                                // A new round resets the turn counter and
                                // stamps the elapsed-timer origin.
                                mutations.send(M::SetCurrentTurn(0)).await;
                                mutations
                                    .send(M::SetRoundStartedAt(Some(std::time::Instant::now())))
                                    .await;
                            }
                            mutations.send(M::SetResponding(running)).await;
                            if !running {
                                // The dispatch cycle is complete: any
                                // command component still Pending will
                                // never receive its reply on this pass
                                // mark Cancelled.
                                mutations
                                    .send(M::Transcript {
                                        edit: E::CancelPendingCommands,
                                    })
                                    .await;
                                mutations.send(M::SetPhase(None)).await;
                                mutations.send(M::SetCurrentTurn(0)).await;
                                mutations.send(M::SetRoundStartedAt(None)).await;
                            }
                            if !running && needs_round_rebase {
                                // A session switch replaced the transcript
                                // before its authoritative idle snapshot
                                // arrived: rebase the reconstructed tail
                                // exactly once, now that the persisted
                                // round counter is known.
                                mutations
                                    .send(M::Transcript {
                                        edit: E::RebaseRounds {
                                            round_counter: snapshot.round_counter,
                                        },
                                    })
                                    .await;
                                needs_round_rebase = false;
                            }
                            mutations.send(M::Harness(snapshot.clone())).await;

                            // A harness state change is always a round
                            // boundary. If the previous round ended
                            // mid-reasoning, `StreamReasoningEnd` never
                            // arrives; freeze the orphaned trace so the
                            // spinner stops.
                            let duration_ms = reasoning_start
                                .take()
                                .map(|started| started.elapsed().as_millis() as u64);
                            if !running {
                                retry_progress = None;
                                // ADR-0166: a `HarnessState` sample is not a
                                // round boundary. The provisional-row sweep
                                // lives on the round-outcome events
                                // (`RoundCompleted` / `RoundInterrupted` /
                                // `Error`), which *mean* "the round ended".
                                // Making an idle sample harmless requires the
                                // server to publish idle only when it is idle;
                                // that is enforced at every publisher.
                            }
                            transcript!(E::FinalizeOrphanedReasoning { duration_ms });
                        }
                        RoundEvent::TodosUpdated(_) => {}
                        RoundEvent::UnattendedChanged(enabled) => {
                            harness.unattended = enabled;
                            mutations.send(M::HarnessUnattended(enabled)).await;
                        }
                        RoundEvent::ConfinementChanged(confined) => {
                            harness.confined = confined;
                            mutations.send(M::HarnessConfined(confined)).await;
                        }
                        RoundEvent::RetryScheduled {
                            attempt,
                            max_attempts,
                            delay_ms,
                            message,
                        } => {
                            // ADR-0166: a scheduled retry is an attempt
                            // boundary — the failed attempt is over and its
                            // provisional rows cannot become calls. The
                            // harness retracted them by id; this sweep makes
                            // the invariant hold even if that frame was lost
                            // or arrived after the retry.
                            transcript!(E::RetractPendingToolSteps);
                            let delay = std::time::Duration::from_millis(delay_ms);
                            let retry_at = std::time::Instant::now() + delay;
                            // Advance the one typed state machine. Every
                            // consumer below reads *this* value, so the
                            // ordinal on the bar and the ordinal in the
                            // receipt are the same number by construction.
                            let progress = match retry_progress.take() {
                                Some(progress) => progress.with_next_attempt(
                                    attempt,
                                    max_attempts,
                                    retry_at,
                                    message.clone(),
                                ),
                                None => nuo_view::retry::RetryProgress::scheduled(
                                    attempt,
                                    max_attempts,
                                    retry_at,
                                    message.clone(),
                                ),
                            };
                            let fault = nuo_view::model::notice_text::display_text(&message);
                            // The record's ordinal pair, composed by the same
                            // owner as the bar's clause (`receipt_label`), so
                            // the two can never disagree about which retry
                            // this was — and this is the one place the
                            // attempt *budget* is stated: `retry 2 of 30`
                            // answers "how much rope is left?" for a reader
                            // consulting the log afterwards, which is exactly
                            // the question the live bar must not spend
                            // columns on.
                            let receipt = progress.receipt_label();
                            // The bar's clause and the stored state are the
                            // same value: clone once here so the published
                            // setback cannot be a second, independently
                            // constructed copy.
                            let published = progress.clone();
                            retry_progress = Some(progress);

                            // Publish-only. The clause is retired by the
                            // next phase write for *this* session — each
                            // store owns its own phase — never by this
                            // translator, which is why no arm of this match
                            // carries a clause clear. The primary's slot is
                            // the App mirror; a background session's is its
                            // own chrome entry, so a background retry cannot
                            // paint a countdown onto the primary's bar.
                            mutations.send(M::SetProviderRetry(published)).await;
                            // Transport setback, not a workflow phase: the
                            // countdown rides the dedicated clause channel.
                            mutations
                                .send(M::SetPhase(Some(Phase::AwaitingModel)))
                                .await;
                            mutations.send(M::SetResponding(true)).await;

                            // The receipt: the setback is retained in the
                            // notice log (a viewport the user can open,
                            // scroll and copy from), never in the
                            // conversation. One line per scheduled attempt,
                            // tagged with the route that failed.
                            let (provider, model) = attribution!();
                            let summary = if provider.is_empty() {
                                format!("{receipt} — {fault}")
                            } else if model.is_empty() {
                                format!("{receipt} — {provider}: {fault}")
                            } else {
                                format!("{receipt} — {provider}/{model}: {fault}")
                            };
                            mutations
                                    .send(M::NoticeDisclosure(
                                        crate::shell::app::notice_catalog::ClientNotice::ProviderRetryScheduled {
                                            summary,
                                            // The detail is the fault verbatim so
                                            // the provider's full body stays
                                            // readable and pasteable in the viewer
                                            // now that the transcript no longer
                                            // carries it.
                                            detail: nuo_view::model::notice_text::display_text(
                                                &message,
                                            ),
                                        }
                                        .render(),
                                    ))
                                    .await;
                        }
                        RoundEvent::Error(e) => {
                            let attempts = retry_progress
                                .as_ref()
                                .map_or(0, nuo_view::retry::RetryProgress::retries);
                            retry_progress = None;
                            // A terminal round error may still carry the raw
                            // retryable-envelope encoding: strip it so the
                            // user sees the message, never the wire framing.
                            let message = if attempts > 1 {
                                if e.starts_with("Failed after") || e.starts_with("Exhausted") {
                                    e
                                } else {
                                    format!("Exhausted {attempts} retry attempts — {e}")
                                }
                            } else {
                                e
                            };
                            // A round-level failure is a turn outcome: it
                            // holds a position in time (the round ended
                            // here) but says nothing to the model.
                            //
                            // ADR-0166: a terminal error ends the round just
                            // as a completion does (`RoundInterrupted` is not
                            // emitted for `reason == Error`), so it is the
                            // boundary the provisional-row sweep must read.
                            // `RoundEvent::Error` is only ever a terminal
                            // round error or a pre-round admission refusal —
                            // never an error while a round continues.
                            transcript!(E::RetractPendingToolSteps);
                            let notice = TranscriptMessage::notice(NoticeSeverity::Error, message)
                                .with_sent_at_ms(now_ms!());
                            transcript!(E::Append {
                                class: nuo_protocol::EventClass::TurnOutcome,
                                message: notice
                            });

                            mutations.send(M::SetResponding(false)).await;
                            mutations.send(M::SetPhase(None)).await;

                            chrome!(event_loop::mutations::ChromeEdit::RoundEnded);
                        }
                        RoundEvent::BackgroundJobStarted(info) => {
                            let label = match &info.spec {
                                nuo_protocol::JobSpec::Process { command, label, .. } => {
                                    label.as_deref().unwrap_or(command)
                                }
                                nuo_protocol::JobSpec::Timer { label, prompt, .. } => {
                                    label.as_deref().unwrap_or(prompt.as_str())
                                }
                            };
                            mutations
                                .send(M::BackgroundTaskStarted {
                                    id: info.id.0.clone(),
                                    label: label.to_string(),
                                    started_at_ms: info.created_at_ms,
                                })
                                .await;
                            let message = TranscriptMessage::notice(
                                NoticeSeverity::Info,
                                format!("Background job started: {label} ({})", info.id.0),
                            )
                            .with_sent_at_ms(now_ms!());
                            transcript!(E::Append {
                                class: nuo_protocol::EventClass::TurnOutcome,
                                message
                            });
                        }
                        RoundEvent::BackgroundJobProgress { .. } => {}
                        RoundEvent::BackgroundJobReady { job_id } => {
                            // A service task reported readiness.
                            let message = TranscriptMessage::notice(
                                NoticeSeverity::Info,
                                format!("Background service ready: {}", job_id.0),
                            )
                            .with_sent_at_ms(now_ms!());
                            transcript!(E::Append {
                                class: nuo_protocol::EventClass::TurnOutcome,
                                message
                            });
                        }
                        RoundEvent::BackgroundJobCompleted(outcome) => {
                            let (label, is_success, exit_code, duration_secs) = match &outcome.state
                            {
                                nuo_protocol::JobState::Succeeded { duration_ms, .. } => (
                                    format!(
                                        "Background job `{}` completed ({}s)",
                                        outcome.job_id.0,
                                        duration_ms / 1000
                                    ),
                                    true,
                                    Some(0),
                                    duration_ms / 1000,
                                ),
                                nuo_protocol::JobState::Failed {
                                    duration_ms,
                                    exit_code,
                                    ..
                                } => (
                                    format!(
                                        "Background job `{}` failed (exit {exit_code}, {}s)",
                                        outcome.job_id.0,
                                        duration_ms / 1000
                                    ),
                                    false,
                                    Some(*exit_code),
                                    duration_ms / 1000,
                                ),
                                nuo_protocol::JobState::Killed { duration_ms } => (
                                    format!(
                                        "Background job `{}` terminated ({}s)",
                                        outcome.job_id.0,
                                        duration_ms / 1000
                                    ),
                                    false,
                                    None,
                                    duration_ms / 1000,
                                ),
                                nuo_protocol::JobState::TimedOut { duration_ms } => (
                                    format!(
                                        "Background job `{}` timed out ({}s)",
                                        outcome.job_id.0,
                                        duration_ms / 1000
                                    ),
                                    false,
                                    None,
                                    duration_ms / 1000,
                                ),
                                _ => (
                                    format!("Background job `{}` completed", outcome.job_id.0),
                                    true,
                                    None,
                                    0,
                                ),
                            };
                            mutations
                                .send(M::BackgroundTaskCompleted {
                                    id: outcome.job_id.0.clone(),
                                    success: is_success,
                                    exit_code,
                                    duration_secs,
                                })
                                .await;
                            let severity = if is_success {
                                NoticeSeverity::Info
                            } else {
                                NoticeSeverity::Warning
                            };
                            let message = TranscriptMessage::notice(severity, label)
                                .with_sent_at_ms(now_ms!());
                            transcript!(E::Append {
                                class: nuo_protocol::EventClass::TurnOutcome,
                                message
                            });
                        }
                    } // end inner `match event`
                }
                AgentResponse::InputHistory(rows) => {
                    mutations.send(M::InputHistory(rows)).await;
                }
                // Dashboard Archivist search hits. The TUI's
                // Archivist pane lands in a later stage; for now the
                // response is acknowledged (logged) so the wire stays
                // exhaustive without inventing premature UI state.
                AgentResponse::HistorySearch(hits) => {
                    tracing::debug!(count = hits.len(), "history search hits");
                }
                AgentResponse::RouteSettings {
                    provider_id,
                    model,
                    overrides,
                } => {
                    mutations
                        .send(M::RouteSettings {
                            provider_id,
                            model,
                            overrides,
                        })
                        .await;
                }
                AgentResponse::PermissionsCleared => {
                    mutations.send(M::ClearPermissions).await;
                    mutations.send(M::SetPhase(None)).await;
                }
                AgentResponse::ProviderKeys(status) => {
                    mutations
                        .send(M::KeyStatus(status.into_iter().collect()))
                        .await;
                }
                AgentResponse::ProviderPicker(snapshot) => {
                    picker = snapshot.clone();
                    mutations.send(M::ProviderPicker(snapshot)).await;
                }
                AgentResponse::ConversationCleared {
                    previous_id,
                    thread_id: session_id,
                } => {
                    // ADR-0166: the buffer is being replaced, so the session
                    // it just left can no longer write into it; the session
                    // it is entering is current again.
                    session_gate.supersede(&previous_id, &session_id);
                    // The left session's terminal frames are now dropped by
                    // the gate, so its per-round translator state must not
                    // leak into the new session's first turn.
                    retry_progress = None;
                    reasoning_start = None;
                    mutations
                        .send(M::Transcript {
                            edit: event_loop::mutations::TranscriptEdit::Clear,
                        })
                        .await;
                    mutations.send(M::SetRoundCount(0)).await;
                    needs_round_rebase = false;
                    mutations.send(M::ClearContextTokens).await;
                    // `/new` minted a fresh thread and switched to it: rebase
                    // **the tab that represented `previous_id`** — never "the
                    // active tab" (ADR-0123 `[INV-TAB-13]`). The two can differ
                    // whenever a thread switch raced this frame, and rebasing
                    // the wrong one manufactures a tab pointing at a thread
                    // that does not exist.
                    mutations
                        .send(M::LiveSession {
                            previous_id,
                            next_id: session_id.clone(),
                        })
                        .await;
                    live_session = session_id.clone();
                    mutations.send(M::TokenReport(None)).await;
                    mutations
                        .send(M::SessionTree(nuo_protocol::ThreadTree::default()))
                        .await;
                }
                AgentResponse::ConversationReplaced {
                    previous_id,
                    thread_id: session_id,
                    messages,
                    commands,
                    round_interrupts,
                } => {
                    // ADR-0166: see `ConversationCleared` — the replaced
                    // buffer must never receive the superseded session's
                    // trailing frames, nor inherit its per-round state.
                    session_gate.supersede(&previous_id, &session_id);
                    retry_progress = None;
                    reasoning_start = None;
                    mutations.send(M::ClearSwitchingSession).await;
                    let mut rebuilt = transcript_messages_from_core(messages, &tui_config_clone);
                    rebuilt =
                        merge_command_rows(rebuilt, transcript_commands_from_ledger(commands));
                    rebuilt = merge_round_interrupt_rows(
                        rebuilt,
                        transcript_interrupts_from_records(round_interrupts),
                    );
                    mutations
                        .send(M::Transcript {
                            edit: event_loop::mutations::TranscriptEdit::ReplaceAll {
                                messages: rebuilt,
                            },
                        })
                        .await;
                    needs_round_rebase = true;
                    // The model-window revision changed; do not reuse an API
                    // anchor from the previous thread/projection.
                    mutations.send(M::ClearContextTokens).await;
                    // Track the new id so thread-scoped client state follows,
                    // and drop the previous thread's cached report. Addressed
                    // by `previous_id`, exactly as `ConversationCleared` is
                    // (ADR-0123 `[INV-TAB-13]`).
                    mutations
                        .send(M::LiveSession {
                            previous_id,
                            next_id: session_id.clone(),
                        })
                        .await;
                    live_session = session_id;
                    mutations.send(M::TokenReport(None)).await;
                    mutations
                        .send(M::SessionTree(nuo_protocol::ThreadTree::default()))
                        .await;
                }
                AgentResponse::ThreadsOverview(sessions) => {
                    mutations.send(M::SessionsOverview(sessions)).await;
                }
                AgentResponse::OpenThreadsPanel => {
                    mutations.send(M::OpenSessionsPanel).await;
                }
                AgentResponse::ThreadTreeSnapshot {
                    thread_id: session_id,
                    tree,
                } => {
                    // A tree query is thread-scoped: reject a reply that
                    // raced a primary-thread switch.
                    if live_session == session_id {
                        mutations.send(M::SessionTree(tree)).await;
                    }
                }
                AgentResponse::OpenTreePanel => {
                    mutations.send(M::OpenTreePanel).await;
                }
                AgentResponse::ThreadForked {
                    child_id,
                    parent_id,
                    fork_point,
                    prompt,
                } => {
                    mutations
                        .send(M::SessionForked {
                            child_id,
                            parent_id,
                            fork_point,
                            prompt,
                        })
                        .await;
                }
                AgentResponse::ThreadDetail(detail) => {
                    mutations.send(M::SessionDetail(detail)).await;
                }
                AgentResponse::ConnectionDetail(detail) => {
                    mutations.send(M::ConnectionDetail(detail)).await;
                }
                AgentResponse::TokenUsageReport {
                    thread_id: session_id,
                    report,
                } => {
                    // Install the report only when it still belongs to the
                    // thread the frontend is viewing — a reply that raced a
                    // thread switch would populate the modal with the
                    // previous thread's rows.
                    let viewed = viewed_session_id_clone.lock().await.clone();
                    if viewed.as_deref() == Some(session_id.as_str()) {
                        mutations.send(M::TokenReport(Some(report))).await;
                    }
                }
                AgentResponse::UsageStatsReport { report } => {
                    // Thread-independent by design.
                    mutations.send(M::UsageStats(report)).await;
                }
                AgentResponse::ProviderQuotas(snapshot) => {
                    mutations.send(M::ProviderQuotas(snapshot)).await;
                }
                AgentResponse::ComposerCompletions {
                    request_id,
                    text,
                    cursor,
                    items,
                } => {
                    mutations
                        .send(M::CompletionSignal(
                            event_loop::mutations::CompletionSignal {
                                request_id,
                                input: text,
                                cursor,
                                items,
                            },
                        ))
                        .await;
                }
                AgentResponse::ThreadContext(snapshot) => {
                    mutations.send(M::SessionContext(snapshot)).await;
                }
                AgentResponse::Exit => {
                    mutations.send(M::Quit).await;
                }
                AgentResponse::ProviderSwitched { provider, model } => {
                    // Refreshes the hint bar; NOT appended to the transcript
                    // (the acknowledgment is a command ack).
                    current_provider = provider.clone();
                    current_model = model.clone();
                    mutations
                        .send(M::ProviderSwitched { provider, model })
                        .await;
                }
                AgentResponse::ConnectStatus(status) => {
                    match status {
                        nuo_protocol::ConnectStatus::Pending {
                            url,
                            user_code,
                            message,
                            ..
                        } => {
                            if !url.is_empty() {
                                let url_for_open = url.clone();
                                tokio::task::spawn_blocking(move || {
                                    if let Err(err) =
                                        crate::shell::browser::open_browser(&url_for_open)
                                    {
                                        tracing::warn!("Failed to open browser: {err}");
                                    }
                                });
                            }
                            mutations
                                .send(M::Oauth(crate::shell::app::OauthAddSignal::Pending {
                                    url: url.clone(),
                                    user_code: user_code.clone(),
                                    message: message.clone(),
                                }))
                                .await;
                            // The add-flow surfaces the URL/code in the
                            // OauthPending modal, so suppress the transcript
                            // notice there. Only the reconnect flow gets it.
                            let in_add_flow = awaiting_oauth_add_clone.load(Ordering::SeqCst);
                            if !in_add_flow {
                                mutations
                                        .send(M::NoticeDisclosure(
                                            crate::shell::app::notice_catalog::ClientNotice::OauthPending {
                                                message: message.clone(),
                                                url: url.clone(),
                                                user_code: user_code.clone(),
                                            }
                                            .render(),
                                        ))
                                        .await;
                            }
                        }
                        nuo_protocol::ConnectStatus::Done { provider } => {
                            mutations
                                .send(M::Oauth(crate::shell::app::OauthAddSignal::Done))
                                .await;
                            mutations
                                    .send(M::NoticeDisclosure(
                                        crate::shell::app::notice_catalog::ClientNotice::OauthAuthorized {
                                            provider,
                                        }
                                        .render(),
                                    ))
                                    .await;
                        }
                        nuo_protocol::ConnectStatus::CatalogSyncWarning {
                            provider,
                            message,
                            kind,
                        } => {
                            // A refusal is actionable and permanent (the
                            // account may not use this provider); a
                            // transient failure is not. Say which.
                            let refused = matches!(kind, nuo_protocol::CatalogSyncFailure::Refused);
                            mutations
                                    .send(M::NoticeDisclosure(
                                        crate::shell::app::notice_catalog::ClientNotice::CatalogSyncWarning {
                                            provider,
                                            message,
                                            refused,
                                        }
                                        .render(),
                                    ))
                                    .await;
                        }
                        nuo_protocol::ConnectStatus::Failed { provider, message } => {
                            mutations
                                .send(M::Oauth(crate::shell::app::OauthAddSignal::Failed {
                                    message: message.clone(),
                                }))
                                .await;
                            mutations
                                    .send(M::NoticeDisclosure(
                                        crate::shell::app::notice_catalog::ClientNotice::OauthConnectFailed {
                                            provider,
                                            message,
                                        }
                                        .render(),
                                    ))
                                    .await;
                        }
                    }
                }
                AgentResponse::Error(msg) => {
                    mutations.send(M::ClearSwitchingSession).await;
                    // A control-plane error is a receipt: the toast
                    // surface carries it, the conversation does not.
                    mutations
                        .send(M::NoticeDisclosure(
                            crate::shell::app::notice_catalog::ClientNotice::LocalFault {
                                topic: crate::shell::app::notice_catalog::ClientTopic::Client,
                                message: msg,
                            }
                            .render(),
                        ))
                        .await;
                }
                AgentResponse::TuiLayoutUpdated(_value) => {
                    // The apply path already set `app.transcript_layout`
                    // optimistically; a save failure surfaces as `Error`.
                }
                AgentResponse::TuiColorSchemeUpdated { .. } => {
                    // Applied optimistically; save failures arrive as `Error`.
                }
                AgentResponse::WebSearchConfigSnapshot(snapshot) => {
                    mutations.send(M::WebSearchConfig(Some(snapshot))).await;
                }
                AgentResponse::WebSearchConfigUpdated(snapshot) => {
                    // Authoritative post-update ack: re-render from persisted
                    // state, discarding any optimistic local edit.
                    mutations.send(M::WebSearchConfig(Some(snapshot))).await;
                }
                AgentResponse::CopyToClipboard { text } => {
                    let _ = crate::shell::clipboard::copy(&text).await;
                }
                AgentResponse::ServerEndpointsStatus(report) => {
                    mutations.send(M::ServerEndpoints(report)).await;
                }
                AgentResponse::ThreadSnapshot(snapshot) => {
                    let sid = snapshot.thread_id.clone();
                    // ADR-0166: a hydration frame names the session the
                    // buffer now holds. It cannot be a superseded one, and it
                    // repoints the live id that gates thread-scoped replies.
                    session_gate.hydrate(&sid);
                    live_session = sid.clone();
                    // `[INV-ATTACH-04]` (ADR-0133): this frame is the
                    // authoritative hydration payload. Rebuild the whole
                    // visible transcript from it (and the command ledger),
                    // exactly like `ConversationReplaced`, but addressed to
                    // the snapshot's own thread id.
                    let mut rebuilt =
                        transcript_messages_from_core(snapshot.messages, &tui_config_clone);
                    rebuilt = merge_command_rows(
                        rebuilt,
                        transcript_commands_from_ledger(snapshot.commands),
                    );
                    rebuilt = merge_round_interrupt_rows(
                        rebuilt,
                        transcript_interrupts_from_records(snapshot.round_interrupts),
                    );
                    mutations
                        .send(M::Transcript {
                            edit: event_loop::mutations::TranscriptEdit::ReplaceAll {
                                messages: rebuilt,
                            },
                        })
                        .await;
                    mutations
                        .send(M::SetRoundCount(snapshot.round_counter))
                        .await;
                    mutations.send(M::ClearContextTokens).await;
                    needs_round_rebase = true;
                    if let Some(picker) = snapshot.provider_picker {
                        mutations.send(M::ProviderPicker(picker)).await;
                    }
                    if let Some(keys) = snapshot.provider_keys {
                        mutations
                            .send(M::KeyStatus(keys.into_iter().collect()))
                            .await;
                    }
                    if let Some(ct) = snapshot.context_tokens {
                        mutations
                            .send(M::ContextTokens {
                                session_id: sid,
                                snapshot: ct,
                            })
                            .await;
                    }
                    if let Some(harness) = snapshot.harness_state {
                        mutations.send(M::Harness(harness)).await;
                    }
                    if let Some(catalog) = snapshot.command_catalog {
                        mutations.send(M::CommandCatalog(catalog)).await;
                    }
                    mutations
                        .send(M::ProviderSwitched {
                            provider: snapshot.provider,
                            model: snapshot.model,
                        })
                        .await;
                }
                AgentResponse::ThreadPhase(update) => {
                    mutations.send(M::SessionPhase(update.phase)).await;
                }
                AgentResponse::StateDelta(delta) => {
                    if let (Some(provider), Some(model)) = (delta.provider, delta.model) {
                        mutations
                            .send(M::ProviderSwitched { provider, model })
                            .await;
                    }
                    if let Some(ct) = delta.context_tokens {
                        mutations
                            .send(M::ContextTokens {
                                session_id: delta.thread_id,
                                snapshot: ct,
                            })
                            .await;
                    }
                }
            }
        }
        // The response stream ended. Only a *transport* failure is a
        // fault: the reader records that case with its real error, so the
        // disclosure names what actually happened instead of guessing
        // (ADR-0136). A clean close is a server instruction — `Exit`
        // already delivered that, and inventing a fault here would
        // contradict it.
        let transport_failure = link_lost_for_reader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(reason) = transport_failure {
            mutations
                .send(event_loop::mutations::AppMutation::LinkLost { reason })
                .await;
        }
    });
}
