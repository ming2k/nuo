//! Attach handshake, hydration, project scoping, and request ingress.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use persistence::thread::ThreadSession;
use protocol::{AgentRequest, AgentResponse, RoundEvent};
use server::runtime::registry::ThreadRegistry;
use server::transport::serve::{self, ThreadTarget, Wire};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::helpers::{host_with_project, prehosted, prehosted_with_catalog};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_hello(ws: &mut WsStream) {
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
    let raw = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(raw.to_text().unwrap_or("")).unwrap();
    assert!(
        matches!(frame, Wire::ServerHello { .. }),
        "expected ServerHello, got {frame:?}"
    );
}

async fn ws_hello_and_subscribe(
    ws: &mut WsStream,
    target: ThreadTarget,
    project: Option<std::path::PathBuf>,
) {
    ws_hello(ws).await;
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Subscribe {
            request_id: 1,
            target,
            project,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn completion_catalog_and_edits_round_trip_over_websocket() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let command = tool::CommandSpec {
        name: "/models".into(),
        summary: "Choose a model".into(),
        ..Default::default()
    };
    let catalog = tool::CommandCatalog {
        commands: vec![command.clone()],
        ..Default::default()
    };
    let (registry, mut req_rx, bc_tx) = prehosted_with_catalog(session, catalog.clone()).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(&mut ws, ThreadTarget::Attach { id: None }, None).await;

    // Subscribed is identity-only (ADR-0133 / ADR-0260); the command catalog
    // arrives in the authoritative snapshot that follows it.
    let mut snapshot_catalog = None;
    while snapshot_catalog.is_none() {
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match serde_json::from_str::<Wire>(frame.to_text().unwrap_or("")).unwrap() {
            Wire::Subscribed { .. } => {}
            Wire::Response {
                response: AgentResponse::ThreadSnapshot(snapshot),
                ..
            } => snapshot_catalog = snapshot.command_catalog.clone(),
            Wire::Response {
                response: AgentResponse::ThreadPhase(_),
                ..
            } => {}
            other => panic!("expected hydration, got {other:?}"),
        }
    }
    assert_eq!(snapshot_catalog.unwrap(), catalog);

    // Consume the readiness transition so the request below has a clean frame.
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Wire::Response {
            response: AgentResponse::ThreadPhase(update),
            ..
        } = serde_json::from_str::<Wire>(frame.to_text().unwrap_or("")).unwrap()
            && update.phase == protocol::ThreadPhase::Ready
        {
            break;
        }
    }

    let request = AgentRequest::CompleteComposer {
        request_id: 41,
        text: "/mod".into(),
        cursor: 4,
    };
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Request {
            thread_id: None,
            request: request.clone(),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let received = tokio::time::timeout(Duration::from_secs(2), req_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        received,
        AgentRequest::CompleteComposer {
            request_id: 41,
            ref text,
            cursor: 4,
        } if text == "/mod"
    ));

    let item = tool::ComposerCompletion {
        label: "/models".into(),
        description: "Choose a model".into(),
        insert_text: "/models ".into(),
        replace_start: 0,
        replace_end: 4,
        kind: tool::ComposerCompletionKind::Slash,
        alias_of: None,
        command: Some(command),
    };
    let _ = bc_tx.send(AgentResponse::ComposerCompletions {
        request_id: 41,
        text: "/mod".into(),
        cursor: 4,
        items: vec![item.clone()],
    });

    let response = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(response.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response:
                AgentResponse::ComposerCompletions {
                    request_id,
                    text,
                    cursor,
                    items,
                    ..
                },
            ..
        } => {
            assert_eq!(request_id, 41);
            assert_eq!(text, "/mod");
            assert_eq!(cursor, 4);
            assert_eq!(items, vec![item]);
        }
        other => panic!("expected ComposerCompletions, got {other:?}"),
    }
}

