//! Transport tests: discovery, server lifecycle, attach, control, and rows.

use super::*;

use super::attach::{AttachError, Handshake, finish_handshake_with_project};
use super::control::finish_control;
use super::lifecycle::{
    CONTROL_REPLY_TIMEOUT, FALLBACK_GRACE, configure_server_detachment, discover, discover_at, stop,
};
use super::monitor::finish_monitor;

/// ADR-0136, control plane: an unmodelled frame must produce a *diagnosable
/// refusal naming the skew*, not a silent timeout.
///
/// A control connection carries exactly one verb and one reply, so unlike
/// the attach path there is nothing to keep reading for: the correct
/// behaviour is to fail — but the failure must say what actually happened.
/// Before ADR-0136 the frame surfaced as `Err`, which this path reported as
/// `wire recv during control: ...`, telling an operator nothing about a
/// version skew and inviting them to go hunting for a network fault.
#[tokio::test]
async fn an_unmodelled_frame_on_the_control_plane_names_the_skew() {
    let (client_side, server_side) = tokio::net::UnixStream::pair().unwrap();
    // No handshake reply: the skewed frame arrives in place of the reply,
    // which is exactly what a peer one revision ahead would send if the
    // verb itself were renamed.
    tokio::spawn(drifted_peer(server_side, Vec::new()));

    let (sink, source) = crate::transport::native_framed_split(client_side);
    let error = finish_control(
        (sink, source),
        AttachAction::Control(ControlRequest::Reload),
        CONTROL_REPLY_TIMEOUT,
    )
    .await
    .expect_err("a control connection cannot decode this verb's reply");

    assert!(
        error.contains("cannot decode"),
        "the refusal must say the frame was unreadable rather than \
         reporting a transport fault, got: {error}"
    );
    assert!(
        error.contains("Response"),
        "the refusal must name the envelope direction, so an operator can \
         tell which side drifted, got: {error}"
    );
}

/// ADR-0136, monitor plane: an unmodelled frame must be **skipped**, not
/// treated as the end of the stream.
///
/// The monitor stream is long-lived — `nuo status --watch` and the dashboard
/// sit on it — so ending it on one unknown frame would silently blank a live
/// surface. The event *after* the skew must still arrive, which is the
/// assertion that proves the reader kept going instead of breaking.
#[tokio::test]
async fn an_unmodelled_frame_on_the_monitor_plane_is_skipped_not_fatal() {
    use tokio::io::AsyncWriteExt;

    let (client_side, mut server_side) = tokio::net::UnixStream::pair().unwrap();

    // Handshake: the snapshot, which must arrive as the first frame.
    let snapshot = Wire::Monitor {
        event: MonitorEvent::Snapshot(nuo_protocol::monitor::MonitorSnapshot {
            project_root: "/tmp/nuo-monitor-test".to_string(),
            server_started_at: 0,
            threads: Vec::new(),
            tasks: Vec::new(),
            persistence_health: None,
        }),
    };
    let after = Wire::Monitor {
        event: MonitorEvent::ServerDraining,
    };

    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut prefix = [0u8; 4];
        server_side.read_exact(&mut prefix).await.unwrap();
        let len = u32::from_be_bytes(prefix) as usize;
        let mut select = vec![0u8; len];
        server_side.read_exact(&mut select).await.unwrap();

        let mut out = Vec::new();
        let snap = serde_json::to_vec(&snapshot).unwrap();
        out.extend_from_slice(&(snap.len() as u32).to_be_bytes());
        out.extend_from_slice(&snap);

        // The skew, sandwiched *between* two valid events.
        const SKEWED: &[u8] = br#"{"type":"Response","response":{"NoSuchVariant":{}}}"#;
        out.extend_from_slice(&(SKEWED.len() as u32).to_be_bytes());
        out.extend_from_slice(SKEWED);

        let next = serde_json::to_vec(&after).unwrap();
        out.extend_from_slice(&(next.len() as u32).to_be_bytes());
        out.extend_from_slice(&next);

        server_side.write_all(&out).await.unwrap();
        server_side.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let (sink, source) = crate::transport::native_framed_split(client_side);
    let mut rx = finish_monitor(
        (sink, source),
        MonitorAction {
            watch: true,
            include_idle: true,
        },
        "test",
    )
    .await
    .expect("the snapshot handshake succeeds");

    // The snapshot is the seeded first frame.
    let first = rx.recv().await.expect("snapshot arrives");
    assert!(
        matches!(first, MonitorEvent::Snapshot(_)),
        "the handshake frame must be the snapshot, got {first:?}"
    );

    // The frame after the skew proves the reader did not stop on it.
    let next = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("a frame after the skew must still arrive: the stream is alive")
        .expect("the monitor channel must still be open");
    assert!(
        matches!(next, MonitorEvent::ServerDraining),
        "the event behind the unmodelled frame must be delivered, got {next:?}"
    );
}

