//! Discovery, spawn, liveness, stop, and diagnostics.

use super::control::control_with_reply_with_timeout;
use super::*;

const LIVENESS_TIMEOUT: Duration = Duration::from_millis(500);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_POLL: Duration = Duration::from_millis(100);
/// How long [`discover`] waits for another client's in-flight spawn to publish
/// its discovery record, and how often it re-reads it. Ten 20 ms polls: the
/// record follows the listener binds immediately, so this is slack for
/// scheduling, not a timeout anyone should normally reach.
const DISCOVER_SPAWN_POLL: Duration = Duration::from_millis(20);
const DISCOVER_SPAWN_POLLS: usize = 10;
/// Attach first-frame liveness deadline.
///
/// `[INV-ATTACH-03]` (ADR-0133): the server acknowledges with `Welcome` before
/// any store load, harness build, skill scan, hook, or credential work. This is
/// therefore a liveness deadline, not a completion deadline — a timeout means
/// the server never spoke, not that a session is slow.
pub(super) const ATTACH_ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Control verbs may stop/restart a server, so they keep a wider budget.
pub(super) const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// Role-scoped history purge can delete thousands of rows and must wait for
/// each resident driver to unwind. It is a bounded server operation, but not a
/// 10-second one; the dedicated budget keeps a large legitimate purge from
/// looking like a dead peer.
const PURGE_REPLY_TIMEOUT: Duration = Duration::from_secs(600);
/// Stopping a scope waits for driver teardown and SessionEnd hooks.
const STOP_REPLY_TIMEOUT: Duration = Duration::from_secs(120);
/// The monitor stream's first snapshot is produced by a separate task.
pub(super) const MONITOR_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const FALLBACK_GRACE: Duration = Duration::from_secs(15);

pub const DEFAULT_SERVER_PORT: u16 = 9800;

pub fn env_default_port() -> u16 {
    std::env::var("NUO_PORT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_SERVER_PORT)
}

/// Discover a live local server, if one is running.
///
/// Async by construction: every wait is a Tokio timer and every liveness probe
/// is a Tokio socket. The synchronous predecessor parked its calling thread —
/// `std::thread::sleep` while another client spawned, plus blocking `connect`
/// calls to test liveness — so any caller inside a runtime stalled a worker
/// thread (the render loop included) for up to ~700 ms.
///
/// Fast path first: a valid discovery record answers in one `read`. Only when
/// no record is readable *and* the instance lock is held does this wait, and
/// then only as long as a spawn is plausibly in flight.
pub async fn discover(_project_root: &Path) -> Option<ServerInfo> {
    let global_path = discovery::global_discovery_path();
    if let Some(info) = discover_at(&global_path).await {
        return Some(info);
    }

    // The record is published *after* both listeners bind, so a held lock with
    // no record means a spawn is in flight: give it a bounded moment.
    let lock_path = discovery::global_lock_path();
    if nuo_host::lock::ProcessLock::is_locked(&lock_path) {
        for _ in 0..DISCOVER_SPAWN_POLLS {
            tokio::time::sleep(DISCOVER_SPAWN_POLL).await;
            if let Some(info) = discover_at(&global_path).await {
                return Some(info);
            }
        }
    }

    if nuo_host::lock::ProcessLock::is_locked(&lock_path)
        && let Some(pid) = nuo_host::lock::ProcessLock::probe_holder(&lock_path)
        && is_process_alive(pid)
    {
        let local_endpoint = discovery::default_local_endpoint().ok();
        let local_connectable = match local_endpoint.as_ref() {
            Some(endpoint) => nuo_host::ipc::probe_async(endpoint).await.connectable,
            None => false,
        };

        let default_port = env_default_port();
        let tcp_addr = std::net::SocketAddr::from(([127, 0, 0, 1], default_port));
        let tcp_connectable =
            tokio::time::timeout(LIVENESS_TIMEOUT, tokio::net::TcpStream::connect(tcp_addr))
                .await
                .is_ok_and(|result| result.is_ok());

        if local_connectable || tcp_connectable {
            let recovered = ServerInfo {
                pid,
                process_birth_token: nuo_host::process::process_identity(pid)
                    .ok()
                    .map(|identity| identity.birth_token),
                port: default_port,
                token: None,
                project_root: String::new(),
                started_at: 0,
                uds_path: None,
                local_endpoint,
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                grace_secs: None,
                protocol: Some(PROTOCOL_VERSION),
                image_digest: None,
                image_len: None,
            };
            tracing::info!(
                pid,
                "discover: recovered in-memory discovery record from live lock holder"
            );
            return Some(recovered);
        }
    }

    None
}

/// Read and validate the discovery record at `path`.
///
/// Async because liveness is proven by *connecting*, and the blocking form of
/// that parks the calling thread. This is the hot path — every client start and
/// every control verb comes through here — so a blocking probe cost a worker
/// thread up to [`LIVENESS_TIMEOUT`] per call.
pub(super) async fn discover_at(path: &Path) -> Option<ServerInfo> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let info: ServerInfo = serde_json::from_slice(&bytes).ok()?;
    if !server_process_matches(&info) {
        return None;
    }
    if !is_alive(&info).await {
        return None;
    }
    Some(info)
}

