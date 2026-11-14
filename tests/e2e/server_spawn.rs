//! End-to-end server spawn coverage owned by the CLI package so Cargo supplies
//! the exact freshly-built `nuo` binary provided by the nextest E2E setup.
//! This must never discover an incidental or stale `target/debug/nuo`.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nuo::runtime::monitor::MonitorTracker;
use nuo::runtime::registry::{HostedThread, ThreadRegistry};
use nuo_persistence::thread::ThreadSession;
use tokio::sync::{Mutex, broadcast, mpsc};

fn nuo_binary() -> PathBuf {
    nuo_test_support::binary_path().expect("the nextest e2e profile setup must export NUO_TEST_BIN")
}

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
    let cli = nuo_binary();

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
            && let Ok(record) = serde_json::from_slice::<nuo_server::transport::serve_discovery::Discovery>(&bytes)
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
    let session = Arc::new(ThreadSession::for_workspace(
        Some(nuo_protocol::WorkspaceBinding::new(project)),
        Some("developer".to_string()),
    ));
    host_thread(registry, session, project, "developer").await
}

async fn host_thread(
    registry: &ThreadRegistry,
    session: Arc<ThreadSession>,
    project: &Path,
    role: &str,
) -> String {
    let id = session.id().await;
    let (req_tx, _req_rx) = mpsc::channel::<nuo_protocol::AgentRequest>(64);
    let (events, _) = broadcast::channel::<nuo_protocol::AgentResponse>(16);
    let mut base = nuo_protocol::MonitoredThread::empty(id.clone());
    base.project_root = project.display().to_string();
    base.role = role.to_string();
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        base,
        nuo_protocol::ThreadActivity::Idle,
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
            driver_done: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            tracker,
            sync_buffer: Arc::new(Mutex::new(nuo_server::transport::serve::AttachSyncBuffer::new())),
            command_catalog: nuo_tool::CommandCatalog::default(),
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
    for _ in 0..3 {
        host_idle_thread(&registry, &project).await;
    }
    assert_eq!(registry.session_count().await, 3);

    let mut handle =
        nuo_server::transport::serve::start_server(nuo_server::transport::serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Publish the record the client discovers. The pid is this test process:
    // it is the process actually serving `port` from the in-process server.
    let instance_dir = home.join("nuo").join("instance");
    std::fs::create_dir_all(&instance_dir).unwrap();
    let record = nuo_server::transport::serve_discovery::Discovery {
        pid: std::process::id(),
        port,
        token: None,
        project_root: project.display().to_string(),
        started_at: 0,
        version: Some(nuo_server::transport::serve::server_version().to_string()),
        protocol: Some(nuo_protocol::PROTOCOL_VERSION),
        ..Default::default()
    };
    std::fs::write(
        instance_dir.join("server.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let cli = nuo_binary();
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
        String::from_utf8_lossy(&output.stdout).contains("Removed 3 thread(s)"),
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
        String::from_utf8_lossy(&output.stderr).contains("no thread history to remove"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// ADR-0170/0171: `ls` and `rm --all` are role-scoped and `rm` deletes
/// durable history, not just the resident driver.
#[tokio::test]
async fn thread_ls_and_rm_are_role_scoped_and_durable() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join(".nuo")).unwrap();
    std::fs::write(
        project.join(".nuo").join("roles.toml"),
        "[roles.coder]\nname = \"Coder\"\nworkspace = \"inherit\"\n",
    )
    .unwrap();

    let dev_store = ThreadSession::for_workspace(
        Some(nuo_protocol::WorkspaceBinding::new(&project)),
        Some("developer".to_string()),
    );
    let dev_id = dev_store.id().await;
    dev_store
        .replace_messages(vec![nuo_tool::Message::new(
            nuo_tool::Role::User,
            "developer history",
        )])
        .await
        .unwrap();

    let coder_store = ThreadSession::for_workspace(
        Some(nuo_protocol::WorkspaceBinding::new(&project)),
        Some("coder".to_string()),
    );
    let coder_id = coder_store.id().await;
    coder_store
        .replace_messages(vec![nuo_tool::Message::new(
            nuo_tool::Role::User,
            "coder history",
        )])
        .await
        .unwrap();

    let registry = Arc::new(ThreadRegistry::prehost_only());
    let mut handle = nuo_server::transport::serve::start_server(nuo_server::transport::serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let instance_dir = home.join("nuo").join("instance");
    std::fs::create_dir_all(&instance_dir).unwrap();
    let record = nuo_server::transport::serve_discovery::Discovery {
        pid: std::process::id(),
        port,
        token: None,
        project_root: project.display().to_string(),
        started_at: 0,
        version: Some(nuo_server::transport::serve::server_version().to_string()),
        protocol: Some(nuo_protocol::PROTOCOL_VERSION),
        ..Default::default()
    };
    std::fs::write(
        instance_dir.join("server.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let cli = nuo_binary();
    let self_exe = std::env::current_exe().unwrap();

    // Default role is developer: only the developer thread is listed.
    let output = run_cli(
        &cli,
        &self_exe,
        &home,
        &project,
        &["thread", "ls", "--json"],
        Stdio::null(),
    )
    .await;
    assert!(output.status.success());
    let rows: Vec<nuo_protocol::MonitoredThread> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 1, "rows: {rows:?}");
    assert_eq!(rows[0].id, dev_id);
    assert_eq!(rows[0].role, "developer");

    // Explicit role expansion shows both role histories in the same workspace.
    let output = run_cli(
        &cli,
        &self_exe,
        &home,
        &project,
        &["thread", "ls", "--all-roles", "--json"],
        Stdio::null(),
    )
    .await;
    assert!(output.status.success());
    let all_roles: Vec<nuo_protocol::MonitoredThread> =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(all_roles.len(), 2, "rows: {all_roles:?}");

    // Developer purge must not touch the coder thread in the same workspace.
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
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let handle = nuo_persistence::db::get_persistence_handle();
    let reader = handle.reader().unwrap();
    assert!(
        reader.get_session(&dev_id).unwrap().is_none(),
        "developer thread must be durably deleted"
    );
    assert!(
        reader.get_session(&coder_id).unwrap().is_some(),
        "coder thread must survive a developer-scoped purge"
    );
    drop(reader);

    // An explicit `--role coder` selects the other scope.
    let output = run_cli(
        &cli,
        &self_exe,
        &home,
        &project,
        &["thread", "rm", "--all", "--role", "coder", "--force"],
        Stdio::null(),
    )
    .await;
    assert!(output.status.success());
    let reader = handle.reader().unwrap();
    assert!(
        reader.get_session(&coder_id).unwrap().is_none(),
        "coder thread must be deleted in its own scope"
    );
}

/// ADR-0171: `stop` is runtime-only; `rm` is the durable delete. The same
/// thread survives the first and disappears after the second.
#[tokio::test]
async fn thread_stop_keeps_history_but_rm_deletes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let store = Arc::new(ThreadSession::for_workspace(
        Some(nuo_protocol::WorkspaceBinding::new(&project)),
        Some("developer".to_string()),
    ));
    store
        .replace_messages(vec![nuo_tool::Message::new(
            nuo_tool::Role::User,
            "keep me across stop",
        )])
        .await
        .unwrap();

    let registry = Arc::new(ThreadRegistry::prehost_only());
    let id = host_thread(&registry, store, &project, "developer").await;
    assert_eq!(registry.session_count().await, 1);

    let mut handle =
        nuo_server::transport::serve::start_server(nuo_server::transport::serve::ServeOptions::default(), registry.clone());
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let instance_dir = home.join("nuo").join("instance");
    std::fs::create_dir_all(&instance_dir).unwrap();
    let record = nuo_server::transport::serve_discovery::Discovery {
        pid: std::process::id(),
        port,
        token: None,
        project_root: project.display().to_string(),
        started_at: 0,
        version: Some(nuo_server::transport::serve::server_version().to_string()),
        protocol: Some(nuo_protocol::PROTOCOL_VERSION),
        ..Default::default()
    };
    std::fs::write(
        instance_dir.join("server.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let cli = nuo_binary();
    let self_exe = std::env::current_exe().unwrap();

    let output = run_cli(
        &cli,
        &self_exe,
        &home,
        &project,
        &["thread", "stop", "--all", "--force"],
        Stdio::null(),
    )
    .await;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(registry.session_count().await, 0, "driver must be stopped");
    let reader = nuo_persistence::db::get_persistence_handle()
        .reader()
        .unwrap();
    assert!(
        reader.get_session(&id).unwrap().is_some(),
        "stop must retain durable history"
    );
    drop(reader);

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
    let reader = nuo_persistence::db::get_persistence_handle()
        .reader()
        .unwrap();
    assert!(
        reader.get_session(&id).unwrap().is_none(),
        "rm must delete durable history"
    );
}
