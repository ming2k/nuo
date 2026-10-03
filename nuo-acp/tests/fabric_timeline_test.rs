#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_acp::{
    AgentAddress, AgentManifest, ChannelId, DelegationBudget, Fabric, MessageIntent,
    SubscriptionMode, Timeline,
};
use std::time::Duration;

#[tokio::test]
async fn test_unified_fabric_and_timeline_primitives() {
    let fabric = Fabric::new("core-mesh");

    let alice_addr = AgentAddress::parse("agent://local/alice").unwrap();
    let bob_addr = AgentAddress::parse("agent://local/bob").unwrap();

    let alice_manifest = AgentManifest::new(alice_addr.clone(), "Alice", "Developer");
    let bob_manifest = AgentManifest::new(bob_addr.clone(), "Bob", "Tester");

    // Connect both agents to the single unified fabric
    let alice_mailbox = fabric.join(alice_manifest, 32).await;
    let mut bob_mailbox = fabric.join(bob_manifest, 32).await;

    // 1. Primitive 1: Monotonic Timeline Log
    let timeline_id = ChannelId::new("engineering-timeline").unwrap();
    let timeline: Timeline = fabric.open_timeline(timeline_id.clone()).await;

    // Bob subscribes to the timeline
    fabric
        .subscribe(&bob_addr, &timeline_id, SubscriptionMode::All)
        .await
        .unwrap();

    // Alice publishes to the monotonic timeline
    let (msg, notified) = fabric
        .publish(
            &alice_addr,
            &timeline_id,
            "Build deployed at v1.2.0",
            vec![bob_addr.clone()],
        )
        .await
        .unwrap();

    assert_eq!(msg.seq, 1);
    assert_eq!(notified, vec![bob_addr.clone()]);
    assert_eq!(timeline.len().await, 1);

    // Bob receives the channel notification envelope via his mailbox
    let notification = bob_mailbox.recv().await.unwrap();
    assert!(matches!(
        notification.intent,
        MessageIntent::ChannelNotify(_)
    ));

    // Bob advances his timeline cursor
    let drained = timeline.drain_for(&bob_addr, 10).await.unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].body, "Build deployed at v1.2.0");

    // 2. Primitive 2: P2P 1:1 Request/Reply over the exact same Fabric
    let handle = bob_mailbox.handle();
    let bob_addr_for_task = bob_addr.clone();
    let bob_task = tokio::spawn(async move {
        while let Some(env) = bob_mailbox.recv().await {
            if let MessageIntent::Delegate(ref p) = env.intent {
                assert_eq!(p.task, "Run sanity tests");
                let reply = env.reply(
                    bob_addr_for_task.clone(),
                    MessageIntent::resolve("All tests passed: 42 green"),
                );
                handle.send(reply).await.unwrap();
                break;
            }
        }
    });

    let outcome = fabric
        .request(
            &alice_mailbox.handle(),
            &alice_addr,
            &bob_addr,
            "Run sanity tests",
            Some(DelegationBudget::default()),
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert!(outcome.is_resolved());
    assert_eq!(outcome.output(), Some("All tests passed: 42 green"));

    bob_task.await.unwrap();
}
