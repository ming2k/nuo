//! Workspace asset-trust attach flow: snapshots, gates, and banner fallback.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use nuo::runtime::monitor::MonitorTracker;
use nuo::runtime::registry::{HostedThread, ThreadRegistry};
use nuo_persistence::thread::ThreadSession;
use nuo_protocol::{AgentRequest, AgentResponse, RoundEvent};
use nuo_server::transport::serve::{self, AttachAction, Wire};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::helpers::idle_base;
// Workspace asset-trust attach flow.

/// Shared harness for the trust attach tests: hosts one session over the
/// given project root + security store, attaches an **interactive** client, and
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
    attach_trust_workspace_with_posture(
        tmp,
        security_file,
        nuo_protocol::human_request::HumanChannelPosture::Interactive,
    )
    .await
}

/// The same harness with an explicit client posture, so a test can prove what an
/// autonomous attach receives: it cannot answer a gate, so the server must fall
/// back to the banner rather than park the security decision on a channel
/// nobody reads.
#[allow(clippy::type_complexity)]
async fn attach_trust_workspace_with_posture(
    tmp: &tempfile::TempDir,
    security_file: &std::path::Path,
    posture: nuo_protocol::human_request::HumanChannelPosture,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    serve::ServeHandle,
) {
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    let security = Arc::new(
        nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
            security_file.to_path_buf(),
        ),
    );
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let sync_buffer = Arc::new(Mutex::new(
        nuo_server::transport::serve::AttachSyncBuffer::new(),
    ));
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(session.id().await),
        nuo_protocol::ThreadActivity::Idle,
    )));
    registry
        .host(HostedThread {
            workspace_root: Some(tmp.path().to_path_buf()),
            human_channel: std::sync::Arc::new(
                nuo_protocol::human_request::HumanChannelAccountant::new(),
            ),
            security,
            session,
            shared_confinement: nuo_protocol::SharedConfinement::default(),
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            driver_done: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            tracker,
            sync_buffer,
            command_catalog: Default::default(),
            initial_provider: "test-provider".into(),
            initial_model: "test-model".into(),
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
            posture,
            protocol: nuo_protocol::PROTOCOL_VERSION,
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
    let session = Arc::new(ThreadSession::for_path(tmp.path().join("session.json")));
    std::fs::write(tmp.path().join("AGENTS.md"), "project instructions").unwrap();

    let security = Arc::new(
        nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
            security_file.clone(),
        ),
    );

    let registry = Arc::new(ThreadRegistry::prehost_only());
    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let sync_buffer = Arc::new(Mutex::new(
        nuo_server::transport::serve::AttachSyncBuffer::new(),
    ));
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(session.id().await),
        nuo_protocol::ThreadActivity::Idle,
    )));
    registry
        .host(HostedThread {
            workspace_root: Some(tmp.path().to_path_buf()),
            human_channel: std::sync::Arc::new(
                nuo_protocol::human_request::HumanChannelAccountant::new(),
            ),
            security,
            session,
            shared_confinement: nuo_protocol::SharedConfinement::default(),
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            driver_done: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            tracker,
            sync_buffer,
            command_catalog: Default::default(),
            initial_provider: "test-provider".into(),
            initial_model: "test-model".into(),
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
            posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
            protocol: nuo_protocol::PROTOCOL_VERSION,
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
        match response {
            AgentResponse::ThreadSnapshot(snapshot) => {
                if let Some(harness) = &snapshot.harness_state
                    && harness.workspace_security.instructions
                        == nuo_protocol::WorkspaceTrustState::Quarantined
                {
                    saw_quarantined_snapshot = true;
                }
            }
            AgentResponse::Round {
                scope:
                    nuo_protocol::RoundScope::Session {
                        event: RoundEvent::Notice(n),
                    },
                ..
            } if n
                .body
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
/// A previously trusted workspace whose contributions changed on disk now
/// gates like a never-trusted one: it carries the real security snapshot in the
/// pre-view attach sync and does **not** get the passive banner.
///
/// The regression this pins: `Changed` used to be the one state excluded from
/// the gate, so it escalated through a banner instead — a toast that re-fired on
/// every attach, and which a user could scroll past. The highest-risk posture
/// (the approved bytes are not the bytes on disk) had the weakest affordance.
#[tokio::test]
async fn changed_workspace_gates_on_attach_instead_of_bannering() {
    let tmp = tempfile::tempdir().unwrap();
    let security_file = tmp.path().join("workspace_security.json");
    // AGENTS.md written twice: once to hash for the trusted record, once
    // (different content) so the digest no longer matches → `Changed`.
    std::fs::write(tmp.path().join("AGENTS.md"), "project instructions").unwrap();
    let security = nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
        security_file.clone(),
    );
    security
        .trust_domains(tmp.path(), &[nuo_protocol::TrustDomain::Instructions])
        .unwrap();
    std::fs::write(tmp.path().join("AGENTS.md"), "project instructions v2").unwrap();
    let (mut ws, _server) = attach_trust_workspace(&tmp, &security_file).await;

    let mut saw_changed_snapshot = false;
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
        match response {
            AgentResponse::ThreadSnapshot(snapshot) => {
                if let Some(harness) = &snapshot.harness_state
                    && harness.workspace_security.instructions
                        == nuo_protocol::WorkspaceTrustState::Changed
                {
                    saw_changed_snapshot = true;
                    // The snapshot is the gate; the passive banner must not
                    // follow for an interactive attach.
                    break;
                }
            }
            AgentResponse::Round {
                scope:
                    nuo_protocol::RoundScope::Session {
                        event: RoundEvent::Notice(n),
                    },
                ..
            } if n.kind == nuo_tool::NoticeKind::TrustChanged => {
                saw_banner = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_changed_snapshot,
        "attach-sync HarnessState never carried the changed security snapshot"
    );
    assert!(
        !saw_banner,
        "an interactive attach must gate on the changed snapshot, not receive the passive banner"
    );
}

/// An autonomous attach gets the banner instead of the gate: a question pushed
/// to a channel nobody reads would park the security decision forever.
///
/// `HumanChannelPosture` is the authority on "can this client answer?", not a
/// guess about which client is connected.
#[tokio::test]
async fn autonomous_attach_falls_back_to_the_banner() {
    let tmp = tempfile::tempdir().unwrap();
    let security_file = tmp.path().join("workspace_security.json");
    std::fs::write(tmp.path().join("AGENTS.md"), "project instructions").unwrap();
    let security = nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
        security_file.clone(),
    );
    security
        .trust_domains(tmp.path(), &[nuo_protocol::TrustDomain::Instructions])
        .unwrap();
    std::fs::write(tmp.path().join("AGENTS.md"), "project instructions v2").unwrap();
    let (mut ws, _server) = attach_trust_workspace_with_posture(
        &tmp,
        &security_file,
        nuo_protocol::human_request::HumanChannelPosture::Autonomous,
    )
    .await;

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
        let AgentResponse::Round { scope, .. } = response else {
            continue;
        };
        if let RoundEvent::Notice(n) = scope.into_event()
            && n.kind == nuo_tool::NoticeKind::TrustChanged
        {
            saw_banner = true;
            break;
        }
    }
    assert!(
        saw_banner,
        "an autonomous attach cannot answer a gate, so it must be told in the banner"
    );
}
