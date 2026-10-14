//! Multi-party conversations: an agent holds many sessions, one per conversation
//! surface, and a channel is a surface in its own right.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::ChannelWake;
use nuo_agent::agent::Agent;
use nuo_agent::provider::MockProvider;
use nuo_agent::session::{InMemorySessionStore, SessionKey, SessionStore};
use acp::{AgentAddress, ChannelId, Fabric, SubscriptionMode};
use std::sync::Arc;

fn addr(uri: &str) -> AgentAddress {
    AgentAddress::parse(uri).unwrap()
}

/// Joins an additional real member, so it can legitimately publish to the room's
/// channels. Publication requires membership, which is what keeps every notifiable
/// subscriber able to read what it was notified about.
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

struct Fixture {
    room: Fabric,
    store: Arc<InMemorySessionStore>,
    agent: Agent,
    provider: Arc<MockProvider>,
}

async fn fixture() -> Fixture {
    let room = Fabric::new("squad");
    let store = Arc::new(InMemorySessionStore::new());
    let provider = Arc::new(MockProvider::new());

    let agent = Agent::builder("agent://local/worker")
        .name("Worker")
        .description("Participates in conversations")
        .provider_arc(provider.clone())
        .with_store_arc(store.clone())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    Fixture {
        room,
        store,
        agent,
        provider,
    }
}

#[tokio::test]
async fn an_agent_holds_one_session_per_conversation_surface() {
    let f = fixture().await;

    // Three distinct conversations involving the same agent.
    f.provider.push_text("answer to A").await;
    f.provider.push_text("answer to B").await;
    f.provider.push_text("channel contribution").await;

    let peer_a = SessionKey::for_peer(&addr("agent://local/a"));
    let peer_b = SessionKey::for_peer(&addr("agent://local/b"));
    let channel_id = ChannelId::new("ops").unwrap();
    f.room.open_channel(channel_id.clone()).await;
    let channel_key = SessionKey::for_channel(channel_id.clone());

    f.agent
        .run_turn(&peer_a, uuid::Uuid::new_v4(), "talk to A")
        .await
        .unwrap();
    f.agent
        .run_turn(&peer_b, uuid::Uuid::new_v4(), "talk to B")
        .await
        .unwrap();
    f.agent
        .run_turn(&channel_key, uuid::Uuid::new_v4(), "talk in ops")
        .await
        .unwrap();

    // Each surface has its own persisted session: not one global session, and
    // not a fragment per message.
    for key in [&peer_a, &peer_b, &channel_key] {
        assert!(
            f.store.load(&key.as_session_id()).await.unwrap().is_some(),
            "missing session for {}",
            key.as_session_id()
        );
    }

    // And they are mutually isolated.
    let a_session = f
        .store
        .load(&peer_a.as_session_id())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !a_session
            .messages
            .iter()
            .any(|m| m.content.contains("talk to B"))
    );
    assert!(
        !a_session
            .messages
            .iter()
            .any(|m| m.content.contains("talk in ops"))
    );

    // The channel session is labelled as multi-party, not as a peer.
    assert_eq!(channel_key.peer(), None);
    assert_eq!(channel_key.channel(), Some(&channel_id));
    assert_eq!(channel_key.as_session_id(), "channel:ops");
}

#[tokio::test]
async fn a_channel_session_is_distinct_from_each_participants_direct_session() {
    let f = fixture().await;

    // Same peer, two surfaces: a direct conversation AND a shared channel where
    // that peer also speaks.
    let peer = addr("agent://local/alice");
    let _alice = member(&f.room, peer.as_str(), "Alice").await;
    let direct = SessionKey::for_peer(&peer);

    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = f.room.open_channel(channel_id.clone()).await;
    let shared = SessionKey::for_channel(channel_id.clone());

    assert_ne!(
        direct.as_session_id(),
        shared.as_session_id(),
        "a shared discussion must not be folded into a private conversation"
    );

    f.provider.push_text("private answer").await;
    f.provider.push_text("group answer").await;

    f.agent
        .run_turn(&direct, uuid::Uuid::new_v4(), "private matter")
        .await
        .unwrap();

    f.room
        .subscribe(f.agent.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();
    f.room
        .publish(&peer, &channel_id, "group matter", Vec::new())
        .await
        .unwrap();
    f.agent
        .run_channel_turn(&channel_id, uuid::Uuid::new_v4(), ChannelWake::Requested)
        .await
        .unwrap();

    // Private history must not contain the group discussion, and vice versa.
    let direct_session = f
        .store
        .load(&direct.as_session_id())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !direct_session
            .messages
            .iter()
            .any(|m| m.content.contains("group matter")),
        "channel content leaked into the direct conversation"
    );

    let channel_session = f
        .store
        .load(&shared.as_session_id())
        .await
        .unwrap()
        .unwrap();
    assert!(
        channel_session
            .messages
            .iter()
            .any(|m| m.content.contains("group matter")),
        "channel session should remember the discussion"
    );
    assert!(
        !channel_session
            .messages
            .iter()
            .any(|m| m.content.contains("private matter")),
        "private content leaked into the channel conversation"
    );
}

