//! Shared configuration schema for MCP servers.
//!
//! Lives in `protocol` for the same reason `WebSearchConfig` does: both the
//! app-layer `Config` (which owns the `[mcp]` table), the runtime layers
//! (`engine`/`server`), and the
//! session/frontend layers exchange these values without depending on one
//! another's implementation details.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// One MCP server entry from the `[mcp.<name>]` table of `agent.toml`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub url: Option<String>,
    pub command: Vec<String>,
    pub environment: HashMap<String, String>,
    pub enabled: bool,
    pub read_only: bool,
    pub allow_tools: Vec<String>,
    pub deny_tools: Vec<String>,
    /// Declare this server's tools as deferred (`tool_search` discoverable)
    /// instead of putting every schema in every prompt. Defaults to `true`;
    /// a small server or an embedding without `tool_search` can set false.
    #[serde(default = "default_defer_tools")]
    pub defer_tools: bool,
    /// Expose the server's `resources/list` and `resources/read` endpoints as
    /// discoverable tools alongside its function tools.
    #[serde(default = "default_true")]
    pub enable_resources: bool,
    #[serde(skip)]
    pub sandbox_root: Option<PathBuf>,
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env_keys: Vec<&String> = self.environment.keys().collect();
        f.debug_struct("McpServerConfig")
            .field("url", &self.url)
            .field("command", &self.command)
            .field("environment_keys", &env_keys)
            .field("enabled", &self.enabled)
            .field("read_only", &self.read_only)
            .field("allow_tools", &self.allow_tools)
            .field("deny_tools", &self.deny_tools)
            .field("defer_tools", &self.defer_tools)
            .field("enable_resources", &self.enable_resources)
            .field("sandbox_root", &self.sandbox_root)
            .finish()
    }
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
            defer_tools: true,
            enable_resources: true,
            sandbox_root: None,
        }
    }
}

fn default_defer_tools() -> bool {
    true
}

fn default_true() -> bool {
    true
}

impl McpServerConfig {
    pub fn admits_tool(&self, original_name: &str) -> bool {
        if self.deny_tools.iter().any(|d| d == original_name) {
            return false;
        }
        self.allow_tools.is_empty() || self.allow_tools.iter().any(|a| a == original_name)
    }
}

/// Runtime status reported by the MCP connector (owned by `server`) for each configured server.
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
