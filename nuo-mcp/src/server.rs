//! Model Context Protocol (MCP) server implementation.
//!
//! Enables exporting internal [`nuo_tool::Tool`] instances as a compliant MCP server
//! for external hosts (e.g. Claude Desktop, Cursor, VSCode, OpenCode, or other agents).

use crate::error::{McpError, Result};
use crate::protocol::{
    CallToolResult, ContentItem, InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    LATEST_PROTOCOL_VERSION, ListToolsResult, McpToolDefinition, ServerCapabilities, ServerInfo,
    ToolsCapability,
};
use nuo_tool::Tool;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

/// MCP Server hosting a collection of [`Tool`] instances.
#[derive(Clone)]
pub struct McpServer {
    name: String,
    version: String,
    tools: Arc<RwLock<HashMap<String, Arc<dyn Tool>>>>,
}

impl McpServer {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Registers a tool on the server.
    pub async fn register_tool(&self, tool: Arc<dyn Tool>) {
        let mut guard = self.tools.write().await;
        guard.insert(tool.name().to_string(), tool);
    }

    /// Handles a single incoming JSON-RPC request and produces a response.
    pub async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => {
                let result = InitializeResult {
                    protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
                    capabilities: ServerCapabilities {
                        tools: Some(ToolsCapability { list_changed: true }),
                        resources: None,
                        prompts: None,
                    },
                    server_info: ServerInfo {
                        name: self.name.clone(),
                        version: Some(self.version.clone()),
                    },
                };
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: Some(req.id),
                    result: serde_json::to_value(result).ok(),
                    error: None,
                }
            }

            "tools/list" => {
                let guard = self.tools.read().await;
                let definitions: Vec<McpToolDefinition> = guard
                    .values()
                    .map(|t| McpToolDefinition {
                        name: t.name().to_string(),
                        description: Some(t.description().to_string()),
                        input_schema: t.parameters_schema(),
                    })
                    .collect();

                let result = ListToolsResult {
                    tools: definitions,
                    next_cursor: None,
                };
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: Some(req.id),
                    result: serde_json::to_value(result).ok(),
                    error: None,
                }
            }

            "tools/call" => {
                let Some(params) = req.params else {
                    return JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: Some(req.id),
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32602,
                            message: "Missing params for tools/call".to_string(),
                            data: None,
                        }),
                    };
                };

                let tool_name = params
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();

                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                let tool_opt = {
                    let guard = self.tools.read().await;
                    guard.get(&tool_name).cloned()
                };

                match tool_opt {
                    Some(tool) => match tool.execute_simple(arguments).await {
                        Ok(output) => {
                            let result = CallToolResult {
                                content: vec![ContentItem::Text {
                                    text: output.content,
                                }],
                                is_error: if output.is_error { Some(true) } else { None },
                            };
                            JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: Some(req.id),
                                result: serde_json::to_value(result).ok(),
                                error: None,
                            }
                        }
                        Err(err) => {
                            let result = CallToolResult {
                                content: vec![ContentItem::Text {
                                    text: err.to_string(),
                                }],
                                is_error: Some(true),
                            };
                            JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: Some(req.id),
                                result: serde_json::to_value(result).ok(),
                                error: None,
                            }
                        }
                    },
                    None => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: Some(req.id),
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32601,
                            message: format!("Tool `{tool_name}` not found"),
                            data: None,
                        }),
                    },
                }
            }

            "ping" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: Some(req.id),
                result: Some(json!({})),
                error: None,
            },

            other => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: Some(req.id),
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method `{other}` not found"),
                    data: None,
                }),
            },
        }
    }

    /// Handles a single incoming line from a stream. Returns serialized JSON response if applicable.
    pub async fn handle_line(&self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Ok(req) = serde_json::from_str::<JsonRpcRequest>(trimmed) {
            let resp = self.handle_request(req).await;
            serde_json::to_string(&resp).ok()
        } else {
            None
        }
    }

    /// Serves standard input/output until stdin closes.
    pub async fn serve_stdio(self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin).lines();

        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?
        {
            if let Some(resp_line) = self.handle_line(&line).await {
                stdout
                    .write_all(resp_line.as_bytes())
                    .await
                    .map_err(|e| McpError::Transport(e.to_string()))?;
                stdout
                    .write_all(b"\n")
                    .await
                    .map_err(|e| McpError::Transport(e.to_string()))?;
                stdout
                    .flush()
                    .await
                    .map_err(|e| McpError::Transport(e.to_string()))?;
            }
        }

        Ok(())
    }
}
