//! Tool call lifecycle event handling.

use nuo_protocol::RoundEvent;
use nuo_view::model::document::TranscriptMessage;
use nuo_view::phase::Phase;

use crate::shell::{event_loop, transcript_turn};

use super::{E, M, TranslatorState, TurnCtx};

impl TranslatorState {
    /// Tool call lifecycle: announcement, argument stream, dispatch, preview,
    /// result, cancellation and output stream.
    pub(super) async fn handle_tool_event(
        &mut self,
        session_id: String,
        event: RoundEvent,
        ctx: TurnCtx,
    ) {
        match event {
            RoundEvent::ToolCallStarted { id, name } => {
                // ADR-0026 / ADR-0166: the call is named but its
                // arguments are still streaming. Show a running
                // step and move the phase to the tool verb now,
                // instead of leaving the bar on the misleading
                // `answering` phase for the whole argument stream.
                // The step carries the call id the later
                // `ToolCall` will carry, so the applier merges
                // instead of inserting a second row.
                self.mutations
                    .send(M::SetPhase(Some(Phase::Tool(event_loop::tool_verb_for(
                        &name,
                    )))))
                    .await;
                self.mutations.send(M::SetResponding(true)).await;

                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::PhaseOnly(Some(Phase::Tool(
                        event_loop::tool_verb_for(&name),
                    ))),
                )
                .await;
                let (provider, model) = self.attribution();
                let effort = self.picker_effort();
                let Some(context) = ctx.turn_context else {
                    return;
                };
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
                self.transcript(E::ToolStepUpsert { message }).await;
            }
            RoundEvent::ToolInputProgress {
                id,
                bytes,
                arguments,
            } => {
                // ADR-0026 / ADR-0046: progress tick and streaming arguments.
                self.mutations.send(M::SetResponding(true)).await;

                self.transcript(E::ToolInputProgress {
                    id,
                    bytes,
                    arguments,
                })
                .await;
            }
            RoundEvent::ToolCallsRetracted { ids } => {
                // The attempt that announced these calls died
                // before dispatch. Drop the provisional rows;
                // they never described a call, so a terminal
                // "cancelled" row would be a lie.
                self.transcript(E::ToolCallsRetracted { ids }).await;
            }
            RoundEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                self.mutations
                    .send(M::SetPhase(Some(Phase::Tool(event_loop::tool_verb_for(
                        &name,
                    )))))
                    .await;

                let (provider, model) = self.attribution();
                let effort = self.picker_effort();
                self.retry_progress = None;
                let Some(context) = ctx.turn_context else {
                    return;
                };
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
                self.transcript(E::ToolStepUpsert { message }).await;

                self.mutations.send(M::SetResponding(true)).await;
            }
            RoundEvent::ToolPreview { id, preview, .. } => {
                // ADR-0125: the preview arrives strictly after the
                // `ToolCall` that created this step, so matching by
                // dispatch id finds it. No phase change and no
                // responding toggle: nothing is running yet, and the
                // card simply gains its diff.
                self.transcript(E::ToolPreview { id, preview }).await;
            }
            RoundEvent::ToolResult {
                id,
                name,
                output,
                structured,
                duration_ms,
            } => {
                self.mutations
                    .send(M::SetPhase(Some(Phase::Preparing)))
                    .await;

                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::PhaseOnly(Some(Phase::Preparing)),
                )
                .await;
                let (provider, model) = self.attribution();
                let Some(context) = ctx.turn_context else {
                    return;
                };
                let position = Some((context.round, transcript_turn(context.turn)));
                // The fallback (no matching in-flight call, e.g. a
                // turn restored from history) is fully finished by
                // the translator; the applier applies the
                // lifecycle-aware default disclosure.
                let mut fallback = TranscriptMessage::tool_step(id.clone(), name.clone(), "{}")
                    .with_attribution(provider, model);
                if let Some((round, turn)) = position {
                    fallback.round = Some(round);
                    fallback.turn = Some(turn);
                }
                fallback.finish_tool_step(&id, output.clone(), structured.clone(), duration_ms);
                self.transcript(E::ToolResult {
                    id,
                    name,
                    output,
                    structured,
                    duration_ms,
                    fallback: Some(fallback),
                })
                .await;
            }
            RoundEvent::ToolCancelled { id, .. } => {
                // An in-flight call was aborted: flip it (and any
                // nested subagent children) to Cancelled.
                let Some(context) = ctx.turn_context else {
                    return;
                };
                let position = Some((context.round, transcript_turn(context.turn)));
                let mut fallback = TranscriptMessage::tool_step(id.clone(), "tool", "{}");
                if let Some((round, turn)) = position {
                    fallback.round = Some(round);
                    fallback.turn = Some(turn);
                }
                fallback.cancel_tool_step(&id);
                fallback.set_tool_step_expanded(false);
                self.transcript(E::ToolCancel {
                    id,
                    fallback: Some(fallback),
                })
                .await;
            }
            RoundEvent::ToolStream { id, stream } => {
                self.transcript(E::ToolStream { id, stream }).await;
            }
            _ => {}
        }
    }
}
