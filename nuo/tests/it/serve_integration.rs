#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use nuo_wire::{AgentRequest, AgentResponse, MonitorAction, MonitorEvent, RoundEvent};
use nuo_persistence::session::SessionStore;
use nuo::monitor::MonitorTracker;
use nuo::registry::{HostedSession, SessionRegistry};
use nuo::serve::{self, AttachAction, Wire};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;

fn idle_base(id: String) -> nuo_wire::MonitoredSession {
    nuo_wire::MonitoredSession {
        id,
        overview: String::new(),
        created_at: 0,
        updated_at: 0,
        message_count: 0,
        status: nuo_wire::SessionStatus::Idle,
        hosting: nuo_wire::SessionHosting::Hosted,
        round: 0,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: String::new(),
        parent_id: None,
        fork_kind: nuo_wire::SessionForkKind::default(),
        digest: None,
    }
}

async fn prehosted(
    session: Arc<SessionStore>,
) -> (
    Arc<SessionRegistry>,
    mpsc::Receiver<AgentRequest>,
    broadcast::Sender<AgentResponse>,
) {
    prehosted_with_catalog(session, nuo_wire::CommandCatalog::default()).await
}

async fn prehosted_with_catalog(
    session: Arc<SessionStore>,
    command_catalog: nuo_wire::CommandCatalog,
) -> (
    Arc<SessionRegistry>,
    mpsc::Receiver<AgentRequest>,
    broadcast::Sender<AgentResponse>,
) {
    let (req_tx, req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let registry = Arc::new(SessionRegistry::prehost_only());
    // The synthetic project has no contributed assets, so the attach path has
    // no quarantine notice to add to these tests' frames. The state file
    // needs its own directory: the
    // atomic-write path chmods the *parent* private (0700), which fails
    // with EPERM on the shared root-owned /tmp itself.
    let state_dir = std::env::temp_dir().join(format!("nuo-serve-it-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&state_dir).unwrap();
    let security = Arc::new(
        nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
            state_dir.join("security.json"),
        ),
    );
    let base = idle_base(session.id().await);

    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        base,
        nuo_wire::SessionStatus::Idle,
    )));
    // Mimic the registry's broadcast-tap: fold every emitted response into the
    // tracker, publish a monitor diff, and buffer the attach-sync events, so
    // attach tests exercise the same path a real hosted session would take.
    let tap_tracker = tracker.clone();
    let mut tap_rx = bc_tx.subscribe();
    let registry_for_tap = registry.clone();
    let sync_buffer = Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new()));
    let sync_buffer_for_tap = sync_buffer.clone();
    tokio::spawn(async move {
        while let Ok(response) = tap_rx.recv().await {
            let row = {
                let mut guard = tap_tracker.lock().await;
                guard.observe(&response);
                guard.row()
            };
            registry_for_tap.publish_for_test(MonitorEvent::SessionUpdated(row));
            if matches!(
                response,
                AgentResponse::ProviderSwitched { .. }
                    | AgentResponse::ProviderPicker(_)
                    | AgentResponse::ProviderKeys(_)
            ) {
                sync_buffer_for_tap.lock().await.observe(&response);
            }
        }
    });
    registry
        .host(HostedSession {
            workspace_root: Some(std::path::PathBuf::from("/tmp/nuo-test-project")),
            human_channel: std::sync::Arc::new(
                nuo_wire::human_request::HumanChannelAccountant::new(),
            ),
            security,
            session,
            shared_confinement: nuo_wire::SharedConfinement::default(),
            req_tx,
            events: bc_tx.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer,
            command_catalog,
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    (registry, req_rx, bc_tx)
}

#[tokio::test]
async fn completion_catalog_and_edits_round_trip_over_websocket() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let command = nuo_wire::CommandSpec {
        name: "/models".into(),
        summary: "Choose a model".into(),
        ..Default::default()
    };
    let catalog = nuo_wire::CommandCatalog {
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
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
            protocol: Some(nuo_wire::PROTOCOL_VERSION),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    let welcome = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(welcome.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome {
            command_catalog, ..
        } => assert_eq!(command_catalog, catalog),
        other => panic!("expected Welcome, got {other:?}"),
    }

    // Skip the attach-time TodosUpdated and HarnessState snapshots.
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
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

    let item = nuo_wire::ComposerCompletion {
        label: "/models".into(),
        description: "Choose a model".into(),
        insert_text: "/models ".into(),
        replace_start: 0,
        replace_end: 4,
        kind: nuo_wire::ComposerCompletionKind::Slash,
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

/// Host a throwaway session rooted at `project`, returning its id. Unlike
/// [`prehosted`] this takes the project explicitly and skips the
/// broadcast-tap: project-scoping tests need several hosted sessions with
/// distinct roots in one registry, and only the registry's project index
/// matters for them.
async fn host_with_project(registry: &SessionRegistry, project: std::path::PathBuf) -> String {
    // `for_path` keeps every artifact under the given project dir instead of
    // minting files in the real XDG project bucket; the registry only needs
    // `project_root` for routing/indexing.
    let session = Arc::new(SessionStore::for_path(
        project.join("sessions").join("session.json"),
    ));
    let id = session.id().await;
    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(id.clone()),
        nuo_wire::SessionStatus::Idle,
    )));
    registry
        .host(HostedSession {
            workspace_root: Some(project),
            human_channel: std::sync::Arc::new(
                nuo_wire::human_request::HumanChannelAccountant::new(),
            ),
            security: std::sync::Arc::new(
                nuo_persistence::workspace_security::WorkspaceSecurityStore::load(),
            ),
            session,
            shared_confinement: nuo_wire::SharedConfinement::default(),
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer: Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new())),
            command_catalog: nuo_wire::CommandCatalog::default(),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    id
}

