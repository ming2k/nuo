//! Agent-to-channel mode: publication must reach every subscriber's reasoning
//! context, subscription policy must control interruption, and a member that was
//! busy must be able to catch up.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::MockProvider;
use nuo_agent::session::SessionKey;
use acp::{ChannelId, Fabric, SubscriptionMode};
use std::time::Duration;

async fn member(room: &Fabric, uri: &str, name: &str) -> Agent {
    Agent::builder(uri)
        .name(name)
        .description(format!("{name} participant"))
        .provider(MockProvider::new())
        .connect_to(room)
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn publishing_reaches_every_subscribers_context() {
    let room = Fabric::new("squad");

    let poster = member(&room, "agent://local/poster", "Poster").await;
    let listener = member(&room, "agent://local/listener", "Listener").await;

    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(listener.address(), &channel_id, SubscriptionMode::All)
        .await
        .unwrap();

    // Nothing unread yet, so no section is injected.
    let before = listener.system_prompt().await;
    assert!(!before.contains("Unread channel messages"));

    room.publish(
        poster.address(),
        &channel_id,
        "standup in 5 minutes",
        Vec::new(),
    )
    .await
    .unwrap();

    let channel_key = SessionKey::for_channel(channel_id.clone());

    // The message is projected monotonically onto the listener's channel conversation timeline.
    let session = listener.session_for(&channel_key).await.unwrap();
    let timeline_text = session
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        timeline_text.contains("Unread channel messages"),
        "channel traffic should surface in the channel session's messages timeline: {timeline_text}"
    );
    assert!(timeline_text.contains("standup in 5 minutes"));
    assert!(timeline_text.contains("agent://local/poster"));

    // Verify system prompt is kept clean & prefix-cache friendly!
    let prompt = listener.system_prompt_for(&channel_key).await;
    assert!(
        !prompt.contains("Unread channel messages"),
        "System prompt must stay pure and frozen for prefix caching"
    );

    // A 1:1 conversation with the poster must NOT carry the group's traffic:
    // mixing surfaces would contaminate the private exchange.
    let direct = listener
        .system_prompt_for(&SessionKey::for_peer(poster.address()))
        .await;
    assert!(
        !direct.contains("standup in 5 minutes"),
        "channel traffic must not leak into a direct conversation: {direct}"
    );
}

#[tokio::test]
async fn a_member_that_was_busy_catches_up_from_the_channel_log() {
    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;
    let latecomer = member(&room, "agent://local/latecomer", "Latecomer").await;

    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(latecomer.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();

    for notice in ["first notice", "second notice", "third notice"] {
        room.publish(poster.address(), &channel_id, notice, Vec::new())
            .await
            .unwrap();
    }

    // The latecomer processed no envelope at all, yet its channel conversation
    // timeline sees the full history.
    let session = latecomer
        .session_for(&SessionKey::for_channel(channel_id.clone()))
        .await
        .unwrap();
    let timeline = session
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(timeline.contains("first notice"));
    assert!(timeline.contains("second notice"));
    assert!(timeline.contains("third notice"));
}

#[tokio::test]
async fn cursor_reading_is_idempotent_and_never_replays() {
    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;
    let reader = member(&room, "agent://local/reader", "Reader").await;

    let channel_id = ChannelId::new("ops").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(reader.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();

    room.publish(poster.address(), &channel_id, "one", Vec::new())
        .await
        .unwrap();
    room.publish(poster.address(), &channel_id, "two", Vec::new())
        .await
        .unwrap();

    // First drain consumes both and advances the cursor.
    let first = channel.drain_for(reader.address(), 10).await.unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].body, "one");

    // Second drain is empty: catch-up is idempotent.
    let second = channel.drain_for(reader.address(), 10).await.unwrap();
    assert!(second.is_empty(), "re-reading must not replay messages");

    // New traffic is picked up without re-delivering old traffic.
    room.publish(poster.address(), &channel_id, "three", Vec::new())
        .await
        .unwrap();
    let third = channel.drain_for(reader.address(), 10).await.unwrap();
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].body, "three");
}