#[tokio::test]
async fn channel_traffic_reaches_only_channel_sessions() {
    let f = fixture().await;

    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = f.room.open_channel(channel_id.clone()).await;
    f.room
        .subscribe(f.agent.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();
    let alice = addr("agent://local/alice");
    let _alice = member(&f.room, alice.as_str(), "Alice").await;
    f.room
        .publish(&alice, &channel_id, "news", Vec::new())
        .await
        .unwrap();

    // A 1:1 conversation must not carry the group's traffic: that would both
    // contaminate the private context and pay its token cost repeatedly.
    let direct = f
        .agent
        .system_prompt_for(&SessionKey::for_peer(&addr("agent://local/alice")))
        .await;
    assert!(
        !direct.contains("news"),
        "channel traffic must not leak into a direct prompt"
    );

    // The channel conversation messages timeline does see it.
    let session = f
        .agent
        .session_for(&SessionKey::for_channel(channel_id))
        .await
        .unwrap();
    let timeline = session
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        timeline.contains("news"),
        "channel conversation must see its own traffic in messages timeline: {timeline}"
    );
}

#[tokio::test]
async fn a_restored_session_refreshes_its_view_of_the_world() {
    // Regression: a restored session used to keep the system prompt captured at
    // creation, so it never saw a channel that gained traffic later.
    let f = fixture().await;

    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = f.room.open_channel(channel_id.clone()).await;
    f.room
        .subscribe(f.agent.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();

    let key = SessionKey::for_channel(channel_id.clone());
    f.provider.push_text("first pass").await;
    f.agent
        .run_turn(&key, uuid::Uuid::new_v4(), "look at ops")
        .await
        .unwrap();

    // Traffic arrives after the session already exists on disk.
    let bob = addr("agent://local/bob");
    let _bob = member(&f.room, bob.as_str(), "Bob").await;
    f.room
        .publish(&bob, &channel_id, "late breaking news", Vec::new())
        .await
        .unwrap();

    // The reloaded session carries the latest traffic appended at the tail of messages timeline.
    let restored = f.agent.session_for(&key).await.unwrap();
    let timeline = restored
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        timeline.contains("late breaking news"),
        "restored session must see traffic appended to its messages timeline: {timeline}"
    );

    // Verify system prompt is kept frozen & pure for prefix caching
    let prompt = f.agent.system_prompt_for(&key).await;
    assert!(
        !prompt.contains("late breaking news"),
        "system prompt must stay frozen to protect prefix caching"
    );
}

#[tokio::test]
async fn refreshing_a_prompt_does_not_duplicate_or_reorder_history() {
    let f = fixture().await;
    let key = SessionKey::for_peer(&addr("agent://local/alice"));

    f.provider.push_text("one").await;
    f.provider.push_text("two").await;

    f.agent
        .run_turn(&key, uuid::Uuid::new_v4(), "first")
        .await
        .unwrap();
    let after_first = f.agent.session_for(&key).await.unwrap();
    let count_first = after_first.messages.len();

    f.agent
        .run_turn(&key, uuid::Uuid::new_v4(), "second")
        .await
        .unwrap();
    let after_second = f.agent.session_for(&key).await.unwrap();

    // Exactly one system message, still in first position, history preserved.
    let system_count = after_second
        .messages
        .iter()
        .filter(|m| m.role == nuo_agent::Role::System)
        .count();
    assert_eq!(system_count, 1, "refresh must replace, never append");
    assert_eq!(
        after_second.messages.first().unwrap().role,
        nuo_agent::Role::System
    );

    assert!(
        after_second.messages.len() > count_first,
        "history must grow across turns"
    );
}

