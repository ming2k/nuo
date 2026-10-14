//! Incremental commit delta for Session IR (ADR-0241).
//!
//! Guarantees `INV-SESSION-05`: turn persistence is bounded by O(Δ)
//! allocations and writes. Only newly appended causal nodes (`seq > watermark`)
//! and updated state registers travel in a delta.

use super::types::{
    CausalNode, ExecutionStatus, NodeId, SessionIR, SessionPolicy, SystemNoticePayload,
};
use serde::{Deserialize, Serialize};

/// An incremental mutation payload of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionDelta {
    /// Identifier of the target session.
    pub session_id: String,
    /// Parent session identifier if this session was forked or spawned as a subagent.
    #[serde(default)]
    pub parent_session_id: Option<String>,
    /// Watermark sequence number before this delta.
    pub previous_watermark_seq: u64,
    /// Highest sequence number contained in this delta.
    pub new_watermark_seq: u64,
    /// Causal nodes added since `previous_watermark_seq`, sorted by seq ascending.
    pub new_nodes: Vec<CausalNode>,
    /// Shallow snapshot of updated state registers.
    pub state_update: StateUpdate,
    /// Updated policy, present only if modified during this turn.
    pub policy_update: Option<SessionPolicy>,
    /// Update timestamp in epoch seconds.
    pub updated_at_s: u64,
}

/// Shallow update to session working memory and cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateUpdate {
    pub active_leaf: Option<NodeId>,
    pub status: ExecutionStatus,
    pub round_counter: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_notifications: Vec<SystemNoticePayload>,
}

impl SessionIR {
    /// Extract a delta of all mutations since `watermark_seq`.
    ///
    /// If `watermark_seq == 0`, extracts a full snapshot of the causal graph.
    /// Otherwise, extracts strictly nodes with `seq > watermark_seq` in O(Δ) time.
    pub fn drain_delta(&self, watermark_seq: u64) -> SessionDelta {
        let mut new_nodes: Vec<CausalNode> = self
            .history
            .nodes
            .values()
            .filter(|node| node.seq > watermark_seq)
            .cloned()
            .collect();

        new_nodes.sort_by_key(|n| n.seq);
        let new_watermark = self.history.max_seq.max(watermark_seq);

        SessionDelta {
            session_id: self.session_id.clone(),
            parent_session_id: self.parent_session_id.clone(),
            previous_watermark_seq: watermark_seq,
            new_watermark_seq: new_watermark,
            new_nodes,
            state_update: StateUpdate {
                active_leaf: self.state.active_leaf.clone(),
                status: self.state.status.clone(),
                round_counter: self.state.round_counter,
                pending_notifications: self.state.pending_notifications.clone(),
            },
            policy_update: (watermark_seq == 0).then(|| self.policy.clone()),
            updated_at_s: self.updated_at_s,
        }
    }

    /// Apply an external delta into this Session IR.
    pub fn apply_delta(&mut self, delta: SessionDelta) {
        if delta.session_id != self.session_id {
            return;
        }

        for node in delta.new_nodes {
            self.history.insert_node(node);
        }

        self.state.active_leaf = delta.state_update.active_leaf.clone();
        if let Some(timeline) = self.state.timelines.get_mut(&self.state.active_timeline) {
            timeline.head_node = delta.state_update.active_leaf;
            timeline.updated_at_s = delta.updated_at_s;
        }
        self.state.status = delta.state_update.status;
        self.state.round_counter = delta.state_update.round_counter;
        self.state.pending_notifications = delta.state_update.pending_notifications;

        if let Some(policy) = delta.policy_update {
            self.policy = policy;
        }

        self.updated_at_s = self.updated_at_s.max(delta.updated_at_s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, Role};

    #[test]
    fn test_delta_drain_and_apply() {
        let mut ir1 = SessionIR::new("session-test-delta", SessionPolicy::default(), 100);

        // Turn 1
        let id1 = ir1.append_message("msg-1", 100_000, Message::new(Role::User, "Hello"));
        let delta1 = ir1.drain_delta(0);

        assert_eq!(delta1.previous_watermark_seq, 0);
        assert_eq!(delta1.new_watermark_seq, 1);
        assert_eq!(delta1.new_nodes.len(), 1);
        assert_eq!(delta1.new_nodes[0].id, id1);

        // Turn 2
        let id2 = ir1.append_message("msg-2", 101_000, Message::new(Role::Assistant, "Hi there"));
        let delta2 = ir1.drain_delta(1);

        // Delta 2 should only carry msg-2, not msg-1 (O(Δ) invariant)
        assert_eq!(delta2.previous_watermark_seq, 1);
        assert_eq!(delta2.new_watermark_seq, 2);
        assert_eq!(delta2.new_nodes.len(), 1);
        assert_eq!(delta2.new_nodes[0].id, id2);

        // Create empty secondary IR and hydrate using deltas
        let mut ir2 = SessionIR::new("session-test-delta", SessionPolicy::default(), 100);
        ir2.apply_delta(delta1);
        assert_eq!(ir2.history.nodes.len(), 1);
        assert_eq!(ir2.state.active_leaf, Some(id1));

        ir2.apply_delta(delta2);
        assert_eq!(ir2.history.nodes.len(), 2);
        assert_eq!(ir2.state.active_leaf, Some(id2));
        assert_eq!(ir1, ir2);
    }
}
