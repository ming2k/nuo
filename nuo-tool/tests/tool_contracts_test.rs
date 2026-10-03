#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_tool::{
    AlwaysApprove, ApprovalDecision, ApprovalHandler, DynamicTool, McpTool, RiskProfile, Tool,
    ToolCallRequest, ToolContext, ToolError, ToolRegistry,
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn dynamic_tool_basic_execution() {
    let tool = DynamicTool::new(
        "calc",
        "Adds two numbers",
        json!({"type": "object"}),
        |args| async move {
            let a = args["a"].as_i64().unwrap_or(0);
            let b = args["b"].as_i64().unwrap_or(0);
            Ok((a + b).to_string())
        },
    );

    let ctx = ToolContext::default();
    let res = tool.execute(&ctx, json!({"a": 2, "b": 3})).await.unwrap();
    assert_eq!(res.content(), "5");
    assert!(!res.is_error());
}

#[tokio::test]
async fn cooperative_cancellation_is_honored() {
    let cancel = CancellationToken::new();
    cancel.cancel(); // Pre-cancelled

    let tool = DynamicTool::new("dummy", "desc", json!({}), |_| async {
        Ok("never reached".to_string())
    });

    let mut registry = ToolRegistry::new();
    registry.register(tool);

    let ctx = ToolContext::default().with_cancel_token(cancel);
    let err = registry
        .execute(&ctx, "dummy", json!({}))
        .await
        .unwrap_err();

    match err {
        ToolError::Cancelled(name) => assert_eq!(name, "dummy"),
        other => panic!("expected Cancelled, got {other:?}"),
    }
}

#[tokio::test]
async fn risk_profile_and_approval_contract() {
    let tool = DynamicTool::new("wipe_disk", "destroys data", json!({}), |_| async {
        Ok("done".to_string())
    })
    .with_risk(RiskProfile::Destructive)
    .with_approval_check(|args| args.get("force").and_then(|v| v.as_bool()) == Some(true));

    assert_eq!(tool.risk_profile(), RiskProfile::Destructive);

    let ctx = ToolContext::default();
    assert!(!tool.requires_approval(&ctx, &json!({"force": false})));
    assert!(tool.requires_approval(&ctx, &json!({"force": true})));

    let approver = AlwaysApprove;
    let req = ToolCallRequest {
        call_id: "c1".into(),
        tool_name: "wipe_disk".into(),
        arguments: json!({"force": true}),
        risk_profile: tool.risk_profile(),
    };
    let decision = approver.request_approval(&req, &ctx).await.unwrap();
    assert_eq!(decision, ApprovalDecision::Approved);
}

#[tokio::test]
async fn mcp_adapter_invokes_handler() {
    let tool = McpTool::new(
        "git",
        "status",
        "Checks status",
        json!({}),
        Arc::new(|tool_name, _args| {
            let name = tool_name.to_string();
            Box::pin(async move { Ok(format!("{name}: clean")) })
        }),
    );

    assert_eq!(tool.server_name(), "git");
    let ctx = ToolContext::default();
    let out = tool.execute(&ctx, json!({})).await.unwrap();
    assert_eq!(out.content(), "status: clean");
}
