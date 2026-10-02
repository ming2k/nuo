#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::Agent;
use nuo_agent::provider::MockProvider;
use acp::{AgentAddress, Fabric};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn test_p2p_handshake_identity_pairing_and_affinity() {
    let room = Fabric::new("squad");

    let client_inst_id = Uuid::new_v4();
    let server_inst_id = Uuid::new_v4();

    let client_provider = MockProvider::new();
    let server_provider = MockProvider::new();
    server_provider
        .push_text("Handshake confirmed and task executed")
        .await;

    let client = Agent::builder("agent://local/client")
        .name("Client")
        .instance_id(client_inst_id)
        .provider(client_provider)
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let server = Agent::builder("agent://local/server")
        .name("Server")
        .instance_id(server_inst_id)
        .provider(server_provider)
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let (server, server_inbox) = server.into_serving().unwrap();
    tokio::spawn(async move {
        let _ = server.serve(server_inbox).await;
    });

    // 1. Phase 1: Handshake and pairing
    let ack = client
        .handshake_peer(
            &AgentAddress::parse("agent://local/server").unwrap(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert_eq!(ack.instance_id, server_inst_id);
    assert!(!ack.association_id.is_empty());
    assert_eq!(ack.manifest.name, "Server");

    // 2. Phase 2: Delegate with explicit session intent
    let outcome = client
        .delegate_to(
            &AgentAddress::parse("agent://local/server").unwrap(),
            "Process order 992",
        )
        .await
        .unwrap();

    assert!(outcome.is_resolved());
    assert_eq!(
        outcome.output(),
        Some("Handshake confirmed and task executed")
    );
    assert!(outcome.session_id().is_some());
}

#[tokio::test]
async fn test_explicit_session_intent_continuation_and_isolation() {
    let fabric = Fabric::new("session-mesh");

    let client_provider = MockProvider::new();
    let worker_provider = Arc::new(MockProvider::new());

    let client = Agent::builder("agent://local/initiator")
        .name("Initiator")
        .provider(client_provider)
        .connect_to(&fabric)
        .with_p2p_delegation()
        .build()
        .await
        .unwrap();

    let worker = Agent::builder("agent://local/worker")
        .name("Worker")
        .provider_arc(worker_provider.clone())
        .connect_to(&fabric)
        .build()
        .await
        .unwrap();

    let (worker, worker_inbox) = worker.into_serving().unwrap();
    tokio::spawn(async move {
        let _ = worker.serve(worker_inbox).await;
    });

    let target_addr = AgentAddress::parse("agent://local/worker").unwrap();

    // 1. First task: store variable X = 100
    worker_provider.push_text("Stored X = 100").await;
    let res_1 = client
        .delegate_with_thread(&target_addr, "Set X = 100", Some("session_a"))
        .await
        .unwrap();
    assert_eq!(res_1.output(), Some("Stored X = 100"));
    let bound_session_id = res_1.session_id().unwrap();
    assert!(bound_session_id.contains("session_a"));

    // 2. Continue the session using SessionIntent::Continue
    worker_provider.push_text("X is still 100").await;
    let res_2 = client
        .delegate_with_thread(&target_addr, "What is X?", Some("session_a"))
        .await
        .unwrap();
    assert_eq!(res_2.output(), Some("X is still 100"));

    // 3. New Session using SessionIntent::New
    worker_provider
        .push_text("Fresh context, no prior variables")
        .await;
    let res_3 = client
        .delegate_with_thread(&target_addr, "What is X?", Some("session_b"))
        .await
        .unwrap();
    assert_eq!(res_3.output(), Some("Fresh context, no prior variables"));
}