#[tokio::test]
async fn test_select_then_attach_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
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
        Wire::Welcome { session_id: id, .. } => assert_eq!(id, session_id),
        other => panic!("expected Welcome, got {other:?}"),
    }

    // The frame right after the welcome is the attach-time task-list sync
    // (empty here: this session has no todos).
    let sync_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(sync_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response:
                AgentResponse::Round {
                    event: RoundEvent::TodosUpdated(list),
                    ..
                },
        } => assert!(list.items.is_empty()),
        other => panic!("expected TodosUpdated sync, got {other:?}"),
    }

    // Followed by the attach-time HarnessState sync (ADR-0128).
    let state_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(state_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response:
                AgentResponse::Round {
                    event: RoundEvent::HarnessState(_),
                    ..
                },
        } => {}
        other => panic!("expected HarnessState sync, got {other:?}"),
    }

    ws.send(WsMessage::Text(
        serde_json::json!({"type":"Request","Chat":{"text":"hi","images":[]}})
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
        session_id: session_id.clone(),
        event: RoundEvent::Text("back".into()),
    });
    let resp_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let resp: Wire = serde_json::from_str(resp_msg.to_text().unwrap_or("")).unwrap();
    match resp {
        Wire::Response {
            response: AgentResponse::Round { event, .. },
        } => match event {
            RoundEvent::Text(t) => assert_eq!(t, "back"),
            o => panic!("{o:?}"),
        },
        other => panic!("{other:?}"),
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
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;

    // Give the session content so its file persists, then a non-empty list.
    session
        .replace_messages(vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let mut todos = nuo_wire::TodoList::new();
    todos.items.push(nuo_wire::TodoItem {
        id: nuo_wire::TodoId(1),
        content: "restored task".to_string(),
        status: nuo_wire::TodoStatus::InProgress,
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let welcome_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(welcome_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome { session_id: id, .. } => assert_eq!(id, session_id),
        other => panic!("expected Welcome, got {other:?}"),
    }

    // The very next frame must be the restored task list.
    let todos_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(todos_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response:
                AgentResponse::Round {
                    session_id: id,
                    event: RoundEvent::TodosUpdated(list),
                },
        } => {
            assert_eq!(id, session_id);
            assert_eq!(list, todos);
        }
        other => panic!("expected TodosUpdated restore, got {other:?}"),
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
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
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
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
            protocol: None,
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

    let mut saw_provider_switched = false;
    // The attach sync replays buffered provider events; the todos sync rides
    // the same prefix. Read a few frames and confirm the switch is among them.
    for _ in 0..4 {
        let msg = match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        if let Ok(Wire::Response { response }) =
            serde_json::from_str::<Wire>(msg.to_text().unwrap_or(""))
            && let AgentResponse::ProviderSwitched { provider, model } = response
        {
            assert_eq!(provider, "111xianyu");
            assert_eq!(model, "k3");
            saw_provider_switched = true;
            break;
        }
    }
    assert!(
        saw_provider_switched,
        "attach must replay the buffered ProviderSwitched so the hint bar hydrates"
    );
}

#[tokio::test]
async fn unknown_id_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
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

/// Project scoping (ADR-0096): the Select frame's optional `project` declares
/// the caller's working directory, and auto-attach must resolve inside THAT
/// project — not the server process's cwd, which is whatever the first client
/// that spawned the server happened to use. `New` creation and lazy resume
/// are scoped by the same value (`registry::SessionRegistry::resolve`).
#[tokio::test]
async fn select_project_scopes_auto_attach() {
    let registry = Arc::new(SessionRegistry::prehost_only());
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome { session_id, .. } => assert_eq!(session_id, id_a),
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

    let registry = Arc::new(SessionRegistry::prehost_only());
    let cwd_session = host_with_project(&registry, cwd).await;
    let _other = host_with_project(&registry, elsewhere).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Hand-written legacy frame: no `project` key at all. The fallback scope
    // is the process cwd, which the test process shares with the in-process
    // server — so the cwd-rooted session must win. Without the cwd fallback
    // the two hosted sessions could only produce a Pick frame.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        r#"{"type":"Select","action":{"attach":null}}"#.into(),
    ))
    .await
    .unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome { session_id, .. } => assert_eq!(session_id, cwd_session),
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
    let registry = Arc::new(SessionRegistry::prehost_only());
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Pick { sessions } => {
            // The picker offers exactly the foreign session — an explicit
            // choice, never an automatic one.
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, id_b);
        }
        other => panic!("expected Pick (foreign session must not auto-bind), got {other:?}"),
    }
}

