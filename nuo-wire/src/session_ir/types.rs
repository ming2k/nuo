//! Core types of the Session Intermediate Representation (Session IR).
//!
//! Defined by ADR-0241, Session IR is organized into:
//! - [`CausalGraph`]: immutable, append-only historical facts (`history`)
//! - [`SessionState`]: mutable cursor, status machine, and event mailbox (`state`)
//! - [`SessionPolicy`]: declarative governance rules, capabilities, and budgets (`policy`)

use crate::message::Message;
use crate::Role;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for a node in the causal fact graph.
pub type NodeId = String;

/// The canonical, in-memory Session Intermediate Representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionIR {
    /// Unique identifier of the session.
    pub session_id: String,
    /// Parent session identifier if this session was forked or spawned as a subagent.
    pub parent_session_id: Option<String>,
    /// Creation timestamp in epoch seconds.
    pub created_at_s: u64,
    /// Last update timestamp in epoch seconds.
    pub updated_at_s: u64,
    /// 1. Immutable Causal Fact Graph (what happened).
    pub history: CausalGraph,
    /// 2. Mutable Working State & Cursor Registers (where we are).
    pub state: SessionState,
    /// 3. Declarative Policy & Constraints (rules and limits).
    pub policy: SessionPolicy,
}

impl SessionIR {
    /// Initialize a fresh Session IR with the given policy and initial root state.
    pub fn new(session_id: impl Into<String>, policy: SessionPolicy, timestamp_s: u64) -> Self {
        let mut state = SessionState::new();
        state.timelines.insert(
            "main".into(),
            TimelineCursor {
                id: "main".into(),
                name: "Mainline".into(),
                kind: TimelineKind::Main,
                head_node: None,
                forked_from_node: None,
                created_at_s: timestamp_s,
                updated_at_s: timestamp_s,
            },
        );
        Self {
            session_id: session_id.into(),
            parent_session_id: None,
            created_at_s: timestamp_s,
            updated_at_s: timestamp_s,
            history: CausalGraph::new(),
            state,
            policy,
        }
    }

    /// Fork the session IR into a child branch or aside session.
    pub fn fork(&self, new_session_id: impl Into<String>) -> Self {
        let sid = new_session_id.into();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut child_state = self.state.clone();
        child_state.round_counter = 0;
        child_state.pending_notifications.clear();
        Self {
            session_id: sid,
            parent_session_id: Some(self.session_id.clone()),
            created_at_s: now,
            updated_at_s: now,
            history: self.history.clone(),
            state: child_state,
            policy: self.policy.clone(),
        }
    }

    /// Append a dialogue message node to the active branch.
    pub fn append_message(
        &mut self,
        node_id: impl Into<String>,
        timestamp_ms: u64,
        message: Message,
    ) -> NodeId {
        let node_id = node_id.into();
        let parent_id = self.state.active_leaf.clone();
        let seq = self.history.next_seq();

        let node = CausalNode {
            id: node_id.clone(),
            parent_id,
            seq,
            timestamp_ms,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message { message },
        };

        self.history.insert_node(node);
        self.state.active_leaf = Some(node_id.clone());
        if let Some(timeline) = self.state.timelines.get_mut(&self.state.active_timeline) {
            timeline.head_node = Some(node_id.clone());
            timeline.updated_at_s = timestamp_ms / 1000;
        }
        self.updated_at_s = timestamp_ms / 1000;
        node_id
    }

    /// Create a new named timeline branching from the current active leaf (ADR-0251).
    pub fn create_timeline(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        kind: TimelineKind,
    ) -> String {
        let id = id.into();
        let name = name.into();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let cursor = TimelineCursor {
            id: id.clone(),
            name,
            kind,
            head_node: self.state.active_leaf.clone(),
            forked_from_node: self.state.active_leaf.clone(),
            created_at_s: now,
            updated_at_s: now,
        };
        self.state.timelines.insert(id.clone(), cursor);
        self.state.active_timeline = id.clone();
        id
    }

