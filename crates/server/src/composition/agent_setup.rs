//! Agent-context helpers extracted from `main.rs`: resolving the active
//! model's context window and re-seeding the mid-turn prune threshold after a
//! provider/model switch. Pure reads of the live [`Agent`] + [`Config`].

use engine::Agent;
use persistence::config::Config;
use persistence::thread::ThreadSession;
use std::sync::Arc;

/// Resolve the active channel's context window (tokens) from the live provider.
/// A trusted remote catalogue wins over the static model baseline; `0` means
/// neither source knows the window and the compaction policy substitutes its
/// conservative fallback at resolve time.
pub fn active_context_window(agent: &Agent) -> usize {
    agent.provider.model_capabilities().context_window
}

/// Re-seed the agent's model-relative context controls.
///
/// Called at startup and after every provider/model switch so the mid-turn prune
/// threshold tracks the live model's window instead of a frozen budget.
pub fn reseed_context_policy(agent: &Agent) {
    let window = active_context_window(agent);
    let budget = engine::orchestration::ContextProjectionSettings::for_window(window).budget;
    agent.set_context_prune_threshold(budget.prune_threshold_tokens);
}

/// Install the mid-turn projection gate for `session`.
///
/// `project_context_if_needed` asks a gate to relieve pressure between ReAct
/// turns, and nothing ever installed one — so long single rounds, including every
/// subagent (which runs one round to completion and never passes through
/// `execute_round`'s pre-round prune or compaction), had no relief valve.
///
/// The gate needs the session to archive what it clears, and the agent's token
/// weights so its re-estimates reuse the content-addressed cache instead of
/// re-tokenizing the window. `settings` is a closure, not a value: the thresholds
/// are model-relative and the active model can change at any time, so a captured
/// value would freeze the gate on the startup model's window.
pub fn install_projection_gate(
    agent: &Agent,
    session: &Arc<ThreadSession>,
    settings: engine::SettingsForWindow,
) {
    agent.set_context_projection_gate(Some(Arc::new(engine::PruningProjectionGate::new(
        Arc::clone(session),
        settings,
        agent.token_weights_handle(),
        Some(agent.provider.clone()),
    ))));
}

/// Re-seed the per-model tool-variant selection so the resolved toolset (and
/// the schemas sent to the provider) always track the live model. Called at
/// startup and after every provider/model switch. A model with no
/// `[tool_variants.<id>]` entry gets an empty map, realizing every capability
/// with its default variant.
pub fn reseed_tool_variants(agent: &Agent, config: &Config) {
    let model = agent.provider.model();
    agent.set_variant_selection(config.tool_variants.for_model(&model).clone());
}

/// Re-seed the user instruction source and the provider-type fact for the
/// active route (ADR-0165). Called at startup and after every provider/model
/// switch; prompt composition itself never reads the connections store.
///
/// Also seeds the declared answer language (ADR-0222). It is a session fact, not
/// a route fact, but it rides this function because this is the one place the
/// live config is folded into a live agent, and two seed paths for one prompt
/// input is how they drift.
pub fn reseed_instructions(agent: &Agent, config: &Config) {
    agent.set_user_instructions(config.instructions.scoped());
    agent.set_response_language(config.response_language.clone());
    let provider_id = agent.provider.provider_id();
    let provider_kind = persistence::connections::Connections::load()
        .get(&provider_id)
        .map(|connection| connection.provider.clone());
    agent.set_instruction_provider_kind(provider_kind);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn reseed_instructions_populates_agent_from_config() {
        let provider = Arc::new(engine::NoProvider);
        let agent = Agent::new(provider, Vec::new(), protocol::AgentIdentity::default());

        let mut config = Config::default();
        config
            .instructions
            .user
            .push(persistence::config::UserInstructionConfig {
                text: "Speak plain Chinese.".to_string(),
                ..Default::default()
            });

        reseed_instructions(&agent, &config);
        let mut messages = Vec::new();
        agent.prepare_request_messages_debug(&mut messages);
        assert!(
            messages[0].content.contains("Speak plain Chinese."),
            "{}",
            messages[0].content
        );
    }
}