#[tokio::test]
async fn mentions_only_subscription_is_not_woken_by_unrelated_traffic() {
    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;
    let watcher = member(&room, "agent://local/watcher", "Watcher").await;
    let other = member(&room, "agent://local/other", "Other").await;

    let channel_id = ChannelId::new("busy").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(
        watcher.address(),
        &channel_id,
        SubscriptionMode::MentionsOnly,
    )
    .await
    .unwrap();

    // Unrelated chatter: recorded, but nobody is notified.
    let (_, notified) = room
        .publish(poster.address(), &channel_id, "general chatter", Vec::new())
        .await
        .unwrap();
    assert!(
        notified.is_empty(),
        "mention-only subscriber must not be woken"
    );

    // A mention does wake the watcher.
    let (_, notified) = room
        .publish(
            poster.address(),
            &channel_id,
            "watcher, please review",
            vec![watcher.address().clone()],
        )
        .await
        .unwrap();
    assert_eq!(notified, vec![watcher.address().clone()]);

    // Traffic addressed to someone else must not wake the watcher either.
    let (_, notified) = room
        .publish(
            poster.address(),
            &channel_id,
            "other, this is for you",
            vec![other.address().clone()],
        )
        .await
        .unwrap();
    assert!(notified.is_empty());

    // Despite never being woken, the watcher can still read everything.
    let history = channel.recent(10).await;
    assert_eq!(
        history.len(),
        3,
        "notification policy must not affect visibility"
    );
}

#[tokio::test]
async fn manual_subscription_is_never_woken_but_still_reads() {
    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;
    let auditor = member(&room, "agent://local/auditor", "Auditor").await;

    let channel_id = ChannelId::new("audit").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(auditor.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();

    let (_, notified) = room
        .publish(
            poster.address(),
            &channel_id,
            "audit-worthy event",
            vec![auditor.address().clone()],
        )
        .await
        .unwrap();
    assert!(
        notified.is_empty(),
        "manual subscribers are never woken, even when mentioned"
    );

    // The message is still in the log, and the auditor's channel conversation
    // timeline still sees it — visibility is unaffected by notification policy.
    assert_eq!(channel.len().await, 1);
    let session = auditor
        .session_for(&SessionKey::for_channel(channel_id.clone()))
        .await
        .unwrap();
    let timeline = session
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(timeline.contains("audit-worthy event"));
}

#[tokio::test]
async fn changing_notification_mode_preserves_the_read_cursor() {
    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;
    let reader = member(&room, "agent://local/reader", "Reader").await;

    let channel_id = ChannelId::new("ops").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(reader.address(), &channel_id, SubscriptionMode::All)
        .await
        .unwrap();

    room.publish(poster.address(), &channel_id, "seen", Vec::new())
        .await
        .unwrap();
    let consumed = channel.drain_for(reader.address(), 10).await.unwrap();
    assert_eq!(consumed.len(), 1);

    // Switching mode must not reset progress, which would double-read history.
    let sub = room
        .subscribe(
            reader.address(),
            &channel_id,
            SubscriptionMode::MentionsOnly,
        )
        .await
        .unwrap();
    assert_eq!(sub.cursor, 1, "cursor must survive a mode change");

    let again = channel.drain_for(reader.address(), 10).await.unwrap();
    assert!(again.is_empty(), "mode change must not replay history");
}

#[tokio::test]
async fn a_standalone_agent_has_no_channel_surface() {
    let agent = Agent::builder("agent://local/solo")
        .name("Solo")
        .description("Alone")
        .provider(MockProvider::new())
        .build()
        .await
        .unwrap();

    let prompt = agent.system_prompt().await;
    assert!(!prompt.contains("Unread channel messages"));
    assert!(!prompt.contains("Other agents you can delegate to"));

    let tools = agent.tool_names();
    for tool in ["publish_to_channel", "read_channel", "subscribe_to_channel"] {
        assert!(
            !tools.contains(&tool.to_string()),
            "standalone agent must not have `{tool}`"
        );
    }
}

#[tokio::test]
async fn leaving_a_room_removes_channel_subscriptions() {
    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;
    let leaver = member(&room, "agent://local/leaver", "Leaver").await;

    let channel_id = ChannelId::new("ops").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(leaver.address(), &channel_id, SubscriptionMode::All)
        .await
        .unwrap();
    assert!(channel.is_subscribed(leaver.address()).await);

    room.leave(leaver.address()).await;

    assert!(
        !channel.is_subscribed(leaver.address()).await,
        "leaving must not leave a subscription delivering to a departed agent"
    );

    // Publishing after departure notifies nobody for the departed agent.
    let (_, notified) = room
        .publish(poster.address(), &channel_id, "after you left", Vec::new())
        .await
        .unwrap();
    assert!(!notified.contains(leaver.address()));
}

#[tokio::test]
async fn published_messages_are_ordered_and_sequence_numbered() {
    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;

    let channel_id = ChannelId::new("ops").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;

    for i in 1..=5 {
        let (message, _) = room
            .publish(
                poster.address(),
                &channel_id,
                format!("msg {i}"),
                Vec::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            message.seq, i,
            "sequence numbers must be gapless and ordered"
        );
    }

    let all = channel.messages_after(0, 100).await;
    let bodies: Vec<&str> = all.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, vec!["msg 1", "msg 2", "msg 3", "msg 4", "msg 5"]);
}

