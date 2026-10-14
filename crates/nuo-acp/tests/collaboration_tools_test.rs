//! Behavioral coverage for the ACP collaboration tools (`create_acp_tools`),
//! exercised **directly** through the `nuo_tool::Tool` contract — no agent
//! runtime involved.
//!
//! Ported from the former `nuo-agent/tests/channel_tools_test.rs` (ADR-0009):
//! the tools' behavior must be covered where the tools live (`nuo-acp`), not
//! only in the runtime that happened to drive them. This keeps the production
//! wiring (`nuo/src/registry.rs` `collaboration` feature) verified even after
//! the orphaned `nuo-agent` runtime is retired.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_acp::{
    AcpToolContext, AgentAddress, AgentManifest, ChannelId, Fabric, SubscriptionMode,
    create_acp_tools,
};
use nuo_tool::Tool;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn manifest(uri: &str, name: &str, description: &str) -> AgentManifest {
    AgentManifest::new(AgentAddress::parse(uri).unwrap(), name, description)
}

/// Build the full ACP tool set for `agent://local/dev` joined to `room`.
async fn tools_for(room: &Fabric, uri: &str) -> (Vec<Arc<dyn Tool>>, AgentAddress) {
    let m = manifest(uri, "Dev", "Writes code");
    let addr = m.address.clone();
    let mailbox = room.join(m, 16).await;
    let handle = mailbox.handle();
    let ctx = Arc::new(AcpToolContext::for_fabric(
        addr.clone(),
        room.clone(),
        handle,
        Duration::from_secs(5),
    ));
    (create_acp_tools(ctx), addr)
}

fn find<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
    tools
        .iter()
        .find(|t| t.name() == name)
        .unwrap_or_else(|| panic!("tool `{name}` not found"))
}

#[tokio::test]
async fn the_full_collaboration_tool_set_is_exposed() {
    let room = Fabric::new("squad");
    let (tools, _) = tools_for(&room, "agent://local/dev").await;
    let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "delegate_to_peer",
            "list_channels",
            "list_peers",
            "open_channel",
            "publish_to_channel",
            "read_channel",
            "subscribe_to_channel",
        ]
    );
}

#[tokio::test]
async fn publish_to_channel_records_the_message() {
    let room = Fabric::new("squad");
    let channel_id = ChannelId::new("ops").unwrap();
    room.open_channel(channel_id.clone()).await;

    let (tools, addr) = tools_for(&room, "agent://local/dev").await;
    let publish = find(&tools, "publish_to_channel");
    let out = publish
        .execute_simple(json!({"channel": "ops", "message": "deploy starting"}))
        .await
        .unwrap();
    assert!(!out.is_error(), "publish failed: {}", out.to_text());

    let channel = room.channel(&channel_id).await.unwrap();
    let messages = channel.recent(10).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "deploy starting");
    assert_eq!(messages[0].from, addr);
}

#[tokio::test]
async fn subscribe_then_read_channel_returns_history() {
    let room = Fabric::new("squad");
    let channel_id = ChannelId::new("ops").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;

    let poster = manifest("agent://local/poster", "Poster", "Posts");
    let poster_addr = poster.address.clone();
    room.join(poster, 16).await;
    room.subscribe(&poster_addr, &channel_id, SubscriptionMode::All)
        .await
        .unwrap();
    room.publish(&poster_addr, &channel_id, "important detail", Vec::new())
        .await
        .unwrap();

    let (tools, addr) = tools_for(&room, "agent://local/reader").await;
    find(&tools, "subscribe_to_channel")
        .execute_simple(json!({"channel": "ops", "mode": "all"}))
        .await
        .unwrap();
    let sub = channel.subscription_of(&addr).await.unwrap();
    assert_eq!(sub.mode, SubscriptionMode::All);

    let out = find(&tools, "read_channel")
        .execute_simple(json!({"channel": "ops", "limit": 5}))
        .await
        .unwrap();
    assert!(
        out.to_text().contains("important detail"),
        "read_channel must return history, got: {}",
        out.to_text()
    );
}

#[tokio::test]
async fn open_channel_creates_the_channel() {
    let room = Fabric::new("squad");
    let (tools, _) = tools_for(&room, "agent://local/dev").await;
    find(&tools, "open_channel")
        .execute_simple(json!({"channel": "incidents-p0"}))
        .await
        .unwrap();

    let id = ChannelId::new("incidents-p0").unwrap();
    assert!(
        room.channel(&id).await.is_some(),
        "channel should now exist"
    );
}

#[tokio::test]
async fn invalid_channel_names_are_rejected() {
    let room = Fabric::new("squad");
    let (tools, _) = tools_for(&room, "agent://local/dev").await;
    let res = find(&tools, "open_channel")
        .execute_simple(json!({"channel": "Bad Name"}))
        .await;
    assert!(res.is_err(), "a malformed name must fail loudly");
    assert!(room.channels().await.is_empty(), "no stray channel created");
}

#[tokio::test]
async fn delegate_to_peer_resolves_against_a_live_peer() {
    let room = Fabric::new("squad");
    let kanban = manifest("agent://local/kanban", "Kanban", "Tracks issues");
    let kanban_addr = kanban.address.clone();
    let mut kanban_mailbox = room.join(kanban, 16).await;

    // Peer answers the delegation.
    let responder = tokio::spawn(async move {
        let request = kanban_mailbox.recv().await.unwrap();
        let reply = request.reply(
            kanban_addr,
            nuo_acp::MessageIntent::resolve("Created LIN-409"),
        );
        kanban_mailbox.send(reply).await.unwrap();
    });

    let (tools, _) = tools_for(&room, "agent://local/dev").await;
    let out = find(&tools, "delegate_to_peer")
        .execute_simple(json!({"peer": "agent://local/kanban", "task": "file an issue"}))
        .await
        .unwrap();
    assert!(
        out.to_text().contains("Created LIN-409"),
        "delegation must surface the peer's resolution, got: {}",
        out.to_text()
    );
    responder.await.unwrap();
}

#[tokio::test]
async fn delegate_to_self_is_refused() {
    let room = Fabric::new("squad");
    let (tools, _) = tools_for(&room, "agent://local/dev").await;
    let res = find(&tools, "delegate_to_peer")
        .execute_simple(json!({"peer": "agent://local/dev", "task": "recurse"}))
        .await;
    assert!(res.is_err(), "self-delegation must be refused");
}