fn server_process_matches(info: &ServerInfo) -> bool {
    nuo_host::process::process_identity(info.pid).is_ok_and(|identity| {
        info.process_birth_token
            .is_none_or(|expected| expected == identity.birth_token)
    })
}

/// Whether the advertised server still answers.
///
/// A completed connect is the signal; the stream is dropped at once rather
/// than spoken to.
async fn is_alive(info: &ServerInfo) -> bool {
    if !server_process_matches(info) {
        return false;
    }
    match info.effective_local_endpoint() {
        Some(endpoint) if nuo_host::ipc::probe_async(&endpoint).await.connectable => true,
        _ => {
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], info.port));
            tokio::time::timeout(LIVENESS_TIMEOUT, tokio::net::TcpStream::connect(addr))
                .await
                .is_ok_and(|result| result.is_ok())
        }
    }
}

pub fn is_process_alive(pid: u32) -> bool {
    nuo_host::process::process_identity(pid).is_ok()
}

async fn wait_for_process_exit(
    identity: nuo_host::process::ProcessIdentity,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !nuo_host::process::process_is_alive(identity) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !nuo_host::process::process_is_alive(identity)
}

/// Whether a discovered server is a stale image that should be replaced rather
/// than reused.
///
/// The `same_version` guard this used to carry made the check unreachable for
/// exactly the case ADR-0136 was filed for: `nuo` on disk had been upgraded
/// while the old process kept running, so the versions *differed* and the drift
/// was therefore never detected — the client attached to a server built from a
/// binary that no longer exists on disk. A replaced image is drift whatever
/// version it reports; the protocol window only decides whether the record is
/// readable at all.
fn is_dev_drift(info: &ServerInfo) -> bool {
    let protocol_readable = info.protocol.is_none_or(protocol_accepts);
    protocol_readable && !server_image_is_current(info)
}

pub async fn reload(info: &ServerInfo) -> Result<(), String> {
    if !versions_compatible(info) {
        return Err(
            "server/client version mismatch; `server reload` needs a compatible server".into(),
        );
    }
    control(info, ControlRequest::Reload).await
}

pub async fn open_tcp_listener(
    info: &ServerInfo,
    port: Option<u16>,
) -> Result<ServerEndpointsReport, String> {
    let reply = control_with_reply(info, ControlRequest::OpenTcpListener { port }).await?;
    serde_json::from_str(&reply).map_err(|e| format!("invalid server endpoints reply: {e}"))
}

pub async fn close_tcp_listener(info: &ServerInfo) -> Result<ServerEndpointsReport, String> {
    let reply = control_with_reply(info, ControlRequest::CloseTcpListener).await?;
    serde_json::from_str(&reply).map_err(|e| format!("invalid server endpoints reply: {e}"))
}

pub async fn get_server_endpoints(info: &ServerInfo) -> Result<ServerEndpointsReport, String> {
    let reply = control_with_reply(info, ControlRequest::GetServerEndpoints).await?;
    serde_json::from_str(&reply).map_err(|e| format!("invalid server endpoints reply: {e}"))
}

