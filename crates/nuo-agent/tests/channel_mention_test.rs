//! Being mentioned in a channel must actually get a response, and that response
//! must land *in the channel* rather than as a whisper the peer never reads.
//!
//! Regression: the mention path used to send an uncorrelated `Inform` directly
//! to the mentioning agent. A serve loop acts only on Delegate/Query/Steer, so
//! that reply was silently discarded and the answer was lost entirely.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::MockProvider;
use acp::{ChannelId, Fabric, SubscriptionMode};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Builds a served listener and a poster, with `poster` publishing to `channel`.
async fn squad() -> (
    Fabric,
    Agent,
    Arc<MockProvider>,
    ChannelId,
    acp::AgentAddress,
) {
    let room = Fabric::new("squad");

    let listener_provider = Arc::new(MockProvider::new());
    let listener = Agent::builder("agent://local/listener")
        .name("Listener")
        .description("Listens")
        .provider_arc(listener_provider.clone())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let poster = Agent::builder("agent://local/poster")
        .name("Poster")
        .description("Posts")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let channel_id = ChannelId::new("ops").unwrap();
    let _channel = room.open_channel(channel_id.clone()).await;
    room.subscribe(
        listener.address(),
        &channel_id,
        SubscriptionMode::MentionsOnly,
    )
    .await
    .unwrap();

    let poster_addr = poster.address().clone();
    (room, listener, listener_provider, channel_id, poster_addr)
}

