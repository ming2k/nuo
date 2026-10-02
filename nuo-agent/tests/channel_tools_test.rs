//! The channel tools, exercised through the cognitive loop rather than the
//! protocol API: an agent must be able to drive channels from natural language
//! tool calls alone.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ModelResponse};
use acp::{ChannelId, Fabric, SubscriptionMode};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn an_agent_publishes_to_a_channel_via_tool_call() {
    let room = Fabric::new("squad");
    let agent = Agent::builder("agent://local/dev")
        .name("Dev Agent")
        .description("Writes code")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let channel_id = ChannelId::new("ops").unwrap();
    room.open_channel(channel_id.clone()).await;

    // Register a second member so the agent has both tool families available.
    assert!(
        agent
            .tool_names()
            .contains(&"publish_to_channel".to_string()),
        "channel tools must be registered for a room member"
    );

    // Invoke via the loop: the provider asks to publish, then concludes.

    // Invoke via the loop: the provider asks to publish, then concludes.
    let provider = Arc::new(MockProvider::new());
    provider
        .push_response(ModelResponse::tool_call(
            "call_pub",
            "publish_to_channel",
            json!({"channel": "ops", "message": "deploy starting"}),
            20,
            10,
        ))
        .await;
    provider
        .push_response(ModelResponse::text("Notified the team.", 30, 8))
        .await;

    let agent = Agent::builder("agent://local/dev2")
        .name("Dev Agent")
        .description("Writes code")
        .provider_arc(provider)
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let answer = agent
        .prompt("Tell the team the deploy is starting.")
        .await
        .unwrap();
    assert_eq!(answer, "Notified the team.");

    // The message really landed on the channel.
    let channel = room.channel(&channel_id).await.unwrap();
    let messages = channel.recent(10).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "deploy starting");
    assert_eq!(messages[0].from, *agent.address());
}

#[tokio::test]
async fn an_agent_subscribes_and_reads_via_tool_calls() {
    let room = Fabric::new("squad");
    let channel_id = ChannelId::new("ops").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;

    // Seed history from another member.
    let poster = Agent::builder("agent://local/poster")
        .name("Poster")
        .description("Posts")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();
    room.publish(poster.address(), &channel_id, "seeded history", Vec::new())
        .await
        .unwrap();

    let provider = Arc::new(MockProvider::new());
    // The agent first subscribes...
    provider
        .push_response(ModelResponse::tool_call(
            "call_sub",
            "subscribe_to_channel",
            json!({"channel": "ops", "mode": "mentions_only"}),
            20,
            10,
        ))
        .await;
    // ...then reads the channel...
    provider
        .push_response(ModelResponse::tool_call(
            "call_read",
            "read_channel",
            json!({"channel": "ops", "limit": 5}),
            30,
            12,
        ))
        .await;
    // ...then concludes.
    provider
        .push_response(ModelResponse::text("Caught up on ops.", 40, 9))
        .await;

    let agent = Agent::builder("agent://local/reader")
        .name("Reader")
        .description("Reads")
        .provider_arc(provider)
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let answer = agent.prompt("Catch up on the ops channel.").await.unwrap();
    assert_eq!(answer, "Caught up on ops.");

    // The subscription took effect with the requested policy.
    let sub = channel.subscription_of(agent.address()).await.unwrap();
    assert_eq!(sub.mode, SubscriptionMode::MentionsOnly);
}

#[tokio::test]
async fn read_channel_tool_reports_history_to_the_model() {
    let room = Fabric::new("squad");
    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = room.open_channel(channel_id.clone()).await;

    let poster = Agent::builder("agent://local/poster")
        .name("Poster")
        .description("Posts")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();
    room.subscribe(poster.address(), &channel_id, SubscriptionMode::All)
        .await
        .unwrap();
    room.publish(
        poster.address(),
        &channel_id,
        "important detail",
        Vec::new(),
    )
    .await
    .unwrap();

    let provider = Arc::new(MockProvider::new());
    provider
        .push_response(ModelResponse::tool_call(
            "call_read",
            "read_channel",
            json!({"channel": "ops"}),
            20,
            10,
        ))
        .await;
    provider
        .push_response(ModelResponse::text("Read it.", 30, 6))
        .await;

    let reader = Agent::builder("agent://local/reader")
        .name("Reader")
        .description("Reads")
        .provider_arc(provider.clone())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    reader.prompt("Read the ops channel.").await.unwrap();

    // The channel content must appear in the observation handed back to the model.
    let requests = provider.requests().await;
    let saw_history = requests.iter().any(|req| {
        req.messages
            .iter()
            .any(|msg| msg.content.contains("important detail"))
    });
    assert!(
        saw_history,
        "read_channel must return channel history as an observation"
    );
}

#[tokio::test]
async fn open_channel_tool_creates_the_channel() {
    let room = Fabric::new("squad");
    let provider = Arc::new(MockProvider::new());
    provider
        .push_response(ModelResponse::tool_call(
            "call_open",
            "open_channel",
            json!({"channel": "incidents-p0"}),
            20,
            10,
        ))
        .await;
    provider
        .push_response(ModelResponse::text("Opened it.", 30, 6))
        .await;

    let agent = Agent::builder("agent://local/dev")
        .name("Dev")
        .description("Dev")
        .provider_arc(provider)
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    agent.prompt("Open an incidents channel.").await.unwrap();

    let id = ChannelId::new("incidents-p0").unwrap();
    assert!(
        room.channel(&id).await.is_some(),
        "channel should now exist"
    );
}

#[tokio::test]
async fn invalid_channel_names_are_rejected_as_tool_errors() {
    let room = Fabric::new("squad");
    let provider = Arc::new(MockProvider::new());
    // A malformed name must fail loudly rather than creating a stray channel.
    provider
        .push_response(ModelResponse::tool_call(
            "call_bad",
            "open_channel",
            json!({"channel": "Bad Name"}),
            20,
            10,
        ))
        .await;
    provider
        .push_response(ModelResponse::text(
            "Understood, that name was invalid.",
            30,
            8,
        ))
        .await;

    let agent = Agent::builder("agent://local/dev")
        .name("Dev")
        .description("Dev")
        .provider_arc(provider.clone())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let answer = agent.prompt("Open a channel.").await.unwrap();
    assert_eq!(answer, "Understood, that name was invalid.");

    // No channel was created, and the error was surfaced to the model.
    assert!(room.channels().await.is_empty());
    let requests = provider.requests().await;
    let saw_error = requests.iter().any(|req| {
        req.messages
            .iter()
            .any(|msg| msg.content.contains("invalid channel"))
    });
    assert!(saw_error, "the naming error must be reported to the model");
}
