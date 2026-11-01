//! Agent-context helpers extracted from `main.rs`: resolving the active
//! model's context window and re-seeding the mid-turn prune threshold after a
//! provider/model switch. Pure reads of the live [`Agent`] + [`Config`].

use nuo_harness::Agent;
use nuo_persistence::config::Config;
use nuo_persistence::session::SessionStore;
use std::sync::Arc;

/// Resolve the active channel's context window (tokens) from the live provider.
/// A trusted remote catalogue wins over the static model baseline; `0` means
/// neither source knows the window and the compaction policy substitutes its
/// conservative fallback at resolve time.
pub fn active_context_window(agent: &Agent) -> usize {
    agent.provider.model_capabilities().context_window
}

/// Re-seed the mid-turn prune threshold from the active model's context window.
/// Called at startup and after every provider/model switch so mid-turn relief
/// tracks the live model instead of a frozen, model-agnostic budget. A no-op
/// when pruning is disabled (no gate is installed in that case).
pub fn reseed_prune_threshold(agent: &Agent, config: &Config) {
    if !config.context.lightweight_degradation_enabled {
        return;
    }
    let window = active_context_window(agent);
    let budget = config.context.resolve_budget(window);
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
    session: &Arc<SessionStore>,
    config: &Config,
    settings: nuo_harness::SettingsForWindow,
) {
    if !config.context.lightweight_degradation_enabled {
        // Gate removed, not merely idle: a stale gate would project against a
        // window it no longer tracks.
        agent.set_context_projection_gate(None);
        return;
    }
    agent.set_context_projection_gate(Some(Arc::new(nuo_harness::PruningProjectionGate::new(
        Arc::clone(session),
        settings,
        agent.token_weights_handle(),
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

/// Re-seed the model-specific behavioral guidance so the system prompt always
/// tracks the active route, glob patterns, and model family (ADR-0130). Called at
/// startup and after every provider/model switch.
pub fn reseed_model_guidance(agent: &Agent, config: &Config) {
    let model = agent.provider.model();
    let provider_id = agent.provider.provider_id();
    let family = agent.provider.model_capabilities().family;
    let store = nuo_persistence::route_settings::RouteSettingsStore::load();
    let route = store.settings_for(&provider_id, &model);
    let guidance = config.resolve_model_guidance(route, &model, &family);
    agent.set_model_guidance(guidance);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn reseed_model_guidance_populates_agent_from_config() {
        let provider = Arc::new(nuo_harness::NoProvider);
        let agent = Agent::new(provider, Vec::new(), nuo_protocol::AgentIdentity::default());

        let mut config = Config::default();
        config.model_guidance.0.insert(
            "*none*".to_string(),
            "Prompt engineering: Speak plain Chinese.".to_string(),
        );

        assert_eq!(agent.model_guidance(), "");
        reseed_model_guidance(&agent, &config);
        assert_eq!(
            agent.model_guidance(),
            "Prompt engineering: Speak plain Chinese."
        );
    }
}
