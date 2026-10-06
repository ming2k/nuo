//! The kernel's execution record: the facts a round produces and the registers
//! it works with (ADR-0304 §1).
//!
//! # Why this exists
//!
//! A round needs a working set: the message window it compiles a request from,
//! the round counter, the retry point, the task list, the disabled-tool mask,
//! token accounting, the last projection. Until now all of those lived in the
//! product's session store, which meant the kernel's own state machine was
//! hosted by an application crate — the coupling ADR-0300 names and ADR-0304
//! removes.
//!
//! This type is the kernel's own answer. It holds:
//!
//! - **facts** — the `ExecutionGraph` of what happened (immutable, append-only);
//! - **registers** — the `GraphState` the round reads and writes;
//! - **a watermark** — the highest fact sequence handed to the sink;
//! - **the sink** — where acknowledged facts go.
//!
//! # What it is not
//!
//! It is not a session. It has no lineage, no title, no digest, no partition, no
//! workspace binding, and no product policy — those are the application plane's
//! (ADR-0304 §2), and the ports of [`crate::host`] are how a host supplies the
//! two the round genuinely needs (a title sink and declared roles).
//!
//! # Durability
//!
//! `commit` drains the delta since the watermark, hands it to the sink, and only
//! advances the watermark when the sink *acknowledges* it. A sink failure leaves
//! the watermark where it was, so the next commit re-sends — which is what makes
//! a retry safe and a "durable" claim true.

use std::sync::Arc;

use nuo_wire::{
    CausalGraph, CausalNode, ExecutionStatus, Message, NodeKind, NodePayload, SessionDelta,
    SessionPolicy, StateUpdate, SystemNoticePayload,
};

use crate::durability::{Ack, FactSink, SinkError, SinkHealth};

/// The working registers a round reads and writes.
///
/// Everything here is *execution* state: it changes how a request is compiled or
/// what a guard does, and it has no meaning outside a running agent
/// (ADR-0304 §1). Product state — what the session is called, who forked it, what
/// workspace it belongs to — is deliberately absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphState {
    /// The compile cursor: the fact the next request extends from.
    pub cursor: Option<String>,
    /// Where the instance is in its own lifecycle.
    pub status: ExecutionStatus,
    /// Notices queued while the instance was suspended or parked.
    pub pending_notices: Vec<SystemNoticePayload>,
    /// Executions completed. The one authoritative count (ADR-0304 §5): a
    /// product displays this value and keeps no counter of its own.
    pub steps: u64,
    /// The compaction horizon, when one exists.
    pub compaction_horizon: Option<String>,
    /// Budget-driven pruning gave up for this instance.
    pub pruning_exhausted: bool,
    /// The unified task list the model last saw.
    pub todos: nuo_wire::TodoList,
    /// Tools switched off for this instance (ADR-0048 Phase 2).
    pub disabled_tools: std::collections::HashSet<String>,
    /// The `/retry` point a stopped round left behind, if any.
    pub retry_pending: Option<nuo_wire::RetryPoint>,
    /// Per-request token accounting for this instance.
    pub usage_records: Vec<nuo_wire::RequestUsageRecord>,
    /// Stats of the most recent model-context projection (prune or compaction).
    pub last_projection: Option<nuo_wire::ContextProjectionCheckpoint>,
}

/// The kernel's record for one agent instance.
pub struct ExecutionRecord {
    instance_id: String,
    policy: SessionPolicy,
    history: CausalGraph,
    state: GraphState,
    /// Highest fact sequence the sink has acknowledged.
    watermark: u64,
    sink: Arc<dyn FactSink>,
}

impl std::fmt::Debug for ExecutionRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionRecord")
            .field("instance_id", &self.instance_id)
            .field("facts", &self.history.nodes.len())
            .field("watermark", &self.watermark)
            .field("steps", &self.state.steps)
            .field("status", &self.state.status)
            .finish_non_exhaustive()
    }
}

