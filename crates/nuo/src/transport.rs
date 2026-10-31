//! Client transport, server connection, lifecycle discovery, and control channel for Nuo.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{Sink, SinkExt, Stream, StreamExt, TryStreamExt};
use nuo_protocol::{
    AgentRequest, AgentResponse, MonitorAction, MonitorEvent, MonitoredTask, MonitoredThread,
    ThreadOverview, WireFrame,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_util::codec::Framed;

#[cfg(unix)]
pub use nuo_protocol::discovery::default_uds_path;
pub use nuo_protocol::discovery::{
    self, Discovery, ServerInfo, default_local_endpoint, global_discovery_path, global_lock_path,
    global_spawn_lock_path, instance_dir,
};
pub use nuo_protocol::version::{
    ERR_PROTOCOL_MISMATCH, ERR_VERSION_MISMATCH, VersionRelation, compare_versions,
    incompatibility_error, protocol_accepts, protocol_mismatch, server_image_is_current,
    version_mismatch, versions_compatible,
};
pub use nuo_protocol::{
    AttachAction, ControlRequest, MIN_PROTOCOL_VERSION, NativeWireCodec, PROTOCOL_VERSION,
    ServerEndpointsReport, ThreadInitOptions, Wire,
};
pub type SessionInitOptions = ThreadInitOptions;

/// Initialize client logging guard.
pub fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let level = std::env::var("NUO_LOG").unwrap_or_else(|_| String::from("info"));
    if level.eq_ignore_ascii_case("off") {
        return None;
    }
    let dir = nuo_host::paths::get().log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let file_appender = tracing_appender::rolling::daily(&dir, "client.log");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
    Some(guard)
}

pub type BoxWireSink = Pin<Box<dyn Sink<Wire, Error = io::Error> + Send>>;
pub type BoxWireStream = Pin<Box<dyn Stream<Item = Result<WireFrame, io::Error>> + Send>>;

pub fn native_framed_split<T>(stream: T) -> (BoxWireSink, BoxWireStream)
where
    T: AsyncRead + AsyncWrite + Send + 'static,
{
    let framed = Framed::new(stream, NativeWireCodec);
    let (sink, stream) = framed.split();
    (Box::pin(sink), Box::pin(stream))
}

pub fn websocket_split<T>(ws_stream: T) -> (BoxWireSink, BoxWireStream)
where
    T: Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Send
        + 'static,
{
    let (ws_sink, ws_source) = ws_stream.split();

    let sink = ws_sink
        .sink_map_err(|e| io::Error::new(io::ErrorKind::ConnectionReset, e))
        .with(|wire: Wire| async move {
            let json = serde_json::to_string(&wire).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Failed to serialize Wire to JSON: {e}"),
                )
            })?;
            Ok(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        });

    let stream = ws_source
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionReset, e))
        .filter_map(|msg_res| async move {
            match msg_res {
                // A WebSocket text frame is self-delimiting, so an undecodable
                // payload is reported per frame and the socket stays usable
                // [ADR-0136]. `Err` remains reserved for the socket itself.
                Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                    Some(Ok(nuo_protocol::decode_wire_payload(text.as_bytes())))
                }
                Ok(tokio_tungstenite::tungstenite::Message::Binary(bin)) => {
                    Some(Ok(nuo_protocol::decode_wire_payload(&bin)))
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => None,
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            }
        });

    (Box::pin(sink), Box::pin(stream))
}

static POSTURE_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_posture(posture: nuo_protocol::human_request::HumanChannelPosture) {
    let code = match posture {
        nuo_protocol::human_request::HumanChannelPosture::Interactive => 0,
        nuo_protocol::human_request::HumanChannelPosture::Autonomous => 1,
    };
    POSTURE_OVERRIDE.store(code, std::sync::atomic::Ordering::Relaxed);
}