#[tokio::test]
async fn a_mention_yields_a_reply_that_lands_in_the_channel() {
    let (room, listener, provider, channel_id, poster_addr) = squad().await;
    provider.push_text("acknowledged, taking it").await;

    let (listener, inbox) = listener.into_serving().unwrap();
    let serve = tokio::spawn(async move {
        let _ = listener.serve(inbox).await;
    });

    // Mention the listener explicitly: this is the documented way to get a
    // member's attention inside a channel.
    let (posted, notified) = room
        .publish(
            &poster_addr,
            &channel_id,
            "please look at the build",
            vec![acp::AgentAddress::parse("agent://local/listener").unwrap()],
        )
        .await
        .unwrap();
    assert_eq!(notified.len(), 1, "the mention must notify the subscriber");

    // The turn runs in a spawned task, so poll rather than sleep a fixed time.
    let channel = room.channel(&channel_id).await.unwrap();
    let mut reply = None;
    for _ in 0..100 {
        let messages = channel.messages_after(posted.seq, 10).await;
        if let Some(found) = messages.into_iter().next() {
            reply = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let reply = reply.expect(
        "a mentioned agent must answer, and the answer must be recorded on the channel \
         where the discussion is",
    );
    assert_eq!(
        reply.body, "acknowledged, taking it",
        "the channel record must carry the agent's actual answer"
    );
    assert_ne!(
        reply.from, poster_addr,
        "the reply must come from the mentioned agent, not be an echo of the poster"
    );

    serve.abort();
}

#[tokio::test]
async fn unmentioned_traffic_still_costs_nothing() {
    // The mention gate is what keeps a busy channel from waking every member.
    let (room, listener, provider, channel_id, poster_addr) = squad().await;

    let (listener, inbox) = listener.into_serving().unwrap();
    let serve = tokio::spawn(async move {
        let _ = listener.serve(inbox).await;
    });

    room.publish(&poster_addr, &channel_id, "ambient chatter", Vec::new())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        provider.requests().await.is_empty(),
        "a message that mentions nobody must not trigger inference"
    );

    serve.abort();
}

#[tokio::test]
async fn channel_conversations_do_not_leak_session_locks() {
    // Companion to `probe_lock_leak_test`, which only exercises peer keys: the
    // channel path is a separate entry point and used to release no lock at all.
    let (room, listener, provider, channel_id, poster_addr) = squad().await;

    let _channel = room.channel(&channel_id).await.unwrap();
    room.subscribe(listener.address(), &channel_id, SubscriptionMode::All)
        .await
        .unwrap();

    for i in 0..50 {
        provider.push_text(format!("reply {i}")).await;
        room.publish(
            &poster_addr,
            &channel_id,
            format!("message {i}"),
            Vec::new(),
        )
        .await
        .unwrap();
        listener
            .run_channel_turn(
                &channel_id,
                uuid::Uuid::new_v4(),
                nuo_agent::ChannelWake::Requested,
            )
            .await
            .unwrap();
    }

    assert_eq!(
        listener.retained_session_locks().await,
        0,
        "a finished channel conversation must not retain its lock entry"
    );
}

/// Provider that blocks inside `complete` until the test grants a permit, so a
/// turn can be held mid-flight while still holding its session lock.
struct GatedProvider {
    inner: MockProvider,
    permits: Arc<tokio::sync::Semaphore>,
    entered: Arc<std::sync::atomic::AtomicUsize>,
    active: Arc<std::sync::atomic::AtomicUsize>,
    max_seen: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl nuo_agent::provider::Provider for GatedProvider {
    async fn stream(
        &self,
        request: nuo_agent::provider::ModelRequest,
    ) -> nuo_agent::error::Result<
        futures::channel::mpsc::Receiver<
            nuo_agent::error::Result<nuo_agent::provider::ProviderDelta>,
        >,
    > {
        use std::sync::atomic::Ordering;
        self.entered.fetch_add(1, Ordering::SeqCst);
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_seen.fetch_max(now, Ordering::SeqCst);

        // The permit is deliberately *consumed*, not returned: each turn that
        // enters needs its own permit, so the test decides exactly how many
        // turns may be in flight at once.
        let permit = self.permits.acquire().await.expect("semaphore open");
        permit.forget();
        let out = self.inner.stream(request).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        out
    }
}

async fn wait_for_entered(counter: &Arc<std::sync::atomic::AtomicUsize>, want: usize) {
    for _ in 0..200 {
        if counter.load(std::sync::atomic::Ordering::SeqCst) >= want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("provider never reached {want} concurrent entries");
}

#[tokio::test]
async fn eviction_never_hands_out_a_second_lock_for_a_live_session() {
    // Targets the eviction race in `release_session_lock` deterministically.
    //
    // T1 holds the session lock and blocks in the provider. T2 queues behind it.
    // T1 is released and evicts the entry *while T2 is still queued*. T3 then
    // asks for the same session:
    //
    //   - guarded:   T2 still holds the entry, so T3 queues behind it (max 1)
    //   - unguarded: T3 installs a second mutex and runs alongside T2 (max 2)
    //
    // Two concurrent turns on one session means two writers over one history.
    use std::sync::atomic::{AtomicUsize, Ordering};

    let inner = MockProvider::new();
    for i in 0..4 {
        inner.push_text(format!("reply {i}")).await;
    }

    let permits = Arc::new(tokio::sync::Semaphore::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));

    let agent = Agent::builder("agent://local/worker")
        .name("Worker")
        .description("Serialized")
        .provider(GatedProvider {
            inner,
            permits: permits.clone(),
            entered: entered.clone(),
            active: active.clone(),
            max_seen: max_seen.clone(),
        })
        .build()
        .await
        .unwrap();

    let key = nuo_agent::session::SessionKey::for_channel(ChannelId::new("ops").unwrap());

    // T1: acquires the lock, enters the provider, blocks there.
    let t1 = {
        let agent = agent.clone();
        let key = key.clone();
        tokio::spawn(async move { agent.run_turn(&key, Uuid::new_v4(), "turn 1").await })
    };
    wait_for_entered(&entered, 1).await;

    // T2: queues on the same session, cannot enter the provider yet.
    let t2 = {
        let agent = agent.clone();
        let key = key.clone();
        tokio::spawn(async move { agent.run_turn(&key, Uuid::new_v4(), "turn 2").await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        entered.load(Ordering::SeqCst),
        1,
        "T2 must be blocked on the session lock, not running"
    );

    // Release T1 only. It completes and makes its eviction decision while T2 is
    // still queued or just acquiring.
    permits.add_permits(1);
    t1.await.unwrap().unwrap();

    // T2 is now inside the provider, holding the entry's mutex.
    wait_for_entered(&entered, 2).await;

    // T3 must queue behind T2 rather than starting a parallel lane. With the
    // guard it blocks on the session mutex and never enters the provider; with
    // an unconditional eviction it installs a second mutex and runs alongside
    // T2, which shows up as two concurrent provider calls.
    let t3 = {
        let agent = agent.clone();
        let key = key.clone();
        tokio::spawn(async move { agent.run_turn(&key, Uuid::new_v4(), "turn 3").await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        1,
        "two turns ran on one session at once: the single-writer guarantee broke"
    );
    assert_eq!(
        entered.load(Ordering::SeqCst),
        2,
        "a third turn must not have entered the provider while one is in flight"
    );

    // Let the queue drain.
    permits.add_permits(4);
    t2.await.unwrap().unwrap();
    t3.await.unwrap().unwrap();

    assert_eq!(
        agent.retained_session_locks().await,
        0,
        "the entry must still be reclaimed once the queue drains"
    );
}
