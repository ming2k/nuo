//! End-to-end collaboration: a developer agent delegates issue tracking to a
//! Kanban agent that owns the tracker tooling.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ModelResponse};
use nuo_agent::tools::DynamicTool;
use acp::Fabric;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[tokio::test]
async fn dev_agent_delegates_issue_creation_to_kanban_agent() {
    let room = Fabric::new("product-squad");

    // ---------------------------------------------------------------------
    // Kanban agent: owns the issue tracker integration.
    // ---------------------------------------------------------------------
    let issues_created = Arc::new(AtomicUsize::new(0));
    let counter = issues_created.clone();

    let tracker_tool = DynamicTool::new(
        "linear_create_issue",
        "Creates an issue in the Linear tracker",
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "priority": {"type": "string"}
            },
            "required": ["title"],
            "additionalProperties": false
        }),
        move |args| {
            let counter = counter.clone();
            async move {
                let title = args["title"].as_str().unwrap_or("untitled");
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(format!("Created LIN-409 with title `{title}`"))
            }
        },
    );

    let kanban_provider = Arc::new(MockProvider::new());
    // Round 1: create the issue.
    kanban_provider
        .push_response(ModelResponse::tool_call(
            "call_tracker",
            "linear_create_issue",
            json!({"title": "null pointer crash in AuthMiddleware", "priority": "P0"}),
            20,
            10,
        ))
        .await;
    // Round 2: report the created issue back to the requester.
    kanban_provider
        .push_response(ModelResponse::text(
            "Logged the crash as LIN-409 and marked it P0.",
            40,
            12,
        ))
        .await;

    let kanban_agent = Agent::builder("agent://local/kanban")
        .name("Kanban Agent")
        .description("Tracks bugs and creates issues in Linear")
        .skill("issue-tracking")
        .provider_arc(kanban_provider)
        .tool(tracker_tool)
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    // ---------------------------------------------------------------------
    // Dev agent: diagnoses the bug, then delegates tracking to the peer.
    // ---------------------------------------------------------------------
    let dev_provider = Arc::new(MockProvider::new());
    // Round 1: delegate instead of trying to touch the tracker itself.
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
    // Round 2: fold the peer's answer into the user-facing reply.
    dev_provider
        .push_response(ModelResponse::text(
            "I traced the crash to a null check in AuthMiddleware and filed it as LIN-409 (P0).",
            60,
            22,
        ))
        .await;

    let dev_agent = Agent::builder("agent://local/dev")
        .name("Dev Agent")
        .description("Diagnoses and fixes code defects")
        .provider_arc(dev_provider)
        .connect_to(&room)
        .delegation_timeout(Duration::from_secs(10))
        .build()
        .await
        .unwrap();

    // The dev agent must know about the peer before it can delegate.
    let prompt = dev_agent.system_prompt().await;
    assert!(
        prompt.contains("Kanban Agent"),
        "peer directory missing: {prompt}"
    );

    // ---------------------------------------------------------------------
    // Run both agents' serve loops on their real inboxes.
    // `into_serving` is the single source of the inbox, so registration cannot
    // be duplicated.
    // ---------------------------------------------------------------------
    let (kanban_agent, kanban_inbox) = kanban_agent.into_serving().unwrap();
    let (dev_agent, dev_inbox) = dev_agent.into_serving().unwrap();

    let kanban_handle = {
        let agent = kanban_agent.clone();
        tokio::spawn(async move {
            let _ = agent.serve(kanban_inbox).await;
        })
    };

    let dev_handle = {
        let agent = dev_agent.clone();
        tokio::spawn(async move {
            let _ = agent.serve(dev_inbox).await;
        })
    };

    // ---------------------------------------------------------------------
    // Drive the dev agent as the IDE would; its loop delegates through the room.
    // ---------------------------------------------------------------------
    let answer = dev_agent
        .prompt("Found a null pointer crash in AuthMiddleware. Please make sure it's tracked.")
        .await
        .unwrap();

    assert!(
        answer.contains("LIN-409"),
        "dev agent should incorporate the peer's result, got: {answer}"
    );
    assert_eq!(
        issues_created.load(Ordering::SeqCst),
        1,
        "the Kanban agent's tracker tool must have run exactly once"
    );

    // The delegation returned to a clean state: no request left dangling.
    let dev_handle_ref = dev_agent
        .collaboration()
        .expect("dev agent is in the room")
        .mailbox()
        .clone();
    assert_eq!(
        dev_handle_ref.pending_reply_count().await,
        0,
        "delegation must not leak a pending reply subscription"
    );

    // The dev agent itself must never have had tracker access.
    let dev_tools = dev_agent.tool_names();
    assert!(
        !dev_tools.contains(&"linear_create_issue".to_string()),
        "dev agent must not own the tracker tool: {dev_tools:?}"
    );

    kanban_handle.abort();
    dev_handle.abort();
}
