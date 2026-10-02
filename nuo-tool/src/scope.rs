//! Tool scoping and dynamic assembly primitives.

use serde::{Deserialize, Serialize};

/// Functional capability domain of a tool for stage-gated assembly and security scoping.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolScope {
    /// Safe, read-only inspection or pure calculation (safe for planning & discovery).
    ReadOnly,
    /// Local workspace manipulation (file reading, writing, editing, patching).
    Workspace,
    /// System command execution or process invocation.
    Execution,
    /// Network I/O or external API calls.
    Network,
    /// Collaboration and peer-to-peer delegation.
    Collaboration,
    /// Custom domain or stage tag.
    Custom(String),
}

impl ToolScope {
    pub fn custom(name: impl Into<String>) -> Self {
        Self::Custom(name.into())
    }
}