impl ExecutionRecord {
    /// A fresh record for `instance_id`, hydrating nothing.
    pub fn new(
        instance_id: impl Into<String>,
        policy: SessionPolicy,
        sink: Arc<dyn FactSink>,
    ) -> Self {
        Self {
            instance_id: instance_id.into(),
            policy,
            history: CausalGraph::new(),
            state: GraphState::default(),
            watermark: 0,
            sink,
        }
    }

    /// A record hydrated from facts the host read out of its own store
    /// (hydration is an input, never a query).
    ///
    /// The watermark starts at the highest hydrated sequence, because those facts
    /// are already durable — re-sending them would be a wasted round trip, and a
    /// sink is only required to tolerate a replay, not to want one.
    ///
    /// # The cursor must come with the facts
    ///
    /// `state.cursor` is what the window is projected from, and facts alone do not
    /// imply it: a graph can hold several leaves, and only the host knows which
    /// one the instance was reading. A `None` cursor is therefore *legal* — it
    /// means "this instance has no position yet" — but hydrating facts without one
    /// would silently yield an empty window, which is why this constructor
    /// defaults the cursor to the highest-sequence fact when the caller supplies
    /// none. That default is the only defensible one: the newest fact is where the
    /// instance last wrote.
    pub fn hydrate(
        instance_id: impl Into<String>,
        policy: SessionPolicy,
        sink: Arc<dyn FactSink>,
        facts: Vec<CausalNode>,
        mut state: GraphState,
    ) -> Self {
        let mut history = CausalGraph::new();
        let mut highest = 0;
        let mut newest: Option<(u64, String)> = None;
        for node in facts {
            if node.seq >= newest.as_ref().map(|(seq, _)| *seq).unwrap_or(0) {
                newest = Some((node.seq, node.id.clone()));
            }
            highest = highest.max(node.seq);
            history.insert_node(node);
        }
        if state.cursor.is_none() {
            state.cursor = newest.map(|(_, id)| id);
        }
        Self {
            instance_id: instance_id.into(),
            policy,
            history,
            state,
            watermark: highest,
            sink,
        }
    }

