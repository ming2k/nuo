//! Monitor-plane handshake, live diffs, and attach-driven session teardown.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use persistence::thread::ThreadSession;
use protocol::{
    AgentResponse, MonitorAction, MonitorEvent, RoundEvent, ServerInitStage, ServerPhase,
    ThreadTarget,
};
use server::transport::serve::{self, Wire};
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
    let hello = serde_json::to_string(&Wire::Hello {
        protocol: protocol::PROTOCOL_VERSION,
        version: None,
        posture: protocol::human_request::HumanChannelPosture::Interactive,
        monitor: Some(MonitorAction {
            watch: true,
            include_idle: true,
        }),
    })
    .unwrap();
    ws.send(WsMessage::Text(hello.into())).await.unwrap();

    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::ServerHello {
            phase,
            monitor: Some(snapshot),
            ..
        } => {
            assert_eq!(phase, ServerPhase::Ready);
            assert_eq!(snapshot.server_phase, ServerPhase::Ready);
            assert_eq!(snapshot.threads.len(), 1);
            assert_eq!(snapshot.threads[0].id, session_id);
        }
        other => panic!("expected ServerHello with monitor snapshot, got {other:?}"),
    }

    // A broadcast response flows through the tracker into a diff. A
    // TurnStarted flips the row to Running, which the watch stream reports.
    let _ = bc_tx.send(AgentResponse::Round {
        thread_id: session_id.clone(),
        scope: protocol::RoundScope::Turn {
            context: protocol::TurnContext::new(1, 0, 1_700_000_000_000),
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
            assert_eq!(row.status, protocol::ThreadActivity::Running);
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
        .replace_messages(vec![tool::Message::new(tool::Role::User, "first prompt")])
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
    server::handlers::handlers_thread::rename(
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
    let hello = serde_json::to_string(&Wire::Hello {
        protocol: protocol::PROTOCOL_VERSION,
        version: None,
        posture: protocol::human_request::HumanChannelPosture::Interactive,
        monitor: Some(MonitorAction {
            watch: false,
            include_idle: false,
        }),
    })
    .unwrap();
    ws.send(WsMessage::Text(hello.into())).await.unwrap();

    // include_idle=false with an idle session: the snapshot in ServerHello is empty.
    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::ServerHello {
            monitor: Some(snapshot),
            ..
        } => assert!(snapshot.threads.is_empty()),
        other => panic!("expected ServerHello with empty monitor snapshot, got {other:?}"),
    }
    let _ = ws.close(None).await;
}

/// ADR-0260 `[INV-SRVPHASE-01..03]`: server answers `Wire::Hello` immediately
/// during `Initializing`, refuses `Subscribe` with `ERR_SERVER_NOT_READY`,
/// streams `ServerPhaseChanged` transitions reliably, and admits `Subscribe`
/// on the exact same connection once `Ready`.
#[tokio::test]
async fn server_phase_lifecycle_gates_subscribe_and_streams_transitions() {
    let (_dir, session) = fresh_empty_store("srv-phase");
    let (registry, _req_rx, _bc) = prehosted(session.clone()).await;
    let id = session.id().await;
    registry.set_server_phase(ServerPhase::Initializing {
        stage: ServerInitStage::Storage,
    });

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Hello {
            protocol: protocol::PROTOCOL_VERSION,
            version: None,
            posture: protocol::human_request::HumanChannelPosture::Interactive,
            monitor: Some(MonitorAction {
                watch: true,
                include_idle: true,
            }),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // 1. ServerHello arrives immediately with Initializing(Storage).
    let frame: Wire = serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(
            frame,
            Wire::ServerHello {
                phase: ServerPhase::Initializing {
                    stage: ServerInitStage::Storage
                },
                ..
            }
        ),
        "expected ServerHello(Initializing(Storage)), got {frame:?}"
    );

    // 2. Subscribe during Initializing is refused explicitly with ERR_SERVER_NOT_READY.
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Subscribe {
            request_id: 1,
            target: ThreadTarget::Attach {
                id: Some(id.clone()),
            },
            project: None,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let refused: Wire = serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(
            refused,
            Wire::Error {
                request_id: Some(1),
                code: Some(ref c),
                ..
            } if c == protocol::ERR_SERVER_NOT_READY
        ),
        "expected ERR_SERVER_NOT_READY, got {refused:?}"
    );

    // 3. Server transitions Storage -> Runtime -> Ready; client observes both.
    registry.set_server_phase(ServerPhase::Initializing {
        stage: ServerInitStage::Runtime,
    });
    let ev1: Wire = serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        ev1,
        Wire::Monitor {
            event: MonitorEvent::ServerPhaseChanged(ServerPhase::Initializing {
                stage: ServerInitStage::Runtime
            })
        }
    ));

    registry.set_server_phase(ServerPhase::Ready);
    let ev2: Wire = serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        ev2,
        Wire::Monitor {
            event: MonitorEvent::ServerPhaseChanged(ServerPhase::Ready)
        }
    ));

    // 4. Now Subscribe succeeds on the exact same connection.
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Subscribe {
            request_id: 2,
            target: ThreadTarget::Attach {
                id: Some(id.clone()),
            },
            project: None,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let sub: Wire = serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        sub,
        Wire::Subscribed {
            request_id: 2,
            ref thread_id,
            ..
        } if thread_id == &id
    ));
    handle.cancel.cancel();
}