#[tokio::test]
async fn test_hello_then_subscribe_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;
    let (registry, mut req_rx, bc_tx) = prehosted(session).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(&mut ws, ThreadTarget::Attach { id: None }, None).await;

    let sub_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let sub: Wire = serde_json::from_str(sub_msg.to_text().unwrap_or("")).unwrap();
    match sub {
        Wire::Subscribed { thread_id: id, .. } => assert_eq!(id, session_id),
        other => panic!("expected Subscribed, got {other:?}"),
    }

    // Authoritative hydration snapshot follows immediately.
    let sync_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(sync_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
            ..
        } => {
            assert!(snapshot.todos.is_empty());
            assert!(snapshot.harness_state.is_some());
        }
        other => panic!("expected ThreadSnapshot sync, got {other:?}"),
    }

    ws.send(WsMessage::Text(
        serde_json::json!({"type":"Request","request":{"Chat":{"text":"hi","images":[]}}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let req = tokio::time::timeout(Duration::from_secs(2), req_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match req {
        AgentRequest::Prompt { text, .. } => assert_eq!(text, "hi"),
        other => panic!("{other:?}"),
    }

    let _ = bc_tx.send(AgentResponse::Round {
        thread_id: session_id.clone(),
        scope: protocol::RoundScope::Turn {
            context: protocol::TurnContext::new(1, 0, 1_700_000_000_000),
            event: RoundEvent::Text("back".into()),
        },
    });
    loop {
        let resp_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let resp: Wire = serde_json::from_str(resp_msg.to_text().unwrap_or("")).unwrap();
        match resp {
            Wire::Response {
                response: AgentResponse::ThreadPhase(_),
                ..
            } => continue,
            Wire::Response {
                response: AgentResponse::Round { scope, .. },
                ..
            } => match scope.into_event() {
                RoundEvent::Text(t) => {
                    assert_eq!(t, "back");
                    break;
                }
                o => panic!("{o:?}"),
            },
            other => panic!("{other:?}"),
        }
    }
}

/// `[INV-ATTACH-04]` (ADR-0133 / ADR-0260): Subscribed is an identity
/// acknowledgement, the authoritative snapshot follows it, and only then does
/// the server attest `Ready`.
#[tokio::test]
async fn subscribed_then_snapshot_then_ready_is_the_order() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(
        &mut ws,
        ThreadTarget::Attach {
            id: Some(session_id.clone()),
        },
        None,
    )
    .await;

    macro_rules! next_frame {
        () => {{
            let raw = tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str::<Wire>(raw.to_text().unwrap_or("")).unwrap()
        }};
    }

    match next_frame!() {
        Wire::Subscribed {
            thread_id: id,
            phase,
            ..
        } => {
            assert_eq!(id, session_id);
            assert!(matches!(phase, protocol::ThreadPhase::Ready));
        }
        other => panic!("expected Subscribed first, got {other:?}"),
    }
    match next_frame!() {
        Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
            ..
        } => assert_eq!(snapshot.thread_id, session_id),
        other => panic!("expected ThreadSnapshot second, got {other:?}"),
    }
    match next_frame!() {
        Wire::Response {
            response: AgentResponse::ThreadPhase(update),
            ..
        } => assert_eq!(update.phase, protocol::ThreadPhase::Ready),
        other => panic!("expected ThreadPhase(Ready) third, got {other:?}"),
    }
}

#[tokio::test]
async fn attach_receives_restored_todos_after_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;

    session
        .replace_messages(vec![tool::Message::new(tool::Role::User, "hello")])
        .await
        .unwrap();
    let mut todos = tool::TodoList::new();
    todos.items.push(tool::TodoItem {
        id: tool::TodoId(1),
        content: "restored task".to_string(),
        status: tool::TodoStatus::InProgress,
        created_at: 1,
        updated_at: 1,
    });
    session.set_todos(todos.clone()).await.unwrap();

    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(&mut ws, ThreadTarget::Attach { id: None }, None).await;

    let sub_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(sub_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Subscribed { thread_id: id, .. } => assert_eq!(id, session_id),
        other => panic!("expected Subscribed, got {other:?}"),
    }

    let todos_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(todos_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
            ..
        } => {
            assert_eq!(snapshot.thread_id, session_id);
            assert_eq!(snapshot.todos, todos.items);
        }
        other => panic!("expected ThreadSnapshot restore, got {other:?}"),
    }
}

