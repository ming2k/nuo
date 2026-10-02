//! Canonical domain shapes for the context lifecycle (ADR-0275 §1–§2).
//!
//! These are the *target* shapes, introduced in Phase A. They are pure values
//! (no I/O); the persistence schema, the planner, and the compiler that consume
//! them arrive in later phases (ADR-0280 §6). `SessionIR` remains the aggregate
//! view over these objects; the fact, task, and artifact records are not
//! competing sources of truth.

use crate::context_lifecycle::axes::{
    Capture, Deletion, Representation, Retention, Sensitivity, SourceAuthority, Validity,
};
use crate::context_lifecycle::ids::{
    ArtifactId, BranchId, CheckpointId, ExecutionId, FactId, RequestId, RoundId, TaskId, TurnId,
    ViewId,
};

/// An immutable execution fact (ADR-0275 §2).
///
/// Facts never change after commit; projection and compaction never modify a
/// payload or an edge (`INV-FACT-02`). `parent_ids` records the immutable
/// ancestry; a checkpoint is *not* inserted here as a parent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FactNode {
    /// Stable identity.
    pub id: FactId,
    /// Owning session.
    pub session_id: String,
    /// The branch the fact was first created on (shared by forks).
    pub branch_origin: BranchId,
    /// Immutable ancestry: the facts this one descends from.
    pub parent_ids: Vec<FactId>,
    /// Monotonic sequence within the session.
    pub seq: u64,
    /// Producing round.
    pub round_id: RoundId,
    /// Producing turn.
    pub turn_id: TurnId,
    /// The payload (opaque to this layer; a message, observation, or notice).
    pub payload: FactPayload,
    /// Provenance authority (a summary never outranks its sources).
    pub source_authority: SourceAuthority,
    /// Sensitivity class governing scrub and purge reach.
    pub sensitivity: Sensitivity,
    /// Raw artifacts this fact references.
    pub artifact_refs: Vec<ArtifactId>,
}

/// The payload variants a fact can carry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactPayload {
    /// A user-authored message.
    UserMessage {
        /// The message text.
        text: String,
    },
    /// An assistant message (inference, not fact by itself).
    AssistantMessage {
        /// The message text.
        text: String,
    },
    /// An observation produced by executing a tool.
    Observation(Box<Observation>),
    /// A committed termination (cancel/error/supersede).
    Termination {
        /// Why the turn terminated.
        reason: String,
    },
}

/// The mutable, revisioned cursor and view pointer of a branch (ADR-0275 §2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BranchState {
    /// Branch identity.
    pub branch_id: BranchId,
    /// Head fact.
    pub head_fact_id: FactId,
    /// Optimistic-concurrency revision.
    pub revision: u64,
    /// The active task, if any.
    pub active_task_id: Option<TaskId>,
    /// The active committed view, if any.
    pub active_view_id: Option<ViewId>,
    /// The execution cursor (last committed execution).
    pub execution_cursor: Option<ExecutionId>,
}

/// A user requirement revision (ADR-0275 §1).
///
/// A correction appends a revision; the original is never overwritten, and an
/// inferred "intent" is never promoted to authority automatically.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequirementRevision {
    /// The fact carrying the requirement text.
    pub source_fact_id: FactId,
    /// Whether this is user-authored or a derived candidate.
    pub source_authority: SourceAuthority,
    /// The bounded requirement text or a handle to it.
    pub text: String,
}

/// A cross-round objective with its unresolved work (ADR-0275 §1–§2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskState {
    /// Task identity.
    pub task_id: TaskId,
    /// Objective, referencing the originating user requirement.
    pub objective_fact_id: FactId,
    /// Ordered requirement revisions.
    pub requirements: Vec<RequirementRevision>,
    /// Unresolved items.
    pub open_items: Vec<String>,
    /// Whether the task is still open (governs GC roots and deletion cascade).
    pub open: bool,
}

