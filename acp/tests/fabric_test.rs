#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use acp::*;
use std::time::Duration;

fn manifest(uri: &str, name: &str, description: &str) -> AgentManifest {
    AgentManifest::new(AgentAddress::parse(uri).unwrap(), name, description)
}

#[tokio::test]
async fn room_delegation_is_correlated_and_resolves() {
    let room = Fabric::new("dev-squad");
    let dev = manifest("agent://local/dev", "Dev Agent", "Fixes bugs");
    let kanban = manifest("agent://local/kanban", "Kanban Agent", "Tracks issues");

    let dev_addr = dev.address.clone();
    let kanban_addr = kanban.address.clone();

    let dev_mailbox = room.join(dev, 16).await;
    let mut kanban_mailbox = room.join(kanban, 16).await;

    let dev_handle = dev_mailbox.handle();

    // Peer answers the delegation in a background task.
    let responder = tokio::spawn(async move {
        let request = kanban_mailbox.recv().await.unwrap();
        assert!(matches!(request.intent, MessageIntent::Delegate(_)));
        let reply = request.reply(
            kanban_addr,
            MessageIntent::resolve("Created LIN-409: null pointer crash"),
        );
        dev_handle.send(reply).await.unwrap();
    });

    let outcome = room
        .request(
            &dev_mailbox.handle(),
            &dev_addr,
            &AgentAddress::parse("agent://local/kanban").unwrap(),
            "Log a bug for the AuthMiddleware crash",
            None,
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert!(outcome.is_resolved());
    assert!(outcome.to_natural_language().contains("LIN-409"));
    responder.await.unwrap();
}

#[tokio::test]
async fn awaiting_a_reply_never_consumes_an_unrelated_peer_task() {
    // Regression: the previous mailbox design locked a single stream, so
    // awaiting a reply could swallow an unrelated incoming delegation.
    let room = Fabric::new("squad");

    let me = manifest("agent://local/me", "Me", "Delegator");
    let slow = manifest("agent://local/slow", "Slow", "Slow peer");
    let caller = manifest("agent://local/caller", "Caller", "Delegatee");

    let me_addr = me.address.clone();
    let slow_addr = slow.address.clone();
    let caller_addr = caller.address.clone();

    let mut my_mailbox = room.join(me, 16).await;
    let mut slow_mailbox = room.join(slow, 16).await;
    let caller_mailbox = room.join(caller, 16).await;

    let my_handle = my_mailbox.handle();

    // The slow peer replies only after we are already waiting on it.
    let slow_for_reply = slow_addr.clone();
    tokio::spawn(async move {
        let request = slow_mailbox.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        let reply = request.reply(slow_for_reply, MessageIntent::resolve("slow but done"));
        my_handle.send(reply).await.unwrap();
    });

    // Meanwhile an unrelated peer delegates a brand-new task to us.
    let caller_for_task = caller_addr.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let envelope = AgentEnvelope::new(
            caller_for_task,
            AgentAddress::parse("agent://local/me").unwrap(),
            MessageIntent::delegate("please do something else"),
        );
        caller_mailbox.send(envelope).await.unwrap();
    });

    let outcome = room
        .request(
            &my_mailbox.handle(),
            &me_addr,
            &slow_addr,
            "do the slow thing",
            None,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(outcome.to_natural_language(), "slow but done");

    // The unrelated delegation must still be waiting in the unsolicited inbox.
    let unrelated = tokio::time::timeout(Duration::from_secs(2), my_mailbox.recv())
        .await
        .expect("unrelated task was lost while awaiting a reply")
        .expect("inbox closed");
    assert_eq!(unrelated.source, caller_addr);
    match unrelated.intent {
        MessageIntent::Delegate(payload) => assert_eq!(payload.task, "please do something else"),
        other => panic!("expected Delegate, got {other:?}"),
    }
}

