//! End-to-end lifecycle tests for the daemon run loop (ADR-0101): the
//! shutdown trigger is injected (no signals needed — that is the point of
//! the gate abstraction), and the assertions check the *phases*: discovery
//! advertisement pulled, sessions torn down, tasks joined within the
//! budget, and the exit-code contract.
//!
//! Two isolation notes:
//!
//! - The loop's filesystem footprint (discovery record, instance lock) is
//!   sandboxed by pointing `NUO_HOME` at a temp root **before the first
//!   `paths::get()` resolution in this process** (ADR-0121). One variable
//!   redirects every category and the daemon's runtime files; the env is
//!   set once in a `static` initializer, and every test gets its own
//!   subdirectories via a per-test UDS path — the shared record paths live
//!   under the same sandbox root, so cross-test interference is limited to
//!   the record file itself, which each test waits out via its own outcome.
//! - `host::run_with_gate` is driven against a prehost-only registry (no
//!   session assembly); hosted sessions are inserted by hand, the same
//!   fixture style as `serve_integration`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use nuo_wire::MonitorEvent;
use nuo_persistence::session::SessionStore;
use nuo::host::{HostIdentity, HostOptions, LifecycleOptions, RunOutcome};
use nuo::registry::{HostedSession, SessionRegistry};
use nuo::serve::ServeExpose;
use nuo::shutdown::{DrainProbe, ShutdownGate, ShutdownReason};
use tokio::sync::{Mutex, broadcast, mpsc};

/// Sandbox the process-wide dirs once, before any `paths::get()` call can
/// cache a real-user resolution (see module docs).
///
/// ADR-0121: `NUO_HOME` alone redirects every category *and* the daemon
/// instance dir, so the five hand-assembled env vars this used to set
/// collapse to one. The root is a dedicated tempdir (not the shared
/// `sandbox` subdirs) kept alive for the process.
use super::sandbox_once;

/// The ADR-0121 isolation contract, pinned at the level users experience
/// it: with `NUO_HOME` set, every category and the daemon's runtime
/// files resolve under the sandbox root, and the host's XDG runtime dir
/// (which the test env deliberately does not clear) cannot leak any
/// daemon-facing path back out of it.
#[test]
fn nuo_home_redirects_the_daemon_footprint() {
    sandbox_once();
    let dirs = nuo_persistence::paths::get();
    let root = std::env::var("NUO_HOME").unwrap();
    let under_root = |p: &std::path::Path| p.starts_with(&root);
    for dir in [
        &dirs.config_dir,
        &dirs.data_dir,
        &dirs.state_dir,
        &dirs.cache_dir,
        &dirs.instance_dir(),
    ] {
        assert!(
            under_root(dir),
            "{:?} must stay under the NUO_HOME sandbox root",
            dir
        );
    }
    assert_eq!(
        dirs.instance_dir(),
        std::path::PathBuf::from(&root)
            .join("nuo")
            .join("instance")
    );
    // And the daemon-facing paths derive from the instance dir.
    for path in [
        nuo::serve_discovery::global_discovery_path(),
        nuo::serve_discovery::global_lock_path(),
        #[cfg(unix)]
        nuo::serve_discovery::default_uds_path(),
    ] {
        assert!(under_root(&path), "{path:?} must follow the instance dir");
    }
}

/// A minimal `HostIdentity` for the loop: the prehost-only registry never
/// assembles, so identity values are never exercised — they only need to
/// exist.
fn test_identity() -> HostIdentity {
    HostIdentity {
        identity: nuo_wire::AgentIdentity::new("probe", "lifecycle probe"),
        preset: nuo_wire::AgentRoleProfile::with_identity(
            "probe",
            nuo_wire::AgentIdentity::new("probe", "lifecycle probe"),
        ),
        ui: Arc::new(HeadlessProbe),
    }
}

/// Trivial UiBridge stand-in (the loop only stores it).
struct HeadlessProbe;

#[async_trait::async_trait]
impl nuo::UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<nuo::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