/// A tool observation (ADR-0275 §2, ADR-0276).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Observation {
    /// The execution that produced it.
    pub execution_id: ExecutionId,
    /// The normalized resource identity and version (opaque string).
    pub resource_version: String,
    /// The observed range, when the observation is a slice.
    pub range: Option<(u64, u64)>,
    /// Exit status text (`"0"`, `"1"`, a signal, …).
    pub exit_status: String,
    /// Capture completeness of the raw stream.
    pub capture: Capture,
    /// The published raw artifact, when one exists.
    pub artifact_id: Option<ArtifactId>,
    /// The deterministic preview shown to the model.
    pub preview: String,
}

/// The execution state of a tool call (ADR-0275 §7).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// Dispatched; no result yet.
    Dispatched,
    /// A result was committed.
    Committed {
        /// The result facts.
        result_fact_ids: Vec<FactId>,
    },
    /// A crash after dispatch and before the result commit: the outcome is
    /// unknown and the call is never replayed automatically (`INV-FACT-06`).
    OutcomeUnknown,
    /// Terminated without a result (cancel/supersede).
    Aborted {
        /// The termination reason.
        reason: String,
    },
}

/// The durable record of one tool execution across dispatch and result commit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionRecord {
    /// Execution identity.
    pub execution_id: ExecutionId,
    /// The provider call ID, for wire pairing only.
    pub provider_call_id: String,
    /// Authorization result.
    pub authorized: bool,
    /// Current outcome.
    pub outcome: ExecutionOutcome,
    /// Confirmed effects (only a confirmed effect is a modification fact).
    pub confirmed_effects: Vec<String>,
}

/// One entry of a [`ContextView`]: a fact's representation *in this view*.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepresentationEntry {
    /// The referenced fact.
    pub fact_id: FactId,
    /// How the view represents it.
    pub representation: Representation,
    /// Validity relative to this branch and versions.
    pub validity: Validity,
}

/// A committed branch-local context view (ADR-0275 §2, ADR-0278 §1).
///
/// A view is derived; committing one never writes facts. `tail_after_fact_id`
/// points at a real ancestry boundary, so reading the tail needs no scan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextView {
    /// View identity.
    pub view_id: ViewId,
    /// Owning branch.
    pub branch_id: BranchId,
    /// The fact revision this view was derived from.
    pub basis_revision: u64,
    /// The active checkpoint, if any.
    pub checkpoint_id: Option<CheckpointId>,
    /// The real ancestry boundary the tail begins after.
    pub tail_after_fact_id: Option<FactId>,
    /// Per-fact representation/validity for the folded region.
    pub representations: Vec<RepresentationEntry>,
    /// The policy revision under which the view was planned.
    pub policy_revision: u32,
}

/// One hash-verified source interval of a checkpoint (ADR-0278 §1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceInterval {
    /// Inclusive start sequence.
    pub from_seq: u64,
    /// Inclusive end sequence.
    pub to_seq: u64,
    /// Hash of the covered facts, for verification.
    pub hash: String,
}

/// The ordered, hash-verified manifest of a checkpoint's sources.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceManifest {
    /// The covered intervals.
    pub intervals: Vec<SourceInterval>,
}

/// A derived checkpoint (ADR-0275 §2, ADR-0278).
///
/// A checkpoint *references* a fact interval and the prior checkpoint; it is
/// never inserted as a new execution parent and no edge is reparented.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Checkpoint {
    /// Checkpoint identity.
    pub checkpoint_id: CheckpointId,
    /// The facts this checkpoint summarizes.
    pub source_manifest: SourceManifest,
    /// The previous checkpoint, if any.
    pub prior_checkpoint_id: Option<CheckpointId>,
    /// Mandatory-state-preserving structured summary.
    pub summary: String,
    /// Facts the summary must keep referenced (constraints, open items).
    pub mandatory_fact_refs: Vec<FactId>,
    /// The derived authority class (never above its sources).
    pub source_authority: SourceAuthority,
}

/// The durable manifest of a published raw artifact (ADR-0276).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactManifest {
    /// Scoped artifact identity.
    pub artifact_id: ArtifactId,
    /// Content hash / chunk manifest root.
    pub content_hash: String,
    /// Media type.
    pub media_type: String,
    /// Stream label (`stdout`/`stderr`/`binary`/`image`).
    pub stream: String,
    /// Capture completeness.
    pub capture: Capture,
    /// Byte count retained.
    pub byte_count: u64,
    /// Retention class.
    pub retention: Retention,
    /// Sensitivity class (never lower than a preview derived from it).
    pub sensitivity: Sensitivity,
    /// Deletion state.
    pub deletion: Deletion,
}