/// A peer that answers the client's `Select` with whatever the caller
/// stages, then emits one frame this build cannot model — and finally stays
/// silent with the socket **open**.
///
/// Hand-framed on purpose: the skewed envelope is one no encoder in this
/// build can produce, which is the whole condition being reproduced. The
/// socket is left open so a client that mistakes the frame for a disconnect
/// fails the assertion rather than passing by accident.
///
/// `first_frames` are serialized normally (the handshake replies); the
/// skewed payload follows.
async fn drifted_peer(mut server_side: tokio::net::UnixStream, first_frames: Vec<Wire>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Consume the client's `Select`.
    let mut prefix = [0u8; 4];
    server_side.read_exact(&mut prefix).await.unwrap();
    let len = u32::from_be_bytes(prefix) as usize;
    let mut select = vec![0u8; len];
    server_side.read_exact(&mut select).await.unwrap();

    let mut out = Vec::new();
    for frame in first_frames {
        let bytes = serde_json::to_vec(&frame).unwrap();
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&bytes);
    }

    // The exact shape that killed the client.
    const SKEWED: &[u8] = br#"{"type":"Response","response":{"RetryResolved":{"attempt":1}}}"#;
    out.extend_from_slice(&(SKEWED.len() as u32).to_be_bytes());
    out.extend_from_slice(SKEWED);

    server_side.write_all(&out).await.unwrap();
    server_side.flush().await.unwrap();

    // Hold the connection open: a live server, not a closed socket.
    tokio::time::sleep(Duration::from_secs(5)).await;
}

/// `discover` must not park a Tokio worker.
///
/// The synchronous predecessor slept with `std::thread::sleep` while another
/// client spawned and tested liveness with blocking `connect` calls. Called
/// from inside a runtime — which is where every client-side caller lives —
/// that stalled a worker thread, and with it the render loop.
///
/// This is a *liveness-under-parking* test: the probe runs while a
/// concurrent timer must keep firing. A `discover` that blocks its thread
/// stops the single worker, so the timer misses its deadline and the test
/// fails. It would have failed against the old implementation because the
/// timer and the probe cannot both make progress on one worker otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn discover_never_parks_the_worker() {
    let dir = std::env::temp_dir();

    // Drive discovery continuously from the only worker thread. Any
    // thread-parking implementation stops the timer below from firing.
    let probe = tokio::spawn(async move {
        for _ in 0..50 {
            let _ = discover(&dir).await;
        }
    });

    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;

    assert!(
        observed.is_ok(),
        "the timer starved for 5 s: `discover` parked the worker thread"
    );
    probe.await.unwrap();
}

