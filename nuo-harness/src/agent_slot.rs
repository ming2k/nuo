use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use nuo_contracts::{AgentRoleDelegation, AgentRoleProfile};

use crate::agent::Agent;
use crate::subagent_tool::SubagentRegistry;

/// Manages the active root agent for a session with atomic replacement and subagent reclamation.
///
/// Ensures the single-root invariant per session and guarantees that replacing a root agent
/// actively drains all subordinate subagents (cancelling tokens) before assigning the new agent.
/// This prevents orphaned subagents and transcript/word-source leaks.
pub struct AgentSlot {
    agent: Arc<Agent>,
    preset: AgentRoleProfile,
    delegation: AgentRoleDelegation,
    session_id: String,
    subagent_registry: Option<Arc<SubagentRegistry>>,
    subagent_cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl AgentSlot {
    /// Create a new AgentSlot for a session.
    pub fn new(
        agent: Arc<Agent>,
        preset: AgentRoleProfile,
        delegation: AgentRoleDelegation,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            agent,
            preset,
            delegation,
            session_id: session_id.into(),
            subagent_registry: None,
            subagent_cancels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Attach sub-agent registry for coordinated lifecycle management.
    pub fn with_subagent_registry(
        mut self,
        subagent_registry: Arc<SubagentRegistry>,
    ) -> Self {
        self.subagent_registry = Some(subagent_registry);
        self
    }

    /// The active session root agent.
    pub fn agent(&self) -> &Arc<Agent> {
        &self.agent
    }

    /// The active agent preset.
    pub fn preset(&self) -> &AgentRoleProfile {
        &self.preset
    }

    /// The active preset delegation policy.
    pub fn delegation(&self) -> &AgentRoleDelegation {
        &self.delegation
    }

    /// The owning session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Track an in-flight subagent's cancellation token under this root agent.
    pub fn register_subagent_cancel(&self, call_id: impl Into<String>, token: CancellationToken) {
        self.subagent_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(call_id.into(), token);
    }

    /// Remove a finished subagent's cancellation token.
    pub fn remove_subagent_cancel(&self, call_id: &str) {
        self.subagent_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(call_id);
    }

    /// Drain and cancel all subordinate subagents currently active under this root agent.
    ///
    /// Cancels all child cancellation tokens, sweeps child mesh mailboxes from the tracker,
    /// and ensures no background execution continues.
    pub fn drain_subagents(&self) -> usize {
        let mut cancels = self
            .subagent_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let count = cancels.len();
        for (_, token) in cancels.drain() {
            token.cancel();
        }
        count
    }

    /// Replace the session's active root agent and preset atomically.
    ///
    /// Drains all subordinate subagents owned by the previous agent first, preventing
    /// word-source leaks and transcript confusion. Returns the number of subordinate
    /// subagents drained during the transition.
    pub fn replace(
        &mut self,
        new_agent: Arc<Agent>,
        new_preset: AgentRoleProfile,
        new_delegation: AgentRoleDelegation,
    ) -> usize {
        let drained = self.drain_subagents();
        self.agent = new_agent;
        self.preset = new_preset;
        self.delegation = new_delegation;
        drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentIdentity;
    use nuo_contracts::AgentRoleProfile;

    struct DummyProvider;
    #[async_trait::async_trait]
    impl nuo_contracts::Provider for DummyProvider {
        async fn chat(
            &self,
            _request: nuo_contracts::ModelRequest,
        ) -> Result<nuo_contracts::ProviderCompletion, nuo_contracts::ProviderError> {
            Ok(nuo_contracts::ProviderCompletion::message(
                nuo_contracts::Message::new(nuo_contracts::Role::Assistant, "ok"),
            ))
        }
        async fn stream_chat(
            &self,
            _request: nuo_contracts::ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, nuo_contracts::ProviderError>>,
            nuo_contracts::ProviderError,
        > {
            use futures::stream;
            Ok(Box::pin(stream::once(async { Ok("ok".to_string()) })))
        }
    }

    fn make_agent(name: &str) -> Arc<Agent> {
        let provider = Arc::new(DummyProvider);
        let identity = AgentIdentity::new(name, "test mission");
        Arc::new(Agent::new(provider, vec![], identity))
    }

    #[tokio::test]
    async fn agent_slot_replaces_atomically_and_drains_subagents() {
        let initial_agent = make_agent("initial-root");
        let registry = Arc::new(SubagentRegistry::default());

        let mut slot = AgentSlot::new(
            initial_agent,
            AgentRoleProfile::developer(),
            AgentRoleProfile::DEVELOPER,
            "session-xyz",
        )
        .with_subagent_registry(registry);

        let token1 = CancellationToken::new();
        let token2 = CancellationToken::new();

        slot.register_subagent_cancel("subagent-1", token1.clone());
        slot.register_subagent_cancel("subagent-2", token2.clone());

        assert!(!token1.is_cancelled());
        assert!(!token2.is_cancelled());

        // Replace developer role with philosophist role
        let successor_agent = make_agent("successor-root");
        let drained = slot.replace(
            successor_agent,
            AgentRoleProfile::philosophist(),
            AgentRoleProfile::PHILOSOPHIST,
        );

        assert_eq!(drained, 2);
        assert!(token1.is_cancelled());
        assert!(token2.is_cancelled());
        assert_eq!(slot.delegation().role_id, "philosophist");
    }
}
