//! Tools are the agent's only extension surface; this verifies the three kinds
//! (dynamic closures, MCP bridges, and collaboration) behave uniformly and that
//! their schemas are valid for providers that reject unknown arguments.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::MockProvider;
use nuo_agent::tools::{DynamicTool, McpTool, Tool};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[tokio::test]
async fn dynamic_tool_receives_arguments_and_returns_output() {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let recorder = seen.clone();

    let tool = DynamicTool::new(
        "echo",
        "Echoes the provided value",
        json!({
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
            "additionalProperties": false
        }),
        move |args| {
            let recorder = recorder.clone();
            async move {
                let value = args["value"].as_str().unwrap_or_default().to_string();
                *recorder.lock().unwrap() = Some(value.clone());
                Ok(value)
            }
        },
    );

    let output = tool
        .execute_simple(json!({"value": "hello"}))
        .await
        .unwrap();
    assert_eq!(output.content(), "hello");
    assert_eq!(seen.lock().unwrap().as_deref(), Some("hello"));
    assert_eq!(tool.name(), "echo");
}

#[tokio::test]
async fn mcp_tool_bridges_an_external_server_call() {
    // Simulate an MCP server call routed through the bridge.
    let invoked = Arc::new(AtomicBool::new(false));
    let flag = invoked.clone();

    let handler: nuo_agent::tools::mcp::McpCallHandler = Arc::new(move |tool_name, args| {
        let flag = flag.clone();
        let tool_name = tool_name.to_string();
        Box::pin(async move {
            assert_eq!(tool_name, "read_file");
            flag.store(true, Ordering::SeqCst);
            let path = args["path"].as_str().unwrap_or_default();
            Ok(format!("contents of {path}"))
        })
    });

    let tool = McpTool::new(
        "filesystem",
        "read_file",
        "Reads a file from the MCP filesystem server",
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": false
        }),
        handler,
    );

    assert_eq!(tool.server_name(), "filesystem");
    let output = tool
        .execute_simple(json!({"path": "/tmp/a.txt"}))
        .await
        .unwrap();
    assert_eq!(output.content(), "contents of /tmp/a.txt");
    assert!(invoked.load(Ordering::SeqCst));
}

#[tokio::test]
async fn tools_from_all_sources_share_one_namespace() {
    let agent = Agent::builder("agent://local/multi")
        .name("Multi")
        .description("Has every kind of tool")
        .provider(MockProvider::new())
        .tool(DynamicTool::new(
            "local_tool",
            "A local tool",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            |_| async { Ok("local".into()) },
        ))
        .tool(McpTool::new(
            "remote",
            "remote_tool",
            "A tool from an MCP server",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            Arc::new(|_, _| Box::pin(async { Ok("remote".into()) })),
        ))
        .build()
        .await
        .unwrap();

    let names = agent.tool_names();
    assert!(names.contains(&"local_tool".to_string()));
    assert!(names.contains(&"remote_tool".to_string()));

    // Every advertised spec must be schema-valid for strict providers.
    for spec in agent.model_specs() {
        let name = spec["function"]["name"].as_str().unwrap();
        assert!(
            spec["function"]["description"].is_string(),
            "`{name}` needs a description"
        );
        assert_eq!(spec["function"]["parameters"]["type"], "object");
        assert_eq!(
            spec["function"]["parameters"]["additionalProperties"], false,
            "`{name}` must forbid unknown arguments"
        );
    }
}
