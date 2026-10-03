//! Adaptive Context Budgeting and Pressure Control.
//!
//! Monitors live token pressure against the active model's context window,
//! dividing consumption into tiered zones (Safe, Warning, Critical) and
//! triggering proactive observation folding and Turn-aware compaction.

use nuo_wire::{Message, estimate_tokens};

/// Operational pressure tier of the current context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetTier {
    /// Token usage is within normal bounds (< 60% window). All history retained.
    Safe,
    /// Token usage is elevated (60% ~ 75% window). Observation folding triggered.
    Warning,
    /// Token usage is approaching limit (> 75% window). Compaction required.
    Critical,
}

/// The recommended action to alleviate context pressure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetAction {
    /// Context is healthy; proceed without intervention.
    None,
    /// Fold oversized older tool observation outputs.
    FoldObservations,
    /// Trigger atomic Turn-Aware Compaction to compress history.
    CompactionRequired {
        estimated_current_tokens: usize,
        window_limit: usize,
    },
}

/// Adaptive budget manager for managing session token headroom.
#[derive(Debug, Clone)]
pub struct ContextBudgetManager {
    /// Full context window of the active model (e.g. 128_000, 200_000).
    pub model_window: usize,
    /// Reserved headroom for model output and tool-call payload (e.g. 8_192).
    pub output_headroom: usize,
    /// Threshold fraction to transition from Safe to Warning (default 0.60).
    pub warning_threshold: f64,
    /// Threshold fraction to transition from Warning to Critical (default 0.75).
    pub critical_threshold: f64,
}

impl Default for ContextBudgetManager {
    fn default() -> Self {
        Self {
            model_window: 128_000,
            output_headroom: 8_192,
            warning_threshold: 0.60,
            critical_threshold: 0.75,
        }
    }
}

impl ContextBudgetManager {
    pub fn new(model_window: usize, output_headroom: usize) -> Self {
        Self {
            model_window: model_window.max(16_000),
            output_headroom: output_headroom.min(model_window / 4),
            ..Default::default()
        }
    }

    /// Calculate the effective usable context capacity excluding output reservation.
    pub fn usable_capacity(&self) -> usize {
        self.model_window.saturating_sub(self.output_headroom)
    }

    /// Determine current pressure tier from estimated tokens.
    pub fn tier_for_tokens(&self, tokens: usize) -> BudgetTier {
        let capacity = self.usable_capacity();
        if capacity == 0 {
            return BudgetTier::Critical;
        }
        let ratio = (tokens as f64) / (capacity as f64);
        if ratio >= self.critical_threshold {
            BudgetTier::Critical
        } else if ratio >= self.warning_threshold {
            BudgetTier::Warning
        } else {
            BudgetTier::Safe
        }
    }

    /// Evaluate current messages and determine necessary budget action.
    pub fn evaluate_messages(&self, messages: &[Message]) -> BudgetAction {
        let estimated_tokens = estimate_tokens(messages);
        let tier = self.tier_for_tokens(estimated_tokens);

        match tier {
            BudgetTier::Safe => BudgetAction::None,
            BudgetTier::Warning => BudgetAction::FoldObservations,
            BudgetTier::Critical => BudgetAction::CompactionRequired {
                estimated_current_tokens: estimated_tokens,
                window_limit: self.model_window,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_tier_transitions() {
        let mgr = ContextBudgetManager::new(100_000, 10_000);
        assert_eq!(mgr.usable_capacity(), 90_000);

        // < 60% of 90_000 is < 54_000 -> Safe
        assert_eq!(mgr.tier_for_tokens(50_000), BudgetTier::Safe);
        // 60% ~ 75% -> 54_000..67_500 -> Warning
        assert_eq!(mgr.tier_for_tokens(60_000), BudgetTier::Warning);
        // >= 75% -> >= 67_500 -> Critical
        assert_eq!(mgr.tier_for_tokens(70_000), BudgetTier::Critical);
    }
}