#[tokio::test]
async fn concurrent_delegations_are_matched_to_the_right_reply() {
    let room = Fabric::new("squad");
    let me = manifest("agent://local/me", "Me", "Delegator");
    let a = manifest("agent://local/a", "A", "Peer A");
    let b = manifest("agent://local/b", "B", "Peer B");

    let me_addr = me.address.clone();
    let addr_a = a.address.clone();
    let addr_b = b.address.clone();

    let my_mailbox = room.join(me, 16).await;
    let mut a_mailbox = room.join(a, 16).await;
    let mut b_mailbox = room.join(b, 16).await;

    let my_handle = my_mailbox.handle();

    // Peer B answers faster than peer A, proving replies are matched by
    // correlation rather than arrival order.
    let handle_b = my_handle.clone();
    let b_reply_addr = addr_b.clone();
    tokio::spawn(async move {
        let request = b_mailbox.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        handle_b
            .send(request.reply(b_reply_addr, MessageIntent::resolve("B answered first")))
            .await
            .unwrap();
    });

    let handle_a = my_handle.clone();
    let a_reply_addr = addr_a.clone();
    tokio::spawn(async move {
        let request = a_mailbox.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        handle_a
            .send(request.reply(a_reply_addr, MessageIntent::resolve("A answered later")))
            .await
            .unwrap();
    });

    let (outcome_a, outcome_b) = tokio::join!(
        room.request(
            &my_handle,
            &me_addr,
            &addr_a,
            "task for A",
            None,
            Duration::from_secs(5),
        ),
        room.request(
            &my_handle,
            &me_addr,
            &addr_b,
            "task for B",
            None,
            Duration::from_secs(5),
        ),
    );

    assert_eq!(outcome_a.unwrap().to_natural_language(), "A answered later");
    assert_eq!(outcome_b.unwrap().to_natural_language(), "B answered first");

    // No leaked subscriptions.
    assert_eq!(my_mailbox.handle().pending_reply_count().await, 0);
}

#[tokio::test]
async fn rejection_surfaces_as_a_rejected_outcome() {
    let room = Fabric::new("squad");
    let me = manifest("agent://local/me", "Me", "Delegator");
    let peer = manifest("agent://local/peer", "Peer", "Busy peer");

    let me_addr = me.address.clone();
    let peer_addr = peer.address.clone();

    let my_mailbox = room.join(me, 16).await;
    let mut peer_mailbox = room.join(peer, 16).await;

    let my_handle = my_mailbox.handle();
    tokio::spawn(async move {
        let request = peer_mailbox.recv().await.unwrap();
        let mut reject = MessageIntent::reject("missing reproduction steps");
        if let MessageIntent::Reject(payload) = &mut reject {
            payload.error_code = Some("INSUFFICIENT_CONTEXT".into());
        }
        my_handle
            .send(request.reply(peer_addr, reject))
            .await
            .unwrap();
    });

    let outcome = room
        .request(
            &my_mailbox.handle(),
            &me_addr,
            &AgentAddress::parse("agent://local/peer").unwrap(),
            "fix everything",
            None,
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert!(!outcome.is_resolved());
    let text = outcome.to_natural_language();
    assert!(text.contains("INSUFFICIENT_CONTEXT"));
    assert!(text.contains("missing reproduction steps"));
}

#[tokio::test]
async fn delegation_to_a_non_member_fails_fast() {
    let room = Fabric::new("squad");
    let me = manifest("agent://local/me", "Me", "Delegator");
    let my_mailbox = room.join(me, 8).await;

    let err = room
        .request(
            &my_mailbox.handle(),
            &AgentAddress::parse("agent://local/me").unwrap(),
            &AgentAddress::parse("agent://local/ghost").unwrap(),
            "nobody is listening",
            None,
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, ProtocolError::Unreachable(_)), "got {err:?}");
}

#[tokio::test]
async fn timeout_releases_the_pending_subscription() {
    let room = Fabric::new("squad");
    let me = manifest("agent://local/me", "Me", "Delegator");
    let peer = manifest("agent://local/peer", "Peer", "Never answers");

    let my_mailbox = room.join(me, 8).await;
    let _peer_mailbox = room.join(peer, 8).await;

    let err = room
        .request(
            &my_mailbox.handle(),
            &AgentAddress::parse("agent://local/me").unwrap(),
            &AgentAddress::parse("agent://local/peer").unwrap(),
            "will time out",
            None,
            Duration::from_millis(80),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, ProtocolError::RequestTimeout(_)),
        "got {err:?}"
    );
    assert_eq!(
        my_mailbox.handle().pending_reply_count().await,
        0,
        "timed-out request must not leak a subscription"
    );
}

