//! Shared configuration schema for MCP servers.
//!
//! Lives in `nuo-protocol` for the same reason `WebSearchConfig` does: both the
//! app-layer `Config` (which owns the `[mcp]` table), the runtime layers
//! (`nuo-engine`/`nuo-server`), and the
//! session/frontend layers exchange these values without depending on one
//! another's implementation details.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// One MCP server entry from the `[mcp.<name>]` table of `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub url: Option<String>,
    pub command: Vec<String>,
    pub environment: HashMap<String, String>,
    pub enabled: bool,
    pub read_only: bool,
    pub allow_tools: Vec<String>,
    pub deny_tools: Vec<String>,
    #[serde(skip)]
    pub sandbox_root: Option<PathBuf>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            url: None,
            command: Vec::new(),
            environment: HashMap::new(),
            enabled: true,
            read_only: false,
            allow_tools: Vec::new(),
            deny_tools: Vec::new(),
            sandbox_root: None,
        }
    }
}

impl McpServerConfig {
    pub fn admits_tool(&self, original_name: &str) -> bool {
        if self.deny_tools.iter().any(|d| d == original_name) {
            return false;
        }
        self.allow_tools.is_empty() || self.allow_tools.iter().any(|a| a == original_name)
    }
}

/// Runtime status reported by the MCP connector (owned by `nuo-server`) for each configured server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConnectionStatus {
    Connecting,
    Connected { tools: usize },
    Disabled,
    Failed(String),
}

impl std::fmt::Display for McpConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connecting => write!(f, "connecting…"),
            Self::Connected { tools } => write!(f, "connected ({} tools)", tools),
            Self::Disabled => write!(f, "disabled"),
            Self::Failed(error) => write!(f, "failed: {}", error),
        }
    }
}