/// ADR-0129 [INV-USAGE-01]: Read the cross-session model usage report over the
/// session-free control plane, independent of any active thread.
pub async fn get_usage_stats(
    info: &ServerInfo,
    event_cap: usize,
) -> Result<nuo_protocol::usage_stats::UsageStatsReport, String> {
    let reply = control_with_reply(info, ControlRequest::GetUsageStats { event_cap }).await?;
    serde_json::from_str(&reply).map_err(|e| format!("invalid usage stats reply: {e}"))
}

/// The role-scoped history listing: durable rows plus live status.
pub async fn list_threads(
    info: &ServerInfo,
    filter: nuo_protocol::ThreadFilter,
) -> Result<Vec<nuo_protocol::MonitoredThread>, String> {
    let reply = control_with_reply(info, ControlRequest::ListThreads { filter }).await?;
    serde_json::from_str(&reply).map_err(|e| format!("invalid thread listing reply: {e}"))
}

/// Stop hosted threads without deleting their history.
pub async fn stop_threads(
    info: &ServerInfo,
    filter: Option<nuo_protocol::ThreadFilter>,
    ids: Vec<String>,
) -> Result<nuo_protocol::ThreadRemovalReport, String> {
    let reply = control_with_reply_with_timeout(
        info,
        ControlRequest::StopThreads { filter, ids },
        STOP_REPLY_TIMEOUT,
    )
    .await?;
    serde_json::from_str(&reply).map_err(|e| format!("invalid stop report: {e}"))
}

/// Permanently delete durable thread history in the selected scope.
pub async fn purge_threads(
    info: &ServerInfo,
    filter: Option<nuo_protocol::ThreadFilter>,
    ids: Vec<String>,
) -> Result<nuo_protocol::ThreadRemovalReport, String> {
    let reply = control_with_reply_with_timeout(
        info,
        ControlRequest::PurgeThreads { filter, ids },
        PURGE_REPLY_TIMEOUT,
    )
    .await?;
    serde_json::from_str(&reply).map_err(|e| format!("invalid purge report: {e}"))
}

pub async fn stop(info: &ServerInfo) -> Result<(), String> {
    let grace = info
        .grace_secs
        .map(Duration::from_secs)
        .unwrap_or(FALLBACK_GRACE);
    let target_identity = nuo_host::process::process_identity(info.pid).ok();
    if let (Some(expected), Some(actual)) = (
        info.process_birth_token,
        target_identity.map(|identity| identity.birth_token),
    ) && expected != actual
    {
        return Err(format!(
            "refusing to stop pid {}: discovery process identity is stale",
            info.pid
        ));
    }
    let mut stopped = target_identity.is_none();

    if let Some(identity) = target_identity
        && versions_compatible(info)
        && let Ok(Ok(())) = tokio::time::timeout(
            Duration::from_millis(1500),
            control(info, ControlRequest::Shutdown),
        )
        .await
    {
        stopped = wait_for_process_exit(identity, grace).await;
    }

    if !stopped {
        if let Some(identity) = target_identity {
            if nuo_host::process::request_termination(identity).is_ok() {
                stopped = wait_for_process_exit(identity, grace).await;
            }
            if !stopped && nuo_host::process::process_is_alive(identity) {
                let _ = nuo_host::process::force_terminate(identity);
                stopped = wait_for_process_exit(identity, Duration::from_millis(1000)).await;
            }
        } else {
            stopped = true;
        }
    }

    let gone = stopped
        || target_identity.is_none_or(|identity| !nuo_host::process::process_is_alive(identity));

    if gone
        && let Ok(_cleanup_lock) =
            nuo_host::lock::ProcessLock::acquire(&discovery::global_lock_path())
    {
        discovery::remove_if_matching_process(
            &discovery::global_discovery_path(),
            info.pid,
            info.process_birth_token,
        );
    }
    if gone {
        Ok(())
    } else {
        Err(format!("could not stop server (pid {})", info.pid))
    }
}

pub async fn stop_server(info: &ServerInfo) -> Result<(), String> {
    stop(info).await
}

