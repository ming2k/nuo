//! Context-lifecycle domain contracts (ADR-0275 – ADR-0280).
//!
//! This module is the **Phase A** substrate of the unified context architecture:
//! immutable execution facts, branch-local context views, the orthogonal
//! lifecycle axes, budget arithmetic, execution-group closure, scoped retrieval
//! handles, and the versioned context policy. It is pure domain (ADR-0005): no
//! I/O, no clocks, no provider calls.
//!
//! It intentionally introduces new types *beside* the existing `session_ir`
//! model rather than mutating it in place: the cutover that removes the legacy
//! representation is Phase F (ADR-0280 §5–§6), and no release may carry two
//! online execution paths. Until then these types are the canonical *shape*
//! that later phases wire into persistence and the request pipeline.
//!
//! Modules:
//! - [`ids`]: typed, scope-qualified identifiers.
//! - [`axes`]: the orthogonal lifecycle axes and payload classification.
//! - [`budget`]: `I = W - O - F`, watermarks, and typed admission errors.
//! - [`group`]: execution-group closure (the safe-to-split predicate).
//! - [`retrieval`]: scoped inspect handles, cursors, and the error taxonomy.
//! - [`policy`]: the versioned `context` policy and the legacy remap.
//! - [`model`]: the canonical fact/view/checkpoint/artifact shapes.

pub mod axes;
pub mod budget;
pub mod group;
pub mod ids;
pub mod model;
pub mod policy;
pub mod retrieval;

pub use axes::{
    Capture, Deletion, Representation, Retention, Sensitivity, SourceAuthority, Validity,
};
pub use budget::{
    AdmissionError, Budget, BudgetComponent, BudgetProvenance, CAS_MAX_ATTEMPTS, FramingReserve,
    ModelBudgetContract, ModelWindow, OutputReserve, PLANNING_MAX_ITERATIONS, WatermarkPolicy,
};
pub use group::{ExecutionGroup, ExternalExecution, GroupCall, ResultAcceptance};
pub use ids::{
    ArtifactId, AttemptId, BranchId, CheckpointId, ExecutionId, FactId, RequestId, RoundId, TaskId,
    TurnId, ViewId,
};
pub use model::{
    ArtifactManifest, BranchState, Checkpoint, ContextView, ExecutionOutcome, ExecutionRecord,
    FactNode, FactPayload, Observation, RepresentationEntry, RequestBlock, RequestManifest,
    RequirementRevision, SourceInterval, SourceManifest, TaskState, Tombstone,
};
pub use policy::{
    CONTEXT_POLICY_SCHEMA_VERSION, ContextPolicy, LegacyKeyRejected, PolicyError,
    reject_legacy_runtime_key,
};
pub use retrieval::{
    AuthScope, CursorBinding, HandleScheme, HandleScope, InspectError, InspectHandle,
    InspectStatus, PageLimits,
};
