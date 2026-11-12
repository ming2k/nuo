//! Translation of agent events into round events.

use super::*;

pub fn relay_agent_event(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    event: AgentEvent,
    streamed_text: &std::sync::atomic::AtomicBool,
    current_turn: &mut TurnContext,
) {
    let response = match event {
        AgentEvent::Notice(notice) => round_response(session_id, RoundEvent::Notice(notice)),
        AgentEvent::ModelRequestStarted {
            round,
            turn,
            context_tokens,
        } => {
            // The projection is session-scoped and was computed at the exact
            // pre-wire boundary, after hooks and request preparation.
            let _ = tx.send(round_response(
                session_id,
                RoundEvent::ContextTokens(nuo_protocol::ContextTokenSnapshot::new(
                    context_tokens,
                    nuo_protocol::ContextTokenSource::Projection,
                )),
            ));
            *current_turn = TurnContext::new(round, turn as u64, now_ms());
            // Structured turn signal first, so the Activity modal can show
            // `round N | turn M | waiting for model` with the turn as a
            // first-class field rather than text-mining it out of the status
            // string. The bare status follows as the `Activity` below.
            let _ = tx.send(turn_response(
                session_id,
                *current_turn,
                RoundEvent::TurnStarted,
            ));
            round_response(
                session_id,
                RoundEvent::Activity("waiting for model".to_string()),
            )
        }
        AgentEvent::TurnPerformance(performance) => {
            round_response(session_id, RoundEvent::TurnPerformance(performance))
        }
        AgentEvent::ContextTokens(snapshot) => {
            round_response(session_id, RoundEvent::ContextTokens(snapshot))
        }
        AgentEvent::SteerAdmitted { input, context } => {
            // The admitted message belongs to the turn that will consume it,
            // not to the turn that just finished. The agent supplies the
            // coordinate; the frontend never infers `turn + 1`.
            turn_response(session_id, context, RoundEvent::SteerAdmitted(input))
        }
        AgentEvent::AssistantDelta { delta, start } => {
            let context = *current_turn;
            if start {
                let _ = tx.send(turn_response(session_id, context, RoundEvent::StreamStart));
            }
            streamed_text.store(true, Ordering::SeqCst);
            turn_response(session_id, context, RoundEvent::StreamDelta(delta))
        }
        AgentEvent::AssistantEnd(content) => turn_response(
            session_id,
            *current_turn,
            RoundEvent::StreamEnd(content.trim().to_string()),
        ),
        AgentEvent::AssistantDiscard => {
            turn_response(session_id, *current_turn, RoundEvent::StreamDiscard)
        }
        AgentEvent::ReasoningDelta { delta, start } => {
            let context = *current_turn;
            if start {
                let _ = tx.send(turn_response(session_id, context, RoundEvent::StreamStart));
            }
            streamed_text.store(true, Ordering::SeqCst);
            turn_response(session_id, context, RoundEvent::StreamReasoningDelta(delta))
        }
        AgentEvent::ReasoningEnd(content) => turn_response(
            session_id,
            *current_turn,
            RoundEvent::StreamReasoningEnd(content),
        ),
        AgentEvent::ToolCall {
            id,
            name,
            arguments,
        } => turn_response(
            session_id,
            *current_turn,
            RoundEvent::ToolCall {
                id,
                name,
                arguments,
            },
        ),
        AgentEvent::ToolCallsRetracted { ids } => turn_response(
            session_id,
            *current_turn,
            RoundEvent::ToolCallsRetracted { ids },
        ),
        // The preview rides the same relay as the call it describes (ADR-0125).
        // It is classified `Ephemera` on the far side, so no frontend can persist
        // it into the transcript: a diff nothing applied must not survive a resume.
        AgentEvent::ToolPreview { id, name, preview } => turn_response(
            session_id,
            *current_turn,
            RoundEvent::ToolPreview { id, name, preview },
        ),
        AgentEvent::ToolResult {
            id,
            name,
            output,
            structured,
            duration_ms,
        } => turn_response(
            session_id,
            *current_turn,
            RoundEvent::ToolResult {
                id,
                name,
                output,
                structured,
                duration_ms,
            },
        ),
        AgentEvent::ToolCancelled { id, name } => turn_response(
            session_id,
            *current_turn,
            RoundEvent::ToolCancelled { id, name },
        ),
        AgentEvent::ToolStream { id, stream } => turn_response(
            session_id,
            *current_turn,
            RoundEvent::ToolStream { id, stream },
        ),
        AgentEvent::ToolCallStarted { id, name } => turn_response(
            session_id,
            *current_turn,
            RoundEvent::ToolCallStarted { id, name },
        ),
        AgentEvent::ToolInputProgress {
            id,
            bytes,
            arguments,
        } => turn_response(
            session_id,
            *current_turn,
            RoundEvent::ToolInputProgress {
                id,
                bytes,
                arguments,
            },
        ),
        AgentEvent::TodosUpdated(todos) => {
            round_response(session_id, RoundEvent::TodosUpdated(todos))
        }
        AgentEvent::UnattendedChanged(enabled) => {
            round_response(session_id, RoundEvent::UnattendedChanged(enabled))
        }
        AgentEvent::ConfinementChanged(enabled) => {
            round_response(session_id, RoundEvent::ConfinementChanged(enabled))
        }
        AgentEvent::PermissionRequest(request) => {
            round_response(session_id, RoundEvent::PermissionRequest(request))
        }
        AgentEvent::UserQuestionRequest(request) => {
            round_response(session_id, RoundEvent::UserQuestionRequest(request))
        }
        AgentEvent::StdinRequest(request) => {
            round_response(session_id, RoundEvent::StdinRequest(request))
        }
        AgentEvent::Subagent {
            parent_call_id,
            event,
        } => round_response(
            session_id,
            RoundEvent::SubagentStep {
                parent_call_id,
                event,
            },
        ),
        AgentEvent::BackgroundJobStarted(info) => {
            round_response(session_id, RoundEvent::BackgroundJobStarted(info))
        }
        AgentEvent::BackgroundJobProgress { job_id, line } => round_response(
            session_id,
            RoundEvent::BackgroundJobProgress { job_id, line },
        ),
        AgentEvent::BackgroundJobReady { job_id } => {
            round_response(session_id, RoundEvent::BackgroundJobReady { job_id })
        }
        AgentEvent::BackgroundJobCompleted(outcome) => {
            round_response(session_id, RoundEvent::BackgroundJobCompleted(outcome))
        }
        AgentEvent::CatalogInvalidated => {
            let config = nuo_persistence::config::Config::load();
            let usage = nuo_persistence::connection_usage::ConnectionUsage::load();
            let _ = tx.send(AgentResponse::ProviderPicker(
                nuo_model_routing::build_picker_state(&config, &usage),
            ));
            return;
        }
    };
    let _ = tx.send(response);
}
