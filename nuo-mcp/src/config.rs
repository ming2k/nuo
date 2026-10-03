//! Shared configuration schema for MCP servers.
//!
//! Lives in `muta-contracts` for the same reason `WebSearchConfig` does: both the
//! app-layer `Config` (which owns the `[mcp]` table), `muta-agent` (which owns the MCP connector), and the
//! session/frontend layers exchange these values without depending on one
//! another's implementation details.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// One MCP server entry from the `[mcp.<name>]` table of `config.toml`.
///
/// A server declares exactly one transport: either `url` (a Streamable HTTP
/// endpoint, `https://host/mcp`) or `command` (a local stdio server). A `url`
/// takes precedence when both are present, mirroring the common MCP client
/// configuration shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// Streamable HTTP endpoint. When set, `command` is ignored and the server
    /// is reached over HTTP POST (with SSE-framed responses) instead of a
    /// spawned child process.
    pub url: Option<String>,
    pub command: Vec<String>,
    pub environment: HashMap<String, String>,
    pub enabled: bool,
    pub read_only: bool,
    /// Optional server-side tool allow-list. When non-empty, only tools whose
    /// *original* (server-declared) name appears here are published; combined
    /// with `deny_tools` below (deny wins on conflict). Empty admits every
    /// advertised tool. This is the `future allow` follow-up ADR-0085 §"Config
    /// sources" reserved for `McpServerConfig`.
    pub allow_tools: Vec<String>,
    /// Optional server-side tool deny-list, matched against the original
    /// (server-declared) name — the same axis `allow_tools` uses, so the two
    /// never mix sanitized and raw forms. A denied tool is never published,
    /// even if `allow_tools` also lists it.
    pub deny_tools: Vec<String>,
    /// Runtime-only origin marker. Project-defined servers carry their exact
    /// workspace root and must be launched read-only/offline inside the
    /// workspace sandbox. Global user configuration leaves this unset.
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
    /// Whether a server-advertised tool (by its original, unsanitized name)
    /// passes this server's `allow_tools`/`deny_tools` configuration. Deny
    /// wins over allow; an empty allow-list admits everything.
    pub fn admits_tool(&self, original_name: &str) -> bool {
        if self.deny_tools.iter().any(|d| d == original_name) {
            return false;
        }
        self.allow_tools.is_empty() || self.allow_tools.iter().any(|a| a == original_name)
    }
}

/// Runtime status reported by `muta-agent` (MCP connector) for each configured server.
///
/// Lives in `muta-contracts` (alongside [`McpServerConfig`]) so the TUI can
/// consume it without depending on the connector implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConnectionStatus {
    /// A connection attempt is in flight (background connect at startup, or a
    /// reconnect). The server is not usable yet; the model sees none of its
    /// tools until it transitions to `Connected`.
    Connecting,
    Connected {
        tools: usize,
    },
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
