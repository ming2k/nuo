//! Durable conversation aggregate: Thread, Branch, and Fact.
//!
//! This module implements the model from RFC-0003 / ADR-0173:
//!
//! - a `Thread` owns immutable facts, named branch cursors, a policy, and a
//!   monotonic revision;
//! - a `ThreadFact` is one durable fact, not a ReAct turn;
//! - a `BranchState` is a cursor (head + fork origin + compaction horizon);
//! - a fork copies no facts, it only inserts a cursor;
//! - a compaction appends a fact and moves one branch's horizon.
//!
//! The model is deliberately free of providers, storage adapters, request
//! assembly, and UI policy. Those are projections and live in other layers.
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

use nuo_tool::Message;
use crate::observation::{ObservationLifecycle, ObservationMetrics, TerminationReason, Trust};

/// Current persisted thread-record schema version.
pub const THREAD_SCHEMA_VERSION: u16 = 1;

/// Current persisted fact schema version.
pub const FACT_SCHEMA_VERSION: u16 = 1;

/// Stable identity of a thread.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ThreadId(pub String);

impl ThreadId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable identity of a branch cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BranchId(pub String);

impl BranchId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn main() -> Self {
        Self("main".to_string())
    }
}

impl fmt::Display for BranchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable identity of a durable fact.
///
/// This is an opaque identity, not a sequence and not a turn. It is the key for
/// parent links, fork points, idempotent replay, and GC reachability.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FactId(pub String);

impl FactId {
    pub fn new() -> Self {
        Self(format!("fact_{}", uuid::Uuid::new_v4()))
    }

    pub fn from_raw(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl Default for FactId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for FactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable identity of a live client connection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClientId(pub String);

impl ClientId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Monotonic thread revision.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct ThreadRevision(pub u64);

impl ThreadRevision {
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A content-addressed reference to a large payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub hash: String,
    pub size: u64,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Source trust. Trust is never inferred from the MIME type.
    #[serde(default)]
    pub trust: Trust,
}

impl BlobRef {
    pub fn new(hash: impl Into<String>, size: u64, mime: impl Into<String>) -> Self {
        Self {
            hash: hash.into(),
            size,
            mime: mime.into(),
            preview: None,
            trust: Trust::default(),
        }
    }

    pub fn with_preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    pub fn untrusted(mut self) -> Self {
        self.trust = Trust::Untrusted;
        self
    }
}

/// Classification of a durable fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    Message,
    Observation,
    Compaction,
    Termination,
    SystemNotice,
}

/// Execution coordinates attached to a fact.
///
/// A round/turn is an execution coordinate, never a fact kind. Grouping is
/// metadata; projections must not require it to read a fact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

impl FactContext {
    pub fn turn(round: u64, turn: u64) -> Self {
        Self {
            round: Some(round),
            turn: Some(turn),
        }
    }
}

/// Payload of one durable fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum FactPayload {
    Message {
        message: Message,
    },
    Observation {
        call_id: String,
        tool_name: String,
        output_ref: BlobRef,
        metrics: ObservationMetrics,
        lifecycle: ObservationLifecycle,
    },
    Compaction {
        summary: String,
        first_kept_fact_id: FactId,
        tokens_before: usize,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        read_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        modified_files: Vec<String>,
    },
    Termination {
        reason: TerminationReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    SystemNotice {
        source: String,
        notice_type: String,
        content: String,
    },
}

impl FactPayload {
    pub fn kind(&self) -> FactKind {
        match self {
            Self::Message { .. } => FactKind::Message,
            Self::Observation { .. } => FactKind::Observation,
            Self::Compaction { .. } => FactKind::Compaction,
            Self::Termination { .. } => FactKind::Termination,
            Self::SystemNotice { .. } => FactKind::SystemNotice,
        }
    }

    /// The blob reference a fact owns, if any. GC roots use this; the
    /// compaction's `first_kept_fact_id` is audit metadata, not an owner.
    pub fn blob_ref(&self) -> Option<&BlobRef> {
        match self {
            Self::Observation { output_ref, .. } => Some(output_ref),
            _ => None,
        }
    }

    /// Whether this payload is valid as standalone durable content.
    pub fn validate(&self) -> Result<(), ThreadError> {
        match self {
            Self::Message { message } => {
                if message.role == nuo_tool::Role::Tool {
                    return Err(ThreadError::InvalidFact {
                        fact: FactId::from_raw("<new>"),
                        reason: "tool results are Observations, not Message facts".to_string(),
                    });
                }
                Ok(())
            }
            Self::Observation { call_id, .. } if call_id.trim().is_empty() => {
                Err(ThreadError::InvalidFact {
                    fact: FactId::from_raw("<new>"),
                    reason: "observation call id is empty".to_string(),
                })
            }
            Self::Compaction { summary, .. } if summary.trim().is_empty() => {
                Err(ThreadError::InvalidFact {
                    fact: FactId::from_raw("<new>"),
                    reason: "compaction summary is empty".to_string(),
                })
            }
            _ => Ok(()),
        }
    }
}

/// One immutable durable fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadFact {
    pub id: FactId,
    pub parent_id: Option<FactId>,
    pub seq: u64,
    pub timestamp_ms: u64,
    pub kind: FactKind,
    pub payload: FactPayload,
    pub context: FactContext,
}