#[tokio::test]
async fn attach_receives_buffered_provider_state_after_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, bc_tx) = prehosted(session).await;

    bc_tx
        .send(AgentResponse::ProviderSwitched {
            provider: "111xianyu".to_string(),
            model: "k3".to_string(),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(&mut ws, ThreadTarget::Attach { id: None }, None).await;

    let _sub = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let mut saw_provider = None;
    for _ in 0..4 {
        let msg = match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        if let Ok(Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
            ..
        }) = serde_json::from_str::<Wire>(msg.to_text().unwrap_or(""))
        {
            saw_provider = Some((snapshot.provider, snapshot.model));
            break;
        }
    }
    assert_eq!(
        saw_provider,
        Some(("111xianyu".to_string(), "k3".to_string())),
        "the attach snapshot must hydrate the buffered provider/model so the hint bar is correct"
    );
}

#[tokio::test]
async fn unknown_id_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(
        &mut ws,
        ThreadTarget::Attach {
            id: Some("nope".into()),
        },
        None,
    )
    .await;
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let parsed: Wire = serde_json::from_str(frame.to_text().unwrap_or("")).unwrap();
    match parsed {
        Wire::Error { message, .. } => assert!(message.contains("nope")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn unknown_id_is_refused_with_session_gone_code() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(
        &mut ws,
        ThreadTarget::Attach {
            id: Some("no-such-session-at-all".into()),
        },
        None,
    )
    .await;
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let parsed: Wire = serde_json::from_str(frame.to_text().unwrap_or("")).unwrap();
    match parsed {
        Wire::Error { message, code, .. } => {
            assert!(message.contains("no-such-session-at-all"));
            assert_eq!(
                code.as_deref(),
                Some(protocol::ERR_SESSION_GONE),
                "a session with no durable binding must be refused as gone so the \
                 client can retire the tab that names it"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn subscribe_project_scopes_auto_attach() {
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let project_a = std::env::temp_dir().join(format!("nuo-scope-a-{}", uuid::Uuid::new_v4()));
    let project_b = std::env::temp_dir().join(format!("nuo-scope-b-{}", uuid::Uuid::new_v4()));
    let id_a = host_with_project(&registry, project_a.clone()).await;
    let _id_b = host_with_project(&registry, project_b).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(&mut ws, ThreadTarget::Attach { id: None }, Some(project_a)).await;

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Subscribed {
            thread_id: session_id,
            ..
        } => assert_eq!(session_id, id_a),
        other => panic!("expected Subscribed for project-a's session, got {other:?}"),
    }
}

#[tokio::test]
async fn subscribe_without_project_falls_back_to_server_cwd() {
    let cwd = std::env::current_dir().unwrap();
    let sessions_dir = cwd.join("sessions");
    let elsewhere = std::env::temp_dir().join(format!("nuo-scope-c-{}", uuid::Uuid::new_v4()));
    let elsewhere_sessions = elsewhere.join("sessions");
    let _ = std::fs::remove_dir_all(&sessions_dir);

    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup_elsewhere = Cleanup(elsewhere_sessions);
    let _cleanup_cwd = Cleanup(sessions_dir);

    let registry = Arc::new(ThreadRegistry::prehost_only());
    let cwd_session = host_with_project(&registry, cwd).await;
    let _other = host_with_project(&registry, elsewhere).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello(&mut ws).await;
    ws.send(WsMessage::Text(
        r#"{"type":"Subscribe","request_id":1,"target":{"kind":"attach"}}"#.into(),
    ))
    .await
    .unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Subscribed {
            thread_id: session_id,
            ..
        } => assert_eq!(session_id, cwd_session),
        other => panic!("expected Subscribed for the cwd-rooted session, got {other:?}"),
    }
}

#[tokio::test]
async fn declared_project_is_never_auto_bound_to_a_foreign_session() {
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let project_a = std::env::temp_dir().join(format!("nuo-scope-d-{}", uuid::Uuid::new_v4()));
    let project_b = std::env::temp_dir().join(format!("nuo-scope-e-{}", uuid::Uuid::new_v4()));
    let id_b = host_with_project(&registry, project_b.clone()).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(&mut ws, ThreadTarget::Attach { id: None }, Some(project_a)).await;

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Pick {
            threads: sessions, ..
        } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, id_b);
        }
        other => panic!("expected Pick (foreign session must not auto-bind), got {other:?}"),
    }
}