/// One ordered block of a compiled request (ADR-0277).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestBlock {
    /// Stable block role (for deterministic ordering and cache fingerprinting).
    pub role: String,
    /// Hash of the serialized block.
    pub hash: String,
    /// Token count as metered at compile time.
    pub tokens: u64,
}

/// The immutable manifest of a compiled request (ADR-0275 §7, ADR-0277).
///
/// The manifest contains no authentication headers; once a raw artifact is
/// purged it supports structural audit only, never a full replay claim.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestManifest {
    /// Request identity.
    pub request_id: RequestId,
    /// The branch the request was compiled for.
    pub branch_id: BranchId,
    /// The fact revision and policy revision compiled against.
    pub basis_revision: u64,
    /// The policy revision.
    pub policy_revision: u32,
    /// Ordered blocks with hashes and token counts.
    pub blocks: Vec<RequestBlock>,
    /// The input ceiling this request was admitted under.
    pub input_ceiling: u64,
}

/// A minimal deletion tombstone (ADR-0279 §6).
///
/// Retains structure without the original content; the payload and every
/// derived copy are reclaimed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Tombstone {
    /// The purged identity.
    pub fact_id: FactId,
    /// Structural relations kept for audit.
    pub parent_ids: Vec<FactId>,
    /// Deletion state (always `Purged`).
    pub deletion: Deletion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_lifecycle::axes::SourceAuthority;

    #[test]
    fn a_fact_round_trips_and_keeps_its_ancestry() {
        let fact = FactNode {
            id: FactId::from("f2"),
            session_id: "s1".into(),
            branch_origin: BranchId::from("main"),
            parent_ids: vec![FactId::from("f1")],
            seq: 2,
            round_id: RoundId::from("r1"),
            turn_id: TurnId::from("t1"),
            payload: FactPayload::UserMessage {
                text: "do the thing".into(),
            },
            source_authority: SourceAuthority::User,
            sensitivity: Sensitivity::Internal,
            artifact_refs: vec![],
        };
        let json = serde_json::to_string(&fact).unwrap();
        let back: FactNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, fact);
        assert_eq!(back.parent_ids, vec![FactId::from("f1")]);
    }

    #[test]
    fn a_checkpoint_references_facts_but_has_no_parent_edge() {
        // Structurally, a Checkpoint has no `parent_ids`: it cannot become an
        // execution parent (INV-CKPT-02).
        let cp = Checkpoint {
            checkpoint_id: CheckpointId::from("k1"),
            source_manifest: SourceManifest {
                intervals: vec![SourceInterval {
                    from_seq: 1,
                    to_seq: 4,
                    hash: "h".into(),
                }],
            },
            prior_checkpoint_id: None,
            summary: "phase 1".into(),
            mandatory_fact_refs: vec![FactId::from("f1")],
            source_authority: SourceAuthority::Derived,
        };
        assert_eq!(cp.source_manifest.intervals[0].to_seq, 4);
        // A summary is Derived authority — never above its sources.
        assert_eq!(cp.source_authority, SourceAuthority::Derived);
    }

    #[test]
    fn unknown_execution_state_is_explicit() {
        let record = ExecutionRecord {
            execution_id: ExecutionId::from("e1"),
            provider_call_id: "call_x".into(),
            authorized: true,
            outcome: ExecutionOutcome::OutcomeUnknown,
            confirmed_effects: vec![],
        };
        assert!(matches!(record.outcome, ExecutionOutcome::OutcomeUnknown));
    }

    #[test]
    fn tombstone_keeps_structure_without_payload() {
        let t = Tombstone {
            fact_id: FactId::from("f9"),
            parent_ids: vec![FactId::from("f8")],
            deletion: Deletion::Purged,
        };
        assert!(!t.deletion.is_readable());
        assert_eq!(t.parent_ids.len(), 1);
    }
}
