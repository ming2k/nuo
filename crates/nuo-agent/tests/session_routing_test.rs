//! Session routing, serialization, and steering.
//!
//! These assert the concurrency model rather than a feature: a session has a
//! single writer, conversations are isolated from each other, and guidance
//! reaches a running turn at a round boundary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ModelResponse};
use nuo_agent::session::{InMemorySessionStore, SessionKey, SessionStore};
use nuo_agent::tools::DynamicTool;
use acp::{AgentAddress, SteerAction};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn peer(uri: &str) -> AgentAddress {
    AgentAddress::parse(uri).unwrap()
}

/// Builds an agent that records the sequence of tool invocations, so interleaving
/// becomes observable.
async fn tracking_agent(
    provider: Arc<MockProvider>,
    log: Arc<std::sync::Mutex<Vec<String>>>,
    store: Option<Arc<dyn SessionStore>>,
) -> Agent {
    let recorder = log.clone();
    let mut builder = Agent::builder("agent://local/worker")
        .name("Worker")
        .description("Processes tasks")
        .provider_arc(provider)
        .tool(DynamicTool::new(
            "record",
            "Records a step",
            json!({
                "type": "object",
                "properties": {"step": {"type": "string"}},
                "required": ["step"],
                "additionalProperties": false
            }),
            move |args| {
                let recorder = recorder.clone();
                async move {
                    let step = args["step"].as_str().unwrap_or_default().to_string();
                    recorder.lock().unwrap().push(step.clone());
                    Ok(format!("recorded {step}"))
                }
            },
        ));

    if let Some(store) = store {
        builder = builder.with_store_arc(store);
    }

    builder.build().await.unwrap()
}

#[tokio::test]
async fn successive_turns_from_one_peer_share_session_history() {
    let store = Arc::new(InMemorySessionStore::new());
    let provider = Arc::new(MockProvider::new());
    provider.push_text("first answer").await;
    provider.push_text("second answer").await;

    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = tracking_agent(provider.clone(), log, Some(store.clone())).await;

    let key = SessionKey::for_peer(&peer("agent://local/caller"));

    agent
        .run_turn(&key, uuid::Uuid::new_v4(), "first task")
        .await
        .unwrap();
    agent
        .run_turn(&key, uuid::Uuid::new_v4(), "second task")
        .await
        .unwrap();

    // The second request must include the first exchange, which is what
    // continuity means in practice.
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 2);
    let second = &requests[1];
    let texts: Vec<&str> = second.messages.iter().map(|m| m.content.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("first task")),
        "second turn must see the first turn's prompt: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("first answer")),
        "second turn must see the first turn's answer: {texts:?}"
    );

    // And the persisted session reflects both exchanges.
    let saved = store.load(&key.as_session_id()).await.unwrap().unwrap();
    assert!(
        saved
            .messages
            .iter()
            .any(|m| m.content.contains("second answer")),
        "the answered turn must be persisted"
    );
}

#[tokio::test]
async fn different_peers_get_isolated_sessions() {
    let store = Arc::new(InMemorySessionStore::new());
    let provider = Arc::new(MockProvider::new());
    provider.push_text("answer to A").await;
    provider.push_text("answer to B").await;

    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = tracking_agent(provider.clone(), log, Some(store.clone())).await;

    let key_a = SessionKey::for_peer(&peer("agent://local/a"));
    let key_b = SessionKey::for_peer(&peer("agent://local/b"));

    agent
        .run_turn(&key_a, uuid::Uuid::new_v4(), "secret for A")
        .await
        .unwrap();
    agent
        .run_turn(&key_b, uuid::Uuid::new_v4(), "task for B")
        .await
        .unwrap();

    // B must not see A's conversation: cross-session leakage would be a
    // confidentiality failure, not just a context-quality issue.
    let requests = provider.requests().await;
    let b_request = &requests[1];
    let leaked = b_request
        .messages
        .iter()
        .any(|m| m.content.contains("secret for A"));
    assert!(!leaked, "peer B must not see peer A's history");

    // Each conversation has its own stored session.
    assert!(store.load(&key_a.as_session_id()).await.unwrap().is_some());
    assert!(store.load(&key_b.as_session_id()).await.unwrap().is_some());
    assert_ne!(key_a.as_session_id(), key_b.as_session_id());
}

