//! The cognitive loop, exercised through the public [`Agent`] API.
//!
//! This deliberately drives an agent rather than a loop object: the loop is an
//! implementation detail, so tests must not be able to depend on it directly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ModelResponse};
use nuo_agent::session::{InMemorySessionStore, Session, SessionEvent, SessionStore};
use nuo_agent::tools::DynamicTool;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

#[tokio::test]
async fn agent_calls_a_tool_then_answers() {
    let provider = Arc::new(MockProvider::new());

    // Round 1: decide to add the numbers.
    provider
        .push_response(ModelResponse::tool_call(
            "call_add",
            "add",
            json!({"a": 10, "b": 32}),
            15,
            8,
        ))
        .await;
    // Round 2: answer using the observation.
    provider
        .push_response(ModelResponse::text("10 + 32 = 42", 30, 9))
        .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();

    let agent = Agent::builder("agent://local/calculator")
        .name("Calculator")
        .description("Performs arithmetic")
        .provider_arc(provider.clone())
        .tool(DynamicTool::new(
            "add",
            "Adds two integers",
            json!({
                "type": "object",
                "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
                "required": ["a", "b"],
                "additionalProperties": false
            }),
            move |args| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let a = args["a"].as_i64().unwrap_or_default();
                    let b = args["b"].as_i64().unwrap_or_default();
                    Ok((a + b).to_string())
                }
            },
        ))
        .build()
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel(32);
    let answer = agent
        .prompt_streaming("What is 10 + 32?", tx)
        .await
        .unwrap();

    assert_eq!(answer, "10 + 32 = 42");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "tool must run exactly once"
    );

    // Event stream reflects the real cycle.
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::RoundStarted { round: 1 }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::RoundStarted { round: 2 }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCallStarted { name, .. } if name == "add"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCallFinished { output, is_error: false, .. } if output == "42"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::Done { .. }))
    );

    // Usage from both rounds is accounted for.
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].messages.len(),
        2,
        "first request carries the system prompt plus the user turn"
    );
    assert_eq!(
        requests[1].messages.len(),
        4,
        "second request adds the assistant tool call and its observation"
    );
}

#[tokio::test]
async fn a_failing_tool_is_an_observation_not_an_abort() {
    let provider = Arc::new(MockProvider::new());

    // Round 1: call a tool that will fail.
    provider
        .push_response(ModelResponse::tool_call(
            "call_boom",
            "boom",
            json!({}),
            10,
            5,
        ))
        .await;
    // Round 2: the model recovers and answers.
    provider
        .push_response(ModelResponse::text(
            "The tool failed, so I answered directly.",
            20,
            8,
        ))
        .await;

    let agent = Agent::builder("agent://local/resilient")
        .name("Resilient")
        .description("Handles tool failures")
        .provider_arc(provider)
        .tool(DynamicTool::new(
            "boom",
            "Always fails",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            |_args| async move { Err(nuo_tool::ToolError::execution("boom", "exploded")) },
        ))
        .build()
        .await
        .unwrap();

    let answer = agent.prompt("call the broken tool").await.unwrap();
    assert_eq!(answer, "The tool failed, so I answered directly.");
}

#[tokio::test]
async fn sessions_persist_and_reload() {
    let provider = Arc::new(MockProvider::new());
    provider.push_text("remembered").await;

    let agent = Agent::builder("agent://local/memo")
        .name("Memo")
        .description("Remembers things")
        .provider_arc(provider)
        .build()
        .await
        .unwrap();

    let mut session = Session::new()
        .with_system_prompt("You are a memory test.")
        .with_budget(Default::default());
    session.add_user_message("remember this");

    let answer = agent.run_session(&mut session, None).await.unwrap();
    assert_eq!(answer, "remembered");

    let store = InMemorySessionStore::new();
    store.save(&session).await.unwrap();

    let reloaded = store.load(&session.id).await.unwrap().unwrap();
    assert_eq!(reloaded.messages.len(), session.messages.len());
    assert_eq!(reloaded.last_assistant_reply(), Some("remembered"));
}

#[tokio::test]
async fn round_budget_is_enforced() {
    let provider = Arc::new(MockProvider::new());
    // Always request a tool: the loop must stop at the budget rather than spin.
    for round in 0..12 {
        provider
            .push_response(ModelResponse::tool_call(
                format!("call_{round}"),
                "noop",
                json!({}),
                5,
                5,
            ))
            .await;
    }

    let agent = Agent::builder("agent://local/looper")
        .name("Looper")
        .description("Never stops on its own")
        .provider_arc(provider)
        .budget(nuo_agent::TokenBudget {
            max_rounds: 3,
            ..Default::default()
        })
        .tool(DynamicTool::new(
            "noop",
            "Does nothing",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            |_args| async move { Ok("ok".to_string()) },
        ))
        .build()
        .await
        .unwrap();

    let err = agent.prompt("loop forever").await.unwrap_err();
    assert!(
        matches!(err, nuo_agent::AgentError::MaxRoundsExceeded(3)),
        "expected round budget to trip, got {err:?}"
    );
}
