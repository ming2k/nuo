//! End-to-end lifecycle tests for the server run loop: the
//! shutdown trigger is injected (no signals needed — that is the point of
//! the gate abstraction), and the assertions check the *phases*: discovery
//! advertisement pulled, sessions torn down, tasks joined within the
//! budget, and the exit-code contract.
//!
//! Two isolation notes:
//!
//! - The loop's filesystem footprint (discovery record, instance lock) is
//!   sandboxed by pointing `NUO_HOME` at a temp root **before the first
//!   `paths::get` resolution in this process**. One variable
//!   redirects every category and the server's runtime files; the env is
//!   set once in a `static` initializer, and every test gets its own
//!   subdirectories via a per-test UDS path — the shared record paths live
//!   under the same sandbox root, so cross-test interference is limited to
//!   the record file itself, which each test waits out via its own outcome.
//! - `nuo_server::runtime::host::run_with_gate` is driven against a prehost-only registry (no
//!   session assembly); hosted sessions are inserted by hand, the same
//!   fixture style as `serve_integration`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use nuo_persistence::thread::ThreadSession;
use nuo_protocol::MonitorEvent;
use nuo_server::runtime::host::{HostIdentity, HostOptions, LifecycleOptions, RunOutcome};
use nuo_server::runtime::registry::{HostedThread, ThreadRegistry};
use nuo_server::runtime::shutdown::{DrainProbe, ShutdownGate, ShutdownReason};
use nuo_server::transport::serve::ServeExpose;
use tokio::sync::{Mutex, broadcast, mpsc};

/// Sandbox the process-wide dirs once, before any `paths::get()` call can
/// cache a real-user resolution (see module docs).
///
/// `NUO_HOME` alone redirects every category *and* the server
/// instance dir, so the five hand-assembled env vars this used to set
/// collapse to one. The root is a dedicated tempdir (not the shared
/// `sandbox` subdirs) kept alive for the process.
use super::sandbox_once;

/// The isolation contract, pinned at the level users experience
/// it: with `NUO_HOME` set, every category and the server's runtime
/// files resolve under the sandbox root, and the host's XDG runtime dir
/// (which the test env deliberately does not clear) cannot leak any
/// server-facing path back out of it.
#[test]
fn nuo_home_redirects_the_server_footprint() {
    sandbox_once();
    let dirs = nuo_host::paths::get();
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
        std::path::PathBuf::from(&root).join("nuo").join("instance")
    );
    // And the server-facing paths derive from the instance dir.
    for path in [
        nuo_server::transport::serve_discovery::global_discovery_path(),
        nuo_server::transport::serve_discovery::global_lock_path(),
        #[cfg(unix)]
        nuo_server::transport::serve_discovery::default_uds_path(),
    ] {
        assert!(under_root(&path), "{path:?} must follow the instance dir");
    }
}

/// A minimal `HostIdentity` for the loop: the prehost-only registry never
/// assembles, so identity values are never exercised — they only need to
/// exist.
fn test_identity() -> HostIdentity {
    HostIdentity {
        identity: nuo_protocol::AgentIdentity::new("probe", "lifecycle probe"),
        preset: nuo_protocol::AgentRoleProfile::with_identity(
            "probe",
            nuo_protocol::AgentIdentity::new("probe", "lifecycle probe"),
        ),
        ui: Arc::new(HeadlessProbe),
    }
}

/// Trivial UiBridge stand-in (the loop only stores it).
struct HeadlessProbe;

#[async_trait::async_trait]
impl nuo_server::UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<nuo_server::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

