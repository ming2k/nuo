//! Model Context Protocol (MCP) client and server implementation for the Nuo ecosystem.
//!
//! Provides out-of-process tool discovery, JSON-RPC 2.0 communication, native
//! bridging from external MCP servers to [`nuo_tool::Tool`], and bidirectional
//! MCP server export for external AI agent hosts.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod adapter;
pub mod client;
pub mod config;
pub mod error;
pub mod protocol;
pub mod server;
pub mod transport;

pub use adapter::McpNativeTool;
pub use client::McpClient;
pub use config::{McpConnectionStatus, McpServerConfig};
pub use error::{McpError, Result};
pub use protocol::{
    CallToolResult, ClientCapabilities, ClientInfo, ContentItem, InitializeParams,
    InitializeResult, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    ListToolsResult, McpToolDefinition, ServerCapabilities, ServerInfo,
};
pub use server::McpServer;
pub use transport::StdioTransport;
