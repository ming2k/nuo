//! Canonical tool specification, capability definitions, and execution runtime
//! for the Nuo agent ecosystem.
//!
//! This crate provides zero-agent-runtime contracts allowing independent development
//! and integration of tools, dynamic closures, MCP bridges, and security policies.

pub mod approval;
pub mod command;
pub mod context;
pub mod dynamic;
pub mod error;
pub mod mcp;
pub mod output;
pub mod policy;
pub mod registry;
pub mod risk;
pub mod scope;

pub use approval::{AlwaysApprove, ApprovalDecision, ApprovalHandler, ToolCallRequest};
pub use command::{CommandTool, ShellKind};
pub use context::ToolContext;
pub use dynamic::DynamicTool;
pub use error::{Result, ToolError};
pub use mcp::McpTool;
pub use output::ToolOutput;
pub use policy::ToolPolicy;
pub use registry::ToolRegistry;
pub use risk::RiskProfile;
pub use scope::ToolScope;

pub use nuo_tool_derive::ToolSchema;

use async_trait::async_trait;

/// Core interface for tools callable by an agent.
///
/// Designed to be decoupled from specific agent loop runtimes and errors,
/// tools only require JSON-compatible schemas and arguments, providing
/// standardized execution, cancellation awareness, and risk declarations.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Distinct identifier of the tool (e.g. `read_file`, `jira_create_issue`).
    fn name(&self) -> &str;

    /// Natural language description of what the tool does and how the model should use it.
    fn description(&self) -> &str;

    /// JSON schema describing the accepted input arguments.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Declared capability and risk profile of the tool for policy reasoning.
    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    /// Declared operational scopes for dynamic tool assembly and stage gating.
    fn scopes(&self) -> Vec<ToolScope> {
        match self.risk_profile() {
            RiskProfile::ReadOnly => vec![ToolScope::ReadOnly],
            RiskProfile::IdempotentMutation => vec![ToolScope::Workspace],
            RiskProfile::NetworkAccess => vec![ToolScope::Network],
            RiskProfile::Destructive => vec![ToolScope::Workspace, ToolScope::Execution],
            RiskProfile::ArbitraryExecution => vec![ToolScope::Execution],
        }
    }

    /// Whether invocation of this tool under `ctx` with `arguments` requires explicit approval.
    fn requires_approval(&self, _ctx: &ToolContext, _arguments: &serde_json::Value) -> bool {
        false
    }

    /// Executes the tool with the supplied JSON arguments within the given context.
    async fn execute(&self, ctx: &ToolContext, arguments: serde_json::Value) -> Result<ToolOutput>;

    /// Convenient execution helper using a default context.
    async fn execute_simple(&self, arguments: serde_json::Value) -> Result<ToolOutput> {
        self.execute(&ToolContext::default(), arguments).await
    }
}