    /// Switch active timeline to the given timeline ID (ADR-0251).
    /// Updates `active_leaf` to the timeline's head node.
    pub fn switch_timeline(&mut self, id: &str) -> bool {
        if let Some(cursor) = self.state.timelines.get(id) {
            self.state.active_leaf = cursor.head_node.clone();
            self.state.active_timeline = id.to_string();
            true
        } else {
            false
        }
    }

    /// Resolve the linear causal sequence of nodes along the specified timeline (ADR-0251).
    pub fn resolve_timeline_branch(&self, timeline_id: &str) -> Vec<&CausalNode> {
        let head = self
            .state
            .timelines
            .get(timeline_id)
            .and_then(|c| c.head_node.as_deref())
            .or(self.state.active_leaf.as_deref());
        match head {
            Some(leaf) => self.history.linear_path(leaf),
            None => Vec::new(),
        }
    }

    /// Record an execution termination event (e.g. human interrupt, fatal fault).
    pub fn record_termination(
        &mut self,
        node_id: impl Into<String>,
        timestamp_ms: u64,
        reason: TerminationReason,
        partial_output: Option<String>,
        interrupted_tool_call_id: Option<String>,
        duration_ms: Option<u64>,
    ) -> NodeId {
        let node_id = node_id.into();
        let parent_id = self.state.active_leaf.clone();
        let seq = self.history.next_seq();

        let node = CausalNode {
            id: node_id.clone(),
            parent_id,
            seq,
            timestamp_ms,
            kind: NodeKind::Termination,
            payload: NodePayload::Termination {
                reason,
                partial_output,
                interrupted_tool_call_id,
                duration_ms,
            },
        };

        self.history.insert_node(node);
        self.state.active_leaf = Some(node_id.clone());
        self.state.status = ExecutionStatus::Idle;
        self.updated_at_s = timestamp_ms / 1000;
        node_id
    }

    /// Insert an asynchronous system notice into the fact graph.
    pub fn append_system_notice(
        &mut self,
        node_id: impl Into<String>,
        timestamp_ms: u64,
        source: String,
        notice_type: String,
        content: String,
    ) -> NodeId {
        let node_id = node_id.into();
        let parent_id = self.state.active_leaf.clone();
        let seq = self.history.next_seq();

        let node = CausalNode {
            id: node_id.clone(),
            parent_id,
            seq,
            timestamp_ms,
            kind: NodeKind::SystemNotice,
            payload: NodePayload::SystemNotice {
                source,
                notice_type,
                content,
            },
        };

        self.history.insert_node(node);
        self.state.active_leaf = Some(node_id.clone());
        self.updated_at_s = timestamp_ms / 1000;
        node_id
    }

    /// Append a compaction checkpoint node and advance the authoritative compaction horizon (ADR-0255).
    #[allow(clippy::too_many_arguments)]
    pub fn append_compaction(
        &mut self,
        node_id: impl Into<String>,
        parent_id: Option<NodeId>,
        timestamp_ms: u64,
        summary: String,
        first_kept_node_id: String,
        tokens_before: usize,
        read_files: Vec<String>,
        modified_files: Vec<String>,
    ) -> NodeId {
        let node_id = node_id.into();
        let seq = self.history.next_seq();

        let node = CausalNode {
            id: node_id.clone(),
            parent_id,
            seq,
            timestamp_ms,
            kind: NodeKind::Compaction,
            payload: NodePayload::Compaction {
                summary,
                first_kept_node_id,
                tokens_before,
                read_files,
                modified_files,
                belief_state: None,
            },
        };

        self.history.insert_node(node);
        self.state.compaction_horizon = Some(node_id.clone());
        self.updated_at_s = timestamp_ms / 1000;
        node_id
    }

    /// Resolve the active linear branch (Pass 1 of compiler lowering).
    pub fn resolve_active_branch(&self) -> Vec<&CausalNode> {
        match &self.state.active_leaf {
            Some(leaf_id) => self.history.linear_path(leaf_id),
            None => Vec::new(),
        }
    }