#[tokio::test]
async fn publishing_to_a_missing_channel_fails_loudly() {
    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;

    let missing = ChannelId::new("does-not-exist").unwrap();
    let err = room
        .publish(poster.address(), &missing, "hello", Vec::new())
        .await
        .unwrap_err();

    assert!(
        matches!(err, acp::ProtocolError::ChannelNotFound(_)),
        "expected ChannelNotFound, got {err:?}"
    );
}
#[tokio::test]
async fn subscription_policy_decides_whether_a_notification_costs_a_round() {
    // The three modes must be observably different, not three names for one
    // behaviour:
    //
    // - `All`          — "wake me on every message": does cost a round.
    // - `MentionsOnly` — unaddressed traffic must cost nothing, while still
    //                    showing up in the session's prompt when next read.
    use std::sync::Arc;

    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;

    let subscriber_provider = Arc::new(MockProvider::new());
    let subscriber = Agent::builder("agent://local/subscriber")
        .name("Subscriber")
        .description("Listens to everything")
        .provider_arc(subscriber_provider.clone())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let watcher_provider = Arc::new(MockProvider::new());
    let watcher = Agent::builder("agent://local/watcher")
        .name("Watcher")
        .description("Only wants to be mentioned")
        .provider_arc(watcher_provider.clone())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(subscriber.address(), &channel_id, SubscriptionMode::All)
        .await
        .unwrap();
    room.subscribe(
        watcher.address(),
        &channel_id,
        SubscriptionMode::MentionsOnly,
    )
    .await
    .unwrap();

    subscriber_provider.push_text("noted").await;

    let (subscriber, subscriber_inbox) = subscriber.into_serving().unwrap();
    let (watcher, watcher_inbox) = watcher.into_serving().unwrap();
    // The serving agent is moved into its task; keep a handle to inspect the
    // watcher's session afterwards.
    let watcher_query = watcher.clone();
    let sub_task = tokio::spawn(async move {
        let _ = subscriber.serve(subscriber_inbox).await;
    });
    let watch_task = tokio::spawn(async move {
        let _ = watcher.serve(watcher_inbox).await;
    });

    room.publish(poster.address(), &channel_id, "noisy traffic", Vec::new())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    // `All` asked to be woken on every message, so it was.
    assert_eq!(
        subscriber_provider.requests().await.len(),
        1,
        "an `all` subscriber asked to be woken on every message and must be"
    );

    // `MentionsOnly` was not addressed, so it spends nothing.
    assert!(
        watcher_provider.requests().await.is_empty(),
        "unaddressed traffic must not spend inference for a mentions-only subscriber"
    );

    // The traffic is still visible to the watcher: a suppressed notification
    // costs latency, never content.
    let session = watcher_query
        .session_for(&SessionKey::for_channel(channel_id.clone()))
        .await
        .unwrap();
    let timeline = session
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        timeline.contains("noisy traffic"),
        "suppressed traffic must still be readable in the session timeline: {timeline}"
    );

    sub_task.abort();
    watch_task.abort();
}

#[tokio::test]
async fn a_burst_of_messages_collapses_into_one_turn() {
    // A channel turn drains the whole backlog, so N notifications must not mean
    // N turns: `drain_for` advances the cursor, and later turns find nothing
    // unread and spend no inference.
    use std::sync::Arc;

    let room = Fabric::new("squad");
    let poster = member(&room, "agent://local/poster", "Poster").await;

    let provider = Arc::new(MockProvider::new());
    for i in 0..5 {
        provider.push_text(format!("ack {i}")).await;
    }

    let subscriber = Agent::builder("agent://local/subscriber")
        .name("Subscriber")
        .description("Listens to everything")
        .provider_arc(provider.clone())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(subscriber.address(), &channel_id, SubscriptionMode::All)
        .await
        .unwrap();

    let (subscriber, inbox) = subscriber.into_serving().unwrap();
    let task = tokio::spawn(async move {
        let _ = subscriber.serve(inbox).await;
    });

    for i in 0..5 {
        room.publish(
            poster.address(),
            &channel_id,
            format!("burst {i}"),
            Vec::new(),
        )
        .await
        .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    let requests = provider.requests().await;
    assert!(
        !requests.is_empty(),
        "an `all` subscriber must be woken by the burst"
    );
    assert!(
        requests.len() < 5,
        "the backlog must not be re-reasoned once per message, got {} turns",
        requests.len()
    );
    // Whatever turn ran saw the whole burst, not just one message.
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.content.contains("burst 0")),
        "the turn must see the start of the burst"
    );

    task.abort();
}
