#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ModelResponse};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn agent_discovers_and_invokes_mcp_stdio_tools() {
    let script = r#"
while IFS= read -r line; do
    id=$(echo "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
    if [ -z "$id" ]; then
        continue
    fi
    if echo "$line" | grep -q '"method":"initialize"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"fs-mcp\",\"version\":\"1.0\"}}}"
    elif echo "$line" | grep -q '"method":"tools/list"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"read_file\",\"description\":\"Reads a file\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"path\":{\"type\":\"string\"}},\"required\":[\"path\"],\"additionalProperties\":false}}]}}"
    elif echo "$line" | grep -q '"method":"tools/call"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"File contents of /etc/hosts: 127.0.0.1 localhost\"}],\"isError\":false}}"
    fi
done
"#;

    let provider = Arc::new(MockProvider::new());

    let agent = Agent::builder("agent://local/mcp_reader")
        .name("McpReader")
        .provider_arc(provider.clone())
        .with_mcp_stdio("sh", &["-c", script])
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    // Verify tool discovery
    let tool_names = agent.tool_names();
    assert!(tool_names.contains(&"read_file".to_string()));

    // Model requests calling the MCP-discovered tool
    provider
        .push_response(ModelResponse::tool_call(
            "call_mcp_1",
            "read_file",
            json!({"path": "/etc/hosts"}),
            10,
            10,
        ))
        .await;

    provider
        .push_response(ModelResponse::text(
            "The host file maps localhost to 127.0.0.1.",
            20,
            10,
        ))
        .await;

    let answer = agent.prompt("What is in /etc/hosts?").await.unwrap();
    assert_eq!(answer, "The host file maps localhost to 127.0.0.1.");
}
