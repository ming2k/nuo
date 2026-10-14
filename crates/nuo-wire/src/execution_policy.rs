//! Homogeneous agent execution policies (ADR-0183).
//!
//! Replaces heterogeneous agent species with a unified [`ExecutionPolicy`].
//! Every agent runs the same cognitive engine; its behavioral boundaries,
//! recursion limits, and human-interaction posture are governed entirely by
//! this typed policy.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::ToolPolicy;

/// Error when an execution policy invariant is violated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyViolation {
    RecursionDepthExceeded { current: usize, max: usize },
    ChildrenBudgetExhausted,
    HumanInteractionNotAllowed,
}

impl fmt::Display for PolicyViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RecursionDepthExceeded { current, max } => {
                write!(
                    f,
                    "Child agent recursion depth exceeded: current {current}, max {max}"
                )
            }
            Self::ChildrenBudgetExhausted => {
                write!(f, "Child agent budget exhausted for this parent")
            }
            Self::HumanInteractionNotAllowed => {
                write!(
                    f,
                    "Direct human interaction is not permitted under this execution policy"
                )
            }
        }
    }
}

impl std::error::Error for PolicyViolation {}

/// Lifecycle mode of an agent's context window and storage backing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextLifecycle {
    /// Durable session backed by persistence, surviving across turns and restarts.
    #[default]
    DurableSession,
    /// Ephemeral scratchpad: isolated context destroyed on mission completion,
    /// returning only a consolidated summary to the parent caller.
    EphemeralScratchpad,
}

/// Typed execution policy defining an agent's runtime posture and capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    /// Topology depth in the delegation tree. Root session agent is 0.
    pub depth: usize,
    /// Maximum permitted delegation depth. Once `depth >= max_depth`, spawning is disabled.
    pub max_depth: usize,
    /// Whether this agent may directly interact with the human user (`ask_user`).
    pub allow_human_interaction: bool,
    /// Context lifecycle: durable session vs ephemeral scratchpad.
    pub lifecycle: ContextLifecycle,
    /// Tool policy scoping which tools are admitted. Skipped during serde because
    /// `ToolPolicy` holds static slices.
    #[serde(skip)]
    pub tool_policy: Option<ToolPolicy>,
    /// Maximum number of child agents this agent may spawn concurrently or in total.
    pub max_children_budget: usize,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self::root_default()
    }
}

impl ExecutionPolicy {
    /// Default execution policy for a root-level session agent.
    pub fn root_default() -> Self {
        Self {
            depth: 0,
            max_depth: 1, // By default, allow 1 tier of delegation
            allow_human_interaction: true,
            lifecycle: ContextLifecycle::DurableSession,
            tool_policy: None,
            max_children_budget: 16,
        }
    }

    /// Default execution policy for an unbounded root developer.
    pub fn root_developer(max_depth: usize) -> Self {
        Self {
            depth: 0,
            max_depth,
            allow_human_interaction: true,
            lifecycle: ContextLifecycle::DurableSession,
            tool_policy: None,
            max_children_budget: 32,
        }
    }

    /// Derive a child agent's execution policy from this parent.
    ///
    /// Mathematical invariant:
    /// - `child.depth == parent.depth + 1`
    /// - Fails closed with [`PolicyViolation::RecursionDepthExceeded`] if `parent.depth >= parent.max_depth`.
    /// - Child agents never directly interact with human users (`allow_human_interaction = false`).
    /// - Child agents run in ephemeral scratchpads (`ContextLifecycle::EphemeralScratchpad`).
    pub fn derive_child(
        &self,
        child_tool_policy: Option<ToolPolicy>,
    ) -> Result<Self, PolicyViolation> {
        if self.depth >= self.max_depth {
            return Err(PolicyViolation::RecursionDepthExceeded {
                current: self.depth,
                max: self.max_depth,
            });
        }

        Ok(Self {
            depth: self.depth + 1,
            max_depth: self.max_depth,
            allow_human_interaction: false,
            lifecycle: ContextLifecycle::EphemeralScratchpad,
            tool_policy: child_tool_policy,
            max_children_budget: 0,
        })
    }

    /// Whether this agent is the root node of the session.
    pub fn is_root(&self) -> bool {
        self.depth == 0
    }

    /// Whether this agent is permitted to spawn sub-agents.
    pub fn can_spawn_subagent(&self) -> bool {
        self.depth < self.max_depth && self.max_children_budget > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_policy_can_spawn_child() {
        let root = ExecutionPolicy::root_default();
        assert!(root.is_root());
        assert!(root.allow_human_interaction);
        assert!(root.can_spawn_subagent());

        let child = root
            .derive_child(None)
            .expect("must derive child at depth 0");
        assert_eq!(child.depth, 1);
        assert!(!child.allow_human_interaction);
        assert_eq!(child.lifecycle, ContextLifecycle::EphemeralScratchpad);
        assert!(!child.can_spawn_subagent());
    }

    #[test]
    fn child_cannot_exceed_max_depth() {
        let root = ExecutionPolicy::root_default();
        let child = root.derive_child(None).unwrap();
        let grandchild_err = child.derive_child(None);
        assert_eq!(
            grandchild_err,
            Err(PolicyViolation::RecursionDepthExceeded { current: 1, max: 1 })
        );
    }
}
