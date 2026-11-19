//! Durable request-projection archive.
//!
//! A [`RequestProjection`] is a forensic snapshot of one assembled model
//! request: the cacheable-prefix identity and the request-local temporary
//! context (`E_n`) exactly as the provider saw them.
//!
//! It is **projection state, not transcript**: it lives in its own key-addressed
//! table, never enters the model history view, and is never read back into a
//! later request. Persisting it serves reconstruction and audit only
//! (three data surfaces, boundary).
use serde::{Deserialize, Serialize};

use crate::context::{ContextPlan, PlannedBlockRecord, PrefixDrift};

/// One durable request-projection record, keyed by `(session, round, turn)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestProjection {
    /// Harness round that produced the request.
    pub round: u64,
    /// ReAct turn within the round.
    pub turn: u64,
    pub created_at_ms: u64,
    /// Deterministic identity of the cacheable prefix `S | H | I`.
    pub prefix_fingerprint: String,
    /// Number of `S | H | I` messages carried by the request.
    pub conversation_messages: usize,
    /// Request-local temporary-context tokens (`E_n`).
    pub temporary_context_tokens: usize,
    /// Request-local temporary context (`E_n`) captured verbatim, bounded by the
    /// producer budget. This is evidence, not history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub temporary_context: Vec<tool::Message>,
    /// The measured context plan this request was built from: every declared
    /// block, its band, and its cost (ADR-0217).
    ///
    /// Absent in records written before the plan existed, so a reader must treat
    /// an empty vector as "not recorded" rather than "no blocks".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<PlannedBlockRecord>,
    /// How the cacheable prefix moved since the previous request on this thread.
    #[serde(default)]
    pub drift: PrefixDrift,
}

impl RequestProjection {
    /// Build a record from the request's derived cache plan plus the caller's
    /// round/turn coordinates and the measured context plan.
    pub fn from_request(
        request: &crate::ModelRequest,
        temporary_context_tokens: usize,
        plan: Option<&ContextPlan>,
        round: u64,
        turn: u64,
        created_at_ms: u64,
    ) -> Self {
        let cache_plan = request.cache_plan();
        Self {
            round,
            turn,
            created_at_ms,
            prefix_fingerprint: cache_plan.prefix_fingerprint,
            conversation_messages: cache_plan.conversation_messages,
            temporary_context_tokens,
            temporary_context: request.temporary_context().to_vec(),
            blocks: plan
                .map(|plan| plan.blocks.iter().map(PlannedBlockRecord::from).collect())
                .unwrap_or_default(),
            drift: plan.map(|plan| plan.drift.clone()).unwrap_or_default(),
        }
    }
}