fn current_posture() -> nuo_protocol::human_request::HumanChannelPosture {
    match POSTURE_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => nuo_protocol::human_request::HumanChannelPosture::Autonomous,
        _ => nuo_protocol::human_request::HumanChannelPosture::Interactive,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteServer {
    pub host: String,
    pub port: u16,
    pub token: String,
    pub secure: bool,
}

#[derive(Debug, Clone)]
pub enum ClientTarget {
    Local(ServerInfo),
    Remote(RemoteServer),
}

impl ClientTarget {
    pub async fn connect(&self, action: AttachAction) -> AttachResult {
        match self {
            Self::Local(info) => connect(info, action).await,
            Self::Remote(remote) => remote.connect(action).await,
        }
    }

    pub fn endpoint_display(&self) -> String {
        match self {
            Self::Local(info) => {
                if info.port > 0 {
                    format!("127.0.0.1:{}", info.port)
                } else {
                    "local UDS".to_string()
                }
            }
            Self::Remote(remote) => remote.endpoint_display(),
        }
    }
}

impl RemoteServer {
    pub fn url(&self) -> String {
        let scheme = if self.secure { "wss" } else { "ws" };
        format!("{scheme}://{}:{}/", self.host, self.port)
    }

    pub fn endpoint_display(&self) -> String {
        let scheme = if self.secure { "wss" } else { "ws" };
        if (self.secure && self.port == 443) || (!self.secure && self.port == 80) {
            format!("{scheme}://{}", self.host)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }

    pub fn parse(addr: &str, token: Option<String>) -> Result<Self, String> {
        let trimmed = addr.trim();
        if trimmed.is_empty() {
            return Err("empty remote address".to_string());
        }

        let (without_scheme, secure, scheme_default_port) =
            if let Some(rest) = trimmed.strip_prefix("wss://") {
                (rest, true, Some(443))
            } else if let Some(rest) = trimmed.strip_prefix("https://") {
                (rest, true, Some(443))
            } else if let Some(rest) = trimmed.strip_prefix("ws://") {
                (rest, false, Some(80))
            } else if let Some(rest) = trimmed.strip_prefix("http://") {
                (rest, false, Some(80))
            } else {
                (trimmed, false, None)
            };

        let authority = without_scheme
            .split('/')
            .next()
            .unwrap_or(without_scheme)
            .trim();
        if authority.is_empty() {
            return Err(format!("invalid remote address '{addr}': missing host"));
        }

        let (host, port) = match split_authority(authority)? {
            (host, Some(port)) => (host, port),
            (Some(host), None) => {
                if let Some(def_port) = scheme_default_port {
                    (Some(host), def_port)
                } else {
                    return Err(format!(
                        "'{addr}' is not host:port (must specify port or explicit scheme like wss://)"
                    ));
                }
            }
            (None, None) => {
                return Err(format!("'{addr}' is missing host and port"));
            }
        };

        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());

        let token = token.filter(|t| !t.trim().is_empty()).or_else(|| {
            std::env::var("NUO_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty())
        });

        let Some(token) = token else {
            return Err(
                "--remote needs --token <token> (or NUO_TOKEN env var): every network-exposed server \
                 requires the bearer token from the host's server discovery record"
                    .to_string(),
            );
        };

        Ok(Self {
            host,
            port,
            token,
            secure,
        })
    }

    pub async fn connect(&self, action: AttachAction) -> AttachResult {
        self.connect_with_project(action, None).await
    }

    pub async fn connect_with_project(
        &self,
        action: AttachAction,
        project: Option<std::path::PathBuf>,
    ) -> AttachResult {
        let url = self.url();
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| AttachError::transport(format!("bad ws url {url}: {e}")))?;
        let value = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|e| AttachError::transport(format!("bad bearer token: {e}")))?;
        request.headers_mut().insert("Authorization", value);
        let (ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| AttachError::transport(format!("ws connect to {url}: {e}")))?;
        let (sink, source) = websocket_split(ws);
        finish_handshake_with_project((sink, source), action, project).await
    }
}

