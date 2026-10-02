//! Compiles and runs the delegation example from the top-level `README.md`.
//!
//! Keeping the documented example executable prevents the README from drifting
//! away from the real API.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ModelResponse};
use nuo_agent::tools::DynamicTool;
use acp::Fabric;
use serde_json::json;

async fn readme_example() -> Result<(), Box<dyn std::error::Error>> {
    let fabric = Fabric::new("product-squad");

    // The Kanban agent owns the tracker tool and answers one delegation.
    let kanban_provider = MockProvider::new();
    kanban_provider
        .push_response(ModelResponse::tool_call(
            "call_tracker",
            "linear_create_issue",
            json!({"title": "null pointer crash in AuthMiddleware"}),
            20,
            10,
        ))
        .await;
    kanban_provider
        .push_response(ModelResponse::text("Logged as LIN-409", 40, 12))
        .await;

    let kanban = Agent::builder("agent://local/kanban")
        .name("Kanban Agent")
        .description("Tracks bugs and creates issues in Linear")
        .provider(kanban_provider)
        .tool(DynamicTool::new(
            "linear_create_issue",
            "Creates an issue in Linear",
            json!({
                "type": "object",
                "properties": {"title": {"type": "string"}},
                "required": ["title"],
                "additionalProperties": false
            }),
            |args| async move { Ok(format!("Created LIN-409: {}", args["title"])) },
        ))
        .connect_to(&fabric)
        .build()
        .await?;

    // The dev agent has no tracker tool; it delegates instead.
    let dev_provider = MockProvider::new();
    dev_provider
        .push_response(ModelResponse::tool_call(
            "call_delegate",
            "delegate_to_peer",
            json!({
                "peer": "agent://local/kanban",
                "task": "Create a P0 issue titled 'null pointer crash in AuthMiddleware'"
            }),
            30,
            18,
        ))
        .await;
    dev_provider
        .push_response(ModelResponse::text("Filed as LIN-409", 60, 22))
        .await;

    let dev = Agent::builder("agent://local/dev")
        .name("Dev Agent")
        .description("Diagnoses and fixes code defects")
        .provider(dev_provider)
        .connect_to(&fabric)
        .with_p2p_delegation()
        .build()
        .await?;

    let (kanban, kanban_inbox) = kanban.into_serving()?;
    let (dev, dev_inbox) = dev.into_serving()?;

    let kanban_task = tokio::spawn(async move {
        let _ = kanban.serve(kanban_inbox).await;
    });
    let dev_serving = dev.clone();
    let dev_task = tokio::spawn(async move {
        let _ = dev_serving.serve(dev_inbox).await;
    });

    let answer = dev
        .prompt("Null pointer crash in AuthMiddleware — please get it tracked.")
        .await?;

    assert!(
        answer.contains("LIN-409"),
        "documented example should surface the peer's result, got: {answer}"
    );

    kanban_task.abort();
    dev_task.abort();
    Ok(())
}

#[tokio::test]
async fn readme_delegation_example_works() {
    readme_example().await.expect("README example must run");
}
