//! Governance, safety, and policy enforcement subsystem.
//!
//! Provides execution containment, permission gating, command safety checks,
//! interaction controllers, and trajectory loop supervision.

pub(crate) mod bash_policy;
pub mod guard;
pub mod interaction;
pub(crate) mod permission_policy;
pub(crate) mod permission_store;
pub(crate) mod scoped_tools;
pub(crate) mod shell_input;
pub mod stream_loop_detector;
pub mod trajectory_guard;

pub(crate) use scoped_tools::ScopedToolDisable;

pub use guard::{GuardAction, RoundGuardState};
pub use interaction::{InteractionConfig, InteractionController};
pub use stream_loop_detector::{DegeneratePattern, StreamLoopDetector};
pub use trajectory_guard::TrajectoryLoopGuard;
