//! MCP connector — JSON-RPC transport, server lifecycle, tool adapters, live
//! connection state, and periodic catalog refresh.
//!
//! This is the standalone `muta-mcp` crate of ADR-0060: it owns the stdio
//! JSON-RPC client, server handles, MCP-to-`Tool` adapters, [`McpRuntime`],
//! and [`McpCatalog`]. A session (in `muta-runtime`) owns each runtime because
//! it controls connection lifetime, user enable/disable/reconnect actions,
//! and background refresh. The agent (`muta-agent`) has no MCP protocol
//! dependency: discovered tools reach it through the
//! [`DynamicToolSink`](nuo_wire::DynamicToolSink) port defined in
//! `muta-contracts`, and the agent is both that port's implementor
//! (`DynamicToolRegistry`) and consumer — the trait object doesn't care
//! which crate the MCP impl is compiled in.


mod catalog;
mod client;
mod runtime;
pub mod substrate_bridge;

pub use catalog::McpCatalog;
pub use client::{
    McpAttestationVerifier, McpLoadResult, McpServer, McpTrustVerifier, connect_server,
    load_mcp_tools, reconnect_server, refresh_mcp_tools, set_attestation_verifier,
    set_trust_verifier,
};
pub use runtime::{McpRuntime, ReconfigureReport};
pub use substrate_bridge::{adapt_mcp_tool, load_substrate_mcp_tools};
