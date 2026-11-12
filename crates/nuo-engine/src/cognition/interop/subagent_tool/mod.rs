//! `SubagentTool` — spawns a read-only exploration subagent for research subtasks.
//!
//! Lives in `nuo-harness` proper (not the [`crate::tools`] module) because it
//! constructs an
//! [`crate::Agent`] internally: spawning a subagent is an orchestration
//! concern, not a domain-tool concern. The other tools (Bash/Read/Web/…)
//! stay in [`crate::tools`] and remain pure trait implementations.
//!
//! Admission of tools to the subagent is driven by [`nuo_protocol::SubAgentProfile::EXPLORE`]
//! — the single source of truth for the read-only / non-interactive /
//! non-recursive policy. See ADR-0011.
use std::sync::Arc;

use async_trait::async_trait;

use nuo_protocol::{SubAgentProfile, Tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::cognition::agent::Agent;

/// Canonical tool name for spawning a delegated child agent.
pub const SPAWN_AGENT_TOOL_NAME: &str = nuo_protocol::BuiltinTool::SpawnAgent.as_str();

/// The roles a **model-facing** dispatch tool may request, in the order the
/// schema advertises them.
///
/// This is the single source of truth for both the `role` enum in
/// [`Tool::parameters`] and the runtime check in `run_subagent_outcome`, so the
/// advertised contract and the enforced one cannot drift: a role the schema
/// offers is always resolvable, and a role it does not offer is rejected
/// instead of being silently treated as the bound default (
/// "actionable diagnostics for mode errors").
///
/// Deliberately narrower than [`nuo_protocol::SubAgentProfile::ALL`]:
/// `title` is a harness-internal role (session titling drives it directly
/// through the cognitive pipeline) and must not become spawnable just because
/// it lives in the same pool.
pub const DISPATCH_ROLES: &[&str] = &["explore", "debug", "skill"];

/// Canonical description of the default `spawn_agent` dispatch tool.
pub const SPAWN_AGENT_TOOL_DESCRIPTION: &str = "\
Spawn an isolated child agent to perform a focused subtask in a separate \
context window and return a consolidated summary. Set 'role' to 'explore' for read-only \
research (default), 'debug' for non-interactive diagnosis and root-cause analysis, \
or 'skill' for skill discovery, inspection, and domain guideline synthesis.";

/// Canonical description of the diagnostic `delegate_debug` dispatch tool.
pub const DELEGATE_DEBUG_TOOL_DESCRIPTION: &str = "\
Delegate a defect, crash, or test failure investigation to a child agent that \
runs builds, tests, and non-interactive diagnostics (e.g. gdb -batch, sanitizers) \
in an isolated context window, then returns a structured root-cause analysis and \
proposed fix. Unlike the main developer, it has no file-writing tools and will not \
mutate your workspace.";

/// Retry settings for a subagent, inherited from the session's provider retry configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentRetryConfig {
    pub max_attempts: usize,
    pub base_ms: u64,
    pub max_ms: u64,
}

impl Default for SubagentRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 30,
            base_ms: 1_000,
            max_ms: 10_000,
        }
    }
}

/// Live subagent handles keyed by the parent tool-call id — the lookup table
/// that lets the harness route a down-direction reply (a permission decision
/// or `ask_user` answer the user gave in the TUI) back into the specific
/// running subagent that surfaced the request. Full-duplex (ADR-0029).
///
/// The `task` tool populates this when it spawns a child (and clears the entry
/// when the child finishes); the harness reads it when it needs to reply to a
/// `SubagentEvent::PermissionRequest` / `UserQuestionRequest` that arrived
/// nested under a given `parent_call_id`. Entries are best-effort: a late reply
/// after the child already finished finds no entry (or a dead handle) and
/// degrades to a no-op rather than erroring.
mod registry;
mod tool;

pub use self::registry::SubagentRegistry;
pub use self::tool::SubagentTool;