#[tokio::test]
async fn channel_publish_is_recorded_for_every_subscriber() {
    use acp::{ChannelId, SubscriptionMode};

    let room = Fabric::new("squad");
    let sender = manifest("agent://local/sender", "Sender", "Posts notices");
    let peer_a = manifest("agent://local/a", "A", "Peer A");
    let peer_b = manifest("agent://local/b", "B", "Peer B");

    let sender_addr = sender.address.clone();
    let a_addr = peer_a.address.clone();
    let b_addr = peer_b.address.clone();

    let mut sender_mailbox = room.join(sender, 8).await;
    let mut a_mailbox = room.join(peer_a, 8).await;
    let mut b_mailbox = room.join(peer_b, 8).await;

    let channel_id = ChannelId::new("ops").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(&a_addr, &channel_id, SubscriptionMode::All)
        .await
        .unwrap();
    room.subscribe(&b_addr, &channel_id, SubscriptionMode::All)
        .await
        .unwrap();

    let (message, notified) = room
        .publish(
            &sender_addr,
            &channel_id,
            "standup in 5 minutes",
            Vec::new(),
        )
        .await
        .unwrap();

    assert_eq!(message.seq, 1);
    assert_eq!(notified.len(), 2, "both subscribers should be notified");

    // Subscribers receive a notification pointing at the log.
    for mailbox in [&mut a_mailbox, &mut b_mailbox] {
        let received = tokio::time::timeout(Duration::from_secs(2), mailbox.recv())
            .await
            .unwrap()
            .unwrap();
        match received.intent {
            MessageIntent::ChannelNotify(payload) => {
                assert_eq!(payload.channel, channel_id);
                assert_eq!(payload.seq, 1);
                assert!(payload.preview.contains("standup"));
            }
            other => panic!("expected ChannelNotify, got {other:?}"),
        }
    }

    // The publisher is not notified of its own post.
    let echoed = tokio::time::timeout(Duration::from_millis(100), sender_mailbox.recv()).await;
    assert!(
        echoed.is_err(),
        "publisher should not be notified of its own post"
    );

    // The log is authoritative and readable without having been listening.
    let reread = channel.messages_after(0, 10).await;
    assert_eq!(reread.len(), 1);
    assert_eq!(reread[0].body, "standup in 5 minutes");
}

