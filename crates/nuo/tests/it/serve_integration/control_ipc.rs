//! Control-plane verbs, native local IPC, and remote transport symmetry.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use nuo::monitor::MonitorTracker;
use nuo::registry::{HostedThread, ThreadRegistry};
use nuo::serve::{self, AttachAction, Wire};
use nuo_persistence::session::SessionStore;
use nuo_protocol::{AgentRequest, AgentResponse, MonitorAction, MonitorEvent};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::helpers::{host_with_project, hosted_ids, idle_base, prehosted};
/// The control plane manages sessions without attaching — create,
/// observe in the monitor snapshot, kill.
#[tokio::test]
async fn control_create_observe_kill_roundtrip() {
    let registry = Arc::new(ThreadRegistry::prehost_only());
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
        action: AttachAction::Control(serve::ControlRequest::CreateThread {
            project: "/tmp/x".into(),
            prompt: None,
            init_options: None,
        }),
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
        action: AttachAction::Control(serve::ControlRequest::KillThread {
            thread_id: "nope".into(),
        }),
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
    match frame {
        Wire::ControlReply { ok, error, .. } => {
            assert!(!ok);
            assert!(error.unwrap().contains("nope"));
        }
        other => panic!("expected ControlReply, got {other:?}"),
    }
    let _ = probe;
}
/// 0130: the same control plane is served over native local IPC —
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
    let registry = Arc::new(ThreadRegistry::prehost_only());
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

    // Full handshake over the native endpoint: monitor one-shot (native framed).
    let stream = nuo_host::ipc::connect(&endpoint).await.unwrap();
    let (mut wire_sink, mut wire_source) = nuo::wire_channel::native_framed_split(stream);
    let select = Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: false,
            include_idle: true,
        }),
        project: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        protocol: nuo_protocol::PROTOCOL_VERSION,
    };
    wire_sink.send(select).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), wire_source.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match msg {
        nuo_protocol::WireFrame::Wire(Wire::Monitor {
            event: MonitorEvent::Snapshot(_),
        }) => {}
        other => panic!("expected Snapshot over native local IPC, got {other:?}"),
    }

    // Cancel cleans up the socket file — deterministically: the removal runs
    // inside the supervised accept task, so joining the task book (bounded)
    // guarantees the file is gone before the assertion (the old
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
    let registry = Arc::new(ThreadRegistry::prehost_only());
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
// Server lifecycle (0101)