/// ADR-0260 `[INV-SRVPHASE-01]`, `[INV-SRVPHASE-04]`: same-connection
/// `ClientOutbound::Control` (`send_control_over_link`) executes
/// `ControlRequest::Reload` and streams `Reloading -> Ready` over the exact
/// same `ServerLink` without opening a second connection.
#[tokio::test]
async fn same_connection_control_reload_streams_reloading_to_ready() {
    let (_dir, session) = fresh_empty_store("srv-reload");
    let (registry, _req_rx, _bc) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    let info = nuo::transport::ServerInfo {
        pid: std::process::id(),
        process_birth_token: None,
        port,
        token: None,
        project_root: String::new(),
        started_at: 0,
        uds_path: None,
        local_endpoint: None,
        version: None,
        grace_secs: None,
        protocol: Some(protocol::PROTOCOL_VERSION),
        image_digest: None,
        image_len: None,
    };

    let mut link = nuo::transport::connect_server_local(
        &info,
        Some(MonitorAction {
            watch: true,
            include_idle: true,
        }),
    )
    .await
    .expect("session-free ServerHello must succeed");
    assert_eq!(link.hello.phase, ServerPhase::Ready);

    // Drain initial Snapshot from monitor_rx.
    let first = tokio::time::timeout(Duration::from_secs(2), link.monitor_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(first, MonitorEvent::Snapshot(_)));

    // Execute ControlRequest::Reload over the SAME unified connection.
    nuo::transport::send_control_over_link(&link.req_tx, serve::ControlRequest::Reload)
        .await
        .expect("Reload control verb over unified link must succeed");

    // Must observe Reloading -> Ready transitions on the same connection.
    let mut saw_reloading = false;
    let mut saw_ready = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline && !(saw_reloading && saw_ready) {
        if let Ok(Some(MonitorEvent::ServerPhaseChanged(phase))) =
            tokio::time::timeout_at(deadline, link.monitor_rx.recv()).await
        {
            match phase {
                ServerPhase::Reloading => saw_reloading = true,
                ServerPhase::Ready if saw_reloading => saw_ready = true,
                _ => {}
            }
        }
    }
    assert!(saw_reloading, "must observe ServerPhase::Reloading");
    assert!(saw_ready, "must observe ServerPhase::Ready after Reloading");
    handle.cancel.cancel();
}

/// ADR-0260 `[INV-SRVPHASE-03]`: `ServerPhase::Failed` refuses `Subscribe` with
/// `ERR_SERVER_FAILED`, and `control()` connected while `Initializing` waits
/// cleanly for `ServerPhase::Ready` before executing the verb.
#[tokio::test]
async fn failed_phase_refuses_subscribe_and_control_waits_for_initializing_server() {
    let (_dir, session) = fresh_empty_store("srv-fail-init");
    let (registry, _req_rx, _bc) = prehosted(session.clone()).await;
    let id = session.id().await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let info = nuo::transport::ServerInfo {
        pid: std::process::id(),
        process_birth_token: None,
        port,
        token: None,
        project_root: String::new(),
        started_at: 0,
        uds_path: None,
        local_endpoint: None,
        version: None,
        grace_secs: None,
        protocol: Some(protocol::PROTOCOL_VERSION),
        image_digest: None,
        image_len: None,
    };

    // 1. Server is Initializing -> control() waits for Ready and succeeds.
    registry.set_server_phase(ServerPhase::Initializing {
        stage: ServerInitStage::Storage,
    });
    let reg_bg = registry.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        reg_bg.set_server_phase(ServerPhase::Initializing {
            stage: ServerInitStage::Runtime,
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        reg_bg.set_server_phase(ServerPhase::Ready);
    });
    nuo::transport::control(&info, serve::ControlRequest::Reload)
        .await
        .expect("control() must wait through Initializing and succeed once Ready");

    // 2. Server enters Failed -> Subscribe returns ERR_SERVER_FAILED explicitly.
    registry.set_server_phase(ServerPhase::Failed {
        code: Some("storage_init_failed".into()),
        message: "corrupt db".into(),
    });
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Hello {
            protocol: protocol::PROTOCOL_VERSION,
            version: None,
            posture: protocol::human_request::HumanChannelPosture::Interactive,
            monitor: None,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let hello: Wire = serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        hello,
        Wire::ServerHello {
            phase: ServerPhase::Failed { .. },
            ..
        }
    ));

    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Subscribe {
            request_id: 9,
            target: ThreadTarget::Attach { id: Some(id) },
            project: None,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let refused: Wire = serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(
            refused,
            Wire::Error {
                request_id: Some(9),
                code: Some(ref c),
                ..
            } if c == protocol::ERR_SERVER_FAILED
        ),
        "expected ERR_SERVER_FAILED, got {refused:?}"
    );
    handle.cancel.cancel();
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
    let hello_monitor = serde_json::to_string(&Wire::Hello {
        protocol: protocol::PROTOCOL_VERSION,
        version: None,
        posture: protocol::human_request::HumanChannelPosture::Interactive,
        monitor: Some(MonitorAction {
            watch: true,
            include_idle: true,
        }),
    })
    .unwrap();
    monitor
        .send(WsMessage::Text(hello_monitor.into()))
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
            Wire::ServerHello {
                monitor: Some(_),
                ..
            }
        ),
        "expected ServerHello with monitor snapshot, got {frame:?}"
    );

    // Attach via Hello -> Subscribe.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let hello = serde_json::to_string(&Wire::Hello {
        protocol: protocol::PROTOCOL_VERSION,
        version: None,
        posture: protocol::human_request::HumanChannelPosture::Interactive,
        monitor: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(hello.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    assert!(
        matches!(frame, Wire::ServerHello { .. }),
        "expected ServerHello, got {frame:?}"
    );

    let sub = serde_json::to_string(&Wire::Subscribe {
        request_id: 1,
        target: ThreadTarget::Attach {
            id: Some(id.clone()),
        },
        project: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(sub.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    assert!(
        matches!(frame, Wire::Subscribed { .. }),
        "expected Subscribed, got {frame:?}"
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
            ..
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