impl ThreadFact {
    pub fn new(
        id: FactId,
        parent_id: Option<FactId>,
        seq: u64,
        timestamp_ms: u64,
        payload: FactPayload,
        context: FactContext,
    ) -> Self {
        let kind = payload.kind();
        Self {
            id,
            parent_id,
            seq,
            timestamp_ms,
            kind,
            payload,
            context,
        }
    }

    pub fn validate(&self) -> Result<(), ThreadError> {
        if self.id.0.trim().is_empty() {
            return Err(ThreadError::InvalidFact {
                fact: self.id.clone(),
                reason: "fact id is empty".to_string(),
            });
        }
        if self.seq == 0 {
            return Err(ThreadError::InvalidFact {
                fact: self.id.clone(),
                reason: "fact sequence must be positive".to_string(),
            });
        }
        if self.kind != self.payload.kind() {
            return Err(ThreadError::InvalidFact {
                fact: self.id.clone(),
                reason: "kind does not match payload".to_string(),
            });
        }
        self.payload.validate().map_err(|error| match error {
            ThreadError::InvalidFact { reason, .. } => ThreadError::InvalidFact {
                fact: self.id.clone(),
                reason,
            },
            other => other,
        })
    }
}

/// A persisted fact envelope. The schema version travels with the payload so a
/// reader can refuse to reinterpret bytes it does not understand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactEnvelope {
    pub schema_version: u16,
    pub fact: ThreadFact,
}

impl FactEnvelope {
    pub fn new(fact: ThreadFact) -> Self {
        Self {
            schema_version: FACT_SCHEMA_VERSION,
            fact,
        }
    }

    pub fn into_fact(self) -> Result<ThreadFact, ThreadError> {
        if self.schema_version > FACT_SCHEMA_VERSION {
            return Err(ThreadError::UnsupportedSchema {
                kind: "fact",
                version: self.schema_version,
                supported: FACT_SCHEMA_VERSION,
            });
        }
        Ok(self.fact)
    }
}

/// Branch-local policy overrides. Overrides may change model, route, and
/// effort. They cannot widen tools, permissions, or workspace scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// A named branch cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchState {
    pub id: BranchId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<FactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<FactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_horizon: Option<FactId>,
    #[serde(default)]
    pub policy: BranchPolicy,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl BranchState {
    pub fn main(timestamp_ms: u64) -> Self {
        Self {
            id: BranchId::main(),
            name: "main".to_string(),
            head: None,
            forked_from: None,
            compaction_horizon: None,
            policy: BranchPolicy::default(),
            created_at_ms: timestamp_ms,
            updated_at_ms: timestamp_ms,
        }
    }
}

/// Dynamic status of a thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    #[default]
    Active,
    Idle,
    Archived,
    Deleted,
}

/// Mutable thread state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadState {
    pub active_branch: BranchId,
    pub branches: BTreeMap<BranchId, BranchState>,
    pub round_counter: u64,
    pub status: ThreadStatus,
}

impl ThreadState {
    pub fn new(timestamp_ms: u64) -> Self {
        let main = BranchState::main(timestamp_ms);
        let mut branches = BTreeMap::new();
        branches.insert(main.id.clone(), main);
        Self {
            active_branch: BranchId::main(),
            branches,
            round_counter: 0,
            status: ThreadStatus::Active,
        }
    }

    pub fn branch(&self, id: &BranchId) -> Result<&BranchState, ThreadError> {
        self.branches
            .get(id)
            .ok_or_else(|| ThreadError::UnknownBranch(id.clone()))
    }

    pub fn branch_mut(&mut self, id: &BranchId) -> Result<&mut BranchState, ThreadError> {
        self.branches
            .get_mut(id)
            .ok_or_else(|| ThreadError::UnknownBranch(id.clone()))
    }

    pub fn active_branch(&self) -> Result<&BranchState, ThreadError> {
        self.branch(&self.active_branch)
    }
}

/// Thread-wide policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
}

/// Immutable fact DAG.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadGraph {
    facts: BTreeMap<FactId, ThreadFact>,
}