    /// Hydrate an execution record from a host's `SessionIR`.
    pub fn hydrate_from_ir(
        instance_id: impl Into<String>,
        policy: SessionPolicy,
        sink: Arc<dyn FactSink>,
        ir: nuo_wire::SessionIR,
    ) -> Self {
        let nodes: Vec<CausalNode> = ir.history.nodes.into_values().collect();
        let state = GraphState {
            cursor: ir.state.active_leaf,
            status: ir.state.status,
            pending_notices: ir.state.pending_notifications,
            steps: ir.state.round_counter,
            compaction_horizon: ir.state.compaction_horizon,
            pruning_exhausted: ir.state.pruning_exhausted,
            todos: Default::default(),
            disabled_tools: Default::default(),
            retry_pending: None,
            usage_records: Vec::new(),
            last_projection: None,
        };
        Self::hydrate(instance_id, policy, sink, nodes, state)
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn policy(&self) -> &SessionPolicy {
        &self.policy
    }

    pub fn state(&self) -> &GraphState {
        &self.state
    }

    /// Mutable access to the registers, for a round that owns this record.
    pub fn state_mut(&mut self) -> &mut GraphState {
        &mut self.state
    }

    pub fn history(&self) -> &CausalGraph {
        &self.history
    }

    /// The highest acknowledged fact sequence.
    pub fn watermark(&self) -> u64 {
        self.watermark
    }

    pub fn sink_health(&self) -> SinkHealth {
        self.sink.health()
    }

    /// Append one fact and advance the cursor.
    pub fn append_fact(
        &mut self,
        id: impl Into<String>,
        timestamp_ms: u64,
        kind: NodeKind,
        payload: NodePayload,
    ) -> String {
        let id = id.into();
        let parent_id = self.state.cursor.clone();
        let seq = self.history.next_seq();
        self.history.insert_node(CausalNode {
            id: id.clone(),
            parent_id,
            seq,
            timestamp_ms,
            kind,
            payload,
        });
        self.state.cursor = Some(id.clone());
        id
    }

    /// Append a dialogue message.
    pub fn append_message(&mut self, id: impl Into<String>, timestamp_ms: u64, message: Message) -> String {
        self.append_fact(
            id,
            timestamp_ms,
            NodeKind::Dialogue,
            NodePayload::Message { message },
        )
    }

    /// Append a system notice.
    pub fn append_notice(
        &mut self,
        id: impl Into<String>,
        timestamp_ms: u64,
        source: impl Into<String>,
        notice_type: impl Into<String>,
        content: impl Into<String>,
    ) -> String {
        self.append_fact(
            id,
            timestamp_ms,
            NodeKind::SystemNotice,
            NodePayload::SystemNotice {
                source: source.into(),
                notice_type: notice_type.into(),
                content: content.into(),
            },
        )
    }

    /// The active branch as a message window, in order.
    ///
    /// This is what a request is compiled from: the facts along the cursor's
    /// lineage, with compaction honoured through the horizon.
    pub fn model_window(&self) -> Vec<Message> {
        let Some(cursor) = self.state.cursor.as_deref() else {
            return Vec::new();
        };
        self.history
            .linear_path_with_horizon(cursor, self.state.compaction_horizon.as_deref())
            .into_iter()
            .filter_map(|node| match &node.payload {
                NodePayload::Message { message } => Some(message.clone()),
                // A compaction's summary is itself context: it replaces the
                // rounds it folded.
                NodePayload::Compaction { summary, .. } => {
                    Some(Message::new(nuo_wire::Role::System, summary.clone()))
                }
                // Observations and notices are not dialogue; a request that
                // needs them reads them by claim-check (ADR-0282).
                _ => None,
            })
            .collect()
    }

    /// The delta of facts appended since `watermark`.
    fn drain_delta(&self) -> SessionDelta {
        let mut new_nodes: Vec<CausalNode> = self
            .history
            .nodes
            .values()
            .filter(|node| node.seq > self.watermark)
            .cloned()
            .collect();
        new_nodes.sort_by_key(|node| node.seq);
        SessionDelta {
            session_id: self.instance_id.clone(),
            parent_session_id: None,
            previous_watermark_seq: self.watermark,
            new_watermark_seq: self.history.max_seq,
            new_nodes,
            state_update: StateUpdate {
                active_leaf: self.state.cursor.clone(),
                status: self.state.status.clone(),
                round_counter: self.state.steps,
                pending_notifications: self.state.pending_notices.clone(),
            },
            policy_update: None,
            updated_at_s: 0,
        }
    }

    /// Hand everything unacknowledged to the sink, and advance the watermark
    /// only as far as the sink acknowledged.
    ///
    /// A failure leaves the watermark untouched, so the next commit re-sends the
    /// same facts. That is what makes the retry safe (a sink tolerates a replay)
    /// and the "durable" claim true: nothing is reported as stored that was not
    /// acknowledged.
    pub async fn commit(&mut self) -> Result<Ack, SinkError> {
        let delta = self.drain_delta();
        if delta.new_nodes.is_empty() {
            // Nothing new: report the current position without a round trip.
            return Ok(Ack::durable(self.watermark));
        }
        let ack = self.sink.append(delta).await?;
        self.watermark = self.watermark.max(ack.durable_upto);
        Ok(ack)
    }

    // ---------------------------------------------------------------------
    // The round path's register accessors.
    //
    // These are what `RoundContext` used to reach through `SessionStore` for.
    // Each is the kernel's own state (ADR-0304 §1), which is why it lives here
    // rather than behind a host port: a round changes how it compiles a request,
    // not what the product calls the session.
    // ---------------------------------------------------------------------

    /// The instance's step count. The one authoritative count (ADR-0304 §5).
    pub fn steps(&self) -> u64 {
        self.state.steps
    }

    /// Set the step count. Used by a resume that restores a frozen value.
    pub fn set_steps(&mut self, steps: u64) {
        self.state.steps = steps;
    }

    /// The `/retry` point a stopped round left behind.
    pub fn retry_pending(&self) -> Option<&nuo_wire::RetryPoint> {
        self.state.retry_pending.as_ref()
    }

    /// Arm a `/retry` point.
    pub fn set_retry_pending(&mut self, point: Option<nuo_wire::RetryPoint>) {
        self.state.retry_pending = point;
    }

    /// The unified task list the model last saw.
    pub fn todos(&self) -> &nuo_wire::TodoList {
        &self.state.todos
    }

    pub fn set_todos(&mut self, todos: nuo_wire::TodoList) {
        self.state.todos = todos;
    }

    /// Tools switched off for this instance.
    pub fn disabled_tools(&self) -> &std::collections::HashSet<String> {
        &self.state.disabled_tools
    }

    pub fn set_disabled_tools(&mut self, tools: std::collections::HashSet<String>) {
        self.state.disabled_tools = tools;
    }

    /// Per-request token accounting.
    pub fn usage_records(&self) -> &[nuo_wire::RequestUsageRecord] {
        &self.state.usage_records
    }

    pub fn set_usage_records(&mut self, records: Vec<nuo_wire::RequestUsageRecord>) {
        self.state.usage_records = records;
    }

    /// Stats of the most recent projection.
    pub fn last_projection(&self) -> Option<&nuo_wire::ContextProjectionCheckpoint> {
        self.state.last_projection.as_ref()
    }

    /// Record a projection's checkpoint.
    pub fn set_last_projection(
        &mut self,
        checkpoint: Option<nuo_wire::ContextProjectionCheckpoint>,
    ) {
        self.state.last_projection = checkpoint;
    }

    /// Replace the message window with `messages`, as a `/retry` re-seed does.
    ///
    /// The window is facts, so this appends: the replacement is recorded as a
    /// compaction-shaped fact rather than a rewrite of history, which is what
    /// keeps the record append-only (ADR-0275 `[INV-STATE-03]`).
    pub fn reseed_window(&mut self, messages: &[Message], timestamp_ms: u64) {
        if messages.is_empty() {
            return;
        }
        let id = format!("reseed-{}", self.history.max_seq + 1);
        self.append_fact(
            id,
            timestamp_ms,
            NodeKind::Compaction,
            NodePayload::Compaction {
                summary: messages
                    .iter()
                    .map(|message| message.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                first_kept_node_id: self.state.cursor.clone().unwrap_or_default(),
                tokens_before: 0,
                read_files: Vec::new(),
                modified_files: Vec::new(),
                belief_state: None,
            },
        );
    }

    /// The record as a `SessionIR`, for the compaction engine.
    ///
    /// The compactor's subject is a graph, and this record *is* one; the
    /// projection exists because the compactor was written against the IR
    /// aggregate. It is a view, not a second authority: nothing is written back
    /// except through [`Self::apply_compacted_ir`].
    pub fn as_ir(&self) -> nuo_wire::SessionIR {
        nuo_wire::SessionIR {
            session_id: self.instance_id.clone(),
            parent_session_id: None,
            created_at_s: 0,
            updated_at_s: 0,
            history: self.history.clone(),
            state: nuo_wire::SessionState {
                active_leaf: self.state.cursor.clone(),
                active_timeline: "main".to_string(),
                timelines: std::collections::HashMap::new(),
                compaction_horizon: self.state.compaction_horizon.clone(),
                status: self.state.status.clone(),
                pending_notifications: self.state.pending_notices.clone(),
                pruning_exhausted: self.state.pruning_exhausted,
                round_counter: self.state.steps,
            },
            policy: self.policy.clone(),
        }
    }

    /// Fold a compacted IR view back into the record.
    ///
    /// The compaction added facts (the summary node, the new horizon) rather than
    /// rewriting any, so this is an append of what is new plus a register update —
    /// which is exactly what keeps the record's history immutable while its view
    /// moves (ADR-0275).
    pub fn apply_compacted_ir(&mut self, ir: &nuo_wire::SessionIR) {
        for node in ir.history.nodes.values() {
            if self.history.get_node(&node.id).is_none() {
                self.history.insert_node(node.clone());
            }
        }
        self.state.cursor = ir.state.active_leaf.clone();
        self.state.compaction_horizon = ir.state.compaction_horizon.clone();
        self.state.pruning_exhausted = ir.state.pruning_exhausted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durability::NullSink;
    use crate::durability_conformance::MemorySink;
    use nuo_wire::Role;

    fn record(sink: Arc<dyn FactSink>) -> ExecutionRecord {
        ExecutionRecord::new("instance-a", SessionPolicy::default(), sink)
    }

    fn message(text: &str) -> Message {
        Message::new(Role::User, text)
    }

    #[test]
    fn appending_a_fact_advances_the_cursor_and_the_window() {
        let mut record = record(Arc::new(NullSink));
        assert!(record.model_window().is_empty());
        record.append_message("n1", 1_000, message("hello"));
        assert_eq!(record.state().cursor.as_deref(), Some("n1"));
        let window = record.model_window();
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].content, "hello");
    }

    #[test]
    fn the_window_follows_the_lineage_in_order() {
        let mut record = record(Arc::new(NullSink));
        record.append_message("n1", 1_000, message("first"));
        record.append_message("n2", 2_000, message("second"));
        let window = record.model_window();
        assert_eq!(
            window.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn a_compaction_summary_enters_the_window_in_place_of_what_it_folded() {
        let mut record = record(Arc::new(NullSink));
        record.append_message("n1", 1_000, message("old"));
        record.append_message("n2", 2_000, message("recent"));
        // A compaction node whose summary stands for the folded prefix.
        record.append_fact(
            "c1",
            3_000,
            NodeKind::Compaction,
            NodePayload::Compaction {
                summary: "earlier rounds folded".into(),
                first_kept_node_id: "n2".into(),
                tokens_before: 100,
                read_files: Vec::new(),
                modified_files: Vec::new(),
                belief_state: None,
            },
        );
        let window = record.model_window();
        assert_eq!(
            window.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            vec!["earlier rounds folded"],
            "the summary is context; the folded round is not re-sent"
        );
    }

    #[test]
    fn observations_are_not_dialogue_and_do_not_enter_the_window() {
        let mut record = record(Arc::new(NullSink));
        record.append_message("n1", 1_000, message("ask"));
        record.append_fact(
            "o1",
            2_000,
            NodeKind::Observation,
            NodePayload::Observation {
                call_id: "call-1".into(),
                tool_name: "read_text".into(),
                blob_hash: "sha".into(),
                metrics: nuo_wire::ObservationMetrics::default(),
                lifecycle: nuo_wire::ObservationLifecycle::Raw {
                    content: "…".into(),
                },
            },
        );
        let window = record.model_window();
        assert_eq!(window.len(), 1, "only the dialogue is in the window");
        assert_eq!(window[0].content, "ask");
    }

    #[tokio::test]
    async fn committing_advances_the_watermark_to_what_the_sink_acknowledged() {
        let sink = Arc::new(MemorySink::new());
        let mut record = record(sink.clone());
        record.append_message("n1", 1_000, message("a"));
        let ack = record.commit().await.expect("commit succeeds");
        assert_eq!(ack.durable_upto, 1);
        assert_eq!(record.watermark(), 1);
        assert_eq!(sink.nodes().len(), 1, "the fact reached the sink");
    }

    #[tokio::test]
    async fn a_commit_with_nothing_new_is_a_no_op_round_trip() {
        let sink = Arc::new(MemorySink::new());
        let mut record = record(sink.clone());
        let ack = record.commit().await.expect("commit succeeds");
        assert_eq!(ack.durable_upto, 0);
        assert!(sink.nodes().is_empty(), "nothing was sent, nothing was stored");
    }

    #[tokio::test]
    async fn a_failing_sink_leaves_the_watermark_so_the_next_commit_resends() {
        /// A sink that fails its first append and then succeeds, recording what
        /// it stored so the test can prove the resend stored the fact once.
        struct FlakySink {
            attempts: std::sync::atomic::AtomicUsize,
            stored: std::sync::Mutex<std::collections::BTreeMap<u64, CausalNode>>,
        }

        impl FlakySink {
            fn stored_count(&self) -> usize {
                self.stored
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .len()
            }
        }

        impl FactSink for FlakySink {
            fn append(
                &self,
                batch: SessionDelta,
            ) -> futures::future::BoxFuture<'static, Result<Ack, SinkError>> {
                let first = self.attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                if first {
                    return Box::pin(async { Err(SinkError::retryable("transient failure")) });
                }
                let watermark = batch.new_watermark_seq;
                {
                    let mut stored = self
                        .stored
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    for node in batch.new_nodes {
                        stored.insert(node.seq, node);
                    }
                }
                Box::pin(async move { Ok(Ack::durable(watermark)) })
            }
        }

        let sink = Arc::new(FlakySink {
            attempts: std::sync::atomic::AtomicUsize::new(0),
            stored: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        });
        let mut record = record(sink.clone());
        record.append_message("n1", 1_000, message("a"));

        let error = record.commit().await.expect_err("the first commit fails");
        assert!(error.retryable);
        assert_eq!(
            record.watermark(),
            0,
            "an unacknowledged fact is not reported as durable"
        );

        let ack = record.commit().await.expect("the retry succeeds");
        assert_eq!(ack.durable_upto, 1);
        assert_eq!(record.watermark(), 1);
        assert_eq!(
            sink.stored_count(),
            1,
            "the resent fact was stored exactly once"
        );
    }

    #[tokio::test]
    async fn hydration_starts_the_watermark_at_the_facts_it_received() {
        let sink = Arc::new(MemorySink::new());
        let mut original = record(sink.clone());
        original.append_message("n1", 1_000, message("stored"));
        original.commit().await.unwrap();

        // A host reads its own store and hands the facts in. The
        // cursor is not supplied, so hydration defaults it to the newest fact.
        let mut resumed = ExecutionRecord::hydrate(
            "instance-a",
            SessionPolicy::default(),
            sink.clone(),
            sink.nodes(),
            GraphState::default(),
        );
        assert_eq!(resumed.watermark(), 1, "hydrated facts are already durable");
        assert_eq!(
            resumed.state().cursor.as_deref(),
            Some("n1"),
            "hydration positions the instance at its newest fact"
        );
        assert_eq!(resumed.model_window()[0].content, "stored");

        // A commit with nothing new does not re-send what the host just gave us.
        let ack = resumed.commit().await.unwrap();
        assert_eq!(ack.durable_upto, 1);
        assert_eq!(sink.nodes().len(), 1, "no duplicate write");
    }

    #[tokio::test]
    async fn registers_are_the_kernels_own() {
        let mut record = record(Arc::new(NullSink));
        record.state_mut().steps = 7;
        record.state_mut().retry_pending = Some(nuo_wire::RetryPoint {
            round: 7,
            turns_committed: 2,
            history_watermark: 12,
            at_ms: 1_000,
            paused_ms: 0,
        });
        record
            .state_mut()
            .disabled_tools
            .insert("execute_command".to_string());
        record.state_mut().todos = nuo_wire::TodoList::default();

        assert_eq!(record.state().steps, 7);
        assert!(record.state().retry_pending.is_some());
        assert!(record.state().disabled_tools.contains("execute_command"));
    }

    #[test]
    fn a_null_sink_reports_healthy_and_keeps_nothing() {
        let record = record(Arc::new(NullSink));
        assert!(record.sink_health().is_healthy());
    }
}
