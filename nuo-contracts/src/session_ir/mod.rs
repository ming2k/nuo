//! Canonical Session Intermediate Representation (Session IR).
//!
//! Defined by ADR-0241, Session IR is the headless, platform-agnostic domain
//! model and single source of truth for an agent conversation.
//!
//! It replaces the dual representation of `SessionTree` and `Transcript`,
//! purging all presentation artifacts, and organizes session mechanics into:
//! - [`CausalGraph`]: immutable, append-only historical facts (`history`)
//! - [`SessionState`]: mutable cursor, status machine, and event mailbox (`state`)
//! - [`SessionPolicy`]: declarative governance rules, capabilities, and budgets (`policy`)
//!
//! Modules:
//! - [`types`]: Fundamental domain structures and node definitions.
//! - [`delta`]: Incremental mutation deltas and $O(\Delta)$ sync primitives.
//! - [`compiler`]: Multi-pass request compilation targeting provider wire protocols.

pub mod compiler;
pub mod delta;
pub mod types;

pub use compiler::{
    CacheBoundary, CompilationArtifact, CompilationStats, CompilerError, CompilerOptions,
    compile_session_request,
};
pub use delta::{SessionDelta, StateUpdate};
pub use types::{
    BeliefState, BudgetPolicy, CapabilityPolicy, CausalGraph, CausalNode, ExecutionStatus,
    GuardrailPolicy, InvalidationReason, NodeId, NodeKind, NodePayload, ObservationLifecycle,
    ObservationMetrics, RuleSet, SessionIR, SessionPolicy, SessionState, SuspensionReason,
    SystemNoticePayload, TerminationReason, TimelineCursor, TimelineKind,
};
