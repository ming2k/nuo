use crate::error::{McpError, Result};
use crate::protocol::{
    CallToolResult, ClientCapabilities, ClientInfo, InitializeParams, InitializeResult,
    LATEST_PROTOCOL_VERSION, ListToolsResult, McpToolDefinition, ServerCapabilities, ServerInfo,
};
use crate::transport::StdioTransport;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// High-level Model Context Protocol (MCP) client managing transport,
/// initialization handshake, and capability dispatch.
pub struct McpClient {
    transport: StdioTransport,
    server_info: RwLock<Option<ServerInfo>>,
    capabilities: RwLock<Option<ServerCapabilities>>,
}

impl McpClient {
    /// Connects to an MCP server spawned as a local child process over standard I/O.
    pub async fn connect_stdio(program: &str, args: &[&str]) -> Result<Arc<Self>> {
        Self::connect_stdio_with_options(program, args, None, None).await
    }

    /// Connects to an MCP server from a pre-configured `tokio::process::Command`.
    pub async fn connect_command(cmd: tokio::process::Command) -> Result<Arc<Self>> {
        let transport = StdioTransport::from_command(cmd)?;
        let client = Arc::new(Self {
            transport,
            server_info: RwLock::new(None),
            capabilities: RwLock::new(None),
        });

        client.initialize().await?;

        Ok(client)
    }

    /// Connects to an MCP server over stdio with custom working directory and environment variables.
    pub async fn connect_stdio_with_options(
        program: &str,
        args: &[&str],
        working_dir: Option<PathBuf>,
        env_vars: Option<HashMap<String, String>>,
    ) -> Result<Arc<Self>> {
        let transport = StdioTransport::spawn(program, args, working_dir, env_vars).await?;
        let client = Arc::new(Self {
            transport,
            server_info: RwLock::new(None),
            capabilities: RwLock::new(None),
        });

        // Perform initialization handshake automatically
        client.initialize().await?;

        Ok(client)
    }

    /// Performs the `initialize` handshake protocol and sends the `initialized` notification.
    pub async fn initialize(&self) -> Result<InitializeResult> {
        let init_params = InitializeParams {
            protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "nous-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let resp = self
            .transport
            .send_request("initialize", Some(json!(init_params)))
            .await?;

        if let Some(err) = resp.error {
            return Err(McpError::protocol(err.code, err.message, err.data));
        }

        let result_value = resp
            .result
            .ok_or_else(|| McpError::initialization("server returned empty initialize result"))?;

        let init_result: InitializeResult = serde_json::from_value(result_value)
            .map_err(|e| McpError::serialization(e.to_string()))?;

        {
            let mut info_guard = self.server_info.write().await;
            *info_guard = Some(init_result.server_info.clone());
            let mut caps_guard = self.capabilities.write().await;
            *caps_guard = Some(init_result.capabilities.clone());
        }

        // Notify server that initialization is complete
        self.transport
            .send_notification("notifications/initialized", None)
            .await?;

        Ok(init_result)
    }

    /// Queries the server for available tools via `tools/list`.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDefinition>> {
        let resp = self.transport.send_request("tools/list", None).await?;

        if let Some(err) = resp.error {
            return Err(McpError::protocol(err.code, err.message, err.data));
        }

        let result_value = resp
            .result
            .ok_or_else(|| McpError::protocol(-1, "empty result in tools/list", None))?;

        let list_res: ListToolsResult = serde_json::from_value(result_value)
            .map_err(|e| McpError::serialization(e.to_string()))?;

        Ok(list_res.tools)
    }

    /// Dispatches a tool invocation via `tools/call`.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult> {
        let params = json!({
            "name": name,
            "arguments": arguments,
        });

        let resp = self
            .transport
            .send_request("tools/call", Some(params))
            .await?;

        if let Some(err) = resp.error {
            return Err(McpError::protocol(err.code, err.message, err.data));
        }

        let result_value = resp
            .result
            .ok_or_else(|| McpError::protocol(-1, "empty result in tools/call", None))?;

        let call_res: CallToolResult = serde_json::from_value(result_value)
            .map_err(|e| McpError::serialization(e.to_string()))?;

        Ok(call_res)
    }

    /// Discovers all tools on the server and wraps them into native `nuo_tool::Tool` instances.
    pub async fn into_tools(self: &Arc<Self>) -> Result<Vec<Arc<dyn nuo_tool::Tool>>> {
        let definitions = self.list_tools().await?;
        let mut tools: Vec<Arc<dyn nuo_tool::Tool>> = Vec::with_capacity(definitions.len());

        for def in definitions {
            let adapter = crate::adapter::McpNativeTool::new(self.clone(), def);
            tools.push(Arc::new(adapter));
        }

        Ok(tools)
    }

    /// Closes the transport pipe.
    pub fn close(&self) {
        self.transport.close();
    }
}