pub fn startup_log_path() -> PathBuf {
    let dirs = nuo_host::paths::get();
    dirs.state_dir.join("log").join("server-startup.log")
}

pub async fn ensure_server(project_root: &Path) -> Result<ServerInfo, String> {
    ensure_server_explicit(project_root, None).await
}

pub async fn ensure_server_explicit(
    project_root: &Path,
    explicit_port: Option<u16>,
) -> Result<ServerInfo, String> {
    if let Some(info) = discover(project_root).await {
        if versions_compatible(&info) && !is_dev_drift(&info) {
            return Ok(info);
        }
        tracing::info!(
            pid = info.pid,
            "ensure_server: replacing existing server on version drift or rebuild"
        );
        let _ = stop(&info).await;
    }

    let spawn_lock_path = discovery::global_spawn_lock_path();
    let _spawn_guard = match nuo_host::lock::ProcessLock::acquire(&spawn_lock_path) {
        Ok(guard) => Some(guard),
        Err(_) => {
            // Another client is spawning. Poll for its discovery record, but
            // keep retrying the lock: abandon the wait the moment the holder
            // releases it, instead of burning the full 6 s budget even after
            // the spawning client has finished or died.
            let wait_deadline = tokio::time::Instant::now() + Duration::from_secs(6);
            let mut acquired = None;
            while tokio::time::Instant::now() < wait_deadline {
                tokio::time::sleep(SERVER_START_POLL).await;
                if let Some(info) = discover(project_root).await {
                    if versions_compatible(&info) && !is_dev_drift(&info) {
                        return Ok(info);
                    }
                }
                if let Ok(guard) = nuo_host::lock::ProcessLock::acquire(&spawn_lock_path) {
                    acquired = Some(guard);
                    break;
                }
            }
            acquired
        }
    };

    if let Some(info) = discover(project_root).await {
        if versions_compatible(&info) && !is_dev_drift(&info) {
            return Ok(info);
        }
        let _ = stop(&info).await;
    }

    let lock_path = discovery::global_lock_path();
    let db_lock_path = nuo_host::paths::get()
        .db_file()
        .with_extension("db.owner.lock");
    let mut candidate_pids = std::collections::HashSet::new();
    if nuo_host::lock::ProcessLock::is_locked(&lock_path)
        && let Some(holder_pid) = nuo_host::lock::ProcessLock::probe_holder(&lock_path)
        && is_process_alive(holder_pid)
    {
        candidate_pids.insert(holder_pid);
    }
    if nuo_host::lock::ProcessLock::is_locked(&db_lock_path)
        && let Some(holder_pid) = nuo_host::lock::ProcessLock::probe_holder(&db_lock_path)
        && is_process_alive(holder_pid)
    {
        candidate_pids.insert(holder_pid);
    }
    for holder_pid in candidate_pids {
        tracing::info!(
            holder_pid,
            "ensure_server: replacing conflicting server holding instance or database lock"
        );
        let _ = nuo_host::process::takeover_pid(
            holder_pid,
            nuo_host::process::TakeoverOptions::default(),
        )
        .await;
    }

    let mut child = spawn_server(explicit_port)?;
    let deadline = std::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Some(info) = discover(project_root).await {
            if versions_compatible(&info) {
                return Ok(info);
            } else {
                return Err(incompatibility_error(&info));
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "nuo server exited prematurely ({status}): {log_trimmed}"
                ));
            } else {
                return Err(format!("nuo server exited prematurely with {status}"));
            }
        }
        if std::time::Instant::now() > deadline {
            return Err("timed out waiting for nuo server to start".to_string());
        }
    }
}

fn spawn_server(explicit_port: Option<u16>) -> Result<tokio::process::Child, String> {
    let program = server_program();
    let mut command = tokio::process::Command::new(&program);
    command.arg("server").arg("start").arg("--fg");
    if let Some(port) = explicit_port {
        command.arg("--port").arg(port.to_string());
    }

    nuo_host::process::configure_server(&mut command);

    let log_path = startup_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stderr_sink = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| format!("cannot open startup log {}: {e}", log_path.display()))?;

    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(stderr_sink);

    command.spawn().map_err(|e| {
        format!(
            "failed to spawn {} server start --fg: {e}",
            program.display()
        )
    })
}

