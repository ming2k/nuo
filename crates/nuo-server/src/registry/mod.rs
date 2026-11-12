//! Thread registry: owns live sessions, coordinates hosting and lifecycle.

pub(super) use crate::UiBridge;
pub(super) use crate::bootstrap::BootstrapParams;
pub(super) use crate::monitor::MonitorTracker;
pub(super) use crate::serve::{AttachAction, AttachSyncBuffer, is_attach_sync_event};
pub(super) use nuo_harness::{Agent, AgentIdentity, AgentRoleProfile};
pub(super) use nuo_persistence::session::SessionStore;
pub(super) use nuo_protocol::{
    AgentRequest, AgentResponse, MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredThread,
    PermissionDecision, ThreadFilter, ThreadHosting, ThreadOverview, ThreadRemovalReport,
    ThreadRemovalSkip, ThreadStatus,
};
pub(super) use std::collections::HashMap;
pub(super) use std::path::PathBuf;
pub(super) use std::sync::Arc;
pub(super) use tokio::sync::{Mutex, broadcast, mpsc};
pub(super) use tokio_util::sync::CancellationToken;

pub mod lifecycle;
pub mod monitor;
pub mod resolution;
pub mod types;

#[cfg(test)]
mod tests;

pub use types::*;
