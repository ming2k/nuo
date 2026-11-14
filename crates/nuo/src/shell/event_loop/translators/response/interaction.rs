//! Human-in-the-loop and subagent event handling.

use nuo_protocol::RoundEvent;
use nuo_view::phase::Phase;

use super::{E, M, TranslatorState};

impl TranslatorState {
    /// Human-in-the-loop requests and nested subagent events.
    pub(super) async fn handle_interaction_event(&mut self, event: RoundEvent) {
        match event {
            RoundEvent::SubagentStep {
                parent_call_id,
                event,
            } => {
                // Full-duplex (ADR-0029): a subagent's permission
                // broker or `ask_user` request bubbles up nested
                // under this `parent_call_id`; the reply is tagged
                // for down-routing into the child.
                match &event {
                    nuo_tool::SubagentEvent::PermissionRequest(req) => {
                        self.mutations
                            .send(M::QueuePermission {
                                request: req.clone(),
                                parent_call_id: Some(parent_call_id.clone()),
                            })
                            .await;

                        self.mutations
                            .send(M::SetPhase(Some(Phase::AwaitingUser)))
                            .await;
                        self.mutations.send(M::SetResponding(true)).await;
                    }
                    nuo_tool::SubagentEvent::UserQuestionRequest(req) => {
                        self.mutations
                            .send(M::QueueQuestion {
                                request: req.clone(),
                                parent_call_id: Some(parent_call_id.clone()),
                            })
                            .await;

                        self.mutations
                            .send(M::SetPhase(Some(Phase::AwaitingUser)))
                            .await;
                        self.mutations.send(M::SetResponding(true)).await;
                    }
                    _ => {}
                }
                self.transcript(E::SubagentEvent {
                    parent_call_id,
                    event,
                })
                .await;
            }
            RoundEvent::PermissionRequest(request) => {
                // Stays global regardless of session so the modal
                // always surfaces (ADR-0017 → ADR-0058).
                self.mutations
                    .send(M::QueuePermission {
                        request,
                        parent_call_id: None,
                    })
                    .await;

                self.mutations
                    .send(M::SetPhase(Some(Phase::AwaitingUser)))
                    .await;
                self.mutations.send(M::SetResponding(true)).await;
            }
            RoundEvent::UserQuestionRequest(request) => {
                self.mutations
                    .send(M::QueueQuestion {
                        request,
                        parent_call_id: None,
                    })
                    .await;

                self.mutations
                    .send(M::SetPhase(Some(Phase::AwaitingUser)))
                    .await;
                self.mutations.send(M::SetResponding(true)).await;
            }
            RoundEvent::StdinRequest(request) => {
                self.mutations.send(M::QueueInput(request)).await;

                self.mutations
                    .send(M::SetPhase(Some(Phase::AwaitingUser)))
                    .await;
                self.mutations.send(M::SetResponding(true)).await;
            }
            _ => {}
        }
    }
}