/// `[INV-ATTACH-01]` — an attach refusal must preserve the server's
/// machine-readable taxonomy on its way to the decision site.
///
/// The bug this locks: the handshake destructured `Wire::Error { message, .. }`
/// — the `..` discarding the code — so an expected outcome (`session_gone`)
/// was indistinguishable from a transport fault and the client had no branch
/// but "die".
#[test]
fn attach_refusal_preserves_code() {
    let gone = AttachError {
        code: Some(nuo_protocol::ERR_SESSION_GONE.to_string()),
        message: "server rejected the attach: session 'x' no longer exists".to_string(),
    };
    assert_eq!(gone.code(), Some(nuo_protocol::ERR_SESSION_GONE));
    assert!(gone.is_session_gone());
    assert!(
        !gone.is_session_unresumable(),
        "the two recoveries are mutually exclusive"
    );

    let unresumable = AttachError {
        code: Some(nuo_protocol::ERR_SESSION_UNRESUMABLE.to_string()),
        message: "server rejected the attach: could not resume session x: boom".to_string(),
    };
    assert!(unresumable.is_session_unresumable());
    assert!(
        !unresumable.is_session_gone(),
        "a resumable session must never be retired"
    );

    // A transport fault carries no taxonomy: it must stay fatal rather than
    // be mistaken for a retirement instruction.
    let transport = AttachError::transport("ws connect failed");
    assert_eq!(transport.code(), None);
    assert!(!transport.is_session_gone());
    assert!(!transport.is_session_unresumable());

    // `Display` stays human prose — a caller must never parse the code back
    // out of the rendered message.
    assert_eq!(gone.to_string(), gone.message);
    assert!(!gone.to_string().contains(nuo_protocol::ERR_SESSION_GONE));
}

/// An attach refusal's wire code round-trips through the frame the client
/// actually parses.
#[test]
fn wire_error_frame_carries_the_refusal_code() {
    let frame = Wire::Error {
        message: "session 'abc' no longer exists".to_string(),
        code: Some(nuo_protocol::ERR_SESSION_GONE.to_string()),
    };
    let json = serde_json::to_string(&frame).unwrap();
    let back: Wire = serde_json::from_str(&json).unwrap();
    match back {
        Wire::Error { code, .. } => {
            assert_eq!(code.as_deref(), Some(nuo_protocol::ERR_SESSION_GONE));
        }
        other => panic!("expected an Error frame, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn server_detachment_creates_a_fresh_session_and_process_group() {
    use std::process::{Command, Stdio};

    let mut command = Command::new("sleep");
    command
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_server_detachment(&mut command);

    let mut child = command
        .spawn()
        .expect("a correctly detached child must spawn");
    let pid = child.id() as libc::pid_t;
    // SAFETY: `pid` names the live child owned by this test.
    let sid = unsafe { libc::getsid(pid) };
    // SAFETY: `pid` names the live child owned by this test.
    let pgid = unsafe { libc::getpgid(pid) };

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(sid, pid, "setsid must make the server a session leader");
    assert_eq!(
        pgid, pid,
        "setsid must also make the server a process-group leader"
    );
}

#[test]
fn remote_server_parses_the_documented_address_forms() {
    let t = |addr| RemoteServer::parse(addr, Some("tok".into())).unwrap();
    assert_eq!(
        (t("192.168.1.4:9800").host, t("192.168.1.4:9800").port),
        ("192.168.1.4".to_string(), 9800)
    );
    assert!(!t("192.168.1.4:9800").secure);
    assert_eq!(t("ws://box.lan:9800").host, "box.lan");
    assert_eq!(t("ws://box.lan:9800").port, 9800);
    assert!(!t("ws://box.lan:9800").secure);
    assert_eq!(t(":9800").host, "127.0.0.1");
    assert_eq!(t(":9800").port, 9800);
    assert_eq!(t("  box.lan:9800  ").host, "box.lan");

    let wss = t("wss://tunnel.example.com");
    assert_eq!(wss.host, "tunnel.example.com");
    assert_eq!(wss.port, 443);
    assert!(wss.secure);
    assert_eq!(wss.endpoint_display(), "wss://tunnel.example.com");

    let wss_port = t("wss://tunnel.example.com:8443");
    assert_eq!(wss_port.host, "tunnel.example.com");
    assert_eq!(wss_port.port, 8443);
    assert!(wss_port.secure);
    assert_eq!(wss_port.endpoint_display(), "wss://tunnel.example.com:8443");

    let https = t("https://tunnel.example.com/ws");
    assert_eq!(https.host, "tunnel.example.com");
    assert_eq!(https.port, 443);
    assert!(https.secure);

    let ws_def = t("ws://box.lan");
    assert_eq!(ws_def.host, "box.lan");
    assert_eq!(ws_def.port, 80);
    assert!(!ws_def.secure);
    assert_eq!(ws_def.endpoint_display(), "ws://box.lan");

    let ipv6 = t("[::1]:9800");
    assert_eq!(ipv6.host, "[::1]");
    assert_eq!(ipv6.port, 9800);
    assert!(!ipv6.secure);
}

#[test]
fn remote_server_requires_a_port_and_a_token() {
    let err = RemoteServer::parse("box.lan", Some("tok".into())).unwrap_err();
    assert!(err.contains("not host:port"), "{err}");
    let err = RemoteServer::parse("box.lan:notaport", Some("tok".into())).unwrap_err();
    assert!(err.contains("not a port number"), "{err}");
    let err = RemoteServer::parse("box.lan:9800", None).unwrap_err();
    assert!(err.contains("--token"), "{err}");
    let err = RemoteServer::parse("box.lan:9800", Some(String::new())).unwrap_err();
    assert!(err.contains("--token"), "{err}");
}

fn record(port: u16, token: Option<String>) -> ServerInfo {
    ServerInfo {
        pid: 99999999, // Unused/dead pid
        process_birth_token: None,
        port,
        token,
        project_root: "/tmp/proj".to_string(),
        started_at: 0,
        uds_path: None,
        local_endpoint: None,
        version: None,
        grace_secs: None,
        protocol: None,
        image_digest: None,
        image_len: None,
    }
}

fn dead_port() -> u16 {
    for _ in 0..10 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        if std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            std::time::Duration::from_millis(5),
        )
        .is_err()
        {
            return port;
        }
    }
    1
}

