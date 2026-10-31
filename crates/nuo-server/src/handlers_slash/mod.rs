//! The `AgentRequest::SlashCommand` dispatcher and domain sub-handlers.

pub mod commands;
pub mod dispatch;
pub mod record;
pub mod security_ops;
pub mod session_ops;
pub mod session_route;

#[cfg(test)]
mod tests;

use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

use crate::mcp::McpRuntime;
use crate::slash_handler::SlashCommandRegistry;
use crate::startup::SessionStart;
use nuo_harness::cognition::skills::SkillRegistry;
use nuo_harness::{Agent, RoundLifecycle};
use nuo_persistence::{
    config::Config, connection_usage::ConnectionUsage, session::SessionStore,
    workspace_security::WorkspaceSecurityStore,
};
use nuo_protocol::{AgentRequest, AgentResponse, Provider};

pub use dispatch::dispatch;

/// Bundled slash-dispatch environment: the server plumbing a slash command
/// needs beyond the command text itself.
pub struct SlashEnv<'a> {
    pub config: &'a Config,
    pub agent: &'a Arc<Agent>,
    pub mcp_runtime: &'a Arc<McpRuntime>,
    pub workspace_security: &'a Arc<WorkspaceSecurityStore>,
    pub shared_additional_roots: &'a nuo_protocol::SharedAdditionalRoots,
    pub shared_confinement: &'a nuo_protocol::SharedConfinement,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub session: &'a Arc<SessionStore>,
    pub lifecycle: &'a Arc<RoundLifecycle>,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub provider_usage: &'a mut ConnectionUsage,
    pub skills_registry: &'a Arc<SkillRegistry>,
    pub req_tx_for_commands: &'a mpsc::Sender<AgentRequest>,
    pub startup: &'a SessionStart,
    pub ui: &'a dyn crate::UiBridge,
    pub extra_commands: &'a SlashCommandRegistry,
    pub websearch_shared: &'a nuo_protocol::SharedWebConfig,
    pub background_jobs: &'a crate::background_jobs::BackgroundJobManager,
}