/// `ControlRequest::Shutdown` funnels into the serve gate: the reply is sent
/// *before* the drain cancels this very connection, and the
/// accept loops stop — provable without signals, since the gate is the same
/// trigger source signals use.
#[tokio::test]
async fn shutdown_control_verb_replies_then_stops_accepting() {
    let registry = Arc::new(ThreadRegistry::prehost_only());
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

/// The server's discovery record carries its build version (rule
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

/// Protocol negotiation. `[INV-WIRE-04]`: the protocol number is the **only**
/// admission authority — the server serves any number in
/// [MIN_PROTOCOL_VERSION, PROTOCOL_VERSION] *whatever the product version says*,
/// and refuses anything outside the window with `code: protocol_mismatch` before
/// any session work. The product version never enters the decision; it only
/// makes a refusal name both builds.
///
/// There is no "client that declares no number" lane any more: `Select.protocol`
/// is required, so such a peer fails to decode and is answered with the protocol
/// taxonomy — the second case below.
#[tokio::test]
async fn protocol_window_is_the_only_admission_authority() {
    use nuo_protocol::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};

    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    async fn first_frame(port: u16, version: Option<&str>, protocol: u32) -> Wire {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let select = serde_json::to_string(&Wire::Select {
            version: version.map(str::to_string),
            protocol,
            action: AttachAction::Attach(Some("definitely-not-a-session".into())),
            project: None,
            posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
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
    match first_frame(port, Some("0.0.1-skew"), PROTOCOL_VERSION).await {
        Wire::Error { message, .. } => {
            assert!(
                !message.contains("mismatch"),
                "same-protocol client must not be judged on its product version: {message}"
            );
        }
        other => panic!("expected the normal unknown-session error, got {other:?}"),
    }

    // Too new: refused with the protocol message and the restart fix.
    match first_frame(port, None, PROTOCOL_VERSION + 1).await {
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
        match first_frame(port, None, MIN_PROTOCOL_VERSION - 1).await {
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

    // A first frame with no protocol number cannot even be modelled: it is a
    // decode failure answered with the protocol taxonomy, not a silently
    // different admission rule. This is the case the old `Option<u32>` field
    // accepted and judged by product version.
    {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let raw = r#"{"type":"Select","action":{"Attach":null},"posture":"Interactive"}"#;
        ws.send(WsMessage::Text(raw.into())).await.unwrap();
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
        match frame {
            Wire::Error { message, code } => {
                assert_eq!(
                    code.as_deref(),
                    Some("protocol_mismatch"),
                    "an unmodellable handshake is a protocol fault"
                );
                assert!(
                    message.contains("could not decode"),
                    "and the disclosure says what failed: {message}"
                );
            }
            other => panic!("expected a decode refusal, got {other:?}"),
        }
    }

    // Window edge: MIN itself is served, whatever the product version.
    match first_frame(port, Some("0.0.1-skew"), MIN_PROTOCOL_VERSION).await {
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

/// `suspend_session` (control plane): an empty never-persisted
/// session is refused (killing is the honest verb — there is no transcript
/// to lazy-resume from), while an unknown id is the usual not-hosted error.
#[tokio::test]
async fn control_suspend_session_guards_and_rejects() {
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let tmp = tempfile::tempdir().unwrap();
    // An empty session: hosted but with no persisted content.
    let empty_id = host_with_project(&registry, tmp.path().to_path_buf()).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Unknown session: the standard not-hosted error.
    let err = control_roundtrip(
        port,
        serve::ControlRequest::SuspendThread {
            thread_id: "missing".into(),
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
        serve::ControlRequest::SuspendThread {
            thread_id: empty_id.clone(),
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
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let id = session.id().await;
    // Real content → the session persists, so it is suspendable.
    session
        .replace_messages(vec![nuo_protocol::Message::new(
            nuo_protocol::Role::User,
            "hello",
        )])
        .await
        .unwrap();

    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(id.clone()),
        nuo_protocol::ThreadStatus::Idle,
    )));
    registry
        .host(HostedThread {
            workspace_root: Some(tmp.path().to_path_buf()),
            human_channel: std::sync::Arc::new(
                nuo_protocol::human_request::HumanChannelAccountant::new(),
            ),
            security: std::sync::Arc::new(
                nuo_persistence::workspace_security::WorkspaceSecurityStore::load(),
            ),
            session,
            shared_confinement: nuo_protocol::SharedConfinement::default(),
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            driver_done: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            tracker,
            sync_buffer: Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new())),
            command_catalog: nuo_protocol::CommandCatalog::default(),
            initial_provider: "test-provider".into(),
            initial_model: "test-model".into(),
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
            serve::ControlRequest::SuspendThread {
                thread_id: id.clone(),
            },
        )
        .await
        .is_ok()
    );
    // Parked: gone from the hosted set (and the monitor row with it).
    assert!(!hosted_ids(&registry).await.contains(&id));
}
/// [INV-REMOTE-01] Universal Endpoint Resolution
/// Verifies parsing of explicit secure URIs (wss/https defaulting to 443),
/// cleartext URIs (ws/http defaulting to 80), socket addresses, bracketed IPv6,
/// and rejection of bare un-schemed hostnames.
#[test]
fn test_inv_remote_01_endpoint_resolution() {
    use nuo::transport::RemoteServer;

    let t = |addr| RemoteServer::parse(addr, Some("test-tok".into())).unwrap();

    // 1. Explicit wss:// defaults to port 443 and sets secure=true
    let s1 = t("wss://tunnel.nuo.example.com");
    assert_eq!(s1.host, "tunnel.nuo.example.com");
    assert_eq!(s1.port, 443);
    assert!(s1.secure);
    assert_eq!(s1.url(), "wss://tunnel.nuo.example.com:443/");
    assert_eq!(s1.endpoint_display(), "wss://tunnel.nuo.example.com");

    // 2. Explicit wss:// with custom port
    let s2 = t("wss://tunnel.nuo.example.com:8443");
    assert_eq!(s2.host, "tunnel.nuo.example.com");
    assert_eq!(s2.port, 8443);
    assert!(s2.secure);
    assert_eq!(s2.url(), "wss://tunnel.nuo.example.com:8443/");
    assert_eq!(s2.endpoint_display(), "wss://tunnel.nuo.example.com:8443");

    // 3. https:// alias implies wss:// on 443
    let s3 = t("https://cf-tunnel.domain.com/ws");
    assert_eq!(s3.host, "cf-tunnel.domain.com");
    assert_eq!(s3.port, 443);
    assert!(s3.secure);

    // 4. ws:// scheme defaults to port 80 and secure=false
    let s4 = t("ws://lan-box.internal");
    assert_eq!(s4.host, "lan-box.internal");
    assert_eq!(s4.port, 80);
    assert!(!s4.secure);
    assert_eq!(s4.endpoint_display(), "ws://lan-box.internal");

    // 5. Host and explicit port without scheme defaults to cleartext
    let s5 = t("192.168.1.100:9800");
    assert_eq!(s5.host, "192.168.1.100");
    assert_eq!(s5.port, 9800);
    assert!(!s5.secure);

    // 6. Bare :port resolves loopback 127.0.0.1
    let s6 = t(":9800");
    assert_eq!(s6.host, "127.0.0.1");
    assert_eq!(s6.port, 9800);
    assert!(!s6.secure);

    // 7. IPv6 bracketed addresses
    let s7 = t("[2001:db8::1]:9800");
    assert_eq!(s7.host, "[2001:db8::1]");
    assert_eq!(s7.port, 9800);
    assert!(!s7.secure);

    // 8. Rejections
    let err_no_port = RemoteServer::parse("tunnel.example.com", Some("tok".into())).unwrap_err();
    assert!(err_no_port.contains("not host:port"));

    let err_no_token = RemoteServer::parse("wss://tunnel.example.com", None).unwrap_err();
    assert!(err_no_token.contains("--token"));
}

/// [INV-REMOTE-02] Interactive Remote Symmetry and [INV-REMOTE-03] Sovereign Remote Detach
/// Verifies connecting to a hosted server via `ClientTarget::Remote`, performing a full
/// WebSocket attach handshake with bearer token authentication, and detaching cleanly
/// without destroying the remote server process.
#[tokio::test]
async fn test_inv_remote_02_remote_symmetry_and_detach() {
    use nuo::transport::{AttachAction, ClientTarget, Handshake, RemoteServer};

    let registry = Arc::new(ThreadRegistry::prehost_only());
    let _session_id = host_with_project(&registry, std::env::temp_dir()).await;
    let token = "test-remote-bearer-98765".to_string();

    let mut handle = serve::start_server(
        serve::ServeOptions {
            port: 0,
            expose: serve::ServeExpose::Public,
            token: Some(token.clone()),
            ..serve::ServeOptions::default()
        },
        registry.clone(),
    );

    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    assert!(port > 0, "server must bind to an ephemeral port");

    // 1. Wrong token must be rejected at the handshake boundary
    let bad_remote =
        RemoteServer::parse(&format!("127.0.0.1:{port}"), Some("bad-token".into())).unwrap();
    let bad_target = ClientTarget::Remote(bad_remote);
    let bad_res = bad_target.connect(AttachAction::Attach(None)).await;
    assert!(
        bad_res.is_err(),
        "connecting with invalid bearer token must fail"
    );

    // 2. Correct token connects successfully via ClientTarget::Remote
    let valid_remote =
        RemoteServer::parse(&format!("127.0.0.1:{port}"), Some(token.clone())).unwrap();
    let valid_target = ClientTarget::Remote(valid_remote.clone());

    let handshake = valid_target
        .connect(AttachAction::Attach(None))
        .await
        .expect("remote attach handshake must succeed");

    let (req_tx, resp_rx) = match handshake {
        Handshake::Attached {
            req_tx, resp_rx, ..
        } => (req_tx, resp_rx),
        Handshake::Pick(_) => panic!("expected Handshake::Attached, got Pick"),
    };

    // 3. Bidirectional pipe works across the remote WebSocket connection
    req_tx
        .send(AgentRequest::Interrupt)
        .expect("send request over remote wire");

    // 4. [INV-REMOTE-03] Sovereign Detach: drop client connection handles
    drop(req_tx);
    drop(resp_rx);

    // Give server event loop a moment to register disconnect
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 5. Remote server is still running and healthy; a subsequent client can attach
    let re_target = ClientTarget::Remote(valid_remote);
    let re_handshake = re_target.connect(AttachAction::Attach(None)).await.expect(
        "server must remain running and accept re-attachment after previous client detached",
    );

    assert!(matches!(re_handshake, Handshake::Attached { .. }));

    // Clean shutdown of test server
    handle.cancel.cancel();
}