/// ADR-0093: a monitor client receives a snapshot whose rows reflect the
/// registry's trackers — and, with `watch`, live diffs as sessions report.
#[tokio::test]
async fn monitor_handshake_yields_snapshot_then_diffs() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
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
            assert_eq!(snapshot.sessions.len(), 1);
            assert_eq!(snapshot.sessions[0].id, session_id);
        }
        other => panic!("expected monitor Snapshot, got {other:?}"),
    }

    // A broadcast response flows through the tracker into a diff. A
    // TurnStarted flips the row to Running, which the watch stream reports.
    let _ = bc_tx.send(AgentResponse::Round {
        session_id: session_id.clone(),
        event: RoundEvent::TurnStarted { round: 1, turn: 0 },
    });
    let diff = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(diff.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::SessionUpdated(row),
        } => {
            assert_eq!(row.id, session_id);
            assert_eq!(row.status, nuo_wire::SessionStatus::Running);
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
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    session
        .replace_messages(vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
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
    nuo::handlers_session::rename(
        &session,
        &resp_tx,
        session_id.clone(),
        Some("panel rename".to_string()),
    )
    .await;

    let row = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match monitor_rx.recv().await {
                Ok(MonitorEvent::SessionUpdated(row)) if row.id == session_id => break row,
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
        .sessions
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
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
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
        } => assert!(snapshot.sessions.is_empty()),
        other => panic!("expected monitor Snapshot, got {other:?}"),
    }
    // Then the stream ends.
    let end = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
    match end {
        Ok(None) | Ok(Some(Ok(WsMessage::Close(_)))) => {}
        other => panic!("expected connection close, got {other:?}"),
    }
}

/// ADR-0096: the control plane manages sessions without attaching — create,
/// observe in the monitor snapshot, kill.
/// ADR-0112: a client declares the session ended over its attach
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
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
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
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
        r#"{"type":"Request","EndSession":null}"#.into(),
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
        "EndSession must not reach the driver queue"
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
            event: MonitorEvent::SessionRemoved { session_id },
        } => assert_eq!(session_id, id),
        other => panic!("expected monitor SessionRemoved, got {other:?}"),
    }

    let _ = handle;
    let _ = tmp;
}

/// ADR-0096: the control plane manages sessions without attaching — create,
/// observe in the monitor snapshot, kill.
#[tokio::test]
async fn control_create_observe_kill_roundtrip() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let probe = registry.clone();
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Create a session via the control verb. prehost_only cannot assemble, so
    // this must fail cleanly — proving the verb reaches the registry and the
    // reply shape is right.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Control(serve::ControlRequest::CreateSession {
            project: "/tmp/x".into(),
            prompt: None,
            init_options: None,
        }),
        project: None,
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::ControlReply { ok, error, .. } => {
            assert!(!ok, "prehost registry cannot create sessions");
            assert!(error.unwrap().contains("cannot create"));
        }
        other => panic!("expected ControlReply, got {other:?}"),
    }

    // Kill on a missing session is a clean error too.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Control(serve::ControlRequest::KillSession {
            session_id: "nope".into(),
        }),
        project: None,
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::ControlReply { ok, error, .. } => {
            assert!(!ok);
            assert!(error.unwrap().contains("nope"));
        }
        other => panic!("expected ControlReply, got {other:?}"),
    }
    let _ = probe;
}