#[tokio::test]
async fn second_client_attach_receives_complete_non_drained_sync_state() {
    let session = Arc::new(ThreadSession::load_for_project(std::path::PathBuf::from(
        "/tmp/nuo-test-multi-attach",
    )));
    let (registry, _req_rx, bc_tx) = prehosted(session.clone()).await;

    bc_tx
        .send(AgentResponse::ProviderKeys(vec![(
            "test-provider".to_string(),
            true,
        )]))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let session_id = session.id().await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    // Client 1 attaches
    let (mut ws1, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(
        &mut ws1,
        ThreadTarget::Attach {
            id: Some(session_id.clone()),
        },
        None,
    )
    .await;

    let mut ws1_saw_keys = false;
    for _ in 0..10 {
        let raw = tokio::time::timeout(Duration::from_secs(2), ws1.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame: Wire = serde_json::from_str(raw.to_text().unwrap_or("")).unwrap();
        if let Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
            ..
        } = frame
            && snapshot
                .provider_keys
                .as_ref()
                .is_some_and(|keys| keys.iter().any(|(p, _)| p == "test-provider"))
        {
            ws1_saw_keys = true;
            break;
        }
    }
    assert!(ws1_saw_keys, "client 1 did not receive ProviderKeys");

    // Client 2 attaches to the same session
    let (mut ws2, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(
        &mut ws2,
        ThreadTarget::Attach {
            id: Some(session_id.clone()),
        },
        None,
    )
    .await;

    let mut ws2_saw_keys = false;
    for _ in 0..10 {
        let raw = tokio::time::timeout(Duration::from_secs(2), ws2.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame: Wire = serde_json::from_str(raw.to_text().unwrap_or("")).unwrap();
        if let Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
            ..
        } = frame
            && snapshot
                .provider_keys
                .as_ref()
                .is_some_and(|keys| keys.iter().any(|(p, _)| p == "test-provider"))
        {
            ws2_saw_keys = true;
            break;
        }
    }
    assert!(
        ws2_saw_keys,
        "client 2 did not receive ProviderKeys (buffer was drained by client 1)"
    );
}

#[tokio::test]
async fn bounded_request_ingress_sheds_load_with_server_busy() {
    let session = Arc::new(ThreadSession::load_for_project(std::path::PathBuf::from(
        "/tmp/nuo-test-overload",
    )));
    let (registry, _req_rx, _bc_tx) = prehosted(session.clone()).await;
    let session_id = session.id().await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws_hello_and_subscribe(
        &mut ws,
        ThreadTarget::Attach {
            id: Some(session_id.clone()),
        },
        None,
    )
    .await;

    // Consume the Subscribed frame
    let _ = ws.next().await.unwrap().unwrap();

    // Flood the channel beyond capacity (SESSION_REQUEST_CAPACITY = 512)
    for _ in 0..600 {
        let req = Wire::Request {
            thread_id: None,
            request: AgentRequest::Interrupt,
        };
        let _ = ws
            .send(WsMessage::Text(serde_json::to_string(&req).unwrap().into()))
            .await;
    }

    let mut saw_server_busy = false;
    while let Ok(Some(Ok(raw))) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
        if let Ok(Wire::Error { code, .. }) =
            serde_json::from_str::<Wire>(raw.to_text().unwrap_or(""))
            && code.as_deref() == Some("server_busy")
        {
            saw_server_busy = true;
            break;
        }
    }
    assert!(
        saw_server_busy,
        "server did not respond with server_busy error upon queue saturation"
    );
}