impl ThreadGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.facts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    pub fn get(&self, id: &FactId) -> Option<&ThreadFact> {
        self.facts.get(id)
    }

    pub fn contains(&self, id: &FactId) -> bool {
        self.facts.contains_key(id)
    }

    /// Iterate every stored fact, in fact-id order.
    pub fn iter(&self) -> impl Iterator<Item = &ThreadFact> {
        self.facts.values()
    }

    /// Every stored fact in sequence order.
    pub fn facts_in_seq_order(&self) -> Vec<&ThreadFact> {
        let mut facts: Vec<&ThreadFact> = self.facts.values().collect();
        facts.sort_by_key(|fact| fact.seq);
        facts
    }

    pub fn insert(&mut self, fact: ThreadFact) -> Result<(), ThreadError> {
        fact.validate()?;
        if self.facts.contains_key(&fact.id) {
            return Err(ThreadError::DuplicateFact(fact.id));
        }
        if let Some(parent) = &fact.parent_id {
            let Some(parent_fact) = self.facts.get(parent) else {
                return Err(ThreadError::MissingParent {
                    fact: fact.id,
                    parent: parent.clone(),
                });
            };
            if parent_fact.seq >= fact.seq {
                return Err(ThreadError::InvalidFact {
                    fact: fact.id,
                    reason: format!(
                        "sequence {} is not greater than parent sequence {}",
                        fact.seq, parent_fact.seq
                    ),
                });
            }
            // An append-only graph may never acquire a cycle. Walking the
            // parent chain is O(depth) and only happens on commit.
            let mut cursor = Some(parent.clone());
            let mut seen = HashSet::new();
            while let Some(id) = cursor {
                if id == fact.id {
                    return Err(ThreadError::Cycle(fact.id));
                }
                if !seen.insert(id.clone()) {
                    return Err(ThreadError::Cycle(id));
                }
                cursor = self.facts.get(&id).and_then(|node| node.parent_id.clone());
            }
        }
        if self.facts.values().any(|existing| existing.seq == fact.seq) {
            return Err(ThreadError::DuplicateSequence {
                seq: fact.seq,
                fact: fact.id,
            });
        }
        self.facts.insert(fact.id.clone(), fact);
        Ok(())
    }

    /// Mutable fact access reserved for storage-side payload offload before
    /// insertion. Committed facts are immutable; this method is only called on
    /// an in-memory aggregate that has not escaped the write path.
    pub fn fact_mut(&mut self, id: &FactId) -> Option<&mut ThreadFact> {
        self.facts.get_mut(id)
    }

    /// Remove one fact. GC is the only caller; normal writes never delete.
    pub fn remove(&mut self, id: &FactId) -> Option<ThreadFact> {
        self.facts.remove(id)
    }

    pub fn next_seq(&self) -> u64 {
        self.facts
            .values()
            .map(|fact| fact.seq)
            .max()
            .map_or(1, |seq| seq.saturating_add(1))
    }

    pub fn max_seq(&self) -> u64 {
        self.facts.values().map(|fact| fact.seq).max().unwrap_or(0)
    }
}

/// Errors from the thread model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadError {
    UnknownBranch(BranchId),
    UnknownFact(FactId),
    DuplicateFact(FactId),
    DuplicateSequence {
        seq: u64,
        fact: FactId,
    },
    MissingParent {
        fact: FactId,
        parent: FactId,
    },
    Cycle(FactId),
    InvalidFact {
        fact: FactId,
        reason: String,
    },
    FactNotLive(FactId),
    BranchNameTaken(String),
    LastBranch,
    RevisionConflict {
        expected: ThreadRevision,
        actual: ThreadRevision,
    },
    WriterLeaseLost,
    UnsupportedPromotion,
    UnsupportedSchema {
        kind: &'static str,
        version: u16,
        supported: u16,
    },
    Archived,
    Deleted,
}

impl fmt::Display for ThreadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBranch(id) => write!(f, "unknown branch '{id}'"),
            Self::UnknownFact(id) => write!(f, "unknown fact '{id}'"),
            Self::DuplicateFact(id) => write!(f, "duplicate fact '{id}'"),
            Self::DuplicateSequence { seq, fact } => {
                write!(f, "fact '{fact}' reuses sequence {seq}")
            }
            Self::MissingParent { fact, parent } => {
                write!(f, "fact '{fact}' references missing parent '{parent}'")
            }
            Self::Cycle(id) => write!(f, "fact cycle at '{id}'"),
            Self::InvalidFact { fact, reason } => {
                write!(f, "invalid fact '{fact}': {reason}")
            }
            Self::FactNotLive(id) => write!(f, "fact '{id}' is outside the branch live path"),
            Self::BranchNameTaken(name) => write!(f, "branch name '{name}' is already in use"),
            Self::LastBranch => write!(f, "cannot delete the last branch"),
            Self::RevisionConflict { expected, actual } => write!(
                f,
                "revision conflict: expected {}, got {}",
                expected.0, actual.0
            ),
            Self::WriterLeaseLost => write!(f, "writer lease was lost"),
            Self::UnsupportedPromotion => {
                write!(f, "branch promotion is unsupported in this version")
            }
            Self::UnsupportedSchema {
                kind,
                version,
                supported,
            } => write!(
                f,
                "{kind} schema version {version} is newer than supported version {supported}"
            ),
            Self::Archived => write!(f, "thread is archived and rejects writes"),
            Self::Deleted => write!(f, "thread is deleted"),
        }
    }
}

impl std::error::Error for ThreadError {}

/// Durable conversation aggregate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub id: ThreadId,
    pub parent_thread_id: Option<ThreadId>,
    /// Fact where this thread was spawned by a parent. Subagents are threads,
    /// never branches, so lineage is a thread-level fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_from_fact_id: Option<FactId>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub graph: ThreadGraph,
    pub state: ThreadState,
    pub policy: ThreadPolicy,
    pub revision: ThreadRevision,
    #[serde(default = "default_thread_schema_version")]
    pub schema_version: u16,
}

const fn default_thread_schema_version() -> u16 {
    THREAD_SCHEMA_VERSION
}

impl Thread {
    pub fn new(id: impl Into<ThreadId>, timestamp_ms: u64) -> Self {
        Self {
            id: id.into(),
            parent_thread_id: None,
            spawned_from_fact_id: None,
            created_at_ms: timestamp_ms,
            updated_at_ms: timestamp_ms,
            graph: ThreadGraph::new(),
            state: ThreadState::new(timestamp_ms),
            policy: ThreadPolicy::default(),
            revision: ThreadRevision::default(),
            schema_version: THREAD_SCHEMA_VERSION,
        }
    }

