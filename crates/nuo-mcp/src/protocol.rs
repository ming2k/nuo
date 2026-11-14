use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LATEST_PROTOCOL_VERSION: &str = "2024-11-05";

/// Standard JSON-RPC 2.0 Request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// Standard JSON-RPC 2.0 Notification envelope (no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }
}

/// Standard JSON-RPC 2.0 Response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// Standard JSON-RPC 2.0 Error payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ---------------------------------------------------------------------------
// Model Context Protocol (MCP) Domain Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

/// Tool definition discovered from an MCP server via `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    pub tools: Vec<McpToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Content element returned from a tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentItem {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
    #[serde(other)]
    Unknown,
}

/// Result returned from an MCP `tools/call` invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<ContentItem>,
    #[serde(default)]
    pub is_error: Option<bool>,
}

impl CallToolResult {
    /// Combines all textual parts into a single string observation.
    pub fn to_combined_text(&self) -> String {
        let texts: Vec<&str> = self
            .content
            .iter()
            .filter_map(|item| match item {
                ContentItem::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();

        if texts.is_empty() {
            String::new()
        } else {
            texts.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trip_preserves_identity_and_params() {
        let request = JsonRpcRequest::new(7, "tools/call", Some(json!({"name": "read"})));
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["jsonrpc"], "2.0");
        assert_eq!(encoded["id"], 7);
        assert_eq!(encoded["method"], "tools/call");
        assert_eq!(encoded["params"]["name"], "read");

        let decoded: JsonRpcRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.id, 7);
        assert_eq!(decoded.method, "tools/call");
    }

    #[test]
    fn response_omits_absent_result_and_error() {
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: "method not found".to_string(),
                data: None,
            }),
        };
        let encoded = serde_json::to_value(&response).unwrap();
        assert!(
            encoded.get("result").is_none(),
            "an absent result must not serialize as null"
        );
        assert_eq!(encoded["error"]["code"], -32601);

        let without_error = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(2),
            result: Some(json!({"ok": true})),
            error: None,
        };
        let encoded = serde_json::to_value(&without_error).unwrap();
        assert!(encoded.get("error").is_none());
    }

    #[test]
    fn content_item_keeps_text_and_degrades_unknown_kinds() {
        let text: ContentItem = serde_json::from_value(json!({
            "type": "text",
            "text": "hello"
        }))
        .unwrap();
        assert!(matches!(text, ContentItem::Text { text } if text == "hello"));

        let unknown: ContentItem = serde_json::from_value(json!({
            "type": "audio",
            "data": "ignored"
        }))
        .unwrap();
        assert!(matches!(unknown, ContentItem::Unknown));
    }
}
