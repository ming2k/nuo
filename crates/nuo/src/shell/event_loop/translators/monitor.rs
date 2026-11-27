//! Unified server-event translator: folds the single connection's `MonitorEvent`
//! stream (server lifecycle phases, persistence health, hosted threads) into
//! typed `AppMutation`s for both local and `--remote` clients
//! (ADR-0260 `[INV-SRVPHASE-04]`).

use crate::shell::{client, event_loop};
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

/// Spawn the server-event translator over the unified connection's
/// `monitor_rx`. No second discovery or socket connection is opened.
pub(crate) fn spawn_monitor_translator(
    runtime: &event_loop::UiRuntime,
    mut rx: mpsc::UnboundedReceiver<nuo_protocol::MonitorEvent>,
) {
    let mutations = runtime.mutations.clone();
    let dirty = runtime.dirty.clone();
    let dirty_notify = runtime.dirty_notify.clone();

    tokio::spawn(async move {
        let mut rows: Vec<nuo_protocol::MonitoredThread> = Vec::new();
        while let Some(event) = rx.recv().await {
            let mut rows_changed = false;
            match event {
                nuo_protocol::MonitorEvent::Snapshot(snap) => {
                    rows = snap.threads;
                    rows_changed = true;
                    mutations
                        .send(event_loop::AppMutation::ServerPhase(snap.server_phase))
                        .await;
                    mutations
                        .send(event_loop::AppMutation::PersistenceHealth(
                            snap.persistence_health,
                        ))
                        .await;
                }
                nuo_protocol::MonitorEvent::ServerPhaseChanged(phase) => {
                    mutations
                        .send(event_loop::AppMutation::ServerPhase(phase))
                        .await;
                }
                nuo_protocol::MonitorEvent::ThreadAdded(row)
                | nuo_protocol::MonitorEvent::ThreadUpdated(row) => {
                    client::upsert_session_row(&mut rows, row);
                    rows_changed = true;
                }
                nuo_protocol::MonitorEvent::ThreadRemoved { thread_id } => {
                    rows.retain(|r| r.id != thread_id);
                    rows_changed = true;
                }
                nuo_protocol::MonitorEvent::TaskUpdated(_)
                | nuo_protocol::MonitorEvent::TaskRemoved { .. } => {}
                nuo_protocol::MonitorEvent::PersistenceHealth(health) => {
                    mutations
                        .send(event_loop::AppMutation::PersistenceHealth(
                            (!health.is_healthy()).then_some(health),
                        ))
                        .await;
                }
            }
            if rows_changed {
                mutations
                    .send(event_loop::AppMutation::HostSessions(rows.clone()))
                    .await;
            }
            dirty.store(true, Ordering::SeqCst);
            dirty_notify.notify_one();
        }
    });
}

/// Forward structured link-level events (`SubscribeRefused`, `PickRequired`)
/// from the unified connection into `AppMutation`s (`[INV-ATTACH-01]`, ADR-0260).
pub(crate) fn spawn_link_event_translator(
    runtime: &event_loop::UiRuntime,
    mut rx: mpsc::UnboundedReceiver<client::LinkEvent>,
) {
    let mutations = runtime.mutations.clone();
    let dirty = runtime.dirty.clone();
    let dirty_notify = runtime.dirty_notify.clone();

    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let mutation = match event {
                client::LinkEvent::SubscribeRefused(error) => {
                    event_loop::AppMutation::SubscribeRefused(error)
                }
                client::LinkEvent::PickRequired(threads) => {
                    event_loop::AppMutation::PickRequired(threads)
                }
            };
            mutations.send(mutation).await;
            dirty.store(true, Ordering::SeqCst);
            dirty_notify.notify_one();
        }
    });
}
