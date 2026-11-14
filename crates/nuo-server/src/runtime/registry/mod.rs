//! Thread registry: owns live sessions, coordinates hosting and lifecycle.

pub(super) use crate::UiBridge;
pub(super) use crate::composition::bootstrap::BootstrapParams;
pub(super) use crate::runtime::monitor::MonitorTracker;
pub(super) use crate::transport::serve::{AttachAction, AttachSyncBuffer, is_attach_sync_event};
pub(super) use nuo_engine::{Agent, AgentIdentity, AgentRoleProfile};
pub(super) use nuo_persistence::thread::ThreadSession;
pub(super) use nuo_protocol::{
    AgentRequest, AgentResponse, MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredThread,
    PermissionDecision, ThreadActivity, ThreadFilter, ThreadHosting, ThreadOverview,
    ThreadRemovalReport, ThreadRemovalSkip,
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
