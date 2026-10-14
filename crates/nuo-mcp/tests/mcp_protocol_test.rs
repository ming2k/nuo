#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_mcp::McpClient;
use nuo_mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use serde_json::json;

#[test]
fn jsonrpc_request_response_serialization() {
    let req = JsonRpcRequest::new(1, "tools/list", None);
    let serialized = serde_json::to_string(&req).unwrap();
    assert!(serialized.contains("\"id\":1"));
    assert!(serialized.contains("\"method\":\"tools/list\""));

    let resp_json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}"#;
    let resp: JsonRpcResponse = serde_json::from_str(resp_json).unwrap();
    assert_eq!(resp.id, Some(1));
    assert!(resp.result.is_some());
}

#[tokio::test]
async fn mock_mcp_server_stdio_interaction() {
    // Shell script dynamically echoing back matching ID for any JSON-RPC request
    let script = r#"
while IFS= read -r line; do
    id=$(echo "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
    if [ -z "$id" ]; then
        continue
    fi
    if echo "$line" | grep -q '"method":"initialize"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"test-server\",\"version\":\"1.0\"}}}"
    elif echo "$line" | grep -q '"method":"tools/list"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"test_tool\",\"description\":\"A mock tool\",\"inputSchema\":{\"type\":\"object\"}}]}}"
    elif echo "$line" | grep -q '"method":"tools/call"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hello from mcp\"}],\"isError\":false}}"
    fi
done
"#;

    let client = McpClient::connect_stdio("sh", &["-c", script])
        .await
        .unwrap();

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "test_tool");

    let call_res = client
        .call_tool("test_tool", json!({"param": "val"}))
        .await
        .unwrap();
    assert_eq!(call_res.to_combined_text(), "hello from mcp");

    // Convert into native Tool instances
    let native_tools = client.into_tools().await.unwrap();
    assert_eq!(native_tools.len(), 1);
    assert_eq!(native_tools[0].name(), "test_tool");

    let ctx = nuo_tool::ToolContext::default();
    let out = native_tools[0].execute(&ctx, json!({})).await.unwrap();
    assert_eq!(out.content(), "hello from mcp");
    assert!(!out.is_error());

    client.close();
}

#[tokio::test]
async fn test_mcp_server_request_handling_and_tool_export() {
    use nuo_mcp::McpServer;
    use nuo_tool::DynamicTool;
    use std::sync::Arc;

    let server = McpServer::new("nous-export-server", "1.0.0");
    let calc_tool = DynamicTool::new(
        "add",
        "Adds two numbers",
        json!({
            "type": "object",
            "properties": {
                "a": { "type": "number" },
                "b": { "type": "number" }
            }
        }),
        |args| async move {
            let a = args["a"].as_f64().unwrap_or(0.0);
            let b = args["b"].as_f64().unwrap_or(0.0);
            Ok((a + b).to_string())
        },
    );

    server.register_tool(Arc::new(calc_tool)).await;

    // 1. Initialize request
    let init_req = JsonRpcRequest::new(1, "initialize", None);
    let init_resp = server.handle_request(init_req).await;
    assert_eq!(init_resp.id, Some(1));
    assert!(init_resp.error.is_none());
    let init_res = init_resp.result.unwrap();
    assert_eq!(init_res["serverInfo"]["name"], "nous-export-server");

    // 2. Tools list request
    let list_req = JsonRpcRequest::new(2, "tools/list", None);
    let list_resp = server.handle_request(list_req).await;
    assert_eq!(list_resp.id, Some(2));
    let tools = list_resp.result.unwrap()["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "add");

    // 3. Tools call request
    let call_req = JsonRpcRequest::new(
        3,
        "tools/call",
        Some(json!({
            "name": "add",
            "arguments": { "a": 15.5, "b": 26.5 }
        })),
    );
    let call_resp = server.handle_request(call_req).await;
    assert_eq!(call_resp.id, Some(3));
    let call_res = call_resp.result.unwrap();
    assert_eq!(call_res["content"][0]["text"], "42");
    assert!(call_res["isError"].is_null() || call_res["isError"] == false);
}