fn server_program() -> PathBuf {
    if let Some(program) = std::env::var_os("NUO_BIN").filter(|value| !value.is_empty()) {
        return PathBuf::from(program);
    }
    if let Ok(current) = std::env::current_exe() {
        let sibling_nuo = current.with_file_name(format!("nuo{}", std::env::consts::EXE_SUFFIX));
        if sibling_nuo.is_file() {
            return sibling_nuo;
        }
    }
    PathBuf::from(format!("nuo{}", std::env::consts::EXE_SUFFIX))
}

pub fn configure_server_detachment(command: &mut std::process::Command) {
    nuo_host::process::configure_server_std(command);
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerDiagnostics {
    pub instance_dir: PathBuf,
    pub default_port: u16,
    pub discovery_path: PathBuf,
    pub discovery_record: Option<ServerInfo>,
    pub discovery_valid: bool,
    pub lock_path: PathBuf,
    pub lock_held: bool,
    pub lock_holder_pid: Option<u32>,
    pub lock_holder_alive: bool,
    pub local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
    pub local_endpoint_exists: bool,
    pub local_endpoint_connectable: bool,
    pub tcp_port: u16,
    pub tcp_listening: bool,
    pub startup_log_path: PathBuf,
    pub last_startup_log: Option<String>,
    pub installed_image: Option<PathBuf>,
    pub installed_image_digest: Option<String>,
    pub server_image_digest: Option<String>,
    pub server_image_current: bool,
    pub recent_events: Vec<nuo_host::diagnostics::DiagnosticEvent>,
}

pub async fn diagnose_server() -> ServerDiagnostics {
    let discovery_path = discovery::global_discovery_path();
    let discovery_record = discover_at(&discovery_path).await;
    let raw_record: Option<ServerInfo> = std::fs::read(&discovery_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let lock_path = discovery::global_lock_path();
    let lock_held = nuo_host::lock::ProcessLock::is_locked(&lock_path);
    let lock_holder_pid = nuo_host::lock::ProcessLock::probe_holder(&lock_path);
    let lock_holder_alive = lock_holder_pid.map(is_process_alive).unwrap_or(false);

    let local_endpoint = raw_record
        .as_ref()
        .and_then(discovery::Discovery::effective_local_endpoint)
        .or_else(|| discovery::default_local_endpoint().ok());
    let local_probe = local_endpoint
        .as_ref()
        .map(nuo_host::ipc::probe)
        .unwrap_or_default();

    let default_port = env_default_port();
    let port = discovery_record
        .as_ref()
        .map(|d| d.port)
        .or_else(|| raw_record.as_ref().map(|d| d.port))
        .unwrap_or(default_port);

    let tcp_addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let tcp_listening =
        std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(200)).is_ok();

    let startup_log = startup_log_path();
    let last_startup_log = std::fs::read_to_string(&startup_log)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let installed_image = server_program();
    let installed_image = installed_image.is_file().then_some(installed_image);
    let installed_image_digest = installed_image
        .as_ref()
        .and_then(|p| nuo_host::process::image_digest_len(p).map(|(_, d)| d));
    let discovery_record = discovery_record.or(raw_record);
    let server_image_digest = discovery_record
        .as_ref()
        .and_then(|r| r.image_digest.clone());
    let server_image_current = discovery_record
        .as_ref()
        .is_none_or(server_image_is_current);

    ServerDiagnostics {
        instance_dir: discovery::instance_dir(),
        default_port,
        discovery_path,
        discovery_record,
        discovery_valid: discover_at(&discovery::global_discovery_path())
            .await
            .is_some(),
        lock_path,
        lock_held,
        lock_holder_pid,
        lock_holder_alive,
        local_endpoint,
        local_endpoint_exists: local_probe.exists,
        local_endpoint_connectable: local_probe.connectable,
        tcp_port: port,
        tcp_listening,
        startup_log_path: startup_log,
        last_startup_log,
        installed_image,
        installed_image_digest,
        server_image_digest,
        server_image_current,
        recent_events: nuo_host::diagnostics::recent_events(8),
    }
}
