//! Monitor-plane handshake, live diffs, and attach-driven session teardown.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use nuo::serve::{self, AttachAction, Wire};
use nuo_persistence::thread::ThreadSession;
use nuo_protocol::{AgentResponse, MonitorAction, MonitorEvent, RoundEvent};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::helpers::{fresh_empty_store, hosted_ids, prehosted};
/// A monitor client receives a snapshot whose rows reflect the
/// registry's trackers — and, with `watch`, live diffs as sessions report.
#[tokio::test]
async fn monitor_handshake_yields_snapshot_then_diffs() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;
    let (registry, _req_rx, bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: true,
            include_idle: true,
        }),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::Snapshot(snapshot),
        } => {
            assert_eq!(snapshot.threads.len(), 1);
            assert_eq!(snapshot.threads[0].id, session_id);
        }
        other => panic!("expected monitor Snapshot, got {other:?}"),
    }

    // A broadcast response flows through the tracker into a diff. A
    // TurnStarted flips the row to Running, which the watch stream reports.
    let _ = bc_tx.send(AgentResponse::Round {
        thread_id: session_id.clone(),
        scope: nuo_protocol::RoundScope::Turn {
            context: nuo_protocol::TurnContext::new(1, 0, 1_700_000_000_000),
            event: RoundEvent::TurnStarted,
        },
    });
    let diff = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(diff.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::ThreadUpdated(row),
        } => {
            assert_eq!(row.id, session_id);
            assert_eq!(row.status, nuo_protocol::ThreadActivity::Running);
            assert_eq!(row.round, 1);
            assert_eq!(row.turn, Some(0));
        }
        other => panic!("expected monitor SessionUpdated, got {other:?}"),
    }
}
/// A rename flows handler → broadcast-tap → monitor diff: the republished row
/// carries the new title because the tracker's base header is re-seeded from
/// the sessions-overview snapshot the rename handler pushes.
#[tokio::test]
async fn rename_live_session_republishes_monitor_row() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact under the tempdir; the session needs
    // real content so it persists and appears in `list()`.
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    session
        .replace_messages(vec![nuo_protocol::Message::new(
            nuo_protocol::Role::User,
            "first prompt",
        )])
        .await
        .unwrap();
    let session_id = session.id().await;
    let (registry, _req_rx, bc_tx) = prehosted(session.clone()).await;

    // Subscribe before the rename so the diff is captured.
    let mut monitor_rx = registry.subscribe_monitor();

    // Drive the production handler; forward its replies onto the session
    // broadcast exactly like the driver's response channel does.
    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<AgentResponse>();
    let forward = bc_tx.clone();
    tokio::spawn(async move {
        while let Some(response) = resp_rx.recv().await {
            let _ = forward.send(response);
        }
    });
    nuo::handlers_thread::rename(
        &session,
        &resp_tx,
        session_id.clone(),
        Some("panel rename".to_string()),
    )
    .await;

    let row = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match monitor_rx.recv().await {
                Ok(MonitorEvent::ThreadUpdated(row)) if row.id == session_id => break row,
                Ok(_) => continue,
                Err(error) => panic!("monitor stream ended before the rename diff: {error}"),
            }
        }
    })
    .await
    .expect("the rename must republish the session's monitor row");
    assert_eq!(row.overview, "panel rename");

    // A fresh monitor subscriber sees the renamed row in the snapshot too.
    let snapshot = registry
        .monitor_snapshot(MonitorAction {
            watch: false,
            include_idle: true,
        })
        .await;
    let row = snapshot
        .threads
        .iter()
        .find(|row| row.id == session_id)
        .unwrap();
    assert_eq!(row.overview, "panel rename");
}
/// Without `watch` the server closes the connection after the snapshot.
#[tokio::test]
async fn monitor_one_shot_closes_after_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: false,
            include_idle: false,
        }),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    // include_idle=false with an idle session: the snapshot is empty.
    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::Snapshot(snapshot),
        } => assert!(snapshot.threads.is_empty()),
        other => panic!("expected monitor Snapshot, got {other:?}"),
    }
    // Then the stream ends.
    let end = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
    match end {
        Ok(None) | Ok(Some(Ok(WsMessage::Close(_)))) => {}
        other => panic!("expected connection close, got {other:?}"),
    }
}
/// The control plane manages sessions without attaching — create,
/// observe in the monitor snapshot, kill.
/// A client declares the session ended over its attach
/// connection; the server tears the session down (registry entry gone,
/// `SessionRemoved` published, terminal `Exit` flushed to the attach
/// client) and the connection closes. The request must never reach the
/// driver queue — the teardown races what it would cancel.
#[tokio::test]
async fn attach_end_session_tears_down_and_notifies() {
    let tmp = tempfile::tempdir().unwrap();
    let (_dir, session) = fresh_empty_store("end-session");
    let (registry, mut _req_rx, _bc) = prehosted(session.clone()).await;
    let id = session.id().await;

    // Watch the monitor plane so the SessionRemoved diff is observable.
    let registry_for_serve = registry.clone();
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry_for_serve);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    // A dashboard watches the monitor plane; its row must disappear when
    // the attach client ends the session.
    let (mut monitor, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select_monitor = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: true,
            include_idle: true,
        }),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    })
    .unwrap();
    monitor
        .send(WsMessage::Text(select_monitor.into()))
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), monitor.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    assert!(
        matches!(
            frame,
            Wire::Monitor {
                event: MonitorEvent::Snapshot(_)
            }
        ),
        "expected monitor Snapshot, got {frame:?}"
    );

    // Attach.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(Some(id.clone())),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    assert!(
        matches!(frame, Wire::Welcome { .. }),
        "expected Welcome, got {frame:?}"
    );

    // Declare the session ended — raw frame shape the Web app sends.
    ws.send(WsMessage::Text(
        r#"{"type":"Request","request":"EndThread"}"#.into(),
    ))
    .await
    .unwrap();

    // The attach connection receives the terminal Exit before closing.
    let mut saw_exit = false;
    let deadline = Duration::from_secs(3);
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(deadline, ws.next()).await {
        if let Ok(Wire::Response {
            response: AgentResponse::Exit,
        }) = serde_json::from_str::<Wire>(msg.to_text().unwrap_or(""))
        {
            saw_exit = true;
            break;
        }
    }
    assert!(saw_exit, "attach client must observe the terminal Exit");

    // The registry no longer hosts the session.
    assert!(!hosted_ids(&registry).await.contains(&id));

    // The driver queue never saw the request (it was intercepted at the
    // connection layer, not forwarded).
    assert!(
        _req_rx.try_recv().is_err(),
        "EndThread must not reach the driver queue"
    );

    // The dashboard's monitor stream sees the row disappear.
    let removed = tokio::time::timeout(Duration::from_secs(3), monitor.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(removed.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::ThreadRemoved { thread_id },
        } => assert_eq!(thread_id, id),
        other => panic!("expected monitor ThreadRemoved, got {other:?}"),
    }

    let _ = handle;
    let _ = tmp;
}