    /// Append one payload to the active branch.
    pub fn append(
        &mut self,
        payload: FactPayload,
        context: FactContext,
        timestamp_ms: u64,
    ) -> Result<FactId, ThreadError> {
        let branch = self.state.active_branch.clone();
        self.append_to(&branch, payload, context, timestamp_ms)
    }

    /// Append one payload to a specific branch.
    pub fn append_to(
        &mut self,
        branch_id: &BranchId,
        payload: FactPayload,
        context: FactContext,
        timestamp_ms: u64,
    ) -> Result<FactId, ThreadError> {
        self.ensure_writable()?;
        self.append_fact_inner(branch_id, payload, context, timestamp_ms)
    }

    fn append_fact_inner(
        &mut self,
        branch_id: &BranchId,
        payload: FactPayload,
        context: FactContext,
        timestamp_ms: u64,
    ) -> Result<FactId, ThreadError> {
        let head = {
            let branch = self.state.branch(branch_id)?;
            branch.head.clone()
        };
        let seq = self.graph.next_seq();
        let id = FactId::new();
        let fact = ThreadFact::new(id.clone(), head, seq, timestamp_ms, payload, context);
        self.graph.insert(fact)?;
        let branch = self.state.branch_mut(branch_id)?;
        branch.head = Some(id.clone());
        branch.updated_at_ms = timestamp_ms;
        self.updated_at_ms = timestamp_ms;
        self.revision = self.revision.next();
        Ok(id)
    }

    fn ensure_writable(&self) -> Result<(), ThreadError> {
        match self.state.status {
            ThreadStatus::Archived => Err(ThreadError::Archived),
            ThreadStatus::Deleted => Err(ThreadError::Deleted),
            ThreadStatus::Active | ThreadStatus::Idle => Ok(()),
        }
    }

    /// Fork a new branch from a fact on the selected branch's live path.
    pub fn fork(
        &mut self,
        from: FactId,
        name: impl Into<String>,
        policy: BranchPolicy,
        timestamp_ms: u64,
    ) -> Result<BranchId, ThreadError> {
        let active = self.state.active_branch.clone();
        self.fork_from(&active, from, name, policy, timestamp_ms)
    }

    /// Fork from a fact that must lie on `branch_id`'s live path.
    pub fn fork_from(
        &mut self,
        branch_id: &BranchId,
        from: FactId,
        name: impl Into<String>,
        policy: BranchPolicy,
        timestamp_ms: u64,
    ) -> Result<BranchId, ThreadError> {
        self.ensure_writable()?;
        if !self.path_contains(branch_id, &from)? {
            return Err(ThreadError::FactNotLive(from));
        }
        let name = name.into();
        if name.trim().is_empty() {
            return Err(ThreadError::InvalidFact {
                fact: from,
                reason: "branch name is empty".to_string(),
            });
        }
        if self
            .state
            .branches
            .values()
            .any(|branch| branch.name == name)
        {
            return Err(ThreadError::BranchNameTaken(name));
        }
        let id = BranchId(format!("branch_{}", uuid::Uuid::new_v4()));
        self.state.branches.insert(
            id.clone(),
            BranchState {
                id: id.clone(),
                name,
                head: Some(from.clone()),
                forked_from: Some(from),
                compaction_horizon: None,
                policy,
                created_at_ms: timestamp_ms,
                updated_at_ms: timestamp_ms,
            },
        );
        self.updated_at_ms = timestamp_ms;
        self.revision = self.revision.next();
        Ok(id)
    }

    /// Delete a branch cursor. Facts remain if reachable from another branch.
    pub fn delete_branch(
        &mut self,
        branch: &BranchId,
        timestamp_ms: u64,
    ) -> Result<(), ThreadError> {
        self.ensure_writable()?;
        if self.state.branches.len() <= 1 {
            return Err(ThreadError::LastBranch);
        }
        self.state
            .branches
            .remove(branch)
            .ok_or_else(|| ThreadError::UnknownBranch(branch.clone()))?;
        if &self.state.active_branch == branch
            && let Some(next) = self.state.branches.keys().next().cloned()
        {
            self.state.active_branch = next;
        }
        self.updated_at_ms = timestamp_ms;
        self.revision = self.revision.next();
        Ok(())
    }

    /// Append a compaction fact and move one branch's horizon.
    pub fn compact(
        &mut self,
        branch_id: &BranchId,
        summary: impl Into<String>,
        first_kept: FactId,
        tokens_before: usize,
        timestamp_ms: u64,
    ) -> Result<FactId, ThreadError> {
        self.compact_with_files(
            branch_id,
            summary,
            first_kept,
            tokens_before,
            Vec::new(),
            Vec::new(),
            timestamp_ms,
        )
    }

