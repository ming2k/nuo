//! Guardrails, safety, and policy enforcement subsystem for Nuo.
//!
//! Provides execution containment, permission gating, command safety checks,
//! interaction controllers, and trajectory/stream loop supervision.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod bash_policy;
pub mod guard;
pub mod interaction;
pub mod permission_policy;
pub mod permission_store;
pub mod scoped_tools;
pub mod shell_input;
pub mod stream_loop_detector;
pub mod trajectory_guard;

pub use bash_policy::{BashPolicy, BashPolicyAction, BashPolicyMatch};
pub use guard::{GuardAction, RoundGuardState};
pub use interaction::{InteractionConfig, InteractionController};
pub use permission_policy::{
    BashVerdict, HookSideEffects, PermissionChain, PermissionContext, PermissionPolicy,
    PolicyContext, PolicyDecision, PreToolUseVerdict, default_chain,
};
pub use permission_store::{PermissionRule, PermissionStore};
pub use scoped_tools::ScopedToolDisable;
pub use stream_loop_detector::{DegeneratePattern, StreamLoopDetector};
pub use trajectory_guard::TrajectoryLoopGuard;

/// Acquire a `Mutex` guard, recovering from poisoning via `into_inner()`.
#[inline]
pub(crate) fn poison_lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