#[tokio::test]
async fn a_non_member_channel_turn_is_refused() {
    let f = fixture().await;
    let channel_id = ChannelId::new("private").unwrap();
    f.room.open_channel(channel_id.clone()).await;

    // Not subscribed: reading is not permitted, and saying so beats silently
    // returning nothing.
    let err = f
        .agent
        .run_channel_turn(&channel_id, uuid::Uuid::new_v4(), ChannelWake::Requested)
        .await
        .unwrap_err();
    assert!(
        matches!(err, nuo_agent::AgentError::Session(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn a_channel_turn_with_nothing_unread_does_nothing() {
    let f = fixture().await;
    let channel_id = ChannelId::new("quiet").unwrap();
    let _channel = f.room.open_channel(channel_id.clone()).await;
    f.room
        .subscribe(f.agent.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();

    f.provider.push_text("should not be called").await;

    let outcome = f
        .agent
        .run_channel_turn(&channel_id, uuid::Uuid::new_v4(), ChannelWake::Requested)
        .await
        .unwrap();

    assert!(outcome.is_none(), "no unread traffic means no turn");
    assert!(
        f.provider.requests().await.is_empty(),
        "an empty channel must not spend inference"
    );
}

#[tokio::test]
async fn a_channel_turn_consumes_the_backlog_once() {
    let f = fixture().await;
    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = f.room.open_channel(channel_id.clone()).await;
    f.room
        .subscribe(f.agent.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();

    let alice = addr("agent://local/alice");
    let _alice = member(&f.room, alice.as_str(), "Alice").await;

    for msg in ["one", "two", "three"] {
        f.room
            .publish(&alice, &channel_id, msg, Vec::new())
            .await
            .unwrap();
    }

    f.provider.push_text("contributed").await;

    let first = f
        .agent
        .run_channel_turn(&channel_id, uuid::Uuid::new_v4(), ChannelWake::Requested)
        .await
        .unwrap();
    assert!(first.is_some());

    // Second pass finds nothing: the cursor advanced, so the same discussion is
    // not re-reasoned about.
    let second = f
        .agent
        .run_channel_turn(&channel_id, uuid::Uuid::new_v4(), ChannelWake::Requested)
        .await
        .unwrap();
    assert!(second.is_none(), "backlog must not be consumed twice");

    // The model saw all three messages in one coherent turn.
    let requests = f.provider.requests().await;
    assert_eq!(requests.len(), 1);
    let saw_all = ["one", "two", "three"].iter().all(|m| {
        requests[0]
            .messages
            .iter()
            .any(|msg| msg.content.contains(m))
    });
    assert!(saw_all, "the turn must see the whole discussion, in order");
}

#[tokio::test]
async fn thread_and_channel_and_peer_keys_never_collide() {
    let peer = addr("agent://local/alice");
    let channel = ChannelId::new("ops").unwrap();

    let ids = [
        SessionKey::for_peer(&peer).as_session_id(),
        SessionKey::for_thread(&peer, "t1").as_session_id(),
        SessionKey::for_channel(channel).as_session_id(),
    ];

    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "session ids must be unambiguous");

    // A peer-less key is exactly what distinguishes multi-party from 1:1.
    assert!(
        SessionKey::for_channel(ChannelId::new("ops").unwrap())
            .peer()
            .is_none()
    );
    assert!(SessionKey::for_peer(&peer).channel().is_none());
}

#[tokio::test]
async fn a_truncated_backlog_is_announced_and_the_rest_stays_unread() {
    // A channel turn reads a bounded window. Presenting that as a complete
    // transcript would invite the agent to answer a partial discussion as though
    // it were the whole of it, so the prompt must say the view is partial — and
    // nothing may be dropped: the remainder stays unread.
    use acp::SubscriptionMode;

    let f = fixture().await;
    let channel_id = ChannelId::new("deep").unwrap();
    let _channel = f.room.open_channel(channel_id.clone()).await;
    f.room
        .subscribe(f.agent.address(), &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();

    let alice = addr("agent://local/alice");
    let _alice = member(&f.room, alice.as_str(), "Alice").await;

    // More than the per-turn limit.
    let total = 30;
    for i in 0..total {
        f.room
            .publish(&alice, &channel_id, format!("message {i}"), Vec::new())
            .await
            .unwrap();
    }

    f.provider.push_text("caught up").await;
    f.agent
        .run_channel_turn(&channel_id, uuid::Uuid::new_v4(), ChannelWake::Requested)
        .await
        .unwrap();

    // The model was told its view was partial.
    let request = &f.provider.requests().await[0];
    let prompt = request
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        prompt.contains("older unread messages remain"),
        "a truncated backlog must be announced, not presented as complete: {prompt}"
    );

    // And the remainder is still pending rather than silently skipped.
    let channel = f.room.channel(&channel_id).await.unwrap();
    let remaining = channel
        .subscription_of(f.agent.address())
        .await
        .unwrap()
        .cursor;
    assert!(
        remaining < total,
        "messages beyond the window must stay unread, cursor={remaining}, total={total}"
    );

    // Draining again continues where the first turn stopped.
    f.provider.push_text("second pass").await;
    f.agent
        .run_channel_turn(&channel_id, uuid::Uuid::new_v4(), ChannelWake::Requested)
        .await
        .unwrap();
    let after = channel
        .subscription_of(f.agent.address())
        .await
        .unwrap()
        .cursor;
    assert!(
        after > remaining,
        "a second turn must make progress on the backlog: {remaining} -> {after}"
    );
}