fn split_authority(s: &str) -> Result<(Option<String>, Option<u16>), String> {
    if s.is_empty() {
        return Ok((None, None));
    }
    if s.starts_with('[') {
        if let Some(close_idx) = s.find(']') {
            let host = &s[..=close_idx];
            let rest = &s[close_idx + 1..];
            if rest.is_empty() {
                return Ok((Some(host.to_string()), None));
            } else if let Some(p_str) = rest.strip_prefix(':') {
                let port: u16 = p_str
                    .parse()
                    .map_err(|_| format!("'{p_str}' is not a port number"))?;
                return Ok((Some(host.to_string()), Some(port)));
            } else {
                return Err(format!("invalid IPv6 authority '{s}'"));
            }
        } else {
            return Err(format!("unclosed IPv6 bracket in '{s}'"));
        }
    }
    match s.rsplit_once(':') {
        Some((host, port_str)) => {
            let port: u16 = port_str
                .parse()
                .map_err(|_| format!("'{port_str}' is not a port number"))?;
            let host_opt = (!host.is_empty()).then(|| host.to_string());
            Ok((host_opt, Some(port)))
        }
        None => Ok((Some(s.to_string()), None)),
    }
}

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
const ATTACH_ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Control verbs may stop/restart a server, so they keep a wider budget.
const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// The monitor stream's first snapshot is produced by a separate task.
const MONITOR_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const FALLBACK_GRACE: Duration = Duration::from_secs(15);

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
async fn discover_at(path: &Path) -> Option<ServerInfo> {
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
    }
}

/// A failed attach.
///
/// `[INV-ATTACH-01]`: `code` is the server's machine-readable taxonomy, carried
/// verbatim from `Wire::Error`. Downgrading a refusal to a bare `String` is what
/// made an expected outcome (a tab whose session is gone) indistinguishable from
/// a transport fault, so the client had no choice but to die on it (ADR-0123).
///
/// `code: None` covers both "transport failure" (the server never spoke) and
/// "refusal with no declared taxonomy". Both are unrecoverable by contract: a
/// client may not invent a recovery the server did not authorize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachError {
    pub code: Option<String>,
    pub message: String,
}

impl AttachError {
    /// A transport-level failure: no server taxonomy exists.
    fn transport(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }

    /// The refusal code, without the `Option` ceremony at every call site.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Whether the target session provably no longer exists
    /// (`ERR_SESSION_GONE`). The tab naming it must be retired.
    pub fn is_session_gone(&self) -> bool {
        self.code() == Some(nuo_protocol::ERR_SESSION_GONE)
    }

    /// Whether the target session exists durably but could not be assembled
    /// (`ERR_SESSION_UNRESUMABLE`). The tab must be kept — its history is real.
    pub fn is_session_unresumable(&self) -> bool {
        self.code() == Some(nuo_protocol::ERR_SESSION_UNRESUMABLE)
    }
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The message alone: diagnostics must not grow a code prefix into prose
        // a human reads (and a caller must never parse it back out).
        f.write_str(&self.message)
    }
}

impl std::error::Error for AttachError {}

impl From<AttachError> for String {
    fn from(error: AttachError) -> Self {
        error.message
    }
}

pub type AttachResult = Result<Handshake, AttachError>;

/// Where an attached reader records the *transport* failure that ended it.
///
/// Deliberately not an [`AgentResponse`]: a socket death is not something the
/// server said, and encoding it as one made a decode fault indistinguishable
/// from a server instruction to stop (ADR-0136). The consumer reads this after
/// the response stream closes — `None` means the connection ended cleanly.
pub type LinkLostMarker = Arc<Mutex<Option<String>>>;