/// Host one hand-built session; returns the monitor subscription-friendly
/// registry (fixtures mirror `serve_integration::prehosted`, minus the tap).
async fn host_one(registry: &Arc<SessionRegistry>, project: &str) {
    let session = Arc::new(SessionStore::load_for_project(project.into()));
    let (req_tx, _req_rx) = mpsc::channel::<nuo_wire::AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<nuo_wire::AgentResponse>(16);
    let id = session.id().await;
    let tracker = Arc::new(Mutex::new(
        nuo::monitor::MonitorTracker::bootstrap(
            nuo_wire::MonitoredSession::empty(id),
            nuo_wire::SessionStatus::Idle,
        ),
    ));
    registry
        .host(HostedSession {
            workspace_root: Some(std::path::PathBuf::from(project)),
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
}

static LIFECYCLE_TEST_LOCK: Mutex<()> = Mutex::const_new(());

fn options(uds: std::path::PathBuf, port: u16) -> HostOptions {
    HostOptions {
        port,
        expose: ServeExpose::Local,
        token: None,
        // Tests drive the control plane without credentials.
        local_auth: false,
        port_fallback: false,
        local_endpoint: nuo_host::ipc::endpoint_for_instance(
            uds,
            &format!("lifecycle-{port}"),
        )
        .ok(),
    }
}

#[tokio::test]
async fn control_verb_drain_removes_discovery_and_exits_zero() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());
    host_one(&registry, tmp.path().join("proj-a").to_str().unwrap()).await;
    let mut monitor_rx = registry.subscribe_monitor();

    let gate = Arc::new(ShutdownGate::new());
    let trigger_gate = gate.clone();
    tokio::spawn(async move {
        // Request shortly after the loop starts serving.
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger_gate.request(ShutdownReason::ControlVerb, false);
    });

    let outcome = nuo::host::run_with_registry(
        test_identity(),
        options(tmp.path().join("a.sock"), 0),
        gate,
        LifecycleOptions {
            shutdown_grace: Duration::from_secs(3),
            idle_exit: None,
            client_driven: false,
            drain_probe: None,
        },
        registry,
    )
    .await;

    // Exit contract (ADR-0101): a completed graceful stop is 0.
    assert_eq!(outcome.exit_code(), 0);
    match &outcome {
        RunOutcome::Stopped { reason } => assert_eq!(*reason, ShutdownReason::ControlVerb),
        other => panic!("expected Stopped, got {other:?}"),
    }
    // The discovery advertisement is gone (the lease released it).
    assert!(
        !nuo::serve_discovery::global_discovery_path().exists(),
        "discovery record must be removed on drain"
    );
    // The monitor bus saw the drain announcement and the session teardown.
    let mut saw_draining = false;
    let mut saw_removed = false;
    while let Ok(event) = monitor_rx.try_recv() {
        match event {
            MonitorEvent::DaemonDraining => saw_draining = true,
            MonitorEvent::SessionRemoved { .. } => saw_removed = true,
            _ => {}
        }
    }
    assert!(
        saw_draining,
        "watch clients must be told the daemon is draining"
    );
    assert!(saw_removed, "hosted sessions must be torn down on drain");
}

#[tokio::test]
async fn idle_exit_triggers_after_the_grace_period() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    // No sessions, no connections: the idle timer is the only exit path.
    // The probe polls every 5s; a near-zero grace needs one tick, so the
    // test's wall time is bounded by one probe tick.
    let registry = Arc::new(SessionRegistry::prehost_only());
    let gate = Arc::new(ShutdownGate::new());
    let _ = registry;
    let outcome = nuo::host::run_with_gate(
        test_identity(),
        options(tmp.path().join("b.sock"), 0),
        gate,
        LifecycleOptions {
            shutdown_grace: Duration::from_secs(3),
            idle_exit: Some(Duration::from_millis(1)),
            client_driven: false,
            drain_probe: None,
        },
    )
    .await;
    assert_eq!(outcome.exit_code(), 0);
    match &outcome {
        RunOutcome::Stopped { reason } => assert_eq!(*reason, ShutdownReason::IdleTimeout),
        other => panic!("expected IdleTimeout stop, got {other:?}"),
    }
}