/// ADR-0096/0130: the same control plane is served over native local IPC —
/// UDS on Unix and a per-user named pipe on Windows. No bearer token is
/// needed because the OS endpoint permissions are the authentication boundary.
#[tokio::test]
async fn native_local_ipc_serves_same_protocol_without_token() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("server.sock");
    let endpoint = nuo_host::ipc::endpoint_for_instance(
        socket_path.clone(),
        &format!("serve-integration-{}", std::process::id()),
    )
    .unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());
    let mut handle = serve::start_server(
        serve::ServeOptions {
            local_endpoint: Some(endpoint.clone()),
            ..serve::ServeOptions::default()
        },
        registry,
    );
    let bound = handle
        .startup
        .local_ready
        .take()
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bound, Some(endpoint.clone()));
    // Socket file is 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "socket must be 0600, got {mode:o}");
    }

    // Full handshake over the native endpoint: monitor one-shot (ADR-0158 native framed).
    let stream = nuo_host::ipc::connect(&endpoint).await.unwrap();
    let (mut wire_sink, mut wire_source) = nuo::wire_channel::native_framed_split(stream);
    let select = Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: false,
            include_idle: true,
        }),
        project: None,
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    };
    wire_sink.send(select).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), wire_source.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match msg {
        Wire::Monitor {
            event: MonitorEvent::Snapshot(_),
        } => {}
        other => panic!("expected Snapshot over native local IPC, got {other:?}"),
    }

    // Cancel cleans up the socket file — deterministically: the removal runs
    // inside the supervised accept task, so joining the task book (bounded)
    // guarantees the file is gone before the assertion (ADR-0101; the old
    // 100ms sleep papered over exactly this race).
    handle.cancel.cancel();
    let hung = handle
        .tasks
        .join_all_with_budget(Duration::from_secs(2))
        .await;
    assert!(
        hung.is_empty(),
        "accept tasks must stop on cancel: {hung:?}"
    );
    let probe = nuo_host::ipc::probe(&endpoint);
    assert!(
        !probe.exists,
        "local endpoint removed on shutdown: {endpoint}"
    );
}

#[tokio::test]
async fn native_local_ipc_bind_failure_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = nuo_host::ipc::endpoint_for_instance(
        tmp.path().join("occupied.sock"),
        &format!("occupied-{}", std::process::id()),
    )
    .unwrap();
    let _occupied = nuo_host::ipc::LocalListener::bind(&endpoint).unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());
    let mut handle = serve::start_server(
        serve::ServeOptions {
            local_endpoint: Some(endpoint),
            ..serve::ServeOptions::default()
        },
        registry,
    );
    let result = handle.startup.local_ready.take().unwrap().await.unwrap();
    assert!(
        result.is_err(),
        "an occupied native endpoint must fail startup"
    );
    handle.cancel.cancel();
}

// Idle-empty session reaper

/// Construct and host a bare session with no broadcast-tap subscriber, so the
/// only potential event receiver is one the test adds explicitly. Unlike
/// [`prehosted`] this leaves `events.receiver_count() == 0`, which is what the
/// reaper's "no attached client" probe keys on.
async fn host_bare(
    session: Arc<SessionStore>,
    created_at: std::time::Instant,
) -> (
    Arc<SessionRegistry>,
    broadcast::Sender<AgentResponse>,
    String,
) {
    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let registry = Arc::new(SessionRegistry::prehost_only());
    let base = idle_base(session.id().await);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        base,
        nuo_wire::SessionStatus::Idle,
    )));
    let id = session.id().await;
    registry
        .host(HostedSession {
            workspace_root: Some(std::env::temp_dir().join("nuo-reaper-project")),
            human_channel: std::sync::Arc::new(
                nuo_wire::human_request::HumanChannelAccountant::new(),
            ),
            security: std::sync::Arc::new(
                nuo_persistence::workspace_security::WorkspaceSecurityStore::load(),
            ),
            session,
            shared_confinement: nuo_wire::SharedConfinement::default(),
            req_tx,
            events: bc_tx.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer: Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new())),
            command_catalog: nuo_wire::CommandCatalog::default(),
            created_at,
            last_activity: tokio::sync::Mutex::new(created_at),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    (registry, bc_tx, id)
}

