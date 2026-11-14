//! Idle-empty reaping and idle-hosted suspension.

use std::time::Duration;

use super::helpers::{fresh_empty_store, host_bare, hosted_ids};
#[tokio::test]
async fn reaper_removes_idle_never_persisted_session() {
    let (_dir, store) = fresh_empty_store("idle");
    // A zero TTL makes the freshly hosted session immediately eligible
    // without assuming the OS monotonic clock has at least an hour of epoch.
    let (registry, _tx, id) = host_bare(store, std::time::Instant::now()).await;
    assert!(hosted_ids(&registry).await.contains(&id));

    let reaped = registry.reap_idle_empty_sessions_with(Duration::ZERO).await;
    assert_eq!(reaped, vec![id.clone()], "the idle empty session is reaped");
    assert!(
        !hosted_ids(&registry).await.contains(&id),
        "reaped session is gone from the registry"
    );
}

#[tokio::test]
async fn reaper_keeps_empty_session_within_ttl() {
    let (_dir, store) = fresh_empty_store("fresh");
    // Brand-new: created just now, so a 60s TTL must leave it alone.
    let (registry, _tx, id) = host_bare(store, std::time::Instant::now()).await;

    let reaped = registry
        .reap_idle_empty_sessions_with(Duration::from_secs(60))
        .await;
    assert!(reaped.is_empty(), "a fresh empty session is not yet idle");
    assert!(hosted_ids(&registry).await.contains(&id));
}

#[tokio::test]
async fn reaper_keeps_empty_session_with_attached_client() {
    let (_dir, store) = fresh_empty_store("watched");
    let (registry, tx, id) = host_bare(store, std::time::Instant::now()).await;
    // An attached client holds an event subscription open.
    let _client_rx = tx.subscribe();

    let reaped = registry.reap_idle_empty_sessions_with(Duration::ZERO).await;
    assert!(
        reaped.is_empty(),
        "an empty session someone is watching is never reaped"
    );
    assert!(hosted_ids(&registry).await.contains(&id));
}

#[tokio::test]
async fn reaper_keeps_session_once_it_has_content() {
    let (dir, store) = fresh_empty_store("content");
    // Give the session real content: this persists it, so it is user history
    // and must never be reaped no matter how idle.
    store
        .replace_messages(vec![nuo_protocol::Message::new(
            nuo_protocol::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let (registry, _tx, id) = host_bare(store, std::time::Instant::now()).await;

    let reaped = registry.reap_idle_empty_sessions_with(Duration::ZERO).await;
    assert!(reaped.is_empty(), "a persisted session is never reaped");
    assert!(hosted_ids(&registry).await.contains(&id));
    let _ = std::fs::remove_dir_all(dir);
}
// Idle-hosted suspension (memory bounding for real sessions)

/// A persisted idle session with no clients is suspended after the TTL: the
/// server's memory must be bounded by *active* work, not by session history.
#[tokio::test]
async fn suspension_removes_idle_persisted_session() {
    let (dir, store) = fresh_empty_store("suspend");
    store
        .replace_messages(vec![nuo_protocol::Message::new(
            nuo_protocol::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let (registry, _tx, id) = host_bare(store, std::time::Instant::now()).await;

    let suspended = registry.suspend_idle_sessions_with(Duration::ZERO).await;
    assert_eq!(
        suspended,
        vec![id.clone()],
        "idle persisted session suspends"
    );
    assert!(
        !hosted_ids(&registry).await.contains(&id),
        "suspended session leaves the hosted set"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// An attached client keeps its session resident — suspension must never
/// yank a session out from under a live observer.
#[tokio::test]
async fn suspension_keeps_session_with_attached_client() {
    let (dir, store) = fresh_empty_store("suspend-attached");
    store
        .replace_messages(vec![nuo_protocol::Message::new(
            nuo_protocol::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let (registry, bc_tx, id) = host_bare(store, std::time::Instant::now()).await;
    // Attach: a live broadcast receiver counts as an attached client.
    let _rx = bc_tx.subscribe();

    let suspended = registry.suspend_idle_sessions_with(Duration::ZERO).await;
    assert!(suspended.is_empty(), "attached session must not suspend");
    assert!(hosted_ids(&registry).await.contains(&id));
    drop(_rx);
    let _ = std::fs::remove_dir_all(dir);
}

/// Recent tap activity defers suspension: the idle clock refreshes when the
/// tap tick advances between sweeps.
#[tokio::test]
async fn suspension_deferred_by_recent_activity() {
    let (dir, store) = fresh_empty_store("suspend-active");
    store
        .replace_messages(vec![nuo_protocol::Message::new(
            nuo_protocol::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    // Hosted "recently": the idle clock has not run out yet.
    let fresh = std::time::Instant::now();
    let (registry, _tx, id) = host_bare(store, fresh).await;

    let suspended = registry
        .suspend_idle_sessions_with(Duration::from_secs(3600))
        .await;
    assert!(suspended.is_empty(), "fresh session must not suspend");
    assert!(hosted_ids(&registry).await.contains(&id));
    let _ = std::fs::remove_dir_all(dir);
}

/// Never-persisted empty sessions stay with the tighter empty-reaper; the
/// suspension path must not race it.
#[tokio::test]
async fn suspension_skips_empty_unpersisted_sessions() {
    let (_dir, store) = fresh_empty_store("suspend-empty");
    let (registry, _tx, id) = host_bare(store, std::time::Instant::now()).await;

    let suspended = registry.suspend_idle_sessions_with(Duration::ZERO).await;
    assert!(
        suspended.is_empty(),
        "empty session is the reaper's, not suspension's"
    );
    assert!(hosted_ids(&registry).await.contains(&id));
}
