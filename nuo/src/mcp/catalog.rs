//! MCP tool catalog — a [`DynamicCatalog`] that periodically reconnects MCP
//! servers and re-discovers their tools.
//!
//! On each refresh (every 10 minutes), every enabled server's connection is
//! reset and re-established, and `tools/list` is re-run via [`McpRuntime`]. The
//! refreshed tool list is published through a dynamic-tool sink — so new tools
//! a server exposes appear without a restart, and
//! a recovered server is transparently reconnected. Individual tool calls also
//! auto-reconnect on failure (see `McpTool::call`), and the `/mcp` modal can
//! reconnect a single server on demand, so this periodic refresh is a
//! belt-and-suspenders recovery path.

use std::sync::Arc;
use std::time::Duration;

use nuo_wire::DynamicCatalog;

use super::McpRuntime;

/// A [`DynamicCatalog`] driving the shared [`McpRuntime`]. The runtime owns the
/// server handles and publication sink; the catalog just ticks its
/// periodic `refresh_all`.
pub struct McpCatalog {
    runtime: Arc<McpRuntime>,
}

impl McpCatalog {
    pub fn new(runtime: Arc<McpRuntime>) -> Self {
        Self { runtime }
    }
}

impl DynamicCatalog for McpCatalog {
    fn id(&self) -> &'static str {
        "mcp"
    }

    async fn refresh(&self) -> Result<(), String> {
        if self.runtime.is_empty() {
            return Ok(());
        }
        self.runtime.refresh_all().await;
        let statuses = self.runtime.statuses_snapshot();
        let connected = statuses
            .iter()
            .filter(|(_, s)| {
                matches!(
                    s,
                    nuo_wire::mcp::McpConnectionStatus::Connected { .. }
                )
            })
            .count();
        tracing::info!(servers = statuses.len(), connected, "MCP refresh complete");
        Ok(())
    }

    fn refresh_period(&self) -> Duration {
        Duration::from_secs(10 * 60)
    }
}
