//! Session lock lifecycle: the per-session serialization map must not grow
//! without bound, because a retained entry is a permanent leak for a
//! conversation that will never run again.
//!
//! The companion tests that contend a lock and verify single-writer behaviour
//! live in `channel_mention_test.rs`; this file covers the population case where
//! many conversations each finish once.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::MockProvider;
use nuo_agent::session::SessionKey;
use acp::AgentAddress;
use std::sync::Arc;

#[tokio::test]
async fn completed_conversations_release_their_locks() {
    const CONVERSATIONS: usize = 200;

    let provider = Arc::new(MockProvider::new());
    for _ in 0..CONVERSATIONS {
        provider.push_text("ok").await;
    }

    let agent = Agent::builder("agent://local/worker")
        .name("Worker")
        .description("Talks to many peers")
        .provider_arc(provider)
        .build()
        .await
        .unwrap();

    // One distinct peer per conversation: the realistic case for an agent that
    // serves many correspondents over its lifetime.
    for i in 0..CONVERSATIONS {
        let key =
            SessionKey::for_peer(&AgentAddress::parse(&format!("agent://local/peer-{i}")).unwrap());
        agent
            .run_turn(&key, uuid::Uuid::new_v4(), "hello")
            .await
            .unwrap();
    }

    // Every turn has finished, so nothing needs a lock any more.
    assert_eq!(
        agent.retained_session_locks().await,
        0,
        "lock entries for finished conversations must be released"
    );
}

#[tokio::test]
async fn a_failed_turn_still_releases_its_lock() {
    // Releasing must not depend on the turn succeeding: a provider error would
    // otherwise strand the entry, and the leak would only show up after a bad
    // day rather than a long one.
    let provider = Arc::new(MockProvider::new());
    // Deliberately queue no responses, so the provider fails.

    let agent = Agent::builder("agent://local/worker")
        .name("Worker")
        .description("Fails")
        .provider_arc(provider)
        .build()
        .await
        .unwrap();

    let key = SessionKey::for_peer(&AgentAddress::parse("agent://local/peer").unwrap());
    let outcome = agent.run_turn(&key, uuid::Uuid::new_v4(), "hello").await;
    assert!(outcome.is_err(), "the turn was expected to fail");

    assert_eq!(
        agent.retained_session_locks().await,
        0,
        "a failed turn must still release its lock"
    );
}