#[tokio::test]
async fn explicit_threads_split_one_peer_into_independent_conversations() {
    let store = Arc::new(InMemorySessionStore::new());
    let provider = Arc::new(MockProvider::new());
    provider.push_text("release answer").await;
    provider.push_text("hotfix answer").await;

    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = tracking_agent(provider.clone(), log, Some(store.clone())).await;

    let same_peer = peer("agent://local/caller");
    let release = SessionKey::for_thread(&same_peer, "release-1");
    let hotfix = SessionKey::for_thread(&same_peer, "hotfix");

    agent
        .run_turn(&release, uuid::Uuid::new_v4(), "release work")
        .await
        .unwrap();
    agent
        .run_turn(&hotfix, uuid::Uuid::new_v4(), "hotfix work")
        .await
        .unwrap();

    // Same peer, separate threads: no cross-contamination.
    let requests = provider.requests().await;
    let hotfix_request = &requests[1];
    assert!(
        !hotfix_request
            .messages
            .iter()
            .any(|m| m.content.contains("release work")),
        "threads must be isolated even for the same peer"
    );

    assert_ne!(release.as_session_id(), hotfix.as_session_id());
}

#[tokio::test]
async fn turns_in_one_session_are_serialized_not_interleaved() {
    // The observable claim: for one session, tool effects happen in submission
    // order with no interleaving between turns.
    let provider = Arc::new(MockProvider::new());
    let concurrent = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let observed_peak = peak.clone();
    let observed_concurrent = concurrent.clone();

    let store = Arc::new(InMemorySessionStore::new());

    let agent = Agent::builder("agent://local/serial")
        .name("Serial")
        .description("Serializes turns")
        .provider_arc(provider.clone())
        .with_store_arc(store)
        .tool(DynamicTool::new(
            "slow_step",
            "Observes concurrency",
            json!({
                "type": "object",
                "properties": {"step": {"type": "string"}},
                "required": ["step"],
                "additionalProperties": false
            }),
            move |_args| {
                let concurrent = observed_concurrent.clone();
                let peak = observed_peak.clone();
                async move {
                    let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    concurrent.fetch_sub(1, Ordering::SeqCst);
                    Ok("done".into())
                }
            },
        ))
        .build()
        .await
        .unwrap();

    // Each turn: one tool call, then a final answer.
    for _ in 0..4 {
        provider
            .push_response(ModelResponse::tool_call(
                "c1",
                "slow_step",
                json!({"step": "x"}),
                10,
                5,
            ))
            .await;
        provider.push_text("finished").await;
    }

    let key = SessionKey::for_peer(&peer("agent://local/caller"));

    // Fire four turns at the SAME session concurrently.
    let mut handles = Vec::new();
    for i in 0..4 {
        let agent = agent.clone();
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            agent
                .run_turn(&key, uuid::Uuid::new_v4(), format!("turn {i}"))
                .await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "two turns in one session must never run concurrently"
    );
}

