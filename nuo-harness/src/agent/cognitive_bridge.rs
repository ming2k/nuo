//! Cognitive runtime bridge connecting [`crate::Agent`] (governance harness)
//! to [`nuo_agent::Agent`] (canonical cognitive core) per ADR-0010.

use crate::agent::Agent;
use nuo_agent::session::SessionEvent;
use nuo_wire::AgentEvent;
use tokio::sync::mpsc;

/// Translates a canonical [`SessionEvent`] from `nuo-agent` into wire-level
/// [`AgentEvent`]s for frontends (TUI/Server).
pub fn session_event_to_agent_events(event: SessionEvent) -> Vec<AgentEvent> {
    match event {
        SessionEvent::RoundStarted { round } => {
            vec![AgentEvent::ModelRequestStarted {
                round: round as u64,
                turn: (round.saturating_sub(1)) as usize,
                context_tokens: 0,
            }]
        }
        SessionEvent::ContentDelta { delta } => {
            vec![AgentEvent::AssistantDelta {
                delta,
                start: false,
            }]
        }
        SessionEvent::ThinkingDelta { delta } => {
            vec![AgentEvent::ReasoningDelta {
                delta,
                start: false,
            }]
        }
        SessionEvent::ToolCallStarted {
            call_id,
            name,
            arguments,
        } => {
            vec![AgentEvent::ToolCall {
                id: call_id,
                name,
                arguments: arguments.to_string(),
            }]
        }
        SessionEvent::ToolCallFinished {
            call_id,
            name,
            output,
            is_error: _,
        } => {
            let structured = nuo_tool::ToolOutput::success(&output);
            vec![AgentEvent::ToolResult {
                id: call_id,
                name,
                output,
                structured,
                duration_ms: 0,
            }]
        }
        SessionEvent::Done { final_content, .. } => {
            vec![AgentEvent::AssistantEnd(final_content)]
        }
        _ => Vec::new(),
    }
}

impl Agent {
    /// Executes a streaming cognitive prompt through the canonical [`nuo_agent::Agent`]
    /// core, projecting streaming [`SessionEvent`]s into the harness's [`AgentEvent`] channel.
    pub async fn run_cognitive_prompt(
        &self,
        prompt: impl Into<String>,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<String, nuo_agent::AgentError> {
        let cognitive_agent = self.to_cognitive_agent().await?;
        let (session_tx, mut session_rx) = mpsc::channel::<SessionEvent>(128);

        let forward_handle = tokio::spawn(async move {
            while let Some(se) = session_rx.recv().await {
                for ae in session_event_to_agent_events(se) {
                    if event_tx.send(ae).await.is_err() {
                        break;
                    }
                }
            }
        });

        let result = cognitive_agent.prompt_streaming(prompt, session_tx).await;
        let _ = forward_handle.await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_translation() {
        let events = session_event_to_agent_events(SessionEvent::ContentDelta {
            delta: "hello world".into(),
        });
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::AssistantDelta { delta, .. } => assert_eq!(delta, "hello world"),
            _ => panic!("unexpected event variant"),
        }
    }
}