/// A fresh project dir + SessionStore that has never persisted (empty).
fn fresh_empty_store(tag: &str) -> (std::path::PathBuf, Arc<SessionStore>) {
    let dir = std::env::temp_dir().join(format!("nuo-reaper-{tag}-{}", uuid::Uuid::new_v4()));
    // `for_path` keeps every artifact under the throwaway dir; nothing lands
    // in the real XDG project bucket.
    let store = Arc::new(SessionStore::for_path(dir.join("session.json")));
    (dir, store)
}

/// Snapshot the set of currently hosted session ids.
async fn hosted_ids(registry: &SessionRegistry) -> std::collections::HashSet<String> {
    registry
        .monitor_snapshot(MonitorAction {
            watch: false,
            include_idle: true,
        })
        .await
        .sessions
        .into_iter()
        .map(|r| r.id)
        .collect()
}

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
        .replace_messages(vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
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
        .replace_messages(vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
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
        .replace_messages(vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
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
        .replace_messages(vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
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

// Server lifecycle (ADR-0100/0101)

/// `ControlRequest::Shutdown` funnels into the serve gate: the reply is sent
/// *before* the drain cancels this very connection (ADR-0100), and the
/// accept loops stop — provable without signals, since the gate is the same
/// trigger source signals use.
#[tokio::test]
async fn shutdown_control_verb_replies_then_stops_accepting() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    // Issue the verb; the ControlReply must land before the drain kills us.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Control(serve::ControlRequest::Shutdown),
        project: None,
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::ControlReply { ok, .. } => assert!(ok, "shutdown verb must be accepted"),
        other => panic!("expected ControlReply, got {other:?}"),
    }

    // The gate latched with the ControlVerb reason and the accept tasks stop
    // deterministically (joined through the task book — no sleeps).
    assert!(
        tokio::time::timeout(Duration::from_secs(2), handle.gate.triggered())
            .await
            .is_ok(),
        "the serve gate must fire"
    );
    let hung = handle
        .tasks
        .join_all_with_budget(Duration::from_secs(2))
        .await;
    assert!(hung.is_empty(), "accept tasks must stop: {hung:?}");
}

/// Version negotiation (ADR-0100 rule 4): a skewed `Select.version` is
/// refused with a both-versions error before any session work; a matching
/// version proceeds normally (and an absent version is served).
#[tokio::test]
async fn version_skew_is_refused_with_both_versions() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/nuo.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    async fn first_frame(port: u16, version: Option<&str>) -> Wire {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let select = serde_json::to_string(&Wire::Select {
            version: version.map(str::to_string),
            action: AttachAction::Attach(Some("definitely-not-a-session".into())),
            project: None,
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
            protocol: None,
        })
        .unwrap();
        ws.send(WsMessage::Text(select.into())).await.unwrap();
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(msg.to_text().unwrap_or("")).unwrap()
    }

    // Skewed older client: refused with a message recommending client update.
    match first_frame(port, Some("0.0.0")).await {
        Wire::Error { message, .. } => {
            assert!(
                message.contains("0.0.0"),
                "names the client build: {message}"
            );
            assert!(
                message.contains(serve::server_version()),
                "names the server build: {message}"
            );
            assert!(
                message.contains("update your nuo client"),
                "names the client update recommendation: {message}"
            );
        }
        other => panic!("expected Error for a skewed version, got {other:?}"),
    }
    // Skewed newer client: refused with a message recommending server stop/restart.
    match first_frame(port, Some("99.0.0")).await {
        Wire::Error { message, .. } => {
            assert!(
                message.contains("99.0.0"),
                "names the client build: {message}"
            );
            assert!(
                message.contains(serve::server_version()),
                "names the server build: {message}"
            );
            assert!(
                message.contains("nuo stop"),
                "names the server restart fix: {message}"
            );
        }
        other => panic!("expected Error for a skewed version, got {other:?}"),
    }
    // Absent version: served (legacy-tolerant; the error is the normal
    // unknown-session one, not a version refusal).
    match first_frame(port, None).await {
        Wire::Error { message, .. } => {
            assert!(
                !message.contains("version mismatch"),
                "absent version must be served, got: {message}"
            );
        }
        other => panic!("expected the normal unknown-session error, got {other:?}"),
    }
}

/// The server's discovery record carries its build version (ADR-0100 rule
/// 4), so a client reading a stale record can refuse before speaking.
#[test]
fn global_record_carries_the_server_version() {
    let record = nuo::serve_discovery::Discovery {
        pid: 1,
        process_birth_token: None,
        port: 2,
        token: None,
        project_root: String::new(),
        started_at: 3,
        uds_path: None,
        local_endpoint: None,
        version: Some(serve::server_version().to_string()),
        grace_secs: None,
        protocol: None,
        ..Default::default()
    };
    let json = serde_json::to_string(&record).unwrap();
    assert!(json.contains(serve::server_version()));
    let back: nuo::serve_discovery::Discovery = serde_json::from_str(&json).unwrap();
    assert_eq!(back.version.as_deref(), Some(serve::server_version()));
}

/// Protocol negotiation (ADR-0134). The protocol number is the authority
/// when a client declares one: the server serves any number in
/// [MIN_PROTOCOL_VERSION, PROTOCOL_VERSION] — *whatever the product version
/// says* — and refuses anything outside the window with
/// `code: protocol_mismatch` before any session work. A client that
/// declares no number keeps the ADR-0100 product-version judgment.
#[tokio::test]
async fn protocol_window_governs_when_declared() {
    use nuo_wire::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};

    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    async fn first_frame(port: u16, version: Option<&str>, protocol: Option<u32>) -> Wire {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let select = serde_json::to_string(&Wire::Select {
            version: version.map(str::to_string),
            protocol,
            action: AttachAction::Attach(Some("definitely-not-a-session".into())),
            project: None,
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        })
        .unwrap();
        ws.send(WsMessage::Text(select.into())).await.unwrap();
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(msg.to_text().unwrap_or("")).unwrap()
    }

    // Same protocol, wildly skewed product version: SERVED. This is the
    // whole point of the ADR — a pinned client keeps talking to a newer
    // server across additive wire changes.
    match first_frame(port, Some("0.0.1-skew"), Some(PROTOCOL_VERSION)).await {
        Wire::Error { message, .. } => {
            assert!(
                !message.contains("mismatch"),
                "same-protocol client must not be judged on its product version: {message}"
            );
        }
        other => panic!("expected the normal unknown-session error, got {other:?}"),
    }

    // Too new: refused with the protocol message and the restart fix.
    match first_frame(port, None, Some(PROTOCOL_VERSION + 1)).await {
        Wire::Error { message, code } => {
            assert_eq!(code.as_deref(), Some("protocol_mismatch"));
            assert!(message.contains("nuo stop"), "names the fix: {message}");
            assert!(
                message.contains(&format!("protocol {}", PROTOCOL_VERSION + 1)),
                "names the client's protocol number: {message}"
            );
        }
        other => panic!("expected Error for a too-new protocol, got {other:?}"),
    }

    // Too old (only meaningful once MIN rises above 1; the guard keeps the
    // test correct whenever that happens).
    if MIN_PROTOCOL_VERSION > 1 {
        match first_frame(port, None, Some(MIN_PROTOCOL_VERSION - 1)).await {
            Wire::Error { message, code } => {
                assert_eq!(code.as_deref(), Some("protocol_mismatch"));
                assert!(
                    message.contains("update your nuo client"),
                    "names the update fix: {message}"
                );
            }
            other => panic!("expected Error for a too-old protocol, got {other:?}"),
        }
    }

    // Window edge: MIN itself is served, whatever the product version.
    match first_frame(port, Some("0.0.1-skew"), Some(MIN_PROTOCOL_VERSION)).await {
        Wire::Error { message, .. } => {
            assert!(
                !message.contains("mismatch"),
                "MIN is inside the window: {message}"
            );
        }
        other => panic!("expected the normal unknown-session error, got {other:?}"),
    }
}

