//! Attach handshake, hydration, project scoping, and request ingress.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use nuo::runtime::registry::ThreadRegistry;
use nuo_persistence::thread::ThreadSession;
use nuo_protocol::{AgentRequest, AgentResponse, RoundEvent};
use nuo_server::transport::serve::{self, AttachAction, Wire};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::helpers::{host_with_project, prehosted, prehosted_with_catalog};
#[tokio::test]
async fn completion_catalog_and_edits_round_trip_over_websocket() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let command = nuo_tool::CommandSpec {
        name: "/models".into(),
        summary: "Choose a model".into(),
        ..Default::default()
    };
    let catalog = nuo_tool::CommandCatalog {
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
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(None),
            project: None,
            posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
            protocol: nuo_protocol::PROTOCOL_VERSION,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Welcome is identity-only (ADR-0133); the command catalog arrives in the
    // authoritative snapshot that follows it.
    let mut snapshot_catalog = None;
    while snapshot_catalog.is_none() {
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match serde_json::from_str::<Wire>(frame.to_text().unwrap_or("")).unwrap() {
            Wire::Welcome { .. } => {}
            Wire::Response {
                response: AgentResponse::ThreadSnapshot(snapshot),
            } => snapshot_catalog = snapshot.command_catalog.clone(),
            Wire::Response {
                response: AgentResponse::ThreadPhase(_),
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
        } = serde_json::from_str::<Wire>(frame.to_text().unwrap_or("")).unwrap()
            && update.phase == nuo_protocol::ThreadPhase::Ready
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

    let item = nuo_tool::ComposerCompletion {
        label: "/models".into(),
        description: "Choose a model".into(),
        insert_text: "/models ".into(),
        replace_start: 0,
        replace_end: 4,
        kind: nuo_tool::ComposerCompletionKind::Slash,
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
                },
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
async fn test_select_then_attach_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;
    let (registry, mut req_rx, bc_tx) = prehosted(session).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let welcome_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let welcome: Wire = serde_json::from_str(welcome_msg.to_text().unwrap_or("")).unwrap();
    match welcome {
        Wire::Welcome { thread_id: id, .. } => assert_eq!(id, session_id),
        other => panic!("expected Welcome, got {other:?}"),
    }

    // The frame right after the welcome is the authoritative hydration
    // snapshot (ADR-0133). It carries the task list and the harness state in
    // one frame; there is no separate TodosUpdated/HarnessState prefix.
    let sync_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(sync_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
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
        scope: nuo_protocol::RoundScope::Turn {
            context: nuo_protocol::TurnContext::new(1, 0, 1_700_000_000_000),
            event: RoundEvent::Text("back".into()),
        },
    });
    // Skip the readiness transition that follows the hydration snapshot.
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
            } => continue,
            Wire::Response {
                response: AgentResponse::Round { scope, .. },
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
/// `[INV-ATTACH-04]` (ADR-0133): Welcome is an identity acknowledgement, the
/// authoritative snapshot follows it, and only then does the server attest
/// `Ready`. The client gates input on that transition.
#[tokio::test]
async fn welcome_then_snapshot_then_ready_is_the_order() {
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
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(Some(session_id.clone())),
            project: None,
            posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
            protocol: nuo_protocol::PROTOCOL_VERSION,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

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
        Wire::Welcome {
            thread_id: id,
            phase,
        } => {
            assert_eq!(id, session_id);
            assert!(matches!(phase, nuo_protocol::ThreadPhase::Ready));
        }
        other => panic!("expected Welcome first, got {other:?}"),
    }
    match next_frame!() {
        Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
        } => assert_eq!(snapshot.thread_id, session_id),
        other => panic!("expected ThreadSnapshot second, got {other:?}"),
    }
    match next_frame!() {
        Wire::Response {
            response: AgentResponse::ThreadPhase(update),
        } => assert_eq!(update.phase, nuo_protocol::ThreadPhase::Ready),
        other => panic!("expected ThreadPhase(Ready) third, got {other:?}"),
    }
}
/// Attach-time state sync: a client that attaches to a session with a
/// persisted task list receives it as a `TodosUpdated` round event right
/// after the welcome — otherwise its todo panel would stay empty until the
/// model next touched the list (resume loses the restored todos).
#[tokio::test]
async fn attach_receives_restored_todos_after_welcome() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;

    // Give the session content so its file persists, then a non-empty list.
    session
        .replace_messages(vec![nuo_tool::Message::new(nuo_tool::Role::User, "hello")])
        .await
        .unwrap();
    let mut todos = nuo_tool::TodoList::new();
    todos.items.push(nuo_tool::TodoItem {
        id: nuo_tool::TodoId(1),
        content: "restored task".to_string(),
        status: nuo_tool::TodoStatus::InProgress,
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
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let welcome_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(welcome_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome { thread_id: id, .. } => assert_eq!(id, session_id),
        other => panic!("expected Welcome, got {other:?}"),
    }

    // The very next frame is the authoritative snapshot carrying the restored
    // task list (ADR-0133).
    let todos_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(todos_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
        } => {
            assert_eq!(snapshot.thread_id, session_id);
            assert_eq!(snapshot.todos, todos.items);
        }
        other => panic!("expected SessionSnapshot restore, got {other:?}"),
    }
}
/// Attach-time state sync (hint bar): a provider/model switch emitted while no
/// client is attached is buffered and replayed to the next client that
/// attaches, so the TUI's hint bar (model name, reasoning effort, `@instance`)
/// hydrates immediately instead of staying blank until the next mutation.
#[tokio::test]
async fn attach_receives_buffered_provider_state_after_welcome() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, bc_tx) = prehosted(session).await;

    // Emit the startup provider sync BEFORE any client subscribes — this is
    // the production scenario: the driver broadcasts it at session start, when
    // the only subscriber is the registry tap, so a later attacher would
    // otherwise never see it.
    bc_tx
        .send(AgentResponse::ProviderSwitched {
            provider: "111xianyu".to_string(),
            model: "k3".to_string(),
        })
        .unwrap();
    // Give the tap task a moment to fold the event into the sync buffer before
    // the client attaches (the tap runs on its own task).
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(None),
            project: None,
            posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
            protocol: nuo_protocol::PROTOCOL_VERSION,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Skip the welcome, then collect the sync frames that follow.
    let _welcome = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // The authoritative snapshot carries the buffered provider/model pair.
    let mut saw_provider = None;
    for _ in 0..4 {
        let msg = match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        if let Ok(Wire::Response {
            response: AgentResponse::ThreadSnapshot(snapshot),
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
        action: AttachAction::Attach(Some("nope".into())),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
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
/// `[INV-ATTACH-01]` end to end: an unknown id is refused with the
/// `session_gone` taxonomy on the actual wire frame, not merely in the message.
///
/// The client's entire recovery decision (retire the tab, keep running) hangs on
/// this code arriving intact. A prose-only refusal would leave it indistinguishable
/// from a transport fault — which is what made clicking a stale tab fatal.
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
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(Some("no-such-session-at-all".into())),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let parsed: Wire = serde_json::from_str(frame.to_text().unwrap_or("")).unwrap();
    match parsed {
        Wire::Error { message, code } => {
            assert!(message.contains("no-such-session-at-all"));
            assert_eq!(
                code.as_deref(),
                Some(nuo_protocol::ERR_SESSION_GONE),
                "a session with no durable binding must be refused as gone so the \
                 client can retire the tab that names it"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
}
/// Project scoping: the Select frame's optional `project` declares
/// the caller's working directory, and auto-attach must resolve inside THAT
/// project — not the server process's cwd, which is whatever the first client
/// that spawned the server happened to use. `New` creation and lazy resume
/// are scoped by the same value (`registry::ThreadRegistry::resolve`).
#[tokio::test]
async fn select_project_scopes_auto_attach() {
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let project_a = std::env::temp_dir().join(format!("nuo-scope-a-{}", uuid::Uuid::new_v4()));
    let project_b = std::env::temp_dir().join(format!("nuo-scope-b-{}", uuid::Uuid::new_v4()));
    let id_a = host_with_project(&registry, project_a.clone()).await;
    let _id_b = host_with_project(&registry, project_b).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Two hosted sessions, neither rooted at the server's cwd: under the old
    // cwd-only behavior this attach could only yield a Pick frame. Declaring
    // project A must bind A's session directly.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        project: Some(project_a),
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
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome {
            thread_id: session_id,
            ..
        } => assert_eq!(session_id, id_a),
        other => panic!("expected Welcome for project-a's session, got {other:?}"),
    }
}
/// Wire compatibility: a Select frame without `project` — what every client
/// sent before the field existed — still deserializes, and the server falls
/// back to its own process cwd as the caller's project scope.
#[tokio::test]
async fn select_without_project_falls_back_to_server_cwd() {
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

    // Hand-written frame with **no `project` key at all**: the fallback scope is
    // the process cwd, which the test process shares with the in-process server,
    // so the cwd-rooted session must win (without the fallback the two hosted
    // sessions could only produce a Pick frame).
    //
    // `protocol` is present because it is required (`[INV-OUTBOX-08]`): the
    // frame's point is the *absent project*, and a frame that also omitted the
    // protocol would be a different test (an unmodellable handshake, covered by
    // `protocol_window_is_the_only_admission_authority`).
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        format!(
            r#"{{"type":"Select","action":{{"attach":null}},"protocol":{}}}"#,
            nuo_protocol::PROTOCOL_VERSION
        )
        .into(),
    ))
    .await
    .unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome {
            thread_id: session_id,
            ..
        } => assert_eq!(session_id, cwd_session),
        other => panic!("expected Welcome for the cwd-rooted session, got {other:?}"),
    }
}
/// Regression (the "wrong workspace" bug): a client that *declared* its
/// project must never be silently auto-bound to the server's one hosted
/// session when that session belongs to a different project. Launching
/// `nuo attach` from project A with only project B's session live used to
/// attach straight into B's session — the model then read and edited B while
/// the header showed A. The declared-project client now gets the picker; the
/// cross-project session remains an explicit choice.
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
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        project: Some(project_a),
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
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Pick { threads: sessions } => {
            // The picker offers exactly the foreign session — an explicit
            // choice, never an automatic one.
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

    // Send an attach-sync event through the session's broadcast channel,
    // which the tap task captures into sync_buffer.
    bc_tx
        .send(AgentResponse::ProviderKeys(vec![(
            "test-provider".to_string(),
            true,
        )]))
        .unwrap();
    // Yield briefly to let the tap task observe the event
    tokio::time::sleep(Duration::from_millis(50)).await;

    let session_id = session.id().await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    // Client 1 attaches
    let (mut ws1, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws1.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(Some(session_id.clone())),
            project: None,
            posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
            protocol: nuo_protocol::PROTOCOL_VERSION,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

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
    ws2.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(Some(session_id.clone())),
            project: None,
            posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
            protocol: nuo_protocol::PROTOCOL_VERSION,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

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
    // Client 2 MUST also receive the ProviderKeys (verifying the buffer was not drained by client 1!)
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
    // Intentionally do NOT drain _req_rx so the bounded channel fills up
    let (registry, _req_rx, _bc_tx) = prehosted(session.clone()).await;
    let session_id = session.id().await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(Some(session_id.clone())),
            project: None,
            posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
            protocol: nuo_protocol::PROTOCOL_VERSION,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Consume the initial welcome frame
    let _ = ws.next().await.unwrap().unwrap();

    // Flood the channel beyond capacity (SESSION_REQUEST_CAPACITY = 512)
    for _ in 0..600 {
        let req = Wire::Request {
            request: AgentRequest::Interrupt,
        };
        let _ = ws
            .send(WsMessage::Text(serde_json::to_string(&req).unwrap().into()))
            .await;
    }

    // Read until server_busy error is seen
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
