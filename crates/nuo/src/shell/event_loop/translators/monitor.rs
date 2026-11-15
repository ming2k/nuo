//! Server monitor translator: folds monitor stream into application mutations.

use crate::shell::{client, event_loop};
use std::sync::atomic::Ordering;

/// Spawn the server monitor client: a *translator* over the
/// monitor stream. It owns the session-row snapshot (its own domain) and
/// publishes whole snapshots to `App` via mutations — no cells, no
/// rev counters.
pub(crate) fn spawn_monitor_translator(runtime: &event_loop::UiRuntime) {
    let mutations = runtime.mutations.clone();
    let dirty = runtime.dirty.clone();
    let dirty_notify = runtime.dirty_notify.clone();

    tokio::spawn(async move {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let Some(info) = client::discover(&project_root).await else {
            return;
        };
        let action = protocol::MonitorAction {
            watch: true,
            include_idle: true,
        };
        let Ok(mut rx) = client::monitor_stream(&info, action).await else {
            return;
        };
        let mut rows: Vec<protocol::MonitoredThread> = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                protocol::MonitorEvent::Snapshot(snap) => {
                    rows = snap.threads;
                    mutations
                        .send(event_loop::AppMutation::PersistenceHealth(
                            snap.persistence_health,
                        ))
                        .await;
                }
                protocol::MonitorEvent::ThreadAdded(row)
                | protocol::MonitorEvent::ThreadUpdated(row) => {
                    client::upsert_session_row(&mut rows, row);
                }
                protocol::MonitorEvent::ThreadRemoved { thread_id } => {
                    rows.retain(|r| r.id != thread_id);
                }
                // Server-level task diffs: the dashboard's
                // task section is folded in a later phase.
                protocol::MonitorEvent::TaskUpdated(_) => {}
                protocol::MonitorEvent::TaskRemoved { .. } => {}
                // Durability-health transitions (D4): a degraded
                // state retains the visible banner; Healthy clears it.
                protocol::MonitorEvent::PersistenceHealth(health) => {
                    mutations
                        .send(event_loop::AppMutation::PersistenceHealth(
                            (!health.is_healthy()).then_some(health),
                        ))
                        .await;
                }
                // The server began its graceful shutdown: the
                // stream closes right after; the next server interaction
                // re-discovers or re-spawns.
                protocol::MonitorEvent::ServerDraining => {}
            }
            mutations
                .send(event_loop::AppMutation::HostSessions(rows.clone()))
                .await;
            dirty.store(true, Ordering::SeqCst);
            dirty_notify.notify_one();
        }
    });
}