#[tokio::test]
async fn discover_at_returns_none_without_racing_to_delete_stale_record() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("serve.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&record(dead_port(), None)).unwrap(),
    )
    .unwrap();
    assert!(discover_at(&path).await.is_none());
    assert!(
        path.exists(),
        "a reader must leave stale cleanup to the lock-owning lifecycle path"
    );
}

#[tokio::test]
async fn discover_at_preserves_record_if_pid_is_still_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("serve.json");
    let live_rec = ServerInfo {
        pid: std::process::id(),
        process_birth_token: None,
        port: dead_port(),
        token: None,
        project_root: "/tmp/proj".to_string(),
        started_at: 0,
        uds_path: None,
        local_endpoint: None,
        version: None,
        grace_secs: None,
        protocol: None,
        image_digest: None,
        image_len: None,
    };
    std::fs::write(&path, serde_json::to_vec(&live_rec).unwrap()).unwrap();
    assert!(discover_at(&path).await.is_none());
    assert!(
        path.exists(),
        "discovery file for living PID must NOT be deleted on transient probe fail"
    );
}

#[tokio::test]
async fn discover_at_tolerates_missing_and_corrupt_files() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("serve.json");
    assert!(discover_at(&path).await.is_none());
    std::fs::write(&path, b"not json").unwrap();
    assert!(discover_at(&path).await.is_none());
    assert!(path.exists());
}