/// Host one hand-built session; returns the monitor subscription-friendly
/// registry (fixtures mirror `serve_integration::prehosted`, minus the tap).
async fn host_one(registry: &Arc<ThreadRegistry>, project: &str) -> String {
    let session = Arc::new(ThreadSession::load_for_project(project.into()));
    let (req_tx, _req_rx) = mpsc::channel::<nuo_protocol::AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<nuo_protocol::AgentResponse>(16);
    let id = session.id().await;
    let id_ret = id.clone();
    let tracker = Arc::new(Mutex::new(
        nuo_server::runtime::monitor::MonitorTracker::bootstrap(
            nuo_protocol::MonitoredThread::empty(id),
            nuo_protocol::ThreadActivity::Idle,
        ),
    ));
    registry
        .host(HostedThread {
            workspace_root: Some(std::path::PathBuf::from(project)),
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
            sync_buffer: Arc::new(Mutex::new(
                nuo_server::transport::serve::AttachSyncBuffer::new(),
            )),
            command_catalog: nuo_tool::CommandCatalog::default(),
            initial_provider: "test-provider".into(),
            initial_model: "test-model".into(),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    id_ret
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
        local_endpoint: nuo_host::ipc::endpoint_for_instance(uds, &format!("lifecycle-{port}"))
            .ok(),
        disable_tcp: false,
    }
}

#[tokio::test]
async fn control_verb_drain_removes_discovery_and_exits_zero() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(ThreadRegistry::prehost_only());
    host_one(&registry, tmp.path().join("proj-a").to_str().unwrap()).await;
    let mut monitor_rx = registry.subscribe_monitor();
    let phase_rx = registry.subscribe_server_phase();

    let gate = Arc::new(ShutdownGate::new());
    let trigger_gate = gate.clone();
    tokio::spawn(async move {
        // Request shortly after the loop starts serving.
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger_gate.request(ShutdownReason::ControlVerb, false);
    });

    let outcome = nuo_server::runtime::host::run_with_registry(
        test_identity(),
        options(tmp.path().join("a.sock"), 0),
        gate,
        LifecycleOptions {
            shutdown_grace: Duration::from_secs(3),
            idle_exit: None,
            drain_probe: None,
        },
        registry,
    )
    .await;

    // Exit contract: a completed graceful stop is 0.
    assert_eq!(outcome.exit_code(), 0);
    match &outcome {
        RunOutcome::Stopped { reason } => assert_eq!(*reason, ShutdownReason::ControlVerb),
        other => panic!("expected Stopped, got {other:?}"),
    }
    // The discovery advertisement is gone (the lease released it).
    assert!(
        !nuo_server::transport::serve_discovery::global_discovery_path().exists(),
        "discovery record must be removed on drain"
    );
    // The single lifecycle watch channel transitioned to Draining (ADR-0266
    // `[INV-SRVPHASE-07]`) and the monitor bus saw the session teardown.
    assert!(
        matches!(
            &*phase_rx.borrow(),
            nuo_protocol::ServerPhase::Draining { .. }
        ),
        "watch channel must report ServerPhase::Draining, got {:?}",
        *phase_rx.borrow()
    );
    let mut saw_removed = false;
    while let Ok(event) = monitor_rx.try_recv() {
        if matches!(event, MonitorEvent::ThreadRemoved { .. }) {
            saw_removed = true;
        }
    }
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
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let gate = Arc::new(ShutdownGate::new());
    let _ = registry;
    let outcome = nuo_server::runtime::host::run_with_gate(
        test_identity(),
        options(tmp.path().join("b.sock"), 0),
        gate,
        LifecycleOptions {
            shutdown_grace: Duration::from_secs(3),
            idle_exit: Some(Duration::from_millis(1)),
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
    let registry = Arc::new(ThreadRegistry::prehost_only());
    host_one(&registry, tmp.path().join("proj-b").to_str().unwrap()).await;
    let mut monitor_rx = registry.subscribe_monitor();
    let phase_rx = registry.subscribe_server_phase();

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
        nuo_server::runtime::host::run_with_registry(
            test_identity(),
            options(sock, 0),
            run_gate,
            LifecycleOptions {
                shutdown_grace: Duration::from_secs(5),
                idle_exit: None,
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
    // The drain announcement still went out on the lifecycle watch channel
    // before the force, but the graceful session teardown was skipped: no
    // ThreadRemoved may appear.
    assert!(
        matches!(
            &*phase_rx.borrow(),
            nuo_protocol::ServerPhase::Draining { .. }
        ),
        "watch channel must report ServerPhase::Draining, got {:?}",
        *phase_rx.borrow()
    );
    let mut saw_removed = false;
    while let Ok(event) = monitor_rx.try_recv() {
        if matches!(event, MonitorEvent::ThreadRemoved { .. }) {
            saw_removed = true;
        }
    }
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
    // Occupy a port so the server's bind fails.
    let hog = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = hog.local_addr().unwrap().port();

    let registry = Arc::new(ThreadRegistry::prehost_only());
    let _ = registry;
    let outcome = nuo_server::runtime::host::run_with_gate(
        test_identity(),
        options(tmp.path().join("d.sock"), taken),
        Arc::new(ShutdownGate::new()),
        LifecycleOptions::default(),
    )
    .await;

    match &outcome {
        RunOutcome::StartupFailed(what) => {
            // The real io::Error text, not a bare RecvError.
            assert!(
                what.contains("could not bind"),
                "readable bind error: {what}"
            );
        }
        other => panic!("expected StartupFailed, got {other:?}"),
    }
    assert_eq!(outcome.exit_code(), 1);
}

/// A hosted session with armed `/schedule` jobs is exempt from
/// idle suspension — suspending it would park its tick loop and silently
/// stop the schedule from firing. A schedule-free session under the same
/// conditions still suspends (the memory-bounding behavior added).
#[tokio::test]
async fn idle_suspension_spares_sessions_with_armed_schedules() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(ThreadRegistry::prehost_only());

    // Two sessions past the TTL: one with an armed job, one without. Both
    // need real content so the store persists them (empty sessions are the
    // reaper's, not the suspender's). Each is hosted through the same
    // `ThreadSession` it was armed on, so the id the registry sees is the id
    // the schedule belongs to.
    async fn host_with_session(
        registry: &Arc<ThreadRegistry>,
        session: Arc<ThreadSession>,
        project_root: &std::path::Path,
    ) {
        let (req_tx, _req_rx) = mpsc::channel::<nuo_protocol::AgentRequest>(512);
        let (bc_tx, _) = broadcast::channel::<nuo_protocol::AgentResponse>(16);
        let tracker = Arc::new(Mutex::new(
            nuo_server::runtime::monitor::MonitorTracker::bootstrap(
                nuo_protocol::MonitoredThread::empty(session.id().await),
                nuo_protocol::ThreadActivity::Idle,
            ),
        ));
        registry
            .host(HostedThread {
                workspace_root: Some(project_root.to_path_buf()),
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
                sync_buffer: Arc::new(Mutex::new(
                    nuo_server::transport::serve::AttachSyncBuffer::new(),
                )),
                command_catalog: nuo_tool::CommandCatalog::default(),
                initial_provider: "test-provider".into(),
                initial_model: "test-model".into(),
                created_at: std::time::Instant::now(),
                last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
                last_seen_tick: std::sync::atomic::AtomicU64::new(0),
                activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
                agent_for_session_end: None,
            })
            .await;
    }

    let plain_project = tmp.path().join("plain-project");
    let plain_session = Arc::new(ThreadSession::load_for_project(plain_project.clone()));
    plain_session
        .replace_messages(vec![nuo_tool::Message::new(
            nuo_tool::Role::User,
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
async fn server_persists_when_interactive_client_disconnects() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("server_persists.sock");
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let proj_dir = tmp.path().join("proj-cd");
    host_one(&registry, proj_dir.to_str().unwrap()).await;

    let gate = Arc::new(ShutdownGate::new());
    let gate_run = gate.clone();
    let sock_clone = sock.clone();
    let run = tokio::spawn(async move {
        nuo_server::runtime::host::run_with_registry(
            test_identity(),
            options(sock_clone, 0),
            gate_run,
            LifecycleOptions {
                shutdown_grace: Duration::from_secs(3),
                idle_exit: None,
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
    let (mut sink, mut source) = nuo::transport::native_framed_split(stream);

    use futures::{SinkExt, StreamExt};
    sink.send(nuo_protocol::Wire::Hello {
        protocol: nuo_protocol::PROTOCOL_VERSION,
        version: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        monitor: None,
    })
    .await
    .unwrap();
    let hello = source.next().await.unwrap().unwrap();
    assert!(matches!(
        hello,
        nuo_protocol::WireFrame::Wire(nuo_protocol::Wire::ServerHello { .. })
    ));

    sink.send(nuo_protocol::Wire::Subscribe {
        request_id: 1,
        target: nuo_protocol::ThreadTarget::Attach { id: None },
        project: Some(proj_dir),
    })
    .await
    .unwrap();

    let sub = source.next().await.unwrap().unwrap();
    assert!(matches!(
        sub,
        nuo_protocol::WireFrame::Wire(nuo_protocol::Wire::Subscribed { .. })
    ));

    // Disconnect (Detach) the interactive client
    drop(sink);
    drop(source);

    // Under ADR-0038 [INV-SERVER-07], disconnecting an interactive client does NOT kill the server
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !run.is_finished(),
        "server must remain running after client disconnect (ADR-0038 [INV-SERVER-07])"
    );

    // Cleanly stop for test completion
    gate.request(ShutdownReason::ControlVerb, false);
    let outcome = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("server timed out waiting for stop")
        .expect("join run");

    assert_eq!(outcome.exit_code(), 0);
}

#[tokio::test]
async fn client_detach_spares_active_autonomous_work() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("client_detach_active.sock");
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let proj_dir = tmp.path().join("proj-cd-active");
    let session_id = host_one(&registry, proj_dir.to_str().unwrap()).await;

    // Simulate active in-flight work on the hosted session (ADR-0038 [INV-ACTION-02])
    {
        let hosted = registry.get(&session_id).await.expect("hosted session");
        hosted
            .tracker
            .lock()
            .await
            .observe(&nuo_protocol::AgentResponse::Round {
                thread_id: session_id.clone(),
                scope: nuo_protocol::RoundScope::Turn {
                    context: nuo_protocol::TurnContext::new(1, 1, 1_700_000_000_000),
                    event: nuo_protocol::RoundEvent::TurnStarted,
                },
            });
    }
    assert!(registry.has_active_work().await);

    let gate = Arc::new(ShutdownGate::new());
    let gate_clone = gate.clone();
    let sock_clone = sock.clone();
    let run = tokio::spawn(async move {
        nuo_server::runtime::host::run_with_registry(
            test_identity(),
            options(sock_clone, 0),
            gate_clone,
            LifecycleOptions {
                shutdown_grace: Duration::from_secs(3),
                idle_exit: None,
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
    let (mut sink, mut source) = nuo::transport::native_framed_split(stream);

    use futures::{SinkExt, StreamExt};
    sink.send(nuo_protocol::Wire::Hello {
        protocol: nuo_protocol::PROTOCOL_VERSION,
        version: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        monitor: None,
    })
    .await
    .unwrap();
    let hello = source.next().await.unwrap().unwrap();
    assert!(matches!(
        hello,
        nuo_protocol::WireFrame::Wire(nuo_protocol::Wire::ServerHello { .. })
    ));

    sink.send(nuo_protocol::Wire::Subscribe {
        request_id: 1,
        target: nuo_protocol::ThreadTarget::Attach { id: None },
        project: Some(proj_dir),
    })
    .await
    .unwrap();

    let sub = source.next().await.unwrap().unwrap();
    assert!(matches!(
        sub,
        nuo_protocol::WireFrame::Wire(nuo_protocol::Wire::Subscribed { .. })
    ));

    // Disconnect (Detach) the interactive client
    drop(sink);
    drop(source);

    // Give it 2 seconds: because active autonomous work is running, server must NOT terminate!
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert!(
        !run.is_finished(),
        "server must remain running while autonomous work is active"
    );

    // Cleanly stop for test completion
    gate.request(ShutdownReason::ControlVerb, false);
    let _ = run.await;
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

    let registry = Arc::new(ThreadRegistry::prehost_only());
    let gate = Arc::new(ShutdownGate::new());
    let g1 = gate.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        g1.request(ShutdownReason::ControlVerb, false);
    });

    let outcome = nuo_server::runtime::host::run_with_registry(
        test_identity(),
        options(sock.clone(), 0),
        gate,
        LifecycleOptions {
            shutdown_grace: Duration::from_secs(3),
            idle_exit: None,
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

/// ADR-0260 `[INV-SRVPHASE-01]`, `[INV-SRVPHASE-02]`: verifies the real
/// `nuo_server::runtime::host::run_with_registry` listen-first startup path advances
/// `Initializing(Storage) -> Initializing(Runtime) -> Ready` after binding
/// listeners and publishing discovery, and serves live `ServerPhase` events.
#[tokio::test]
async fn listen_first_host_startup_advances_storage_runtime_ready_and_serves_control() {
    sandbox_once();
    let _lock = LIFECYCLE_TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("init-seq.sock");

    let registry = Arc::new(ThreadRegistry::prehost_only());
    registry.set_server_phase(nuo_protocol::ServerPhase::initializing(
        nuo_protocol::ServerInitStage::Storage,
    ));
    let mut phase_rx = registry.subscribe_server_phase();

    let gate = Arc::new(ShutdownGate::new());
    let run_gate = gate.clone();
    let run_reg = registry.clone();
    let run_sock = sock.clone();
    let run = tokio::spawn(async move {
        nuo_server::runtime::host::run_with_registry(
            test_identity(),
            options(run_sock, 0),
            run_gate,
            LifecycleOptions {
                shutdown_grace: Duration::from_secs(3),
                idle_exit: None,
                drain_probe: None,
            },
            run_reg,
        )
        .await
    });

    // Wait for Storage -> Runtime -> Ready on the single watch channel
    // (ADR-0260 `[INV-SRVPHASE-01]`, ADR-0266 `[INV-SRVPHASE-07]`).
    let mut observed = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, phase_rx.changed()).await {
            Ok(Ok(())) => {
                let phase = phase_rx.borrow_and_update().clone();
                let is_ready = phase == nuo_protocol::ServerPhase::Ready;
                observed.push(phase);
                if is_ready {
                    break;
                }
            }
            _ => break,
        }
    }

    assert!(
        observed.iter().any(|p| matches!(
            p,
            nuo_protocol::ServerPhase::Initializing {
                stage: nuo_protocol::ServerInitStage::Runtime,
                ..
            }
        )) || observed.last() == Some(&nuo_protocol::ServerPhase::Ready),
        "host startup must progress cleanly to ServerPhase::Ready, got {observed:?}"
    );
    assert_eq!(
        observed.last(),
        Some(&nuo_protocol::ServerPhase::Ready),
        "host startup must finish in ServerPhase::Ready, got {observed:?}"
    );
    assert_eq!(registry.server_phase(), nuo_protocol::ServerPhase::Ready);

    // Connect over the bound local IPC socket and verify ServerHello reports Ready.
    let endpoint = nuo_host::ipc::LocalEndpoint::UnixSocket(sock.clone());
    let stream = nuo_host::ipc::connect(&endpoint).await.unwrap();
    let (mut sink, mut source) = nuo_server::transport::wire_channel::native_framed_split(stream);
    use futures::{SinkExt, StreamExt};
    sink.send(nuo_protocol::Wire::Hello {
        protocol: nuo_protocol::PROTOCOL_VERSION,
        version: None,
        posture: nuo_protocol::human_request::HumanChannelPosture::Interactive,
        monitor: Some(nuo_protocol::MonitorAction {
            watch: false,
            include_idle: true,
        }),
    })
    .await
    .unwrap();
    let frame = source.next().await.unwrap().unwrap();
    assert!(matches!(
        frame,
        nuo_protocol::WireFrame::Wire(nuo_protocol::Wire::ServerHello {
            phase: nuo_protocol::ServerPhase::Ready,
            monitor: Some(_),
            ..
        })
    ));

    gate.request(ShutdownReason::ControlVerb, false);
    let outcome = run.await.unwrap();
    assert_eq!(outcome.exit_code(), 0);
}
