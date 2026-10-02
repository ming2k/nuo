//! Routing access control and topology policy evaluation.

use crate::envelope::AgentEnvelope;
use std::fmt::Debug;

/// Outcome of evaluating a routing decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingDecision {
    /// Allow the envelope to be routed to its intended target.
    Allow,
    /// Deny the envelope with an explanatory violation reason.
    Deny { reason: String },
}

/// Pluggable policy governing inter-agent envelope dispatch and communication topology.
///
/// Implements Access Control (RBAC), hierarchical supervision constraints
/// (e.g. subagents cannot steer siblings or masters), and intent blacklisting.
pub trait RoutingPolicy: Send + Sync + Debug {
    /// Evaluates whether `envelope` is permitted to be routed.
    fn evaluate(&self, envelope: &AgentEnvelope) -> RoutingDecision;
}

/// A permissive policy allowing all envelopes without restriction (default).
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAllPolicy;

impl RoutingPolicy for AllowAllPolicy {
    fn evaluate(&self, _envelope: &AgentEnvelope) -> RoutingDecision {
        RoutingDecision::Allow
    }
}

/// Closure-based routing policy adapter.
pub struct FnRoutingPolicy<F> {
    evaluator: F,
}

impl<F> FnRoutingPolicy<F> {
    pub fn new(evaluator: F) -> Self {
        Self { evaluator }
    }
}

impl<F> Debug for FnRoutingPolicy<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FnRoutingPolicy").finish()
    }
}

impl<F> RoutingPolicy for FnRoutingPolicy<F>
where
    F: Fn(&AgentEnvelope) -> RoutingDecision + Send + Sync,
{
    fn evaluate(&self, envelope: &AgentEnvelope) -> RoutingDecision {
        (self.evaluator)(envelope)
    }
}