#[allow(clippy::large_enum_variant)]
pub enum Handshake {
    Attached {
        req_tx: mpsc::UnboundedSender<AgentRequest>,
        resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
        /// Where the reader's *transport* failure lands, when there is one.
        ///
        /// Separate from `resp_rx` by construction: a socket death is not an
        /// `AgentResponse`, and spelling it `Exit` made a client click look
        /// like a server instruction (ADR-0136). `None` when the reader ended
        /// because the connection closed cleanly.
        link_lost: LinkLostMarker,
        session_id: String,
        /// Readiness at the moment of the handshake. Informational only:
        /// `[INV-ATTACH-04]` (ADR-0133) makes the *post-snapshot*
        /// `AgentResponse::ThreadPhase(Ready)` the gate for input.
        phase: nuo_protocol::ThreadPhase,
    },
    Pick(Vec<ThreadOverview>),
}

pub async fn connect(info: &ServerInfo, action: AttachAction) -> AttachResult {
    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_handshake((sink, source), action).await;
    }
    if info.port == 0 {
        return Err(AttachError::transport(
            "server runs in client-bound mode (UDS only) and local endpoint is not reachable",
        ));
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| AttachError::transport(format!("bad ws url {url}: {e}")))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| AttachError::transport(format!("bad bearer token: {e}")))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| AttachError::transport(format!("ws connect to {url}: {e}")))?;
    let (sink, source) = websocket_split(ws);
    finish_handshake((sink, source), action).await
}

async fn finish_handshake(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
) -> AttachResult {
    finish_handshake_with_project(parts, action, std::env::current_dir().ok()).await
}

