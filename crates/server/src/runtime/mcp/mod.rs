//! MCP connector — JSON-RPC transport, server lifecycle, tool adapters, live
//! connection state, and periodic catalog refresh.
//!
//! This module owns the **one** MCP client: the reconnect-capable stdio HTTP
//! client, the server handles, the MCP-to-`Tool` adapter, [`McpRuntime`], and
//! [`McpCatalog`]. A session owns each runtime because it controls connection
//! lifetime, user enable/disable/reconnect actions, and background refresh. The
//! runtime (`engine`) has no MCP protocol dependency: discovered tools reach it
//! through the [`DynamicToolSink`](protocol::DynamicToolSink) port defined in
//! `protocol`, and the agent is both that port's implementor
//! (`DynamicToolRegistry`) and consumer.
//!
//! There is deliberately no second MCP client and no second adapter module
//! (ADR-0230). The client-facing DTOs live in `protocol::mcp`; the one client
//! and its stdio/HTTP transports live here, and nothing exports Nuo as an MCP
//! server (ADR-0234).

mod catalog;
mod client;
mod runtime;

pub use catalog::McpCatalog;
pub use client::{
    McpAttestationVerifier, McpLoadResult, McpNotification, McpNotificationSink, McpServer,
    McpTrustVerifier, connect_server, load_mcp_tools, reconnect_server, refresh_mcp_tools,
    refresh_server_tools, set_attestation_verifier, set_trust_verifier,
};
pub use runtime::{McpRuntime, ReconfigureReport};
