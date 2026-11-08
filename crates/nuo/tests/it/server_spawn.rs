//! End-to-end server spawn coverage owned by the CLI package so Cargo supplies
//! the exact freshly-built `nuo` binary through `CARGO_BIN_EXE_nuo`.
//! This must never discover an incidental or stale `target/debug/nuo`.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nuo::monitor::MonitorTracker;
use nuo::registry::{HostedThread, ThreadRegistry};
use nuo_persistence::session::SessionStore;
use tokio::sync::{Mutex, broadcast, mpsc};

struct ServerCleanup {
    cli: PathBuf,
    root: PathBuf,
    pid: u32,
    reaper: Option<std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>>,
}

impl ServerCleanup {
    fn stop(&mut self) -> Output {
        let output = Command::new(&self.cli)
            .args(["server", "stop"])
            .env("NUO_HOME", &self.root)
            .stdin(Stdio::null())
            .output()
            .expect("run server stop in the sandbox");
        if !output.status.success() {
            self.kill_process_group();
        }
        self.join_reaper();
        output
    }

    fn kill_process_group(&self) {
        // SAFETY: this test spawned `pid` through the production `setsid`
        // helper, so `-pid` targets only the sandboxed server's process group.
        let _ = unsafe { libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL) };
    }

    fn join_reaper(&mut self) {
        if let Some(reaper) = self.reaper.take() {
            reaper
                .join()
                .expect("reap thread did not panic")
                .expect("wait for sandboxed server");
        }
    }
}