async fn finish_handshake_with_project(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
    project: Option<std::path::PathBuf>,
) -> AttachResult {
    let (mut wire_sink, mut wire_source) = parts;

    let select = Wire::Select {
        action,
        project,
        posture: current_posture(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| AttachError::transport(format!("wire send select: {e}")))?;

    let reply = tokio::time::timeout(ATTACH_ACK_TIMEOUT, async {
        match wire_source.next().await {
            Some(Ok(WireFrame::Wire(wire))) => match wire {
                Wire::Welcome { thread_id, phase } => Ok(Reply::Welcome(Welcome {
                    session_id: thread_id,
                    phase,
                })),
                Wire::Pick { threads } => Ok(Reply::Pick(threads)),
                Wire::Error { message, code } => {
                    // `[INV-ATTACH-01]`: the code travels with the message.
                    // This is the hop that used to discard the taxonomy
                    // with a `..` and leave the client to sniff prose.
                    Err(AttachError {
                        code,
                        message: format!("server rejected the attach: {message}"),
                    })
                }
                other => {
                    // `[INV-ATTACH-03]` (ADR-0133): Welcome is the first
                    // frame by contract. Silently dropping an earlier state
                    // frame hid a protocol violation and lost hydration, so
                    // this is now a hard, diagnosable error.
                    Err(AttachError::transport(format!(
                        "unexpected frame before Welcome: {other:?}"
                    )))
                }
            },
            // The handshake is the one place where a frame we cannot model is
            // still an attach refusal: no thread has been claimed yet, and a
            // Welcome we cannot read is exactly the disagreement the protocol
            // number exists to prevent. Refusing with the taxonomy lets the
            // attach loop disclose it instead of dying on it.
            Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                Err(AttachError::transport(format!(
                    "server sent a frame this client cannot decode{}: {detail}",
                    envelope
                        .map(|tag| format!(" (envelope `{tag}`)"))
                        .unwrap_or_default()
                )))
            }
            Some(Err(error)) => Err(AttachError::transport(format!(
                "wire recv during handshake: {error}"
            ))),
            None => Err(AttachError::transport("server closed the connection")),
        }
    })
    .await
    .map_err(|_| {
        AttachError::transport(
            "server did not acknowledge the attach within the liveness window \
             (the server may be starting, wedged, or speaking a different protocol)",
        )
    })??;

    let welcome = match reply {
        Reply::Welcome(w) => w,
        Reply::Pick(sessions) => {
            let _ = wire_sink.close().await;
            return Ok(Handshake::Pick(sessions));
        }
    };

    let (req_out_tx, mut req_out_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();
    // Shared with the reader task: after `resp_rx` closes, this is *how* the
    // consumer learns whether the link failed and why, without guessing.
    let link_lost_marker = Arc::new(Mutex::new(None::<String>));
    let link_lost_inner = Arc::clone(&link_lost_marker);
    let exit_seen = Arc::new(Mutex::new(false));
    let exit_seen_inner = Arc::clone(&exit_seen);

    tokio::spawn(async move {
        let mut end_pending = false;
        while let Some(request) = req_out_rx.recv().await {
            if matches!(request, AgentRequest::EndThread) {
                end_pending = true;
            }
            if let Err(error) = wire_sink.send(Wire::Request { request }).await {
                tracing::warn!(%error, "attach: wire send failed");
                break;
            }
        }
        if end_pending {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let _ = wire_sink.close().await;
    });

    tokio::spawn(async move {
        while let Some(frame) = wire_source.next().await {
            match frame {
                Ok(WireFrame::Wire(Wire::Response { response })) => {
                    // A server instruction to stop is recorded as such, so the
                    // close that follows is not mistaken for a fault.
                    if matches!(response, AgentResponse::Exit) {
                        *exit_seen_inner.lock().unwrap_or_else(|e| e.into_inner()) = true;
                    }
                    if resp_in_tx.send(response).is_err() {
                        return;
                    }
                }
                // One frame this build cannot model. Both transports are
                // self-delimiting, so the reader stays aligned and the session
                // is untouched — disclose the skew and keep reading
                // [ADR-0136]. Before this arm existed the frame arrived as
                // `Err`, the loop ended, and the synthesized `Exit` closed the
                // window mid-round.
                Ok(WireFrame::Undecodable { envelope, detail }) => {
                    tracing::warn!(
                        envelope = envelope.as_deref().unwrap_or("<none>"),
                        %detail,
                        "attach: server sent a frame this client cannot decode; \
                         connection retained"
                    );
                    let notice = AgentResponse::Error(format!(
                        "The server sent a message this client does not understand{}. \
                         The connection is still live — run `nuo stop` so both \
                         sides come up on the same build.",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    ));
                    if resp_in_tx.send(notice).is_err() {
                        return;
                    }
                }
                Ok(WireFrame::Wire(_)) => {
                    tracing::warn!("attach: unexpected post-handshake frame, ignored")
                }
                // The socket itself failed. This is *not* a response and must
                // never be spelled `Exit`; it is recorded where the consumer
                // can read the real reason after `resp_rx` closes [ADR-0136].
                Err(error) => {
                    tracing::warn!(%error, "attach: wire recv failed; link is down");
                    *link_lost_inner.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(error.to_string());
                    break;
                }
            }
        }
    });

    Ok(Handshake::Attached {
        req_tx: req_out_tx,
        resp_rx: resp_in_rx,
        link_lost: link_lost_marker,
        session_id: welcome.session_id,
        phase: welcome.phase,
    })
}

pub async fn control(info: &ServerInfo, request: ControlRequest) -> Result<(), String> {
    control_with_reply(info, request).await.map(|_| ())
}

pub async fn control_with_reply(
    info: &ServerInfo,
    request: ControlRequest,
) -> Result<String, String> {
    let action = AttachAction::Control(request);

    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_control((sink, source), action).await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        req.headers_mut().insert("Authorization", value);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (sink, source) = websocket_split(ws);
    finish_control((sink, source), action).await
}

async fn finish_control(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
) -> Result<String, String> {
    let (mut wire_sink, mut wire_source) = parts;
    let select = Wire::Select {
        action,
        project: None,
        posture: current_posture(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send control select: {e}"))?;

    tokio::time::timeout(CONTROL_REPLY_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(WireFrame::Wire(wire))) => match wire {
                    Wire::ControlReply { ok, error, .. } => {
                        return if ok {
                            Ok(error.unwrap_or_default())
                        } else {
                            Err(error.unwrap_or_else(|| "control verb rejected".to_string()))
                        };
                    }
                    Wire::Error { message, .. } => return Err(message),
                    _ => tracing::warn!("control: unexpected frame during handshake, ignored"),
                },
                // A control connection exists to carry one verb. A frame this
                // build cannot model means the peer is a different build, and
                // the caller needs that said plainly rather than as a timeout
                // [ADR-0136].
                Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                    return Err(format!(
                        "control: server sent a frame this client cannot decode{}: {detail}",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    ));
                }
                Some(Err(error)) => return Err(format!("wire recv during control: {error}")),
                None => return Err("server closed the control connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for control reply from server".to_string())?
}

struct Welcome {
    session_id: String,
    phase: nuo_protocol::ThreadPhase,
}

enum Reply {
    Welcome(Welcome),
    Pick(Vec<ThreadOverview>),
}

pub async fn monitor_stream(
    info: &ServerInfo,
    action: MonitorAction,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_monitor((sink, source), action, "local IPC").await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (sink, source) = websocket_split(ws);
    finish_monitor((sink, source), action, &url).await
}

async fn finish_monitor(
    parts: (BoxWireSink, BoxWireStream),
    action: MonitorAction,
    target: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    let (mut wire_sink, mut wire_source) = parts;

    let select = Wire::Select {
        action: AttachAction::Monitor(action),
        project: None,
        posture: current_posture(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send select: {e}"))?;

    let first = tokio::time::timeout(MONITOR_FIRST_FRAME_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(WireFrame::Wire(wire))) => match wire {
                    Wire::Monitor { event } => return Ok(event),
                    Wire::Error { message, .. } => return Err(message),
                    _ => tracing::warn!("status: unexpected frame during handshake, ignored"),
                },
                Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                    return Err(format!(
                        "status: server sent a frame this client cannot decode{}: {detail}",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    ));
                }
                Some(Err(error)) => return Err(format!("wire recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for monitor snapshot from {target}"))??;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(first);
    tokio::spawn(async move {
        while let Some(frame) = wire_source.next().await {
            match frame {
                Ok(WireFrame::Wire(Wire::Monitor { event })) => {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                Ok(WireFrame::Wire(_)) => {
                    tracing::warn!("status: unexpected post-handshake frame, ignored")
                }
                // Monitor frames are independent; an unmodelled one is
                // disclosed and skipped, exactly like the attach reader
                // [ADR-0136].
                Ok(WireFrame::Undecodable { envelope, detail }) => {
                    tracing::warn!(
                        envelope = envelope.as_deref().unwrap_or("<none>"),
                        %detail,
                        "status: server sent a frame this client cannot decode; \
                         monitor stream retained"
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "status: wire recv failed");
                    break;
                }
            }
        }
    });
    Ok(rx)
}

pub fn upsert_session_row(rows: &mut Vec<MonitoredThread>, row: MonitoredThread) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
}

pub fn upsert_task_row(rows: &mut Vec<MonitoredTask>, row: MonitoredTask) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
}

#[cfg(test)]
mod tests {
    use super::*;

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
            const SKEWED: &[u8] = br#"{"type":"Response","NoSuchVariant":{}}"#;
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
        const SKEWED: &[u8] = br#"{"type":"Response","RetryResolved":{"attempt":1}}"#;
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
            const SKEWED: &[u8] = br#"{"type":"Response","RetryResolved":{"attempt":1}}"#;
            out.extend_from_slice(&(SKEWED.len() as u32).to_be_bytes());
            out.extend_from_slice(SKEWED);

            server_side.write_all(&out).await.unwrap();
            server_side.flush().await.unwrap();

            // Hold the connection open so the client cannot mistake this for a
            // disconnect — that separation is the whole point.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let (sink, source) = crate::transport::native_framed_split(client_side);
        let handshake =
            finish_handshake_with_project((sink, source), AttachAction::Attach(None), None)
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
}
