//! Permission, input, context, and capability registry records.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// Allow this single invocation (one-off).
    Once,
    /// Allow for the duration of the current session (in-memory).
    Session,
    /// Allow permanently for this workspace (persisted).
    Always,
    /// Deny / reject this execution.
    Reject,
}

/// Reply sent from the TUI back to the agent after the user answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestionReply {
    pub request_id: String,
    /// One array of selected option labels per question.
    pub answers: Vec<Vec<String>>,
}

pub type InputRequest = StdinRequest;

/// Reply sent from the TUI back to the agent carrying the operator's stdin input.
/// An empty `text` signals cancellation (the command runs with closed stdin
/// and fails fast with a non-interactive remedy hint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdinReply {
    pub request_id: String,
    pub text: String,
}

pub type InputReply = StdinReply;

/// A complete, render-ready picture of the live thread, sent from the harness
/// to the TUI for the thread-context modal. Every pane in that modal reads
/// from this one snapshot, so opening the modal and any mutation
/// (revoke / toggle) only needs a single request/response round-trip rather
/// than one per pane. Built by the harness from its own state (provider,
/// tools, permissions, skills) plus the MCP load result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadContextSnapshot {
    pub model: ModelInfo,
    pub tools: Vec<ToolInfo>,
    pub permissions: Vec<PermissionRuleInfo>,
    pub skills: Vec<SkillInfo>,
    pub mcp: Vec<McpServerInfo>,
}

/// Model-side pane of [`ThreadContextSnapshot`]. `capabilities` carries
/// heuristic hints (e.g. "tool calling", "reasoning") since per-model
/// capability data is not yet modeled in the catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub model: String,
    pub display_name: String,
    pub context_window: usize,
    pub api_key_ready: bool,
    pub description: String,
    pub capabilities: Vec<String>,
}

/// One tool in the session, as seen by the modal's Tools pane. `source`
/// classifies origin: `builtin`, `mcp:<server>`, or `plan`. `enabled`
/// reflects the session-level enable/disable flag (toggled via
/// [`AgentRequest::ToggleTool`]); disabled tools stay installed but are hidden
/// from the model and rejected if invoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    #[serde(default)]
    pub requires_user: bool,
    #[serde(default)]
    pub requires_vision: bool,
}

/// One cached "always allow" permission rule, shown in the modal's Permissions
/// pane where it can be revoked individually via [`AgentRequest::RevokePermission`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PermissionRuleInfo {
    pub tool: String,
    pub scope: String,
}

/// One skill in the registry, shown in the modal's Skills pane. `source` is the
/// [`SkillScope`](../nuo_skills/enum.SkillScope.html) display string
/// (system / remote / user / extra / repo).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub enabled: bool,
    pub source: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub quarantined: bool,
}

/// One MCP server, shown in the modal's MCP pane. The connection tri-state
/// (connected / disabled / failed) is unpacked from
/// [`crate::mcp::McpConnectionStatus`] so the DTO stays decoupled from the
/// enum, and `tool_names` carries the per-server tool list that the hint bar
/// collapses to a mere count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub connected: bool,
    pub disabled: bool,
    pub failure: Option<String>,
    pub tool_names: Vec<String>,
}