    /// Extract messages along the active branch for consumption.
    pub fn resolve_active_messages(&self) -> Vec<Message> {
        self.resolve_active_branch()
            .into_iter()
            .filter_map(|node| match &node.payload {
                NodePayload::Message { message } => Some(message.clone()),
                NodePayload::Observation {
                    call_id,
                    tool_name,
                    lifecycle,
                    ..
                } => Some(lifecycle.lower_to_message(call_id, tool_name)),
                NodePayload::Compaction {
                    summary,
                    read_files,
                    modified_files,
                    ..
                } => {
                    let mut content = format!(
                        "[Conversation Summary Checkpoint — inspect with handle \"fold:{}\"]:\n{summary}",
                        node.id
                    );
                    if !read_files.is_empty() || !modified_files.is_empty() {
                        content.push_str("\n\n### Tracked Files:\n");
                        if !modified_files.is_empty() {
                            content.push_str(&format!("- Modified: {}\n", modified_files.join(", ")));
                        }
                        if !read_files.is_empty() {
                            content.push_str(&format!("- Consulted: {}\n", read_files.join(", ")));
                        }
                    }
                    Some(Message::new(Role::System, content))
                }
                _ => None,
            })
            .collect()
    }
}

/// An immutable, append-only causal directed acyclic graph of conversation events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CausalGraph {
    /// Node map keyed by unique NodeId.
    pub nodes: HashMap<NodeId, CausalNode>,
    /// Root node ID of the graph.
    pub root_id: Option<NodeId>,
    /// Highest assigned sequence watermark.
    pub max_seq: u64,
}

impl CausalGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next monotonically increasing sequence number.
    pub fn next_seq(&mut self) -> u64 {
        self.max_seq += 1;
        self.max_seq
    }

    /// Insert a node into the graph.
    pub fn insert_node(&mut self, node: CausalNode) {
        if node.parent_id.is_none() && self.root_id.is_none() {
            self.root_id = Some(node.id.clone());
        }
        if node.seq > self.max_seq {
            self.max_seq = node.seq;
        }
        self.nodes.insert(node.id.clone(), node);
    }

    /// Look up a node by its identifier.
    pub fn get_node(&self, id: &str) -> Option<&CausalNode> {
        self.nodes.get(id)
    }

    /// Find all immediate children of a parent node.
    pub fn children_of(&self, parent_id: &str) -> Vec<&CausalNode> {
        let mut children: Vec<&CausalNode> = self
            .nodes
            .values()
            .filter(|n| n.parent_id.as_deref() == Some(parent_id))
            .collect();
        children.sort_by_key(|n| n.seq);
        children
    }

    /// Retrieve all leaf nodes (nodes with no children).
    pub fn leaves(&self) -> Vec<NodeId> {
        let mut parent_set = std::collections::HashSet::new();
        for node in self.nodes.values() {
            if let Some(ref p) = node.parent_id {
                parent_set.insert(p.as_str());
            }
        }
        self.nodes
            .keys()
            .filter(|k| !parent_set.contains(k.as_str()))
            .cloned()
            .collect()
    }

    /// Trace the linear lineage from a target leaf backward to root or compaction anchor.
    /// Returns the nodes ordered chronologically (root/anchor -> leaf).
    pub fn linear_path(&self, leaf_id: &str) -> Vec<&CausalNode> {
        self.linear_path_with_horizon(leaf_id, None)
    }

    /// Trace linear lineage stopping at the given horizon or compaction anchor (ADR-0255).
    pub fn linear_path_with_horizon(
        &self,
        leaf_id: &str,
        horizon: Option<&str>,
    ) -> Vec<&CausalNode> {
        let mut path = Vec::new();
        let mut current_id = Some(leaf_id);

        let horizon_node = horizon.and_then(|h| self.nodes.get(h));
        let first_kept_id = horizon_node.and_then(|h| match &h.payload {
            NodePayload::Compaction {
                first_kept_node_id, ..
            } => Some(first_kept_node_id.as_str()),
            _ => None,
        });

        while let Some(id) = current_id {
            if let Some(node) = self.nodes.get(id) {
                let is_compaction =
                    matches!(node.kind, NodeKind::Compaction) || horizon == Some(id);
                path.push(node);
                if is_compaction {
                    // Compaction acts as a causal horizon; ancestor nodes prior to
                    // compaction anchor are elided from the active linear view.
                    break;
                }
                // ADR-0275 / ADR-0278: Facts are immutable and parent edges never mutate.
                // When we reach first_kept_node_id, anchor to the horizon node without
                // modifying the preserved tail's head node parent_id.
                if let Some(fkid) = first_kept_id
                    && id == fkid
                {
                    if let Some(h) = horizon_node {
                        path.push(h);
                    }
                    break;
                }
                current_id = node.parent_id.as_deref();
            } else {
                break;
            }
        }

        path.reverse();
        path
    }
}