/// One control verb over a fresh connection, returning the reply.
async fn control_roundtrip(port: u16, request: serve::ControlRequest) -> Result<bool, String> {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Control(request),
        project: None,
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::ControlReply { ok, error, .. } => {
            if ok {
                Ok(true)
            } else {
                Err(error.unwrap_or_else(|| "control verb rejected".to_string()))
            }
        }
        other => panic!("expected ControlReply, got {other:?}"),
    }
}

/// `suspend_session` (ADR-0096 control plane): an empty never-persisted
/// session is refused (killing is the honest verb — there is no transcript
/// to lazy-resume from), while an unknown id is the usual not-hosted error.
#[tokio::test]
async fn control_suspend_session_guards_and_rejects() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let tmp = tempfile::tempdir().unwrap();
    // An empty session: hosted but with no persisted content.
    let empty_id = host_with_project(&registry, tmp.path().to_path_buf()).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Unknown session: the standard not-hosted error.
    let err = control_roundtrip(
        port,
        serve::ControlRequest::SuspendSession {
            session_id: "missing".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("missing"),
        "error should name the session: {err}"
    );

    // Empty session: refused with the "kill it instead" guidance.
    let err = control_roundtrip(
        port,
        serve::ControlRequest::SuspendSession {
            session_id: empty_id.clone(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("kill"),
        "empty-session refusal should point at kill: {err}"
    );

    // The session is still hosted (a refusal must not tear anything down).
    assert!(hosted_ids(&registry).await.contains(&empty_id));
}

/// `suspend_session` happy path: a session with real content is parked
/// (entry removed, monitor row gone) and the reply is ok. The lazy-resume
/// path that rebuilds it on next attach is the same one the idle reaper
/// exercises, so this test pins only the verb's contract.
#[tokio::test]
async fn control_suspend_session_parks_a_contentful_session() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let id = session.id().await;
    // Real content → the session persists, so it is suspendable.
    session
        .replace_messages(vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
            "hello",
        )])
        .await
        .unwrap();

    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(id.clone()),
        nuo_wire::SessionStatus::Idle,
    )));
    registry
        .host(HostedSession {
            workspace_root: Some(tmp.path().to_path_buf()),
            human_channel: std::sync::Arc::new(
                nuo_wire::human_request::HumanChannelAccountant::new(),
            ),
            security: std::sync::Arc::new(
                nuo_persistence::workspace_security::WorkspaceSecurityStore::load(),
            ),
            session,
            shared_confinement: nuo_wire::SharedConfinement::default(),
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer: Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new())),
            command_catalog: nuo_wire::CommandCatalog::default(),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    assert!(hosted_ids(&registry).await.contains(&id));

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    assert!(
        control_roundtrip(
            port,
            serve::ControlRequest::SuspendSession {
                session_id: id.clone(),
            },
        )
        .await
        .is_ok()
    );
    // Parked: gone from the hosted set (and the monitor row with it).
    assert!(!hosted_ids(&registry).await.contains(&id));
}

