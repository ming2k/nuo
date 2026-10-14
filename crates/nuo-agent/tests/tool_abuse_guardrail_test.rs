#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::Agent;
use nuo_agent::message::ToolCall;
use nuo_agent::provider::{MockProvider, ModelResponse};
use nuo_agent::tools::{DynamicTool, ToolPolicy, ToolScope};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn test_circuit_breaker_halts_infinite_tool_loop() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_clone = call_count.clone();

    // Tool that always fails or repeats
    let repeat_tool = DynamicTool::new(
        "cat_file",
        "Reads file",
        json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        move |_args| {
            let count = count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err(nuo_tool::ToolError::execution(
                    "cat_file",
                    "File not found",
                ))
            }
        },
    );

    let provider = MockProvider::new();
    // Simulate model hallucinating and calling the exact same tool with exact same arguments repeatedly
    for _ in 0..4 {
        provider
            .push_response(ModelResponse {
                content: None,
                thinking: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "cat_file".into(),
                    arguments: json!({"path": "/app/missing.txt"}),
                }],
                usage: Default::default(),
            })
            .await;
    }
    // Finally model answers after being broken out of loop
    provider
        .push_response(ModelResponse::text(
            "Recovered from circuit breaker error.",
            10,
            10,
        ))
        .await;

    let policy = ToolPolicy::new().with_anti_loop_threshold(3);

    let agent = Agent::builder("agent://local/loop-guarded")
        .provider(provider)
        .tool(repeat_tool)
        .tool_policy(policy)
        .build()
        .await
        .unwrap();

    let res = agent.prompt("Read the missing file").await.unwrap();
    assert_eq!(res, "Recovered from circuit breaker error.");

    // Execution should have been halted after reaching the circuit breaker threshold (e.g. 2 executed, 3rd trips)
    assert!(
        call_count.load(Ordering::SeqCst) < 4,
        "Circuit breaker must prevent 4th identical execution! Actual calls: {}",
        call_count.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn test_parallel_tool_flooding_is_capped() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_clone = call_count.clone();

    let echo_tool = DynamicTool::new(
        "echo",
        "Echoes text",
        json!({"type": "object", "properties": {"msg": {"type": "string"}}}),
        move |args| {
            let count = count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(args["msg"].as_str().unwrap_or_default().to_string())
            }
        },
    );

    let provider = MockProvider::new();
    // Model emits 10 tool calls in a single round
    let tool_calls: Vec<ToolCall> = (0..10)
        .map(|i| ToolCall {
            id: format!("call_{i}"),
            name: "echo".into(),
            arguments: json!({"msg": format!("msg_{i}")}),
        })
        .collect();

    provider
        .push_response(ModelResponse {
            content: None,
            thinking: None,
            tool_calls,
            usage: Default::default(),
        })
        .await;

    provider
        .push_response(ModelResponse::text("Handled capped tools.", 10, 10))
        .await;

    // Policy caps at 3 calls per round
    let policy = ToolPolicy::new().with_max_calls_per_round(3);

    let agent = Agent::builder("agent://local/flood-guarded")
        .provider(provider)
        .tool(echo_tool)
        .tool_policy(policy)
        .build()
        .await
        .unwrap();

    let res = agent.prompt("Run parallel batch").await.unwrap();
    assert_eq!(res, "Handled capped tools.");

    // Exactly 3 calls should have run, remaining 7 intercepted
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        3,
        "Flooding cap must restrict execution to exactly 3 calls"
    );
}

#[tokio::test]
async fn test_tool_quota_exhaustion_defense() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_clone = call_count.clone();

    let expensive_tool = DynamicTool::new(
        "expensive_benchmark",
        "Runs heavy benchmark",
        json!({"type": "object"}),
        move |_| {
            let count = count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok("Benchmark complete".into())
            }
        },
    );

    let provider = MockProvider::new();
    // Model requests expensive tool 3 times across turns
    for i in 0..3 {
        provider
            .push_response(ModelResponse {
                content: None,
                thinking: None,
                tool_calls: vec![ToolCall {
                    id: format!("call_{i}"),
                    name: "expensive_benchmark".into(),
                    arguments: json!({}),
                }],
                usage: Default::default(),
            })
            .await;
    }

    provider
        .push_response(ModelResponse::text("Finished turn.", 10, 10))
        .await;

    // Quota set to max 2 invocations
    let policy = ToolPolicy::new().with_tool_quota("expensive_benchmark", 2);

    let agent = Agent::builder("agent://local/quota-guarded")
        .provider(provider)
        .tool(expensive_tool)
        .tool_policy(policy)
        .build()
        .await
        .unwrap();

    let res = agent.prompt("Execute benchmarks").await.unwrap();
    assert_eq!(res, "Finished turn.");

    // Exactly 2 invocations executed, 3rd intercepted by quota guard
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "Quota guard must strictly prevent execution past limit 2"
    );
}

#[tokio::test]
async fn test_active_tool_scoping_assembly_and_violation_defense() {
    let read_tool = DynamicTool::new(
        "read_file",
        "Reads file",
        json!({"type": "object"}),
        |_| async { Ok("file content".into()) },
    )
    .with_scope(ToolScope::ReadOnly);

    let write_tool = DynamicTool::new(
        "write_file",
        "Writes file",
        json!({"type": "object"}),
        |_| async { Ok("written".into()) },
    )
    .with_scope(ToolScope::Workspace);

    let provider = MockProvider::new();
    let provider_clone = provider.clone();

    // 1. In Planning mode, only ReadOnly scope is active
    let agent = Agent::builder("agent://local/scoped-agent")
        .provider(provider)
        .tool(read_tool)
        .tool(write_tool)
        .with_active_scopes(vec![ToolScope::ReadOnly])
        .build()
        .await
        .unwrap();

    // Verify model specs only advertise ReadOnly tools
    let specs = agent.model_specs();
    let tool_names: Vec<&str> = specs
        .iter()
        .filter_map(|s| s["function"]["name"].as_str())
        .collect();
    assert_eq!(tool_names, vec!["read_file"]);

    // If model hallucinates and attempts to call write_file anyway:
    provider_clone
        .push_response(ModelResponse {
            content: None,
            thinking: None,
            tool_calls: vec![ToolCall {
                id: "call_illegal".into(),
                name: "write_file".into(),
                arguments: json!({}),
            }],
            usage: Default::default(),
        })
        .await;

    provider_clone
        .push_response(ModelResponse::text("Acknowledged read-only scope.", 10, 10))
        .await;

    let res = agent.prompt("Attempt writing").await.unwrap();
    assert_eq!(res, "Acknowledged read-only scope.");
}