/// A discrete immutable fact node in the causal graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CausalNode {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub seq: u64,
    pub timestamp_ms: u64,
    pub kind: NodeKind,
    pub payload: NodePayload,
}

/// Classification of causal graph nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Dialogue turns: User, Assistant, or Tool result messages.
    Dialogue,
    /// First-class tool execution observation (ADR-0282).
    Observation,
    /// Compaction horizon replacing historical turns up to a checkpoint.
    Compaction,
    /// Execution termination: human interrupt, timeout, or fatal fault.
    Termination,
    /// Asynchronous system notification / background wake.
    SystemNotice,
}

/// Detailed payload of a causal graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum NodePayload {
    Message {
        message: Message,
    },
    /// First-class tool execution observation fact (ADR-0282).
    Observation {
        call_id: String,
        tool_name: String,
        blob_hash: String,
        metrics: ObservationMetrics,
        lifecycle: ObservationLifecycle,
    },
    Compaction {
        summary: String,
        first_kept_node_id: String,
        tokens_before: usize,
        #[serde(default)]
        read_files: Vec<String>,
        #[serde(default)]
        modified_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        belief_state: Option<BeliefState>,
    },
    Termination {
        reason: TerminationReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupted_tool_call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    SystemNotice {
        source: String,
        notice_type: String,
        content: String,
    },
}

/// Physical metrics captured for a tool execution observation (ADR-0282).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ObservationMetrics {
    pub original_bytes: usize,
    pub original_lines: usize,
    pub original_tokens: usize,
}

/// Lifecycle states of a tool execution observation (ADR-0282).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ObservationLifecycle {
    /// State 1: Raw payload retained in working context (within token admission budget).
    Raw { content: String },
    /// State 2: Active Truncated (oversized at ingestion).
    /// Retains a bounded head and tail preview with elision metrics, plus the invariant invoice handle.
    ActiveTruncated {
        head_preview: String,
        tail_preview: String,
        retained_tokens: usize,
        omitted_bytes: usize,
    },
    /// State 3: Retired Pruned (causally superseded or garbage-collected).
    /// The payload body is entirely cleared to a lightweight tombstone, preserving causal intent and the invoice.
    Retired {
        reason: InvalidationReason,
        original_tokens: usize,
        original_lines: usize,
    },
}

impl ObservationLifecycle {
    pub fn lower_to_message(&self, call_id: &str, tool_name: &str) -> Message {
        let content = match self {
            Self::Raw { content } => content.clone(),
            Self::ActiveTruncated {
                head_preview,
                tail_preview,
                omitted_bytes,
                ..
            } => {
                format!(
                    "{head_preview}\n\n[... {omitted_bytes} bytes elided to relieve context — inspect with handle \"call:{call_id}\"]\n\n{tail_preview}"
                )
            }
            Self::Retired {
                reason,
                original_tokens,
                original_lines,
            } => {
                format!(
                    "{} {tool_name} ({original_lines} lines, {original_tokens} tokens) — reason: {} — inspect with handle \"call:{call_id}\"]",
                    crate::pressure::CLEARED_TOOL_PREFIX,
                    reason.summary()
                )
            }
        };
        let mut msg = Message::new(crate::Role::Tool, content);
        msg.tool_call_id = Some(call_id.to_string());
        msg
    }
}

/// Causal invalidation reason for transitioning an observation to Retired (ADR-0282).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvalidationReason {
    /// File read invalidated by a subsequent write/edit to the same resource.
    SupersededByMutation {
        target_resource: String,
        mutator_node_id: String,
    },
    /// Diagnostic/test execution invalidated by a subsequent run.
    SupersededByExecution { successor_node_id: String },
    /// Reclaimed under quantum budget pressure after recency quarantine expiration.
    BudgetRelief,
}