#[tokio::test]
async fn stop_handles_already_dead_process_and_cleans_up() {
    let info = ServerInfo {
        pid: 99999999, // Unused pid
        process_birth_token: None,
        port: 1,
        token: None,
        project_root: String::new(),
        started_at: 0,
        uds_path: None,
        local_endpoint: None,
        version: Some("0.0.1".to_string()),
        grace_secs: None,
        protocol: None,
        image_digest: None,
        image_len: None,
    };
    let res = stop(&info).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn stop_refuses_a_recycled_pid_identity() {
    let identity = nuo_host::process::process_identity(std::process::id()).unwrap();
    let info = ServerInfo {
        pid: identity.pid,
        process_birth_token: Some(identity.birth_token.wrapping_add(1)),
        port: 1,
        token: None,
        project_root: String::new(),
        started_at: 0,
        uds_path: None,
        local_endpoint: None,
        version: Some("0.0.1".to_string()),
        grace_secs: None,
        protocol: None,
        image_digest: None,
        image_len: None,
    };
    let error = stop(&info).await.unwrap_err();
    assert!(error.contains("process identity is stale"));
}

#[test]
fn stop_budget_follows_the_advertised_grace() {
    assert!(FALLBACK_GRACE >= Duration::from_secs(10));
}

/// ADR-0136, end to end: a server that sends a frame this build cannot
/// model must **not** end the client's attachment.
///
/// The real incident: a server binary replaced on disk kept running and
/// kept emitting the deleted `RetryResolved` variant. The decoder reported
/// it as `Err`, the reader task read `Err` as "the socket died", and the
/// synthesized `Exit` closed the window — with the attach line
/// (`nuo: connecting to the session server…`) the only thing the operator
/// ever saw.
///
/// This drives the actual attach path over a socket pair: a peer answers
/// `Select` with `Welcome`, then sends the unmodelled envelope. The
/// assertion that matters is that the response channel is still *open* and
/// a disclosure arrives — not that the stream ended.
#[tokio::test]
async fn an_unmodelled_frame_after_welcome_does_not_end_the_attachment() {
    let (client_side, mut server_side) = tokio::net::UnixStream::pair().unwrap();

    // Minimal server, hand-framed: it must be able to write an envelope no
    // encoder in this build can produce, which is exactly the condition
    // being reproduced.
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        use tokio::io::AsyncWriteExt;

        // Consume the client's `Select` (length prefix + payload).
        let mut prefix = [0u8; 4];
        server_side.read_exact(&mut prefix).await.unwrap();
        let len = u32::from_be_bytes(prefix) as usize;
        let mut select = vec![0u8; len];
        server_side.read_exact(&mut select).await.unwrap();

        let welcome = serde_json::to_vec(&Wire::Welcome {
            thread_id: "thread-drifted".to_string(),
            phase: nuo_protocol::ThreadPhase::Binding,
        })
        .unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(&(welcome.len() as u32).to_be_bytes());
        out.extend_from_slice(&welcome);

        // The exact shape that killed the client: a `Response` envelope
        // naming a variant this build no longer has.
        const SKEWED: &[u8] = br#"{"type":"Response","response":{"RetryResolved":{"attempt":1}}}"#;
        out.extend_from_slice(&(SKEWED.len() as u32).to_be_bytes());
        out.extend_from_slice(SKEWED);

        server_side.write_all(&out).await.unwrap();
        server_side.flush().await.unwrap();

        // Hold the connection open so the client cannot mistake this for a
        // disconnect — that separation is the whole point.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let (sink, source) = crate::transport::native_framed_split(client_side);
    let handshake = finish_handshake_with_project((sink, source), AttachAction::Attach(None), None)
        .await
        .expect("the attach itself succeeds: Welcome arrived first");

    let Handshake::Attached { mut resp_rx, .. } = handshake else {
        panic!("expected an attached handshake");
    };

    // The skew must surface as a disclosure the operator can act on.
    let response = tokio::time::timeout(Duration::from_secs(3), resp_rx.recv())
        .await
        .expect("a disclosure must arrive before the deadline")
        .expect("the channel must still be open — the link is alive");

    match response {
        AgentResponse::Error(message) => {
            assert!(
                message.contains("does not understand"),
                "the disclosure must say the frame was unreadable, got: {message}"
            );
            assert!(
                message.contains("still live"),
                "the disclosure must state the connection survived, got: {message}"
            );
        }
        other => panic!("expected a disclosure, got {other:?}"),
    }

    // And crucially: no `Exit`. The old code synthesized one here, which the
    // TUI applied as `should_quit`.
    let next = tokio::time::timeout(Duration::from_millis(600), resp_rx.recv()).await;
    assert!(
        next.is_err(),
        "a skewed frame must not terminate the session; the reader must simply              continue waiting for the next frame"
    );
}