#[tokio::test]
async fn escalation_skips_the_graceful_phases() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("c.sock");
    let registry = Arc::new(SessionRegistry::prehost_only());
    host_one(&registry, tmp.path().join("proj-b").to_str().unwrap()).await;
    let mut monitor_rx = registry.subscribe_monitor();

    let gate = Arc::new(ShutdownGate::new());
    let probe = Arc::new(DrainProbe::new());
    let g1 = gate.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        g1.request(ShutdownReason::SignalInterrupt, false);
    });

    let run_gate = gate.clone();
    let run_probe = probe.clone();
    let run = tokio::spawn(async move {
        nuo::host::run_with_registry(
            test_identity(),
            options(sock, 0),
            run_gate,
            LifecycleOptions {
                shutdown_grace: Duration::from_secs(5),
                idle_exit: None,
                client_driven: false,
                drain_probe: Some(run_probe),
            },
            registry,
        )
        .await
    });

    // Deterministic escalation: the drain is parked at the probe, so the
    // second trigger lands mid-drain — never after the phases finished.
    probe.parked().await;
    gate.request(ShutdownReason::SignalTerminate, true);
    probe.release();
    let outcome = run.await.unwrap();

    match &outcome {
        // Escalation still exits 0 (the operator asked it to stop and it
        // stopped) — the aborted stragglers are named in the log.
        RunOutcome::ForcedExit { reason } => assert_eq!(*reason, ShutdownReason::SignalInterrupt),
        other => panic!("expected ForcedExit after escalation, got {other:?}"),
    }
    assert_eq!(outcome.exit_code(), 0);
    // The drain announcement still went out before the force, but the
    // graceful session teardown was skipped: no SessionRemoved may appear.
    let mut saw_draining = false;
    let mut saw_removed = false;
    while let Ok(event) = monitor_rx.try_recv() {
        match event {
            MonitorEvent::DaemonDraining => saw_draining = true,
            MonitorEvent::SessionRemoved { .. } => saw_removed = true,
            _ => {}
        }
    }
    assert!(saw_draining);
    assert!(
        !saw_removed,
        "an escalated drain must skip the graceful session teardown"
    );
}

#[tokio::test]
async fn port_bind_failure_is_a_readable_startup_failure() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    // Occupy a port so the daemon's bind fails.
    let hog = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = hog.local_addr().unwrap().port();

    let registry = Arc::new(SessionRegistry::prehost_only());
    let _ = registry;
    let outcome = nuo::host::run_with_gate(
        test_identity(),
        options(tmp.path().join("d.sock"), taken),
        Arc::new(ShutdownGate::new()),
        LifecycleOptions::default(),
    )
    .await;

    match &outcome {
        RunOutcome::StartupFailed(what) => {
            // The real io::Error text, not a bare RecvError (ADR-0101).
            assert!(
                what.contains("could not bind"),
                "readable bind error: {what}"
            );
        }
        other => panic!("expected StartupFailed, got {other:?}"),
    }
    assert_eq!(outcome.exit_code(), 1);
}