    /// Append a compaction fact with its file manifest and move one branch's
    /// horizon. The manifest is audit metadata on the fact, not a graph edge.
    #[allow(clippy::too_many_arguments)]
    pub fn compact_with_files(
        &mut self,
        branch_id: &BranchId,
        summary: impl Into<String>,
        first_kept: FactId,
        tokens_before: usize,
        read_files: Vec<String>,
        modified_files: Vec<String>,
        timestamp_ms: u64,
    ) -> Result<FactId, ThreadError> {
        self.ensure_writable()?;
        let path = self.path(branch_id)?;
        if !path.iter().any(|fact| fact.id == first_kept) {
            return Err(ThreadError::FactNotLive(first_kept));
        }
        let payload = FactPayload::Compaction {
            summary: summary.into(),
            first_kept_fact_id: first_kept.clone(),
            tokens_before,
            read_files,
            modified_files,
        };
        let id =
            self.append_fact_inner(branch_id, payload, FactContext::default(), timestamp_ms)?;
        let branch = self.state.branch_mut(branch_id)?;
        branch.compaction_horizon = Some(first_kept);
        branch.updated_at_ms = timestamp_ms;
        Ok(id)
    }

    /// Facts on one branch from horizon to head, oldest first.
    ///
    /// The horizon is inclusive: the first kept fact is the oldest visible fact.
    /// A branch with no head has an empty path.
    pub fn path(&self, branch_id: &BranchId) -> Result<Vec<&ThreadFact>, ThreadError> {
        let branch = self.state.branch(branch_id)?;
        let mut current = branch.head.clone();
        let mut seen = HashSet::new();
        let mut facts = Vec::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) {
                return Err(ThreadError::Cycle(id));
            }
            let fact = self
                .graph
                .get(&id)
                .ok_or_else(|| ThreadError::UnknownFact(id.clone()))?;
            facts.push(fact);
            if branch.compaction_horizon.as_ref() == Some(&id) {
                break;
            }
            current = fact.parent_id.clone();
        }
        facts.reverse();
        Ok(facts)
    }

    pub fn path_contains(&self, branch_id: &BranchId, fact: &FactId) -> Result<bool, ThreadError> {
        Ok(self
            .path(branch_id)?
            .iter()
            .any(|candidate| &candidate.id == fact))
    }

    /// Whether a fact is on at least one branch's live path.
    pub fn is_live(&self, fact: &FactId) -> bool {
        self.state.branches.keys().any(|branch| {
            self.path(branch)
                .map(|path| path.iter().any(|candidate| &candidate.id == fact))
                .unwrap_or(false)
        })
    }

    /// Union of all branch live paths. This is the GC root set.
    pub fn live_facts(&self) -> BTreeSet<FactId> {
        let mut live = BTreeSet::new();
        for branch in self.state.branches.values() {
            if let Ok(path) = self.path(&branch.id) {
                live.extend(path.into_iter().map(|fact| fact.id.clone()));
            }
        }
        live
    }

    /// Facts unreachable from every branch. The compaction metadata fields are
    /// not owning references, so a pre-horizon fact is GC-eligible once no
    /// sibling branch traverses it.
    pub fn gc_eligible_facts(&self) -> BTreeSet<FactId> {
        let live = self.live_facts();
        self.graph
            .facts_in_seq_order()
            .into_iter()
            .map(|fact| fact.id.clone())
            .filter(|id| !live.contains(id))
            .collect()
    }

    /// Every fact in the graph, in sequence order.
    pub fn all_facts(&self) -> Vec<&ThreadFact> {
        self.graph.facts_in_seq_order()
    }

    /// Highest fact sequence in the graph.
    pub fn max_seq(&self) -> u64 {
        self.graph.max_seq()
    }

    /// The latest compaction fact on a branch's live path, if any.
    pub fn latest_compaction(
        &self,
        branch_id: &BranchId,
    ) -> Result<Option<&ThreadFact>, ThreadError> {
        Ok(self
            .path(branch_id)?
            .into_iter()
            .rev()
            .find(|fact| matches!(fact.payload, FactPayload::Compaction { .. })))
    }

    /// Extract all mutations above `watermark_seq`.
    pub fn drain_delta(&self, watermark_seq: u64) -> ThreadDelta {
        let new_facts: Vec<ThreadFact> = self
            .graph
            .facts_in_seq_order()
            .into_iter()
            .filter(|fact| fact.seq > watermark_seq)
            .cloned()
            .collect();

        ThreadDelta {
            thread_id: self.id.clone(),
            parent_thread_id: self.parent_thread_id.clone(),
            previous_watermark_seq: watermark_seq,
            new_watermark_seq: self.max_seq().max(watermark_seq),
            revision: self.revision,
            new_facts,
            state_update: ThreadStateUpdate {
                branches: self.state.branches.values().cloned().collect(),
                removed_branches: Vec::new(),
                active_branch: Some(self.state.active_branch.clone()),
                round_counter: Some(self.state.round_counter),
                status: Some(self.state.status),
            },
            policy_update: (watermark_seq == 0).then(|| self.policy.clone()),
            updated_at_ms: self.updated_at_ms,
        }
    }

    /// Apply an external delta. Facts already present are accepted only when
    /// their full shape is identical; this makes replay idempotent.
    pub fn apply_delta(&mut self, delta: ThreadDelta) -> Result<(), ThreadError> {
        if delta.thread_id != self.id {
            return Err(ThreadError::InvalidFact {
                fact: FactId::from_raw("<delta>"),
                reason: format!(
                    "delta targets thread '{}' but this is '{}'",
                    delta.thread_id, self.id
                ),
            });
        }
        if delta.revision < self.revision {
            return Err(ThreadError::RevisionConflict {
                expected: self.revision,
                actual: delta.revision,
            });
        }
        let mut facts = delta.new_facts;
        facts.sort_by_key(|fact| fact.seq);
        for fact in facts {
            if let Some(existing) = self.graph.get(&fact.id) {
                if existing != &fact {
                    return Err(ThreadError::DuplicateFact(fact.id));
                }
                continue;
            }
            self.graph.insert(fact)?;
        }
        for branch in delta.state_update.branches {
            self.state.branches.insert(branch.id.clone(), branch);
        }
        for branch in delta.state_update.removed_branches {
            if self.state.branches.len() > 1 {
                self.state.branches.remove(&branch);
            }
        }
        if let Some(active) = delta.state_update.active_branch
            && self.state.branches.contains_key(&active)
        {
            self.state.active_branch = active;
        }
        if let Some(round_counter) = delta.state_update.round_counter {
            self.state.round_counter = round_counter;
        }
        if let Some(status) = delta.state_update.status {
            self.state.status = status;
        }
        if let Some(policy) = delta.policy_update {
            self.policy = policy;
        }
        self.revision = delta.revision;
        self.updated_at_ms = self.updated_at_ms.max(delta.updated_at_ms);
        Ok(())
    }

    /// Apply one command in memory. Storage adapters persist the resulting
    /// delta; this method is the single semantic implementation of every
    /// command.
    pub fn apply_command(
        &mut self,
        command: ThreadCommand,
        timestamp_ms: u64,
    ) -> Result<CommandOutcome, ThreadError> {
        match command {
            ThreadCommand::AppendFacts { facts } => {
                self.ensure_writable()?;
                if facts.is_empty() {
                    return Ok(CommandOutcome::Appended {
                        facts: Vec::new(),
                        revision: self.revision,
                    });
                }
                let branch_id = self.state.active_branch.clone();
                let mut expected_parent = self.state.branch(&branch_id)?.head.clone();
                let mut ids = Vec::with_capacity(facts.len());
                for fact in facts {
                    if fact.parent_id != expected_parent {
                        return Err(ThreadError::InvalidFact {
                            fact: fact.id,
                            reason: "appended fact does not continue the branch head".to_string(),
                        });
                    }
                    expected_parent = Some(fact.id.clone());
                    ids.push(fact.id.clone());
                    self.graph.insert(fact)?;
                }
                let branch = self.state.branch_mut(&branch_id)?;
                branch.head = expected_parent;
                branch.updated_at_ms = timestamp_ms;
                self.updated_at_ms = timestamp_ms;
                self.revision = self.revision.next();
                Ok(CommandOutcome::Appended {
                    facts: ids,
                    revision: self.revision,
                })
            }
            ThreadCommand::Fork {
                branch,
                from,
                name,
                policy,
            } => {
                let branch = branch.unwrap_or_else(|| self.state.active_branch.clone());
                let id = self.fork_from(&branch, from, name, policy, timestamp_ms)?;
                Ok(CommandOutcome::Forked {
                    branch: id,
                    revision: self.revision,
                })
            }
            ThreadCommand::DeleteBranch { branch } => {
                self.delete_branch(&branch, timestamp_ms)?;
                Ok(CommandOutcome::BranchDeleted {
                    branch,
                    revision: self.revision,
                })
            }
            ThreadCommand::Compact {
                branch,
                summary,
                first_kept,
                tokens_before,
            } => {
                let fact = self.compact(
                    &branch,
                    summary,
                    first_kept.clone(),
                    tokens_before,
                    timestamp_ms,
                )?;
                Ok(CommandOutcome::Compacted {
                    fact,
                    horizon: first_kept,
                    revision: self.revision,
                })
            }
            ThreadCommand::Terminate {
                branch,
                reason,
                partial_output,
            } => {
                self.ensure_writable()?;
                let fact = self.append_fact_inner(
                    &branch,
                    FactPayload::Termination {
                        reason,
                        partial_output,
                        duration_ms: None,
                    },
                    FactContext::default(),
                    timestamp_ms,
                )?;
                Ok(CommandOutcome::Terminated {
                    fact,
                    revision: self.revision,
                })
            }
            ThreadCommand::SetPolicy { policy } => {
                self.ensure_writable()?;
                self.policy = policy;
                self.updated_at_ms = timestamp_ms;
                self.revision = self.revision.next();
                Ok(CommandOutcome::PolicySet {
                    revision: self.revision,
                })
            }
            ThreadCommand::Archive => {
                if self.state.status == ThreadStatus::Deleted {
                    return Err(ThreadError::Deleted);
                }
                self.state.status = ThreadStatus::Archived;
                self.updated_at_ms = timestamp_ms;
                self.revision = self.revision.next();
                Ok(CommandOutcome::StatusChanged {
                    status: self.state.status,
                    revision: self.revision,
                })
            }
            ThreadCommand::Unarchive => {
                if self.state.status == ThreadStatus::Deleted {
                    return Err(ThreadError::Deleted);
                }
                self.state.status = ThreadStatus::Idle;
                self.updated_at_ms = timestamp_ms;
                self.revision = self.revision.next();
                Ok(CommandOutcome::StatusChanged {
                    status: self.state.status,
                    revision: self.revision,
                })
            }
            ThreadCommand::DeleteThread => {
                if self.state.status == ThreadStatus::Deleted {
                    return Ok(CommandOutcome::StatusChanged {
                        status: self.state.status,
                        revision: self.revision,
                    });
                }
                self.state.status = ThreadStatus::Deleted;
                self.updated_at_ms = timestamp_ms;
                self.revision = self.revision.next();
                Ok(CommandOutcome::StatusChanged {
                    status: self.state.status,
                    revision: self.revision,
                })
            }
            ThreadCommand::PromoteBranch { branch } => {
                // Promotion needs copy-on-write or cross-thread reachability.
                // v1 refuses explicitly instead of silently copying facts.
                let _ = branch;
                Err(ThreadError::UnsupportedPromotion)
            }
        }
    }
}

