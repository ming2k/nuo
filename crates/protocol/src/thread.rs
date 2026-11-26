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
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::observation::{ObservationLifecycle, ObservationMetrics, TerminationReason, Trust};
use tool::Message;

/// Current persisted thread-record schema version.
pub const THREAD_SCHEMA_VERSION: u16 = 1;

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
        /// The first fact the next request assembles from. The branch cursor's
        /// `compaction_horizon` names this same fact, and every reader derives
        /// the fold's *seam* by resolving this reference through the order it
        /// actually holds — never by parsing an id as a number
        /// (ADR-0246 `[INV-CMP-09]`).
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
                if message.role == tool::Role::Tool {
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

    /// True if any `Message` fact holds inline text or base64 image data at or
    /// above `threshold` bytes (used to skip blob offload work on the fast path).
    pub fn has_large_inline_message_blobs(&self, threshold: usize) -> bool {
        self.facts.values().any(|fact| {
            let FactPayload::Message { message } = &fact.payload else {
                return false;
            };
            message.content.len() >= threshold
                || message
                    .images
                    .as_ref()
                    .is_some_and(|imgs| imgs.iter().any(|img| img.data.len() >= threshold))
        })
    }

    /// Visit every `Message` fact payload mutably for CAS blob offloading or
    /// rehydration while keeping fact identity, lineage, and sequences
    /// structurally immutable (ADR-0255 `[INV-SHARD-04]`).
    pub fn visit_messages_mut<F>(&mut self, mut visit: F) -> Result<(), String>
    where
        F: FnMut(&FactId, &mut Message) -> Result<(), String>,
    {
        for (id, fact) in &mut self.facts {
            if let FactPayload::Message { message } = &mut fact.payload {
                visit(id, message)?;
            }
        }
        Ok(())
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
    DuplicateSequence { seq: u64, fact: FactId },
    MissingParent { fact: FactId, parent: FactId },
    Cycle(FactId),
    InvalidFact { fact: FactId, reason: String },
    FactNotLive(FactId),
    BranchNameTaken(String),
    LastBranch,
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
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub graph: ThreadGraph,
    pub state: ThreadState,
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
            created_at_ms: timestamp_ms,
            updated_at_ms: timestamp_ms,
            graph: ThreadGraph::new(),
            state: ThreadState::new(timestamp_ms),
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
        // A fold may only *keep* something already inside the request window.
        // Resolving against the conversation would let a caller move the horizon
        // backwards, which would re-enlarge the window it was asked to relieve.
        let path = self.live_path(branch_id)?;
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

    /// The branch's **conversation**: head → root, oldest first, every fact the
    /// branch reaches.
    ///
    /// A fold never removes a fact from the conversation (ADR-0246
    /// `[INV-CMP-08]`): the compaction horizon bounds the *request window*
    /// ([`Self::live_path`]), not what the thread has said. The compaction fact
    /// itself sits in the chain where it was appended — after the tail it kept,
    /// the moment the fold happened — so reading order needs no rewriting:
    /// [`crate::Transcript::conversation_messages`] is this walk, minus working
    /// state.
    ///
    /// A branch with no head has an empty path.
    pub fn path(&self, branch_id: &BranchId) -> Result<Vec<&ThreadFact>, ThreadError> {
        self.walk(branch_id, false)
    }

    /// The branch's **request window**: head → `compaction_horizon` inclusive,
    /// oldest first.
    ///
    /// The horizon is inclusive: the first kept fact is the oldest fact the next
    /// model request assembles from. This is the only walk the request path may
    /// use; every reader that answers "what did this thread say" uses
    /// [`Self::path`].
    pub fn live_path(&self, branch_id: &BranchId) -> Result<Vec<&ThreadFact>, ThreadError> {
        self.walk(branch_id, true)
    }

    fn walk(
        &self,
        branch_id: &BranchId,
        stop_at_horizon: bool,
    ) -> Result<Vec<&ThreadFact>, ThreadError> {
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
            if stop_at_horizon && branch.compaction_horizon.as_ref() == Some(&id) {
                break;
            }
            current = fact.parent_id.clone();
        }
        facts.reverse();
        Ok(facts)
    }

    /// Whether `fact` lies on the branch's conversation path. Fork targets are
    /// validated against this: folding is context relief, so a folded fact is
    /// still a legitimate fork point (ADR-0246).
    pub fn path_contains(&self, branch_id: &BranchId, fact: &FactId) -> Result<bool, ThreadError> {
        Ok(self
            .path(branch_id)?
            .iter()
            .any(|candidate| &candidate.id == fact))
    }

    /// Whether a fact is on at least one branch's conversation path.
    pub fn is_live(&self, fact: &FactId) -> bool {
        self.state.branches.keys().any(|branch| {
            self.path(branch)
                .map(|path| path.iter().any(|candidate| &candidate.id == fact))
                .unwrap_or(false)
        })
    }

    /// Union of all branch conversation paths. This is the GC root set: a fact
    /// stays reachable for the whole life of the branch that carries it, folded
    /// or not (ADR-0246 `[INV-CMP-08]`).
    pub fn live_facts(&self) -> BTreeSet<FactId> {
        let mut live = BTreeSet::new();
        for branch in self.state.branches.values() {
            if let Ok(path) = self.path(&branch.id) {
                live.extend(path.into_iter().map(|fact| fact.id.clone()));
            }
        }
        live
    }

    /// Every fact in the graph, in sequence order.
    pub fn all_facts(&self) -> Vec<&ThreadFact> {
        self.graph.facts_in_seq_order()
    }

    /// Highest fact sequence in the graph.
    pub fn max_seq(&self) -> u64 {
        self.graph.max_seq()
    }

    /// The latest compaction fact on a branch's request window, if any.
    pub fn latest_compaction(
        &self,
        branch_id: &BranchId,
    ) -> Result<Option<&ThreadFact>, ThreadError> {
        Ok(self
            .live_path(branch_id)?
            .into_iter()
            .rev()
            .find(|fact| matches!(fact.payload, FactPayload::Compaction { .. })))
    }
}

#[cfg(test)]
mod tests;