#[tokio::test]
async fn hop_budget_decrements_and_eventually_refuses() {
    let budget = DelegationBudget::default();
    assert_eq!(budget.remaining_hops, DEFAULT_MAX_HOPS);

    let mut current = budget;
    for _ in 0..DEFAULT_MAX_HOPS {
        current = current.descend().unwrap();
    }
    assert_eq!(current.remaining_hops, 0);

    let err = current.descend().unwrap_err();
    assert!(
        matches!(err, ProtocolError::HopLimitExceeded(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn peer_directory_omits_self_and_reports_none_when_alone() {
    let room = Fabric::new("squad");
    let solo = manifest("agent://local/solo", "Solo", "Only member");
    let solo_addr = solo.address.clone();
    let _mailbox = room.join(solo, 8).await;

    assert!(room.render_peer_directory(&solo_addr).await.is_none());

    let peer = manifest("agent://local/peer", "Kanban Agent", "Tracks issues");
    let _peer_mailbox = room.join(peer, 8).await;

    let directory = room.render_peer_directory(&solo_addr).await.unwrap();
    assert!(directory.contains("Kanban Agent"));
    assert!(!directory.contains("Solo"));
}

// ---------------------------------------------------------------------------
// Room scope: a channel log belongs to the room that owns it, so every
// participant must be a member. These tests pin that boundary, because the
// failure mode it prevents — an address that can be notified but can never read
// what it was notified about — is silent at runtime.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_non_member_cannot_subscribe() {
    let room = Fabric::new("squad");
    let member = manifest("agent://local/member", "Member", "Present");
    let _mailbox = room.join(member, 8).await;

    let channel_id = ChannelId::new("ops").unwrap();
    room.open_channel(channel_id.clone()).await;

    // A remote address that never joined: subscribing it would create a
    // subscription on a log it cannot read.
    let outsider = AgentAddress::parse("agent://host-b/remote").unwrap();
    let err = room
        .subscribe(&outsider, &channel_id, SubscriptionMode::All)
        .await
        .unwrap_err();

    assert!(
        matches!(err, ProtocolError::Unreachable(_)),
        "expected Unreachable, got {err:?}"
    );
}

#[tokio::test]
async fn subscribing_to_a_missing_channel_is_reported() {
    let room = Fabric::new("squad");
    let member = manifest("agent://local/member", "Member", "Present");
    let addr = member.address.clone();
    let _mailbox = room.join(member, 8).await;

    let err = room
        .subscribe(
            &addr,
            &ChannelId::new("never-opened").unwrap(),
            SubscriptionMode::All,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, ProtocolError::ChannelNotFound(_)),
        "expected ChannelNotFound, got {err:?}"
    );
}

#[tokio::test]
async fn a_mention_of_a_non_member_is_rejected() {
    let room = Fabric::new("squad");
    let poster = manifest("agent://local/poster", "Poster", "Posts");
    let poster_addr = poster.address.clone();
    let _mailbox = room.join(poster, 8).await;

    let channel_id = ChannelId::new("ops").unwrap();
    room.open_channel(channel_id.clone()).await;

    // An unresolvable mention would be a notification nobody can receive —
    // indistinguishable, to the author, from being ignored.
    let ghost = AgentAddress::parse("agent://local/ghost").unwrap();
    let err = room
        .publish(&poster_addr, &channel_id, "ping", vec![ghost])
        .await
        .unwrap_err();

    assert!(
        matches!(err, ProtocolError::Unreachable(_)),
        "expected Unreachable, got {err:?}"
    );

    // And nothing was recorded: a rejected publication leaves no trace.
    let channel = room.channel(&channel_id).await.unwrap();
    assert!(
        channel.is_empty().await,
        "a rejected publish must not append to the log"
    );
}

#[tokio::test]
async fn a_notification_records_why_the_subscriber_was_woken() {
    let room = Fabric::new("squad");
    let poster = manifest("agent://local/poster", "Poster", "Posts");
    let watcher = manifest("agent://local/watcher", "Watcher", "Watches");
    let poster_addr = poster.address.clone();
    let watcher_addr = watcher.address.clone();

    let _poster_mailbox = room.join(poster, 8).await;
    let mut watcher_mailbox = room.join(watcher, 8).await;

    let channel_id = ChannelId::new("ops").unwrap();
    room.open_channel(channel_id.clone()).await;
    room.subscribe(&watcher_addr, &channel_id, SubscriptionMode::MentionsOnly)
        .await
        .unwrap();

    room.publish(
        &poster_addr,
        &channel_id,
        "watcher: over to you",
        vec![watcher_addr.clone()],
    )
    .await
    .unwrap();

    let envelope = tokio::time::timeout(Duration::from_secs(2), watcher_mailbox.recv())
        .await
        .expect("notification should arrive")
        .expect("inbox open");

    // The reason travels with the notification: by the time this is handled, the
    // message could already have been evicted by retention, so it cannot be
    // re-derived from the log.
    match envelope.intent {
        MessageIntent::ChannelNotify(payload) => {
            assert_eq!(payload.reason, NotifyReason::Mentioned);
            assert!(payload.reason.expects_attention());
        }
        other => panic!("expected ChannelNotify, got {other:?}"),
    }
}

#[tokio::test]
async fn an_all_subscriber_is_woken_without_being_addressed() {
    let room = Fabric::new("squad");
    let poster = manifest("agent://local/poster", "Poster", "Posts");
    let listener = manifest("agent://local/listener", "Listener", "Listens");
    let poster_addr = poster.address.clone();
    let listener_addr = listener.address.clone();

    let _poster_mailbox = room.join(poster, 8).await;
    let mut listener_mailbox = room.join(listener, 8).await;

    let channel_id = ChannelId::new("incidents").unwrap();
    room.open_channel(channel_id.clone()).await;
    room.subscribe(&listener_addr, &channel_id, SubscriptionMode::All)
        .await
        .unwrap();

    room.publish(&poster_addr, &channel_id, "deploy is red", Vec::new())
        .await
        .unwrap();

    let envelope = tokio::time::timeout(Duration::from_secs(2), listener_mailbox.recv())
        .await
        .expect("notification should arrive because the subscriber asked for all")
        .expect("inbox open");

    match envelope.intent {
        MessageIntent::ChannelNotify(payload) => {
            assert_eq!(payload.reason, NotifyReason::Subscribed);
            // Subscribing is not being addressed, so no reply is owed.
            assert!(!payload.reason.expects_attention());
        }
        other => panic!("expected ChannelNotify, got {other:?}"),
    }
}

#[tokio::test]
async fn a_manual_subscriber_is_never_woken_even_when_mentioned() {
    let room = Fabric::new("squad");
    let poster = manifest("agent://local/poster", "Poster", "Posts");
    let auditor = manifest("agent://local/auditor", "Auditor", "Audits");
    let poster_addr = poster.address.clone();
    let auditor_addr = auditor.address.clone();

    let _poster_mailbox = room.join(poster, 8).await;
    let mut auditor_mailbox = room.join(auditor, 8).await;

    let channel_id = ChannelId::new("audit").unwrap();
    room.open_channel(channel_id.clone()).await;
    room.subscribe(&auditor_addr, &channel_id, SubscriptionMode::Manual)
        .await
        .unwrap();

    room.publish(
        &poster_addr,
        &channel_id,
        "auditor: look at this",
        vec![auditor_addr.clone()],
    )
    .await
    .unwrap();

    let woken = tokio::time::timeout(Duration::from_millis(150), auditor_mailbox.recv()).await;
    assert!(
        woken.is_err(),
        "a manual subscriber chose never to be woken, and mentioning it must not override that"
    );

    // Visibility is unaffected: the message is in the log.
    let channel = room.channel(&channel_id).await.unwrap();
    assert_eq!(channel.len().await, 1);
}

#[tokio::test]
async fn mentions_only_is_the_default_policy() {
    // Deriving `Default` would have picked `All` — the first variant — which is
    // the one mode the docs warn against for busy channels. An unconfigured
    // caller must get the conservative choice.
    assert_eq!(SubscriptionMode::default(), SubscriptionMode::MentionsOnly);
}