/// Shallow update to thread state carried by a delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadStateUpdate {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<BranchState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_branches: Vec<BranchId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_branch: Option<BranchId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_counter: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ThreadStatus>,
}

/// Incremental durable write unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadDelta {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    pub previous_watermark_seq: u64,
    pub new_watermark_seq: u64,
    pub revision: ThreadRevision,
    pub new_facts: Vec<ThreadFact>,
    pub state_update: ThreadStateUpdate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_update: Option<ThreadPolicy>,
    pub updated_at_ms: u64,
}

/// One command's immutable idempotency identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandMeta {
    /// Client-chosen idempotency key. Replaying it returns the prior ack.
    pub command_id: String,
    pub expected_revision: ThreadRevision,
    pub fencing_token: u64,
}

/// A command envelope. Every durable mutation is one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub meta: CommandMeta,
    pub command: ThreadCommand,
}

/// Durable conversation commands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ThreadCommand {
    AppendFacts {
        facts: Vec<ThreadFact>,
    },
    Fork {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<BranchId>,
        from: FactId,
        name: String,
        #[serde(default)]
        policy: BranchPolicy,
    },
    DeleteBranch {
        branch: BranchId,
    },
    Compact {
        branch: BranchId,
        summary: String,
        first_kept: FactId,
        tokens_before: usize,
    },
    Terminate {
        branch: BranchId,
        reason: TerminationReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_output: Option<String>,
    },
    SetPolicy {
        policy: ThreadPolicy,
    },
    Archive,
    Unarchive,
    DeleteThread,
    /// v1 returns `UnsupportedPromotion` explicitly; it never silently copies
    /// or shares facts across threads.
    PromoteBranch {
        branch: BranchId,
    },
}