impl InvalidationReason {
    pub fn summary(&self) -> &'static str {
        match self {
            Self::SupersededByMutation { .. } => "superseded by mutation",
            Self::SupersededByExecution { .. } => "superseded by execution",
            Self::BudgetRelief => "budget relief",
        }
    }
}

/// Typed BeliefState produced by Generative Horizon Compaction (ADR-0283).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BeliefState {
    pub task_objective: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub established_facts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_state: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_plan: Vec<String>,
}

/// Causality classifier for execution terminations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminationReason {
    /// User intentionally stopped execution (Ctrl+C, cancel verb).
    UserInterrupt,
    /// Wall-clock or per-step timeout exceeded.
    Timeout,
    /// Fatal provider, network, or sandbox failure.
    FatalError { error: String },
    /// Superseded by a newer user turn before completion.
    Superseded,
}

/// Mutable working memory, cursor registers, and execution status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    /// Current active leaf cursor in the causal graph.
    pub active_leaf: Option<NodeId>,
    /// Active timeline identifier (default "main").
    #[serde(default = "default_main_timeline")]
    pub active_timeline: String,
    /// Named timeline branch cursors (ADR-0251).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub timelines: HashMap<String, TimelineCursor>,
    /// Authoritative compaction horizon (ADR-0255).
    /// Nodes strictly prior to this pointer along the active branch are folded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_horizon: Option<NodeId>,
    /// Current execution state machine position.
    pub status: ExecutionStatus,
    /// Queue of asynchronous notifications received during sleep/suspension.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_notifications: Vec<SystemNoticePayload>,
    /// Unidirectional exhaustion flag for budget-driven pruning (ADR-0283).
    #[serde(default)]
    pub pruning_exhausted: bool,
    /// Turn counter within the current session.
    pub round_counter: u64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            active_leaf: None,
            active_timeline: "main".to_string(),
            timelines: HashMap::new(),
            compaction_horizon: None,
            status: ExecutionStatus::Idle,
            pending_notifications: Vec::new(),
            pruning_exhausted: false,
            round_counter: 0,
        }
    }
}

fn default_main_timeline() -> String {
    "main".to_string()
}

/// A named branch cursor on the Causal DAG (ADR-0251).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCursor {
    pub id: String,
    pub name: String,
    pub kind: TimelineKind,
    pub head_node: Option<NodeId>,
    pub forked_from_node: Option<NodeId>,
    pub created_at_s: u64,
    pub updated_at_s: u64,
}

/// Semantic kind of a timeline branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineKind {
    Main,
    Aside,
    Speculation,
}

impl SessionState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Dynamic execution status of the session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExecutionStatus {
    #[default]
    Idle,
    Running {
        turn: u64,
        started_at_ms: u64,
    },
    Suspended {
        reason: SuspensionReason,
    },
}

/// Specific cause of an execution suspension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SuspensionReason {
    /// Waiting for human authorization to execute a restricted tool.
    NeedsApproval {
        tool_call_id: String,
        action: String,
    },
    /// Waiting for required user clarification or input.
    NeedsInput { prompt: String },
    /// Waiting for automated transient retry with backoff.
    RetryPending { retry_token: String, attempts: u32 },
}

/// Payload for pending notifications held in the state mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemNoticePayload {
    pub source: String,
    pub notice_type: String,
    pub content: String,
    pub timestamp_ms: u64,
}

/// Declarative governance policy, capabilities, and resource boundaries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionPolicy {
    /// Cognitive rules, persona, and workspace conventions.
    pub rules: RuleSet,
    /// Capabilities granted to the session.
    pub capabilities: CapabilityPolicy,
    /// Safety boundaries and approval guardrails.
    pub guardrails: GuardrailPolicy,
    /// Resource consumption budgets and compaction triggers.
    pub budget: BudgetPolicy,
}

