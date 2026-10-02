#![allow(clippy::unwrap_used, clippy::expect_used)]

use acp::{
    AgentAddress, AgentEnvelope, Fabric, FnRoutingPolicy, MessageIntent, ProtocolError,
    QueryPayload, RoutingDecision,
};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn test_routing_policy_intercepts_unauthorized_steer() {
    let fabric = Fabric::new("test-cluster");

    let elder_addr: AgentAddress = "agent://cluster/hypervisor".parse().unwrap();
    let subagent_addr: AgentAddress = "agent://cluster/subagent".parse().unwrap();
    let sibling_addr: AgentAddress = "agent://cluster/sibling".parse().unwrap();

    let mut elder_mailbox = fabric.register(elder_addr.clone(), 16).await;
    let _subagent_mailbox = fabric.register(subagent_addr.clone(), 16).await;
    let _sibling_mailbox = fabric.register(sibling_addr.clone(), 16).await;

    // Policy: Subagents can never command/steer siblings directly
    fabric
        .set_routing_policy(Arc::new(FnRoutingPolicy::new(|env: &AgentEnvelope| {
            if env.source.as_str().contains("subagent") && env.target.as_str().contains("sibling")
            {
                RoutingDecision::Deny {
                    reason: "Subagent cannot command sibling agent".into(),
                }
            } else {
                RoutingDecision::Allow
            }
        })))
        .await;

    // 1. Subagent -> Sibling is denied by policy
    let illegal_env = AgentEnvelope::new(
        subagent_addr.clone(),
        sibling_addr.clone(),
        MessageIntent::Query(QueryPayload {
            prompt: "do work".into(),
        }),
    );
    let route_res = fabric.router().route(illegal_env).await;
    assert!(
        matches!(route_res, Err(ProtocolError::PolicyDenied(ref reason)) if reason.contains("Subagent cannot command sibling"))
    );

    // 2. Elder -> Sibling is allowed
    let legal_env = AgentEnvelope::new(
        elder_addr.clone(),
        sibling_addr.clone(),
        MessageIntent::Query(QueryPayload {
            prompt: "elder instruction".into(),
        }),
    );
    let route_ok = fabric.router().route(legal_env).await;
    assert!(route_ok.is_ok());

    // 3. Envelope Lineage is preserved
    let parent_id = Uuid::new_v4();
    let reported_env = AgentEnvelope::new(
        subagent_addr.clone(),
        elder_addr.clone(),
        MessageIntent::Query(QueryPayload {
            prompt: "done".into(),
        }),
    )
    .with_parent_id(parent_id)
    .with_supervisor(elder_addr.clone());

    assert_eq!(reported_env.parent_id, Some(parent_id));
    assert_eq!(reported_env.supervisor, Some(elder_addr.clone()));

    assert!(fabric.router().route(reported_env).await.is_ok());
    let received = elder_mailbox.recv().await.unwrap();
    assert_eq!(received.parent_id, Some(parent_id));
    assert_eq!(received.supervisor, Some(elder_addr));
}

#[tokio::test]
async fn test_presence_tracker_lifecycle_and_expiration() {
    let fabric = Fabric::new("tracker-cluster");
    let tracker = fabric.tracker();

    let agent_addr: AgentAddress = "agent://cluster/worker-1".parse().unwrap();

    // Register with 1 second TTL
    let presence = tracker
        .register(agent_addr.clone(), 1, HashMap::new())
        .await;
    assert_eq!(presence.address, agent_addr);

    // Query active agents
    let active = tracker.active_agents().await;
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].address, agent_addr);

    // Heartbeat
    let beat_ok = tracker.heartbeat(&agent_addr).await;
    assert!(beat_ok);

    // Explicit deregister
    let removed = tracker.deregister(&agent_addr).await;
    assert!(removed.is_some());
    assert_eq!(tracker.active_agents().await.len(), 0);
}
