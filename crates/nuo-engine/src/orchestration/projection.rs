//! Context-projection budgets and the compaction report.

use super::*;

#[derive(Clone)]
pub struct ContextProjectionSettings {
    /// Token thresholds resolved against the active model's context window.
    /// Pressure (estimated in tokens) is compared against these to decide when
    /// to prune and when to run a full summarizing compaction.
    pub budget: nuo_protocol::ContextBudget,
    pub preserve_rounds: usize,
    /// Use the active model to produce an anchored structured summary.
    pub summarize: bool,
    /// Enable cheap tool-result pruning (pre-turn and mid-turn).
    pub prune: bool,
    /// Token budget of the most recent tool results protected from pruning
    /// (token-native; the config key was already tokens, the old
    /// `× CHARS_PER_TOKEN` conversion existed only to feed a char-space
    /// pruner).
    pub prune_protect_tokens: usize,
    /// Tokens the surrounding request (instructions, tool schemas, temporary
    /// context) adds on top of the window. The compaction gate is compared
    /// against the **request** estimate, so a fold must reserve these tokens or
    /// it can hand back a window that is still over the gate.
    pub request_overhead_tokens: usize,
}

impl ContextProjectionSettings {
    /// Mid-turn pruning only fires when it can reclaim at least this many
    /// tokens, to avoid pruning churn for negligible gains (Quantum Floor).
    pub const PRUNE_MIN_RECLAIM_TOKENS: usize = nuo_protocol::PRUNE_QUANTUM_FLOOR_TOKENS;

    /// Resolve settings for the active model's context window. `window_tokens`
    /// is the live model's context window (tokens); `0` means unknown and the
    /// policy's fallback window is substituted.
    /// Resolve settings from a modern versioned context policy.
    pub fn from_context_policy(
        policy: &nuo_protocol::context_policy::ContextPolicy,
        window_tokens: usize,
    ) -> Self {
        Self {
            budget: policy.resolve_budget(window_tokens),
            preserve_rounds: policy.preferred_recent_rounds as usize,
            summarize: policy.checkpoint_enabled,
            prune: policy.lightweight_degradation_enabled,
            prune_protect_tokens: policy.recent_observation_protect_tokens as usize,
            request_overhead_tokens: 0,
        }
    }

    /// Resolve settings from a compaction policy (historical compatibility).
    pub fn from_policy(policy: &nuo_protocol::CompactionPolicy, window_tokens: usize) -> Self {
        Self {
            budget: policy.resolve(window_tokens),
            preserve_rounds: policy.preserve_rounds,
            summarize: policy.summarize,
            prune: policy.prune,
            prune_protect_tokens: policy.prune_protect_tokens,
            request_overhead_tokens: 0,
        }
    }

    pub fn from_config(config: &Config, window_tokens: usize) -> Self {
        Self::from_context_policy(&config.context, window_tokens)
    }

    /// Adjust the post-compaction history target so the complete projected
    /// request — checkpoint, system prompt, injected context, and tool schemas
    /// — lands near `target_utilization`, not merely the durable transcript.
    pub fn for_request(&self, request: RequestTokenEstimate) -> Self {
        let mut resolved = self.clone();
        let floor = resolved.budget.target_tokens.clamp(1, 2_000);
        resolved.budget.target_tokens = resolved
            .budget
            .target_tokens
            .saturating_sub(request.overhead_tokens)
            .max(floor);
        resolved.request_overhead_tokens = request.overhead_tokens;
        resolved
    }
}

/// Outcome of one compaction pass.
///
/// A compaction that cannot bring the window under the gate is still better
/// than no compaction, but it must not be reported as a successful shrink.
/// `satisfied == false` is the explicit "insufficient" signal; the caller
/// surfaces it instead of silently re-triggering on the next send.
#[derive(Debug, Clone)]
pub struct CompactionReport {
    pub checkpoint: ContextProjectionCheckpoint,
    /// `window_after + request_overhead <= compaction_threshold`.
    pub satisfied: bool,
}

/// Tail and summary budgets that make the compaction post-condition reachable.
///
/// The summary is part of the window, and the surrounding request
/// (`instructions`, tool schemas, temporary context) is part of the estimate the
/// gate compares. Reserving both is what keeps a fold from landing *at* the
/// target but *above* the gate.
pub(super) fn compaction_budgets(settings: &ContextProjectionSettings) -> (usize, usize) {
    let gate = settings.budget.compaction_threshold_tokens;
    let summary_budget = settings.budget.target_tokens.clamp(2_000, 24_000);
    let tail_budget = gate
        .saturating_sub(settings.request_overhead_tokens)
        .saturating_sub(summary_budget)
        .max(1_000);
    (
        settings.budget.target_tokens.min(tail_budget).max(1),
        summary_budget,
    )
}
