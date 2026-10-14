//! Verifies uncompromised tool behaviors: cooperative cancellation,
//! capability risk profiles, and execution error observations.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ModelResponse};
use nuo_agent::tools::{
    ApprovalDecision, ApprovalHandler, DynamicTool, RiskProfile, ToolCallRequest, ToolContext,
    ToolError, ToolOutput,
};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

struct RiskAuditingApprover {
    seen_risk: Arc<std::sync::Mutex<Option<RiskProfile>>>,
}

#[async_trait::async_trait]
impl ApprovalHandler for RiskAuditingApprover {
    async fn request_approval(
        &self,
        request: &ToolCallRequest,
        _ctx: &ToolContext,
    ) -> Result<ApprovalDecision, ToolError> {
        *self.seen_risk.lock().unwrap() = Some(request.risk_profile);
        Ok(ApprovalDecision::Approved)
    }
}

#[tokio::test]
async fn tool_call_propagates_risk_profile_to_approval_handler() {
    let provider = Arc::new(MockProvider::new());
    let seen_risk = Arc::new(std::sync::Mutex::new(None));

    let tool = DynamicTool::new(
        "delete_cluster",
        "Destroys an entire k8s cluster",
        json!({"type": "object"}),
        |_| async move { Ok("cluster deleted".to_string()) },
    )
    .with_risk(RiskProfile::Destructive)
    .with_approval_check(|_| true);

    let approver = Arc::new(RiskAuditingApprover {
        seen_risk: seen_risk.clone(),
    });

    let agent = Agent::builder("agent://local/k8s_admin")
        .name("ClusterAdmin")
        .provider_arc(provider.clone())
        .tool(tool)
        .with_approval_handler_arc(approver)
        .build()
        .await
        .unwrap();

    // Model requests tool call
    provider
        .push_response(ModelResponse::tool_call(
            "call_cluster_1",
            "delete_cluster",
            json!({}),
            10,
            10,
        ))
        .await;
    provider
        .push_response(ModelResponse::text("Cluster deletion finalized.", 20, 10))
        .await;

    let reply = agent.prompt("destroy the test cluster").await.unwrap();
    assert_eq!(reply, "Cluster deletion finalized.");
    assert_eq!(
        *seen_risk.lock().unwrap(),
        Some(RiskProfile::Destructive),
        "Approval handler must receive the declared capability risk profile"
    );
}

#[tokio::test]
async fn cancellation_cooperation_in_tool_registry() {
    use nuo_tool::ToolRegistry;

    let cancel = CancellationToken::new();
    let was_executed = Arc::new(AtomicBool::new(false));
    let flag = was_executed.clone();

    let tool = DynamicTool::with_context(
        "heavy_task",
        "A long running computation",
        json!({}),
        move |ctx, _| {
            let flag = flag.clone();
            async move {
                ctx.check_cancelled("heavy_task")?;
                flag.store(true, Ordering::SeqCst);
                Ok(ToolOutput::success("done"))
            }
        },
    );

    let mut registry = ToolRegistry::new();
    registry.register(tool);

    // Cancel before execution
    cancel.cancel();
    let ctx = ToolContext::default().with_cancel_token(cancel);

    let err = registry
        .execute(&ctx, "heavy_task", json!({}))
        .await
        .unwrap_err();

    assert!(matches!(err, ToolError::Cancelled(_)));
    assert!(!was_executed.load(Ordering::SeqCst));
}
