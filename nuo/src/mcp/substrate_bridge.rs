//! Substrate MCP Bridge connecting `nous-mcp` to Nuo's dynamic tool registry (ADR-0002).

use std::sync::Arc;
use nuo_contracts::{NousToolBridge, Tool};
use nuo_mcp::{McpClient, McpNativeTool, McpToolDefinition};

/// Connect to a local stdio MCP server via the `nous-mcp` substrate client and return
/// its discovered tools adapted to Nuo's application `Tool` trait.
pub async fn load_substrate_mcp_tools(
    program: &str,
    args: &[&str],
) -> Result<(Arc<McpClient>, Vec<Arc<dyn Tool>>), String> {
    let client = McpClient::connect_stdio(program, args)
        .await
        .map_err(|err| format!("failed to connect to substrate MCP server '{program}': {err}"))?;

    let tools_res = client
        .list_tools()
        .await
        .map_err(|err| format!("failed to list tools from substrate MCP server '{program}': {err}"))?;

    let adapted_tools: Vec<Arc<dyn Tool>> = tools_res
        .into_iter()
        .map(|tool_def| {
            let native_tool = Arc::new(McpNativeTool::new(Arc::clone(&client), tool_def));
            Arc::new(NousToolBridge::new(native_tool)) as Arc<dyn Tool>
        })
        .collect();

    Ok((client, adapted_tools))
}

/// Convert a `nous-mcp` tool definition and client into a Nuo application `Tool`.
pub fn adapt_mcp_tool(client: Arc<McpClient>, tool_def: McpToolDefinition) -> Arc<dyn Tool> {
    let native_tool = Arc::new(McpNativeTool::new(client, tool_def));
    Arc::new(NousToolBridge::new(native_tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn substrate_mcp_bridge_loads_and_adapts_tools() {
        let script = r#"
while IFS= read -r line; do
    id=$(echo "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
    [ -z "$id" ] && continue
    if echo "$line" | grep -q '"method":"initialize"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"test-server\",\"version\":\"1.0\"}}}"
    elif echo "$line" | grep -q '"method":"tools/list"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"test_mcp_tool\",\"description\":\"A mock tool from substrate\",\"inputSchema\":{\"type\":\"object\"}}]}}"
    elif echo "$line" | grep -q '"method":"tools/call"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"executed via substrate mcp\"}],\"isError\":false}}"
    fi
done
"#;

        let (_client, tools) = load_substrate_mcp_tools("sh", &["-c", script])
            .await
            .expect("load substrate tools");

        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.name(), "test_mcp_tool");
        assert_eq!(tool.description(), "A mock tool from substrate");
        assert_eq!(tool.hazard_level(), nuo_contracts::HazardLevel::NetworkOrExternal);

        let result = tool.call("{}").await.expect("execute tool");
        assert_eq!(result, "executed via substrate mcp");
    }
}
