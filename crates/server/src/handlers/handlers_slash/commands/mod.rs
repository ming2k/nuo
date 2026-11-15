//! Per-command handlers for `AgentRequest::SlashCommand`.
//!
//! Each built-in lives in its own async fn so the dispatcher stays a thin
//! router. One giant async fn accumulated a ~1.6 MiB debug-build stack frame
//! (rustc does not reuse stack slots across a 1700-line state machine), which
//! overflowed the server's 2 MiB Tokio worker stack on every slash command.
//!
//! Handlers are grouped into domain submodules; this module re-exports every
//! dispatched entry point so the router keeps addressing `commands::<name>`.

mod automation;
mod misc;
mod provider;
mod security;
mod thread;
mod workspace;

pub(crate) use automation::{confinement, jobs, unattended};
pub(crate) use misc::{exit, help, stats, trace, user_command};
pub(crate) use provider::{compact, quota, retry, usage};
pub(crate) use security::{permissions, skills};
pub(crate) use thread::{fork, new_session, role, search, sessions, tree};
pub(crate) use workspace::{diff, export, init, trust, undo};