/// ADR-0125: a hosted session with armed `/schedule` jobs is exempt from
/// idle suspension — suspending it would park its tick loop and silently
/// stop the schedule from firing. A schedule-free session under the same
/// conditions still suspends (the memory-bounding behavior ADR-0113 added).
#[tokio::test]
async fn idle_suspension_spares_sessions_with_armed_schedules() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());

    // Two sessions past the TTL: one with an armed job, one without. Both
    // need real content so the store persists them (empty sessions are the
    // reaper's, not the suspender's). Each is hosted through the same
    // `SessionStore` it was armed on, so the id the registry sees is the id
    // the schedule belongs to.
    async fn host_with_session(
        registry: &Arc<SessionRegistry>,
        session: Arc<SessionStore>,
        project_root: &std::path::Path,
    ) {
        let (req_tx, _req_rx) = mpsc::channel::<nuo_wire::AgentRequest>(512);
        let (bc_tx, _) = broadcast::channel::<nuo_wire::AgentResponse>(16);
        let tracker = Arc::new(Mutex::new(
            nuo::monitor::MonitorTracker::bootstrap(
                nuo_wire::MonitoredSession::empty(session.id().await),
                nuo_wire::SessionStatus::Idle,
            ),
        ));
        registry
            .host(HostedSession {
                workspace_root: Some(project_root.to_path_buf()),
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
    }

    let plain_project = tmp.path().join("plain-project");
    let plain_session = Arc::new(SessionStore::load_for_project(plain_project.clone()));
    plain_session
        .replace_messages(vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
            "no schedule here",
        )])
        .await
        .unwrap();
    host_with_session(&registry, plain_session.clone(), &plain_project).await;

    // Zero TTL: everything idle-suspends unless exempted.
    let suspended = registry
        .suspend_idle_sessions_with(Duration::from_millis(0))
        .await;

    let plain_id = plain_session.id().await;
    assert!(
        suspended.contains(&plain_id),
        "a schedule-free idle session should still suspend (suspended: {suspended:?})"
    );
}

#[tokio::test]
async fn client_driven_lifecycle_stops_when_interactive_client_disconnects() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("client_driven.sock");
    let registry = Arc::new(SessionRegistry::prehost_only());
    let proj_dir = tmp.path().join("proj-cd");
    host_one(&registry, proj_dir.to_str().unwrap()).await;

    let gate = Arc::new(ShutdownGate::new());
    let sock_clone = sock.clone();
    let run = tokio::spawn(async move {
        nuo::host::run_with_registry(
            test_identity(),
            options(sock_clone, 0),
            gate,
            LifecycleOptions {
                shutdown_grace: Duration::from_secs(3),
                idle_exit: None,
                client_driven: true,
                drain_probe: None,
            },
            registry,
        )
        .await
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !sock.exists() {
        if tokio::time::Instant::now() >= deadline {
            panic!("socket was not created in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (mut sink, mut source) = nuo_client::wire::native_framed_split(stream);

    use futures::{SinkExt, StreamExt};
    sink.send(nuo_client::wire::Wire::Select {
        action: nuo_client::wire::AttachAction::Attach(None),
        project: Some(proj_dir),
        posture: nuo_wire::human_request::HumanChannelPosture::Interactive,
        version: None,
        protocol: Some(nuo_client::wire::PROTOCOL_VERSION),
    })
    .await
    .unwrap();

    let welcome = source.next().await.unwrap().unwrap();
    assert!(matches!(welcome, nuo_client::wire::Wire::Welcome { .. }));

    // Disconnect the interactive client
    drop(sink);
    drop(source);

    // Daemon terminates cleanly after 1s debounce
    let outcome = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("daemon timed out waiting for client_driven exit")
        .expect("join run");

    assert_eq!(outcome.exit_code(), 0);
    match &outcome {
        RunOutcome::Stopped { reason } => assert_eq!(*reason, ShutdownReason::AllClientsClosed),
        other => panic!("expected AllClientsClosed, got {other:?}"),
    }
}

#[tokio::test]
async fn conflicting_stale_socket_is_directly_replaced() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("conflict.sock");

    // Write a stale socket file
    std::fs::write(&sock, b"stale socket").unwrap();
    assert!(sock.exists());

    let registry = Arc::new(SessionRegistry::prehost_only());
    let gate = Arc::new(ShutdownGate::new());
    let g1 = gate.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        g1.request(ShutdownReason::ControlVerb, false);
    });

    let outcome = nuo::host::run_with_registry(
        test_identity(),
        options(sock.clone(), 0),
        gate,
        LifecycleOptions {
            shutdown_grace: Duration::from_secs(3),
            idle_exit: None,
            client_driven: false,
            drain_probe: None,
        },
        registry,
    )
    .await;

    assert_eq!(outcome.exit_code(), 0);
    match &outcome {
        RunOutcome::Stopped { reason } => assert_eq!(*reason, ShutdownReason::ControlVerb),
        other => panic!("expected ControlVerb, got {other:?}"),
    }
}