/// Cognitive rules and prompt instructions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    /// System baseline identity and core behavioral policies.
    pub system_persona: Option<String>,
    /// Workspace root directory path.
    pub workspace_root: Option<String>,
    /// Project rules read from repository governance (e.g. AGENTS.md).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_rules: Vec<String>,
}

/// Capabilities and tools available to the model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityPolicy {
    /// Whitelisted tool names permitted in this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_tools: Vec<String>,
    /// Explicitly disabled tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    /// Pinned model provider connection name.
    pub provider_pin: Option<String>,
}

/// Safety guardrails and approval requirements.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardrailPolicy {
    /// Whether the session is in unattended mode (auto-approves within policy).
    pub unattended: bool,
    /// Tool names that strictly require explicit human approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_approval_tools: Vec<String>,
}

/// Context window budgets and compaction triggers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetPolicy {
    /// Maximum model context window in tokens.
    pub max_context_tokens: usize,
    /// Token threshold triggering background compaction.
    pub compaction_trigger_tokens: usize,
    /// Maximum tokens reserved for tool output results.
    pub max_tool_output_tokens: usize,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            compaction_trigger_tokens: 96_000,
            max_tool_output_tokens: 8_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    #[test]
    fn test_session_ir_initialization_and_append() {
        let mut ir = SessionIR::new("session-1", SessionPolicy::default(), 1000);
        assert_eq!(ir.session_id, "session-1");
        assert!(ir.state.active_leaf.is_none());

        let msg1 = Message::new(Role::User, "Hello nuo");
        let id1 = ir.append_message("node-1", 1_000_000, msg1);

        assert_eq!(ir.state.active_leaf, Some(id1.clone()));
        assert_eq!(ir.history.nodes.len(), 1);
        assert_eq!(ir.history.max_seq, 1);

        let msg2 = Message::new(Role::Assistant, "Hello! How can I help?");
        let id2 = ir.append_message("node-2", 1_001_000, msg2);

        assert_eq!(ir.state.active_leaf, Some(id2.clone()));
        assert_eq!(ir.history.nodes.len(), 2);
        assert_eq!(ir.history.max_seq, 2);

        let path = ir.resolve_active_branch();
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].id, id1);
        assert_eq!(path[1].id, id2);

        // Test fork
        let forked = ir.fork("session-fork-1");
        assert_eq!(forked.session_id, "session-fork-1");
        assert_eq!(forked.parent_session_id, Some("session-1".into()));
        assert_eq!(forked.history.nodes.len(), 2);
        assert_eq!(forked.state.active_leaf, Some(id2));
        assert_eq!(forked.state.round_counter, 0);
    }

    #[test]
    fn test_session_ir_timelines() {
        let mut ir = SessionIR::new("session-timeline-test", SessionPolicy::default(), 1000);
        let n1 = ir.append_message("n1", 1_000_000, Message::new(Role::User, "Main 1"));
        let n2 = ir.append_message(
            "n2",
            1_001_000,
            Message::new(Role::Assistant, "Main response 1"),
        );

        // Create an aside timeline branching off n2
        let aside_id = ir.create_timeline("aside-1", "Explain this detail", TimelineKind::Aside);
        assert_eq!(ir.state.active_timeline, aside_id);
        assert_eq!(ir.state.active_leaf, Some(n2.clone()));

        // Append to aside timeline
        let a1 = ir.append_message("a1", 1_002_000, Message::new(Role::User, "Aside question"));
        let a2 = ir.append_message(
            "a2",
            1_003_000,
            Message::new(Role::Assistant, "Aside answer"),
        );

        let aside_path = ir.resolve_timeline_branch(&aside_id);
        assert_eq!(aside_path.len(), 4);
        assert_eq!(aside_path[0].id, n1);
        assert_eq!(aside_path[1].id, n2);
        assert_eq!(aside_path[2].id, a1);
        assert_eq!(aside_path[3].id, a2);

        // Switch back to main timeline
        assert!(ir.switch_timeline("main"));
        assert_eq!(ir.state.active_leaf, Some(n2.clone()));

        // Append to main timeline
        let n3 = ir.append_message("n3", 1_004_000, Message::new(Role::User, "Main 2"));
        let main_path = ir.resolve_timeline_branch("main");
        assert_eq!(main_path.len(), 3);
        assert_eq!(main_path[0].id, n1);
        assert_eq!(main_path[1].id, n2);
        assert_eq!(main_path[2].id, n3);
    }

    #[test]
    fn test_session_ir_forensic_interrupt() {
        let mut ir = SessionIR::new("session-2", SessionPolicy::default(), 1000);
        ir.state.status = ExecutionStatus::Running {
            turn: 1,
            started_at_ms: 1_000_000,
        };

        let user_msg = Message::new(Role::User, "Run long test");
        ir.append_message("node-1", 1_000_000, user_msg);

        // User hits Ctrl+C
        let term_id = ir.record_termination(
            "node-term",
            1_005_000,
            TerminationReason::UserInterrupt,
            Some("Compiling...".to_string()),
            Some("call_123".to_string()),
            Some(5000),
        );

        assert_eq!(ir.state.status, ExecutionStatus::Idle);
        assert_eq!(ir.state.active_leaf, Some(term_id.clone()));

        let term_node = ir.history.get_node(&term_id).unwrap();
        assert_eq!(term_node.kind, NodeKind::Termination);
        if let NodePayload::Termination {
            reason,
            partial_output,
            interrupted_tool_call_id,
            duration_ms,
        } = &term_node.payload
        {
            assert_eq!(*reason, TerminationReason::UserInterrupt);
            assert_eq!(partial_output.as_deref(), Some("Compiling..."));
            assert_eq!(interrupted_tool_call_id.as_deref(), Some("call_123"));
            assert_eq!(*duration_ms, Some(5000));
        } else {
            panic!("expected termination payload");
        }
    }

    #[test]
    fn test_causal_graph_branching_and_leaves() {
        let mut graph = CausalGraph::new();

        let n1 = CausalNode {
            id: "root".into(),
            parent_id: None,
            seq: 1,
            timestamp_ms: 100,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, "Root"),
            },
        };
        let n2a = CausalNode {
            id: "branch-a".into(),
            parent_id: Some("root".into()),
            seq: 2,
            timestamp_ms: 200,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::Assistant, "Path A"),
            },
        };
        let n2b = CausalNode {
            id: "branch-b".into(),
            parent_id: Some("root".into()),
            seq: 3,
            timestamp_ms: 300,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::Assistant, "Path B"),
            },
        };

        graph.insert_node(n1);
        graph.insert_node(n2a);
        graph.insert_node(n2b);

        let children = graph.children_of("root");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].id, "branch-a");
        assert_eq!(children[1].id, "branch-b");

        let mut leaves = graph.leaves();
        leaves.sort();
        assert_eq!(leaves, vec!["branch-a", "branch-b"]);

        let path_b = graph.linear_path("branch-b");
        assert_eq!(path_b.len(), 2);
        assert_eq!(path_b[0].id, "root");
        assert_eq!(path_b[1].id, "branch-b");
    }

    #[test]
    fn test_compaction_linear_horizon() {
        let mut graph = CausalGraph::new();

        let n1 = CausalNode {
            id: "n1".into(),
            parent_id: None,
            seq: 1,
            timestamp_ms: 100,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, "Ancient turn"),
            },
        };
        let n2 = CausalNode {
            id: "compaction-checkpoint".into(),
            parent_id: Some("n1".into()),
            seq: 2,
            timestamp_ms: 200,
            kind: NodeKind::Compaction,
            payload: NodePayload::Compaction {
                summary: "Summary of ancient turns".into(),
                first_kept_node_id: "n1".into(),
                tokens_before: 5000,
                read_files: vec![],
                modified_files: vec![],
                belief_state: None,
            },
        };
        let n3 = CausalNode {
            id: "n3".into(),
            parent_id: Some("compaction-checkpoint".into()),
            seq: 3,
            timestamp_ms: 300,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, "Recent turn"),
            },
        };

        graph.insert_node(n1);
        graph.insert_node(n2);
        graph.insert_node(n3);

        let path = graph.linear_path("n3");
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].id, "compaction-checkpoint");
        assert_eq!(path[1].id, "n3");
    }
}