/// What a command durably changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommandOutcome {
    Appended {
        facts: Vec<FactId>,
        revision: ThreadRevision,
    },
    Forked {
        branch: BranchId,
        revision: ThreadRevision,
    },
    BranchDeleted {
        branch: BranchId,
        revision: ThreadRevision,
    },
    Compacted {
        fact: FactId,
        horizon: FactId,
        revision: ThreadRevision,
    },
    Terminated {
        fact: FactId,
        revision: ThreadRevision,
    },
    PolicySet {
        revision: ThreadRevision,
    },
    StatusChanged {
        status: ThreadStatus,
        revision: ThreadRevision,
    },
}

impl CommandOutcome {
    pub fn revision(&self) -> ThreadRevision {
        match self {
            Self::Appended { revision, .. }
            | Self::Forked { revision, .. }
            | Self::BranchDeleted { revision, .. }
            | Self::Compacted { revision, .. }
            | Self::Terminated { revision, .. }
            | Self::PolicySet { revision }
            | Self::StatusChanged { revision, .. } => *revision,
        }
    }
}

/// Read-side operations. They never mutate durable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum ThreadQuery {
    Hydrate {
        thread: ThreadId,
        branch: BranchId,
    },
    BranchList {
        thread: ThreadId,
    },
    TranscriptPage {
        thread: ThreadId,
        branch: BranchId,
        before: Option<FactId>,
        limit: usize,
    },
    Fact {
        thread: ThreadId,
        fact: FactId,
    },
    Blob {
        hash: String,
        range: Option<ByteRange>,
    },
}

/// Half-open byte range for blob paging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// Single-writer lease for one thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterLease {
    pub thread_id: ThreadId,
    pub owner_id: String,
    pub fencing_token: u64,
    pub expires_at_ms: u64,
}

/// One queued user input for a branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedInput {
    pub input_id: String,
    pub client_id: ClientId,
    pub text: String,
    pub queued_at_ms: u64,
}

/// FIFO input queue for one branch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputQueue {
    pub branch_id: BranchId,
    pub pending: VecDeque<QueuedInput>,
}

impl InputQueue {
    pub fn new(branch_id: BranchId) -> Self {
        Self {
            branch_id,
            pending: VecDeque::new(),
        }
    }

    pub fn push(&mut self, input: QueuedInput) {
        self.pending.push_back(input);
    }

