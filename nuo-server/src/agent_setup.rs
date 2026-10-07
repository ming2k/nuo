//! Agent-context helpers extracted from `main.rs`: resolving the active
//! model's context window and re-seeding the mid-turn prune threshold after a
//! provider/model switch. Pure reads of the live [`Agent`] + [`Config`].

use nuo_harness::Agent;
use nuo_persistence::config::Config;

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

/// Re-seed the per-model tool-variant selection so the resolved toolset (and
/// the schemas sent to the provider) always track the live model. Called at
/// startup and after every provider/model switch. A model with no
/// `[tool_variants.<id>]` entry gets an empty map, realizing every capability
/// with its default variant.
pub fn reseed_tool_variants(agent: &Agent, config: &Config) {
    let model = agent.provider.model();
    agent.set_variant_selection(config.tool_variants.for_model(&model).clone());
}
