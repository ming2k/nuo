//! Declarative tool execution policies and anti-abuse limits.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Declarative anti-abuse guardrails, execution quotas, and loop breakers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicy {
    /// Maximum number of tool calls permitted in a single round.
    /// Exceeding calls are intercepted to prevent parallel tool flooding.
    pub max_calls_per_round: usize,

    /// Maximum consecutive identical tool invocations (identical tool name + arguments)
    /// before the anti-loop circuit breaker trips.
    pub max_identical_consecutive_calls: usize,

    /// Maximum times a specific tool may be invoked in a single turn.
    pub tool_quotas: HashMap<String, usize>,

    /// Maximum total tool calls across the entire turn.
    pub max_total_calls_per_turn: usize,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            max_calls_per_round: 8,
            max_identical_consecutive_calls: 3,
            tool_quotas: HashMap::new(),
            max_total_calls_per_turn: 50,
        }
    }
}

impl ToolPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum concurrent tool calls permitted in a single round.
    pub fn with_max_calls_per_round(mut self, limit: usize) -> Self {
        self.max_calls_per_round = limit;
        self
    }

    /// Sets the repetition threshold for the anti-loop circuit breaker.
    pub fn with_anti_loop_threshold(mut self, threshold: usize) -> Self {
        self.max_identical_consecutive_calls = threshold;
        self
    }

    /// Sets an execution quota for a specific named tool across a turn.
    pub fn with_tool_quota(mut self, tool_name: impl Into<String>, max_invocations: usize) -> Self {
        self.tool_quotas.insert(tool_name.into(), max_invocations);
        self
    }

    /// Sets the maximum total tool calls across an entire turn.
    pub fn with_max_total_calls(mut self, limit: usize) -> Self {
        self.max_total_calls_per_turn = limit;
        self
    }
}