    pub fn pop(&mut self) -> Option<QueuedInput> {
        self.pending.pop_front()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Turn lifecycle. Exactly one non-idle turn may exist per branch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    #[default]
    Idle,
    Preparing,
    Streaming,
    ToolRunning,
    Paused,
    Completed,
    Aborted,
    Failed,
}

impl TurnState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Aborted | Self::Failed)
    }

    pub fn owns_branch(self) -> bool {
        !matches!(
            self,
            Self::Idle | Self::Completed | Self::Aborted | Self::Failed
        )
    }
}

/// Live client connection lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientState {
    #[default]
    Attached,
    Detached,
}

/// Permission snapshot visible to one client. The effective turn permission is
/// the intersection of the thread ceiling, this snapshot, and the tool risk
/// profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSnapshot {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub allowed_tools: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_roots: Vec<String>,
    #[serde(default)]
    pub unattended: bool,
}

/// One row of a client-facing transcript projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum TranscriptItem {
    Message {
        fact_id: FactId,
        message: Message,
    },
    Observation {
        fact_id: FactId,
        call_id: String,
        tool_name: String,
        output_ref: BlobRef,
        metrics: ObservationMetrics,
        lifecycle: ObservationLifecycle,
    },
    Compaction {
        fact_id: FactId,
        summary: String,
        first_kept_fact_id: FactId,
        tokens_before: usize,
    },
    Termination {
        fact_id: FactId,
        reason: TerminationReason,
        partial_output: Option<String>,
    },
    Notice {
        fact_id: FactId,
        source: String,
        notice_type: String,
        content: String,
    },
}

impl TranscriptItem {
    pub fn fact_id(&self) -> &FactId {
        match self {
            Self::Message { fact_id, .. }
            | Self::Observation { fact_id, .. }
            | Self::Compaction { fact_id, .. }
            | Self::Termination { fact_id, .. }
            | Self::Notice { fact_id, .. } => fact_id,
        }
    }
}

/// Per-client UI/export projection. There is no durable store for this type;
/// it is rebuilt from facts on attach and updated from deltas while attached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptView {
    pub thread_id: ThreadId,
    pub branch_id: BranchId,
    pub revision: ThreadRevision,
    pub items: Vec<TranscriptItem>,
    /// The oldest fact in this page, or `None` when this page reaches the
    /// branch horizon. A client pages older items with `before`.
    pub next_before: Option<FactId>,
}

impl TranscriptView {
    /// Project one page of a branch. `before` selects the page immediately
    /// older than that fact; `None` selects the newest page.
    pub fn from_thread(
        thread: &Thread,
        branch: &BranchId,
        before: Option<&FactId>,
        limit: usize,
    ) -> Result<Self, ThreadError> {
        let path = thread.path(branch)?;
        let mut end = path.len();
        if let Some(before) = before {
            end = path
                .iter()
                .position(|fact| &fact.id == before)
                .ok_or_else(|| ThreadError::FactNotLive(before.clone()))?;
        }
        let start = end.saturating_sub(limit.max(1));
        let items = path[start..end]
            .iter()
            .filter_map(|fact| transcript_item(fact))
            .collect();
        let next_before = (start > 0).then(|| path[start].id.clone());
        Ok(Self {
            thread_id: thread.id.clone(),
            branch_id: branch.clone(),
            revision: thread.revision,
            items,
            next_before,
        })
    }

    /// The newest `limit` rows of a branch, independent of which fact kinds are
    /// filtered from display.
    pub fn tail(thread: &Thread, branch: &BranchId, limit: usize) -> Result<Self, ThreadError> {
        Self::from_thread(thread, branch, None, limit)
    }
}

fn transcript_item(fact: &ThreadFact) -> Option<TranscriptItem> {
    let fact_id = fact.id.clone();
    match &fact.payload {
        FactPayload::Message { message } => Some(TranscriptItem::Message {
            fact_id,
            message: message.clone(),
        }),
        FactPayload::Observation {
            call_id,
            tool_name,
            output_ref,
            metrics,
            lifecycle,
        } => Some(TranscriptItem::Observation {
            fact_id,
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            output_ref: output_ref.clone(),
            metrics: *metrics,
            lifecycle: lifecycle.clone(),
        }),
        FactPayload::Compaction {
            summary,
            first_kept_fact_id,
            tokens_before,
            ..
        } => Some(TranscriptItem::Compaction {
            fact_id,
            summary: summary.clone(),
            first_kept_fact_id: first_kept_fact_id.clone(),
            tokens_before: *tokens_before,
        }),
        FactPayload::Termination {
            reason,
            partial_output,
            ..
        } => Some(TranscriptItem::Termination {
            fact_id,
            reason: reason.clone(),
            partial_output: partial_output.clone(),
        }),
        FactPayload::SystemNotice {
            source,
            notice_type,
            content,
        } => Some(TranscriptItem::Notice {
            fact_id,
            source: source.clone(),
            notice_type: notice_type.clone(),
            content: content.clone(),
        }),
    }
}

/// A live client/server connection. It owns connection state, permissions, and
/// a transcript page. It never owns conversation facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Client {
    pub id: ClientId,
    pub thread_id: ThreadId,
    pub branch_id: BranchId,
    pub state: ClientState,
    pub permissions: PermissionSnapshot,
    pub transcript: TranscriptView,
}

impl Client {
    pub fn detach(&mut self) {
        self.state = ClientState::Detached;
        self.transcript.items.clear();
    }
}

#[cfg(test)]
mod tests;