impl Drop for ServerCleanup {
    fn drop(&mut self) {
        if self.reaper.is_some() {
            let _ = Command::new(&self.cli)
                .args(["server", "stop"])
                .env("NUO_HOME", &self.root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            self.kill_process_group();
            self.join_reaper();
        }
    }
}

fn discovery_path(root: &Path) -> PathBuf {
    root.join("nuo").join("instance").join("server.json")
}

/// Inheritance invariant plus detachment invariant:
/// a real client binary carrying `NUO_HOME` starts this exact build in a
/// fresh Unix session and keeps every server artifact inside the sandbox.
#[tokio::test]
async fn spawned_server_inherits_the_nuo_home_sandbox() {
    let own = tempfile::tempdir().unwrap();
    let own_root = own.path().to_path_buf();
    let cli = PathBuf::from(env!("CARGO_BIN_EXE_nuo"));

    let mut command = Command::new(&cli);
    command.args(["server", "start", "--fg"]);
    command
        .env("NUO_HOME", &own_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir("/");
    nuo::transport::configure_server_detachment(&mut command);
    let child = command.spawn().expect("spawn the sandboxed server");
    let server_pid = child.id();

    // Reap continuously: `server stop` checks liveness, and an unreaped child
    // would remain a zombie that still answers the process-existence probe.
    let mut reaper_child = child;
    let reaper = std::thread::spawn(move || reaper_child.wait());
    let mut cleanup = ServerCleanup {
        cli,
        root: own_root.clone(),
        pid: server_pid,
        reaper: Some(reaper),
    };

    let own_record = discovery_path(&own_root);
    let deadline = Instant::now() + Duration::from_secs(15);
    let record = loop {
        if let Ok(bytes) = std::fs::read(&own_record)
            && let Ok(record) = serde_json::from_slice::<nuo::serve_discovery::Discovery>(&bytes)
        {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "server never advertised inside its sandbox"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    assert_eq!(record.pid, server_pid, "sandbox record must name the child");
    assert_eq!(
        record.version.as_deref(),
        Some(env!("CARGO_PKG_VERSION")),
        "Cargo must execute the freshly-built CLI, never a stale target artifact"
    );

    // `setsid(2)` makes the server both session and process-group leader.
    // SAFETY: `server_pid` names the live child owned by this test.
    assert_eq!(
        unsafe { libc::getsid(server_pid as libc::pid_t) },
        server_pid as libc::pid_t
    );
    // SAFETY: `server_pid` names the live child owned by this test.
    assert_eq!(
        unsafe { libc::getpgid(server_pid as libc::pid_t) },
        server_pid as libc::pid_t
    );

    let stop_output = cleanup.stop();
    assert!(
        stop_output.status.success(),
        "server stop must succeed: {} (stderr: {})",
        stop_output.status,
        String::from_utf8_lossy(&stop_output.stderr)
    );
    assert!(!own_record.exists(), "drained server must remove discovery");
}

async fn host_idle_thread(registry: &ThreadRegistry, project: &Path) -> String {
    let session = Arc::new(SessionStore::for_path(
        project.join("sessions").join("session.json"),
    ));
    let id = session.id().await;
    let (req_tx, _req_rx) = mpsc::channel::<nuo_protocol::AgentRequest>(64);
    let (events, _) = broadcast::channel::<nuo_protocol::AgentResponse>(16);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        nuo_protocol::MonitoredThread::empty(id.clone()),
        nuo_protocol::ThreadStatus::Idle,
    )));
    registry
        .host(HostedThread {
            workspace_root: Some(project.to_path_buf()),
            human_channel: Arc::new(nuo_protocol::human_request::HumanChannelAccountant::new()),
            security: Arc::new(nuo_persistence::workspace_security::WorkspaceSecurityStore::load()),
            session,
            shared_confinement: nuo_protocol::SharedConfinement::default(),
            req_tx,
            events,
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer: Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new())),
            command_catalog: nuo_protocol::CommandCatalog::default(),
            initial_provider: "test-provider".into(),
            initial_model: "test-model".into(),
            created_at: Instant::now(),
            last_activity: Mutex::new(Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    id
}

async fn run_cli(
    cli: &Path,
    self_exe: &Path,
    home: &Path,
    cwd: &Path,
    args: &[&str],
    stdin: Stdio,
) -> Output {
    // `NUO_BIN` names the image the client expects the server to be running.
    // Here the "server" is this test process, so the expectation must be the
    // test binary itself — otherwise the ADR-0136 image check refuses the pair.
    tokio::process::Command::new(cli)
        .args(args)
        .env("NUO_HOME", home)
        .env("NUO_BIN", self_exe)
        .current_dir(cwd)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("run the nuo CLI")
}

/// ADR-0161: `thread rm --all` is a client-side orchestration over the existing
/// kill contract. Driven through the real binary, it must enumerate a live
/// server's snapshot, refuse without affirmative consent, then kill every row.
#[tokio::test]
async fn thread_rm_all_purges_every_hosted_thread_through_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let registry = Arc::new(ThreadRegistry::prehost_only());
    for index in 0..3 {
        host_idle_thread(&registry, &project.join(format!("p{index}"))).await;
    }
    assert_eq!(registry.session_count().await, 3);

    let mut handle =
        nuo::serve::start_server(nuo::serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Publish the record the client discovers. The pid is this test process:
    // it is the process actually serving `port` from the in-process server.
    let instance_dir = home.join("nuo").join("instance");
    std::fs::create_dir_all(&instance_dir).unwrap();
    let record = nuo::serve_discovery::Discovery {
        pid: std::process::id(),
        port,
        token: None,
        project_root: project.display().to_string(),
        started_at: 0,
        version: Some(nuo::serve::server_version().to_string()),
        protocol: Some(nuo_protocol::PROTOCOL_VERSION),
        ..Default::default()
    };
    std::fs::write(
        instance_dir.join("server.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let cli = PathBuf::from(env!("CARGO_BIN_EXE_nuo"));
    let self_exe = std::env::current_exe().unwrap();

    // Destructive bulk action defaults to "no": an unattended stdin removes nothing.
    let output = run_cli(
        &cli,
        &self_exe,
        &home,
        &project,
        &["thread", "rm", "--all"],
        Stdio::null(),
    )
    .await;
    assert!(output.status.success(), "abort must exit cleanly");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("aborting purge"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(registry.session_count().await, 3);

    // `--force` is the explicit affirmative: every snapshot row is killed.
    let output = run_cli(
        &cli,
        &self_exe,
        &home,
        &project,
        &["thread", "rm", "--all", "--force"],
        Stdio::null(),
    )
    .await;
    assert!(
        output.status.success(),
        "purge failed: {} (stderr: {})",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Removed all 3 threads."),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(registry.session_count().await, 0);
    let snapshot = registry
        .monitor_snapshot(nuo_protocol::MonitorAction {
            watch: false,
            include_idle: true,
        })
        .await;
    assert!(snapshot.threads.is_empty(), "thread table must be emptied");

    // Purging an already-empty server is a clean no-op, not an error.
    let output = run_cli(
        &cli,
        &self_exe,
        &home,
        &project,
        &["thread", "rm", "--all", "--force"],
        Stdio::null(),
    )
    .await;
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no threads to remove"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
