#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::Agent;
use nuo_agent::provider::MockProvider;
use acp::Fabric;

#[tokio::test]
async fn test_agent_connect_to_unified_fabric_with_orthogonal_surfaces() {
    let fabric = Fabric::new("test-mesh");

    let provider = MockProvider::new();

    // 1. Surface A: Dedicated P2P Pipeline Agent (Clean 1:1 Prompt without Channel Tool Pollution)
    let p2p_agent = Agent::builder("agent://local/p2p-worker")
        .name("P2PWorker")
        .provider(provider.clone())
        .connect_to(&fabric)
        .with_p2p_delegation()
        .build()
        .await
        .unwrap();

    let p2p_tools = p2p_agent.advertised_tool_names();
    assert_eq!(
        p2p_tools,
        vec!["delegate_to_peer", "list_peers"],
        "P2P-configured agent must advertise ONLY 1:1 delegation tools, zero channel clutter"
    );

    // 2. Surface B: Full Collaborative Group Agent
    let group_agent = Agent::builder("agent://local/group-member")
        .name("GroupMember")
        .provider(provider)
        .connect_to(&fabric)
        .with_channel_collaboration()
        .build()
        .await
        .unwrap();

    let group_tools = group_agent.advertised_tool_names();
    assert!(
        group_tools.contains(&"publish_to_channel".to_string()),
        "Group-configured agent must expose channel tools"
    );
    assert!(
        group_tools.contains(&"read_channel".to_string()),
        "Group-configured agent must expose read_channel"
    );
    assert!(
        group_tools.contains(&"delegate_to_peer".to_string()),
        "Group-configured agent also retains delegation tool"
    );
}