#[tokio::test]
async fn different_sessions_progress_concurrently() {
    // The counterpart to serialization: isolation must not mean a global lock.
    let provider = Arc::new(MockProvider::new());
    let concurrent = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let observed_peak = peak.clone();
    let observed_concurrent = concurrent.clone();

    let agent = Agent::builder("agent://local/parallel")
        .name("Parallel")
        .description("Runs sessions concurrently")
        .provider_arc(provider.clone())
        .tool(DynamicTool::new(
            "slow_step",
            "Observes concurrency",
            json!({
                "type": "object",
                "properties": {"step": {"type": "string"}},
                "required": ["step"],
                "additionalProperties": false
            }),
            move |_args| {
                let concurrent = observed_concurrent.clone();
                let peak = observed_peak.clone();
                async move {
                    let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    concurrent.fetch_sub(1, Ordering::SeqCst);
                    Ok("done".into())
                }
            },
        ))
        .build()
        .await
        .unwrap();

    for _ in 0..3 {
        provider
            .push_response(ModelResponse::tool_call(
                "c1",
                "slow_step",
                json!({"step": "x"}),
                10,
                5,
            ))
            .await;
        provider.push_text("finished").await;
    }

    // Three DIFFERENT sessions, fired concurrently.
    let mut handles = Vec::new();
    for i in 0..3 {
        let agent = agent.clone();
        handles.push(tokio::spawn(async move {
            let key = SessionKey::for_peer(&peer(&format!("agent://local/peer-{i}")));
            agent
                .run_turn(&key, uuid::Uuid::new_v4(), "concurrent task")
                .await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert!(
        peak.load(Ordering::SeqCst) >= 2,
        "separate sessions must be able to overlap, observed peak {}",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn steering_reaches_a_running_turn_at_a_round_boundary() {
    let provider = Arc::new(MockProvider::new());

    // Round 1: a slow tool call, which gives the steerer a real window.
    provider
        .push_response(ModelResponse::tool_call(
            "c1",
            "slow_record",
            json!({"step": "a"}),
            10,
            5,
        ))
        .await;
    // Round 2: the model sees the steering note and answers.
    provider
        .push_response(ModelResponse::text("applied the correction", 20, 8))
        .await;

    let agent = Agent::builder("agent://local/steered")
        .name("Steered")
        .description("Accepts guidance")
        .provider_arc(provider.clone())
        .tool(DynamicTool::new(
            "slow_record",
            "Takes time, giving steering a window",
            json!({
                "type": "object",
                "properties": {"step": {"type": "string"}},
                "required": ["step"],
                "additionalProperties": false
            }),
            |_args| async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok("recorded".into())
            },
        ))
        .build()
        .await
        .unwrap();

    let key = SessionKey::for_peer(&peer("agent://local/caller"));
    let correlation = uuid::Uuid::new_v4();

    // Steer from another task while the turn is mid-tool.
    let steerer = {
        let agent = agent.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                if agent.active_turns().await > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            agent
                .steer(
                    correlation,
                    &peer("agent://local/supervisor"),
                    "actually use step b, not a",
                    SteerAction::Note,
                )
                .await
        })
    };

    let answer = agent
        .run_turn(&key, correlation, "do the work")
        .await
        .unwrap();
    assert_eq!(answer, "applied the correction");

    assert!(
        steerer.await.unwrap(),
        "steering must have been accepted while the turn was running"
    );

    // The note must appear in the model's second-round context.
    let requests = provider.requests().await;
    let saw_note = requests.iter().any(|req| {
        req.messages
            .iter()
            .any(|m| m.content.contains("actually use step b"))
    });
    assert!(saw_note, "the steering instruction must reach the model");
}

#[tokio::test]
async fn steering_after_completion_is_reported_not_silently_lost() {
    let provider = Arc::new(MockProvider::new());
    provider.push_text("done quickly").await;

    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = tracking_agent(provider, log, None).await;

    let key = SessionKey::for_peer(&peer("agent://local/caller"));
    let correlation = uuid::Uuid::new_v4();

    agent
        .run_turn(&key, correlation, "fast task")
        .await
        .unwrap();

    // Nothing is running now, so guidance is refused rather than queued for a
    // turn that will never read it.
    let applied = agent
        .steer(
            correlation,
            &peer("agent://local/supervisor"),
            "too late",
            SteerAction::Note,
        )
        .await;
    assert!(
        !applied,
        "steering a settled turn must report failure, not pretend"
    );
}

#[tokio::test]
async fn queued_session_id_encodes_thread_and_peer() {
    let p = peer("agent://local/peer");
    assert_eq!(
        SessionKey::for_peer(&p).as_session_id(),
        "peer:agent://local/peer"
    );
    assert_eq!(
        SessionKey::for_thread(&p, "t1").as_session_id(),
        "thread:agent://local/peer#t1"
    );
    // An empty thread degrades to the plain peer conversation.
    assert_eq!(SessionKey::for_thread(&p, ""), SessionKey::for_peer(&p));
}