// Workspace asset-trust attach flow.

/// Shared harness for the trust attach tests: hosts one session over the
/// given project root + security store, attaches an interactive client, and
/// returns the WebSocket stream to read attach frames from — plus the server
/// handle, which the caller must keep alive for the connection to be served.
#[allow(clippy::type_complexity)]
async fn attach_trust_workspace(
    tmp: &tempfile::TempDir,
    security_file: &std::path::Path,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    serve::ServeHandle,
) {
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let security = Arc::new(
        nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
            security_file.to_path_buf(),
        ),
    );
    let registry = Arc::new(SessionRegistry::prehost_only());
    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let sync_buffer = Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new()));
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(session.id().await),
        nuo_wire::SessionStatus::Idle,
    )));
    registry
        .host(HostedSession {
            workspace_root: Some(tmp.path().to_path_buf()),
            human_channel: std::sync::Arc::new(
                nuo_wire::human_request::HumanChannelAccountant::new(),
            ),
            security,
            session,
            shared_confinement: nuo_wire::SharedConfinement::default(),
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer,
            command_catalog: Default::default(),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(None),
            project: Some(tmp.path().to_string_lossy().into_owned().into()),
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
            protocol: Some(nuo_wire::PROTOCOL_VERSION),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    (ws, handle)
}

/// A never-trusted workspace (contributions present, no persisted record)
/// no longer receives the passive quarantine banner: the pre-view
/// `HarnessState` attach-sync frame carries the real security snapshot so
/// an interactive client can gate on it before the composer takes input.
/// The banner is reserved for the `Changed` escalation.
#[tokio::test]
async fn unconfigured_workspace_pushes_security_snapshot_on_attach() {
    let tmp = tempfile::tempdir().unwrap();
    let security_file = tmp.path().join("workspace_security.json");
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    std::fs::write(tmp.path().join("AGENTS.md"), "project instructions").unwrap();

    let security = Arc::new(
        nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
            security_file.clone(),
        ),
    );

    let registry = Arc::new(SessionRegistry::prehost_only());
    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let sync_buffer = Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new()));
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(session.id().await),
        nuo_wire::SessionStatus::Idle,
    )));
    registry
        .host(HostedSession {
            workspace_root: Some(tmp.path().to_path_buf()),
            human_channel: std::sync::Arc::new(
                nuo_wire::human_request::HumanChannelAccountant::new(),
            ),
            security,
            session,
            shared_confinement: nuo_wire::SharedConfinement::default(),
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer,
            command_catalog: Default::default(),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(None),
            project: Some(tmp.path().to_string_lossy().into_owned().into()),
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
            protocol: Some(nuo_wire::PROTOCOL_VERSION),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Welcome first, then the attach-sync HarnessState carrying the real
    // security snapshot. No quarantine banner: the pre-view trust dialog
    // owns the never-trusted case.
    let mut saw_quarantined_snapshot = false;
    let mut saw_banner = false;
    for _ in 0..12 {
        let raw = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame = serde_json::from_str::<Wire>(raw.to_text().unwrap_or("")).unwrap();
        let Wire::Response { response } = frame else {
            continue;
        };
        let AgentResponse::Round { event, .. } = response else {
            continue;
        };
        match event {
            RoundEvent::HarnessState(snapshot)
                if snapshot.workspace_security.instructions
                    == nuo_wire::WorkspaceTrustState::Quarantined =>
            {
                saw_quarantined_snapshot = true;
            }
            RoundEvent::Notice(n)
                if n.body
                    .as_deref()
                    .is_some_and(|b| b.contains("contributions") || b.contains("trust")) =>
            {
                saw_banner = true;
            }
            _ => {}
        }
        if saw_quarantined_snapshot {
            // The banner must never arrive in this scenario; one extra read
            // past the snapshot is enough to prove the frame ordering (the
            // server sends attach-sync before any notice), so stop here —
            // waiting for a "no banner" event would just hit the timeout.
            break;
        }
    }
    assert!(
        saw_quarantined_snapshot,
        "attach-sync HarnessState never carried the quarantined security snapshot"
    );
    assert!(
        !saw_banner,
        "never-trusted workspace must not receive the passive banner; the pre-view dialog owns it"
    );
    let _ = handle;
}

/// A previously trusted workspace whose contributions changed on disk keeps
/// the banner escalation: the user already made a trust decision for this
/// workspace once, so there is nothing to gate on — only to re-escalate.
#[tokio::test]
async fn changed_workspace_keeps_banner_escalation_on_attach() {
    let tmp = tempfile::tempdir().unwrap();
    let security_file = tmp.path().join("workspace_security.json");
    // AGENTS.md written twice: once to hash for the trusted record, once
    // (different content) so the digest no longer matches → `Changed`.
    std::fs::write(tmp.path().join("AGENTS.md"), "project instructions").unwrap();
    let security = nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
        security_file.clone(),
    );
    security
        .trust_domains(tmp.path(), &[nuo_wire::TrustDomain::Instructions])
        .unwrap();
    std::fs::write(tmp.path().join("AGENTS.md"), "project instructions v2").unwrap();
    let (mut ws, _server) = attach_trust_workspace(&tmp, &security_file).await;

    let mut saw_banner = false;
    for _ in 0..12 {
        let raw = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame = serde_json::from_str::<Wire>(raw.to_text().unwrap_or("")).unwrap();
        let Wire::Response { response } = frame else {
            continue;
        };
        let AgentResponse::Round { event, .. } = response else {
            continue;
        };
        if let RoundEvent::Notice(n) = event
            && n.body
                .as_deref()
                .is_some_and(|b| b.contains("changed") || b.contains("trust"))
            && n.kind == nuo_wire::NoticeKind::TrustChanged
        {
            saw_banner = true;
            break;
        }
    }
    assert!(saw_banner, "changed-workspace banner never pushed");
}

#[tokio::test]
async fn second_client_attach_receives_complete_non_drained_sync_state() {
    let session = Arc::new(SessionStore::load_for_project(std::path::PathBuf::from(
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
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
            protocol: Some(nuo_wire::PROTOCOL_VERSION),
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
            response: AgentResponse::ProviderKeys(keys),
        } = frame
            && keys.iter().any(|(p, _)| p == "test-provider")
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
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
            protocol: Some(nuo_wire::PROTOCOL_VERSION),
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
            response: AgentResponse::ProviderKeys(keys),
        } = frame
            && keys.iter().any(|(p, _)| p == "test-provider")
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
    let session = Arc::new(SessionStore::load_for_project(std::path::PathBuf::from(
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
            posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
            protocol: Some(nuo_wire::PROTOCOL_VERSION),
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
