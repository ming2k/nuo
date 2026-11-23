//! Minimal MCP client support for local stdio servers.
//!
//! Each configured server is initialized once at startup. Its advertised tools
//! are adapted to nuo's `Tool` trait and use the same agent execution path
//! as built-in tools.
//!
//! # Error model
//!
//! `McpError` separates **transport** failures (the stdio pipe broke — the
//! server crashed or the child process died) from **protocol** failures (the
//! server replied with a JSON-RPC `error` object, or a well-formed but useless
//! response). This distinction is load-bearing for retry safety: only a
//! transport error justifies a reconnect-and-retry, because a protocol error is
//! a deterministic, server-side result that re-sending the same call would
//! reproduce identically — and for a *non-idempotent* MCP tool re-sending it
//! would repeat a side effect.
use async_trait::async_trait;
use protocol::mcp::{McpConnectionStatus, McpServerConfig};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tool::Tool;

mod request;

/// Pluggable trust verifier for project-level MCP servers.
pub type McpTrustVerifier = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

static TRUST_VERIFIER: OnceLock<McpTrustVerifier> = OnceLock::new();

/// Configure the global trust verifier used by MCP server connections.
pub fn set_trust_verifier(verifier: McpTrustVerifier) {
    let _ = TRUST_VERIFIER.set(verifier);
}

pub fn is_sandbox_trusted(root: &Path) -> bool {
    TRUST_VERIFIER.get().map(|v| v(root)).unwrap_or(true)
}

/// Pluggable universal asset attestation verifier.
pub type McpAttestationVerifier = Arc<
    dyn Fn(&protocol::security::AssetLocator, &protocol::security::AssetSpec) -> bool + Send + Sync,
>;

static ATTESTATION_VERIFIER: OnceLock<McpAttestationVerifier> = OnceLock::new();

/// Configure the universal attestation verifier used across all MCP scopes.
pub fn set_attestation_verifier(verifier: McpAttestationVerifier) {
    let _ = ATTESTATION_VERIFIER.set(verifier);
}

pub fn is_asset_attested(
    locator: &protocol::security::AssetLocator,
    spec: &protocol::security::AssetSpec,
) -> bool {
    ATTESTATION_VERIFIER
        .get()
        .map(|v| v(locator, spec))
        .unwrap_or(true)
}
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

/// Protocol revisions this client can speak, newest first. The client sends
/// its newest and steps down to the newest revision the server offered, per
/// the MCP version-negotiation rule ("the client sends the version it
/// supports; if the server does not support it, the server responds with the
/// version it supports, and the client either downgrades or disconnects").
/// Over stdio the 2024-11-05 baseline remains the guaranteed common tongue.
const MCP_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// The 2026-07-28 stateless revision, which *removed* the
/// `initialize`/`initialized` handshake (SEP-2575): every request carries the
/// protocol version, client identity, and capabilities in `params._meta`
/// instead. A server that only speaks it rejects `initialize`, so
/// [`McpClient::connect`] falls back to this dialect.
const MCP_STATELESS_VERSION: &str = "2026-07-28";
const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Per-call timeout for a single JSON-RPC request over an established
/// connection. A server that accepts the request but never responds (or streams
/// nothing forever) is released instead of pinning the agent indefinitely.
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Hard cap on one newline-delimited JSON-RPC message from a stdio server.
/// A server that emits an unbounded line must fail the transport rather than
/// grow the client's heap until the process is killed.
const MAX_MCP_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// A typed MCP error: either a broken transport (retry-safe) or a protocol
/// result (never retry a protocol error — re-sending an already-applied
/// non-idempotent call would double a side effect).
#[derive(Debug)]
enum McpError {
    /// The stdio pipe broke: a failed write/read, the child exited. The
    /// connection is unusable and a reconnect may help.
    Transport(String),
    /// The request timed out. This is **not** retry-safe: the server may have
    /// applied the call, and re-sending a non-idempotent tool would duplicate
    /// its side effect.
    Timeout(String),
    /// The server replied with a JSON-RPC `error` object (or a malformed
    /// response). This is a deterministic server-side result — retrying the
    /// *same* call yields the *same* outcome, so never reconnect on it.
    Protocol(String),
    /// The caller cancelled the request (or a timeout cancelled it). The client
    /// sends `notifications/cancelled` best-effort; no reconnect is implied.
    Cancelled,
    /// The HTTP server rejected a request carrying an MCP session id with
    /// `404 Not Found` (or `400 Bad Request`). The session is stale; the
    /// client re-initializes once and retries the rejected request.
    SessionExpired,
}

impl McpError {
    /// True only for transport-level failures, where reconnecting and retrying
    /// once could plausibly succeed.
    fn is_transport(&self) -> bool {
        matches!(self, McpError::Transport(_))
    }

    /// True when a server's rejection of `initialize` says it only speaks the
    /// 2026-07-28 stateless revision (no handshake exists — SEP-2575).
    /// Matched on message text because JSON-RPC error codes are not
    /// standardized across revisions; the phrasings below are what
    /// spec-conformant servers answer with.
    fn is_stateless_rejection(&self) -> bool {
        let msg = self.to_string().to_lowercase();
        msg.contains("params._meta")
            || msg.contains("requires _meta")
            || msg.contains("stateless")
            || (msg.contains("unknown method") && msg.contains("initialize"))
            || (msg.contains("method not found") && msg.contains("initialize"))
            || (msg.contains("unsupported") && msg.contains("protocol"))
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Transport(msg) | McpError::Protocol(msg) | McpError::Timeout(msg) => {
                f.write_str(msg)
            }
            McpError::Cancelled => f.write_str("MCP request cancelled"),
            McpError::SessionExpired => f.write_str("MCP HTTP session expired"),
        }
    }
}

impl std::error::Error for McpError {}

impl From<McpError> for String {
    fn from(error: McpError) -> Self {
        error.to_string()
    }
}

pub struct McpLoadResult {
    pub tools: Vec<Arc<dyn Tool>>,
    pub statuses: Vec<(String, McpConnectionStatus)>,
    /// The reconnect-capable server handles, one per connected server. The
    /// background refresh loop uses these to reconnect/re-discover tools.
    pub servers: Vec<Arc<McpServer>>,
}

/// A server-to-client notification worth acting on.
///
/// Progress is not represented here: it is per-request ephemeral data and is
/// routed directly to the waiting call. Everything else either updates the
/// catalog (list_changed), is a diagnostic log message, or is recorded as an
/// unknown notification for observability.
#[derive(Debug, Clone)]
pub enum McpNotification {
    /// `notifications/message` — MCP logging. Never model context.
    Logging {
        level: String,
        logger: Option<String>,
        data: Value,
    },
    ToolsListChanged,
    ResourcesListChanged,
    PromptsListChanged,
    ResourceUpdated {
        uri: String,
    },
    RootsListChanged,
    Other {
        method: String,
        params: Value,
    },
}

/// Consumer for non-progress MCP notifications. The callback must be cheap and
/// non-blocking; the stdio reader invokes it inline.
pub type McpNotificationSink = Arc<dyn Fn(McpNotification) + Send + Sync>;

fn parse_notification(method: &str, params: Option<&Value>) -> McpNotification {
    match method {
        "notifications/message" => McpNotification::Logging {
            level: params
                .and_then(|params| params.get("level"))
                .and_then(Value::as_str)
                .unwrap_or("info")
                .to_string(),
            logger: params
                .and_then(|params| params.get("logger"))
                .and_then(Value::as_str)
                .map(str::to_string),
            data: params
                .and_then(|params| params.get("data"))
                .cloned()
                .unwrap_or(Value::Null),
        },
        "notifications/tools/list_changed" => McpNotification::ToolsListChanged,
        "notifications/resources/list_changed" => McpNotification::ResourcesListChanged,
        "notifications/prompts/list_changed" => McpNotification::PromptsListChanged,
        "notifications/resources/updated" => McpNotification::ResourceUpdated {
            uri: params
                .and_then(|params| params.get("uri"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        "notifications/roots/list_changed" => McpNotification::RootsListChanged,
        other => McpNotification::Other {
            method: other.to_string(),
            params: params.cloned().unwrap_or(Value::Null),
        },
    }
}

struct McpTransport {
    process_tree: host::process::OwnedProcessTree,
    _child: Child,
    /// Shared with the reader task so a server-to-client request can be
    /// answered without going through the request path.
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
}

/// One request waiting for its JSON-RPC response. The reader task owns the
/// response channel; a dropped caller simply removes the entry.
struct PendingRequest {
    response: oneshot::Sender<Result<Value, McpError>>,
    progress: Option<mpsc::UnboundedSender<tool::ToolProgress>>,
}

/// Shared state between the client and its stdio reader task.
#[derive(Default)]
struct Inflight {
    pending: std::sync::Mutex<HashMap<u64, PendingRequest>>,
    alive: AtomicBool,
    notification_sink: std::sync::Mutex<Option<McpNotificationSink>>,
}

impl Inflight {
    fn new() -> Self {
        Self {
            pending: std::sync::Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            notification_sink: std::sync::Mutex::new(None),
        }
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn insert(&self, id: u64, pending: PendingRequest) -> bool {
        let mut guard = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.alive.load(Ordering::Relaxed) {
            return false;
        }
        guard.insert(id, pending);
        true
    }

    fn remove(&self, id: u64) {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&id);
    }

    /// Complete one response. A late response for a cancelled request has no
    /// waiter and is deliberately dropped.
    fn complete(&self, id: u64, message: Value) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&id);
        if let Some(pending) = pending {
            let _ = pending.response.send(Ok(message));
        }
    }

    fn progress(&self, id: u64, tick: tool::ToolProgress) {
        let guard = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(sender) = guard.get(&id).and_then(|pending| pending.progress.as_ref()) {
            let _ = sender.send(tick);
        }
    }

    fn set_notification_sink(&self, sink: McpNotificationSink) {
        *self
            .notification_sink
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(sink);
    }

    fn notify(&self, notification: McpNotification) {
        let sink = self
            .notification_sink
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(sink) = sink {
            sink(notification);
        }
    }

    /// The reader ended: fail every waiter with a transport error and make new
    /// requests fail until the server handle reconnects.
    fn fail_all(&self) {
        self.alive.store(false, Ordering::Relaxed);
        let pending = {
            let mut guard = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            std::mem::take(&mut *guard)
        };
        for (_, pending) in pending {
            let _ = pending
                .response
                .send(Err(McpError::Transport("MCP reader closed".to_string())));
        }
    }
}

/// Parse `nuo-{id}` progress tokens back to the request they belong to.
fn progress_id(message: &Value) -> Option<(u64, tool::ToolProgress)> {
    let params = message.get("params")?;
    let token = params.get("progressToken")?.as_str()?;
    let id = token.strip_prefix("nuo-")?.parse().ok()?;
    Some((
        id,
        tool::ToolProgress::new(
            params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            params.get("progress").and_then(Value::as_u64),
            params.get("total").and_then(Value::as_u64),
        ),
    ))
}

/// Build the JSON-RPC reply for a server-to-client request.
///
/// nuo advertises no sampling, roots, or elicitation capability, so those
/// requests are rejected explicitly. `ping` is always safe and is answered so
/// a server's liveness probe cannot hang.
fn server_request_response(id: &Value, method: &str) -> Value {
    if method == "ping" {
        return json!({"jsonrpc": "2.0", "id": id, "result": {}});
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("MCP client does not support server request `{method}`")
        }
    })
}

/// Own stdout for the whole connection: responses are routed by id, progress
/// by token, and a closed pipe fails every waiter exactly once.
async fn reader_loop(
    mut stdout: BufReader<ChildStdout>,
    inflight: Arc<Inflight>,
    writer: Arc<tokio::sync::Mutex<ChildStdin>>,
) {
    loop {
        let message = match read_message(&mut stdout).await {
            Ok(message) => message,
            Err(_) => {
                inflight.fail_all();
                return;
            }
        };
        if let (Some(id), Some(method)) = (
            message.get("id"),
            message.get("method").and_then(Value::as_str),
        ) {
            // Server-to-client request. Capabilities are empty, so the only
            // spec-defined request we can answer is `ping`; everything else
            // gets a JSON-RPC method-not-found rather than a silent hang.
            let response = server_request_response(id, method);
            let write = tokio::time::timeout(MCP_REQUEST_TIMEOUT, async {
                let mut writer = writer.lock().await;
                write_message(&mut writer, &response).await
            })
            .await;
            if !matches!(write, Ok(Ok(()))) {
                inflight.fail_all();
                return;
            }
            continue;
        }
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if method == "notifications/progress" {
                if let Some((id, tick)) = progress_id(&message) {
                    inflight.progress(id, tick);
                }
            } else {
                inflight.notify(parse_notification(method, message.get("params")));
            }
            continue;
        }
        if let Some(id) = message.get("id").and_then(Value::as_u64)
            && (message.get("result").is_some() || message.get("error").is_some())
        {
            inflight.complete(id, message);
        }
        // Server-to-client requests (sampling, roots, elicitation) are ignored
        // because the client advertises no corresponding capability.
    }
}

/// A Streamable-HTTP connection's cloneable client state.
///
/// `netune::Client` is `Clone` and shares its pool, so two requests can hold
/// independent clones and run concurrently. The only mutable wire state is the
/// MCP session id, which is application-level and therefore guarded here rather
/// than in the transport; the guard is never held across an await.
#[derive(Clone)]
struct HttpTransport {
    /// Absolute URL of the Streamable-HTTP endpoint.
    url: String,
    client: netune::Client,
    /// Session id issued by the server (`Mcp-Session-Id` on initialize),
    /// echoed on every subsequent request. `None` until (and unless) the
    /// server assigns one.
    session_id: Arc<std::sync::Mutex<Option<String>>>,
    /// Serializes session re-initialization after a stale-session response so
    /// concurrent requests do not each run a handshake.
    reinit: Arc<tokio::sync::Mutex<()>>,
}

impl HttpTransport {
    fn session(&self) -> Option<String> {
        self.session_id
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn remember_session(&self, header: Option<&str>) {
        let Some(header) = header else {
            return;
        };
        let mut guard = self
            .session_id
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if guard.as_deref() != Some(header) {
            *guard = Some(header.to_string());
        }
    }

    /// Replace the session id with exactly what a fresh handshake returned.
    /// Unlike [`Self::remember_session`], `None` clears a stale id.
    fn set_session(&self, session: Option<&str>) {
        *self
            .session_id
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = session.map(str::to_string);
    }
}

/// One open connection to an MCP server: either a spawned stdio child or a
/// Streamable-HTTP endpoint. Both speak the same JSON-RPC message grammar, so
/// [`McpClient::request`] normalizes over this enum.
enum McpConnection {
    /// Boxed because the process handles dominate the enum's size and the
    /// connection is held for the server's lifetime, not copied per message.
    Stdio(Box<McpTransport>),
    /// Boxed because the HTTP client state is much larger than the stdio
    /// variant; the enum is held for the life of an MCP server, not per message.
    Http(Box<HttpTransport>),
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        // `kill_on_drop` signals only the direct child. Native tree
        // containment also takes down wrapped servers (`npx`/`uvx`), so a
        // reconfigured or disabled MCP source cannot leak its grandchild.
        if let McpConnection::Stdio(transport) = self {
            let _ = transport.process_tree.terminate();
        }
    }
}

struct McpClient {
    transport: Mutex<McpConnection>,
    inflight: Arc<Inflight>,
    next_id: AtomicU64,
    /// The negotiated wire dialect, swappable on connect. `Legacy` keeps the
    /// initialize/initialized handshake shape; `Stateless` is the 2026-07-28
    /// revision, which has no handshake — every request carries
    /// version/identity/capabilities in `params._meta` instead.
    dialect: tokio::sync::RwLock<McpDialect>,
}

/// The wire dialect one connection speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum McpDialect {
    /// Initialize/initialized handshake, revisions ≤ 2025-06-18.
    #[default]
    Legacy,
    /// 2026-07-28 stateless requests: no handshake, `_meta` on every request.
    Stateless,
}

impl McpClient {
    /// Snapshot the HTTP transport, if this client uses Streamable HTTP.
    ///
    /// The transport lock is held only for the clone; requests run against the
    /// cloned `netune::Client` (which shares the connection pool) without
    /// serializing on it.
    async fn http_transport(&self) -> Option<HttpTransport> {
        match &*self.transport.lock().await {
            McpConnection::Http(http) => Some((**http).clone()),
            McpConnection::Stdio(_) => None,
        }
    }

    /// Install the consumer for non-progress notifications. The sink is shared
    /// with the reader task and may be replaced without reconnecting.
    fn set_notification_sink(&self, sink: McpNotificationSink) {
        self.inflight.set_notification_sink(sink);
    }

    async fn connect(config: &McpServerConfig) -> Result<Arc<Self>, String> {
        Self::connect_with_http(config, None).await
    }

    /// Connect with an optional injected HTTP transport. The stdio tests and
    /// the real client both use `connect`; HTTP tests inject a `netune::mock`
    /// client so the request/streaming path runs against its real machinery
    /// without a listening socket.
    async fn connect_with_http(
        config: &McpServerConfig,
        http_client: Option<netune::Client>,
    ) -> Result<Arc<Self>, String> {
        // Transport selection: a configured `url` reaches the server over
        // Streamable HTTP; otherwise `command` spawns a local stdio child.
        // (`url` wins when both are set, mirroring the common MCP client
        // configuration shape.)
        let mut stdio_stdout: Option<ChildStdout> = None;
        let mut stdio_writer: Option<Arc<tokio::sync::Mutex<ChildStdin>>> = None;
        let connection = if let Some(url) = config.url.as_deref() {
            // Validate the URL up front so a misconfigured server fails at
            // connect time with a clear message, not on first request.
            netune::Target::from_url(url)
                .map_err(|error| format!("invalid MCP url '{url}': {error}"))?;
            let default_http = || {
                let socket = netune::SocketConfig::default();
                #[cfg(unix)]
                let socket = socket.with_keepalive(provider::transport::TCP_KEEPALIVE_IDLE);
                netune::Client::builder().socket_config(socket).build()
            };
            McpConnection::Http(Box::new(HttpTransport {
                url: url.to_string(),
                client: http_client.unwrap_or_else(default_http),
                session_id: Arc::new(std::sync::Mutex::new(None)),
                reinit: Arc::new(tokio::sync::Mutex::new(())),
            }))
        } else {
            let (program, args) = config
                .command
                .split_first()
                .ok_or_else(|| "MCP command must not be empty".to_string())?;

            let mut command = if let Some(root) = &config.sandbox_root {
                // Project-authored servers are untrusted executable extensions.
                // Extension trust permits loading their exact declaration, but it
                // never grants ambient host or workspace-write/network authority.
                host::workspace_sandbox::command_with_environment(
                    program,
                    args,
                    &config.environment,
                    root,
                    host::workspace_sandbox::WorkspaceAccess::ReadOnly,
                    host::workspace_sandbox::NetworkAccess::Disabled,
                )?
            } else {
                let mut command = Command::new(program);
                command.args(args).envs(&config.environment);
                command
            };
            // MCP servers are usually wrappers (`npx`, `uvx`) around a real
            // server process. Native owned-tree containment ensures teardown
            // reaches the wrapped process too.
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);

            let (mut child, process_tree) = host::process::spawn_owned(&mut command)
                .map_err(|error| format!("failed to spawn '{}': {}", program, error))?;
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| "MCP server stdin is unavailable".to_string())?;
            let stdin = Arc::new(tokio::sync::Mutex::new(stdin));
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| "MCP server stdout is unavailable".to_string())?;
            stdio_stdout = Some(stdout);
            stdio_writer = Some(Arc::clone(&stdin));
            McpConnection::Stdio(Box::new(McpTransport {
                process_tree,
                _child: child,
                stdin,
            }))
        };
        let inflight = Arc::new(Inflight::new());
        if let Some(stdout) = stdio_stdout {
            let writer =
                stdio_writer.ok_or_else(|| "MCP stdio writer is unavailable".to_string())?;
            tokio::spawn(reader_loop(
                BufReader::new(stdout),
                Arc::clone(&inflight),
                writer,
            ));
        }
        let client = Arc::new(Self {
            transport: Mutex::new(connection),
            inflight,
            next_id: AtomicU64::new(1),
            dialect: tokio::sync::RwLock::new(McpDialect::Legacy),
        });

        let response = match client
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSIONS[0],
                    "capabilities": {},
                    "clientInfo": {
                        "name": "nuo",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await
        {
            Ok(response) => response,
            Err(error) if error.is_stateless_rejection() => {
                // The server only speaks the 2026-07-28 stateless revision,
                // which has no initialize at all (SEP-2575): it rejected the
                // handshake request itself. Switch dialects and continue —
                // the whole exchange collapses into stateless requests.
                return Self::connect_stateless(client).await;
            }
            Err(error) => return Err(error.to_string()),
        };
        // Version negotiation: a compliant server echoes a version it
        // supports. If it answered with an older revision we still speak,
        // we stay on our newest common revision for the connection's
        // lifetime — a mismatch here is informational only for stdio, since
        // all revisions share the same JSON-RPC framing. A revision we
        // cannot speak at all fails the connection rather than continuing
        // in an undefined dialect — except the 2026-07-28 stateless
        // revision, which we speak by switching dialects (no handshake).
        // (A missing field is the pre-2024-11-05 legacy shape; the baseline
        // revision covers it.)
        if let Some(server_version) = response.get("protocolVersion").and_then(Value::as_str)
            && !MCP_PROTOCOL_VERSIONS.contains(&server_version)
        {
            if server_version == MCP_STATELESS_VERSION {
                return Self::connect_stateless(client).await;
            }
            return Err(format!(
                "MCP server speaks unsupported protocol revision {server_version} \
                     (client supports: {})",
                MCP_PROTOCOL_VERSIONS.join(", ")
            ));
        }
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    /// Adopt the 2026-07-28 stateless dialect on an already-open transport.
    ///
    /// There is no handshake to perform — capabilities are learned lazily via
    /// `tools/list` — so "connecting" is just flipping the dialect marker:
    /// every subsequent [`Self::request`] stamps `params._meta` with the
    /// protocol version and client identity (SEP-2575). A cheap liveness
    /// probe (`tools/list`) validates the server actually serves the dialect
    /// before we report the connection as usable.
    async fn connect_stateless(client: Arc<Self>) -> Result<Arc<Self>, String> {
        {
            let mut dialect = client.dialect.write().await;
            *dialect = McpDialect::Stateless;
        }
        // Liveness + dialect probe: a server that also rejects a stateless
        // tools/list is not speakable at all, and the error surfaces verbatim.
        client
            .request("tools/list", json!({}))
            .await
            .map_err(|error| error.to_string())?;
        Ok(client)
    }
}

/// Extract one MCP progress tick for `token`, if this notification is for us.
fn progress_tick(params: Option<&Value>, token: &str) -> Option<tool::ToolProgress> {
    let params = params?;
    if params.get("progressToken").and_then(Value::as_str) != Some(token) {
        return None;
    }
    Some(tool::ToolProgress::new(
        params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        params.get("progress").and_then(Value::as_u64),
        params.get("total").and_then(Value::as_u64),
    ))
}

/// Parse a Streamable-HTTP response body into a JSON-RPC reply for `method`.
/// A 200/201 may deliver either `application/json` (a single object) or
/// `text/event-stream` (SSE `data:` frames; per spec the *last* `message`
/// event carrying our id is the reply — but practical servers send exactly
/// one, so the first id-matching frame wins).
fn parse_http_message_with_progress(
    body: &str,
    content_type: &str,
    method: &str,
    progress_token: Option<&str>,
    mut on_progress: Option<&mut (dyn FnMut(tool::ToolProgress) + Send)>,
) -> Result<Value, McpError> {
    if content_type.starts_with("text/event-stream") {
        // Collect every `data:` frame into candidate JSON-RPC messages.
        for frame in body.lines().filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .starts_with("data:")
                .then(|| trimmed["data:".len()..].trim())
        }) {
            let Ok(value) = serde_json::from_str::<Value>(frame) else {
                continue;
            };
            if value.get("method").and_then(Value::as_str) == Some("notifications/progress") {
                if let (Some(token), Some(callback)) = (progress_token, on_progress.as_mut())
                    && let Some(progress) = progress_tick(value.get("params"), token)
                {
                    (**callback)(progress);
                }
                continue;
            }
            if value.get("result").is_some() || value.get("error").is_some() {
                return Ok(value);
            }
        }
        return Err(McpError::Protocol(format!(
            "MCP {method} SSE body carried no JSON-RPC message"
        )));
    }
    serde_json::from_str::<Value>(body)
        .map_err(|error| McpError::Protocol(format!("MCP {method} invalid JSON body: {error}")))
}

struct McpTool {
    public_name: String,
    original_name: String,
    description: String,
    parameters: Value,
    output_schema: Option<Value>,
    risk: tool::RiskProfile,
    /// Transport-failure retry is safe only for read-only or explicitly
    /// idempotent operations. A generic mutating MCP tool may have applied its
    /// side effect before the pipe broke, so retrying would duplicate it.
    retry_safe: bool,
    accesses: tool::ToolAccesses,
    /// The reconnect-capable server handle. When a tool call fails with a
    /// connection error, [`execute`](Tool::execute) resets the connection and
    /// retries once — transparent crash recovery without waiting for the next
    /// refresh.
    server: Arc<McpServer>,
}

/// A reconnect-capable MCP server connection. Wraps `McpClient` with the
/// original config so a crashed server (stdout closed mid-session) can be
/// transparently restarted. Used by `McpTool::call` to retry on connection
/// failure.
pub struct McpServer {
    config: McpServerConfig,
    server_name: String,
    read_only: bool,
    client: tokio::sync::Mutex<Option<Arc<McpClient>>>,
    /// Installed before or after the first connect; re-applied whenever a new
    /// client replaces a dead one.
    notification_sink: std::sync::Mutex<Option<McpNotificationSink>>,
}

impl McpServer {
    pub fn new(config: McpServerConfig, server_name: String, read_only: bool) -> Self {
        Self {
            config,
            server_name,
            read_only,
            client: tokio::sync::Mutex::new(None),
            notification_sink: std::sync::Mutex::new(None),
        }
    }

    /// Install (or replace) the consumer for this server's non-progress
    /// notifications. Safe to call before the first connection; the sink is
    /// retained and attached to every replacement client.
    pub(super) async fn set_notification_sink(&self, sink: McpNotificationSink) {
        *self
            .notification_sink
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&sink));
        if let Some(client) = self.client.lock().await.as_ref() {
            client.set_notification_sink(sink);
        }
    }

    /// Connect (or reconnect) and return the live client. If a client is
    /// already held, it is reused; otherwise a fresh connection is established.
    async fn ensure_connected(&self) -> Result<Arc<McpClient>, String> {
        let spec = if let Some(url) = &self.config.url {
            protocol::security::AssetSpec::RemoteEndpoint {
                url: url.clone(),
                headers: self
                    .config
                    .environment
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            }
        } else {
            protocol::security::AssetSpec::Process {
                command: self.config.command.clone(),
                env: self
                    .config
                    .environment
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            }
        };

        let locator = match &self.config.sandbox_root {
            Some(root) => protocol::security::AssetLocator::WorkspaceMcp {
                workspace_root: root.to_string_lossy().to_string(),
                name: self.server_name.clone(),
            },
            None => protocol::security::AssetLocator::UserMcp {
                name: self.server_name.clone(),
            },
        };

        if !is_asset_attested(&locator, &spec) {
            *self.client.lock().await = None;
            return Err(format!(
                "MCP server '{}' is quarantined or expired: external executable [{}] has not been attested in AssetAttestationLedger (run `/trust` to authorize)",
                self.server_name,
                spec.summary()
            ));
        }

        if let Some(root) = &self.config.sandbox_root
            && !is_sandbox_trusted(root)
        {
            *self.client.lock().await = None;
            return Err(format!(
                "project MCP '{}' is quarantined because its extension content is not currently attested (run `/trust mcp` or `/trust` to authorize)",
                self.server_name
            ));
        }
        let mut guard = self.client.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let client = McpClient::connect(&self.config).await?;
        if let Some(sink) = self
            .notification_sink
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            client.set_notification_sink(sink);
        }
        *guard = Some(client.clone());
        Ok(client)
    }

    /// Drop the current connection so the next `ensure_connected` reconnects.
    /// Called when a request fails with a connection error.
    pub async fn reset(&self) {
        *self.client.lock().await = None;
    }

    /// The server's display name (for logging/diagnostics).
    pub fn name(&self) -> &str {
        &self.server_name
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn defer_tools(&self) -> bool {
        self.config.defer_tools
    }

    /// Whether a server-advertised tool (original name) passes this server's
    /// `allow_tools`/`deny_tools` configuration — see
    /// [`McpServerConfig::admits_tool`].
    pub fn admits_tool(&self, original_name: &str) -> bool {
        self.config.admits_tool(original_name)
    }

    /// Send a best-effort `notifications/cancelled` for `request_id`.
    async fn cancel_request(&self, request_id: u64) {
        let client = self.client.lock().await.clone();
        if let Some(client) = client {
            let _ = client
                .notify(
                    "notifications/cancelled",
                    json!({"requestId": request_id, "reason": "cancelled"}),
                )
                .await;
        }
    }

    /// Execute one read-only endpoint (`resources/list` / `resources/read`)
    /// with cancellation and progress.
    async fn request_endpoint(
        &self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
        on_progress: &mut (dyn FnMut(tool::ToolProgress) + Send),
        request_id: Option<&Arc<AtomicU64>>,
    ) -> Result<Value, McpError> {
        let client = self.ensure_connected().await.map_err(McpError::Transport)?;
        let result = client
            .request_with(
                method,
                params,
                Some(cancel),
                Some(&mut *on_progress),
                request_id,
            )
            .await;
        if matches!(result, Err(McpError::Transport(_))) {
            self.reset().await;
        }
        result
    }

    /// Execute `tools/call` with cancellation and progress.
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        cancel: &CancellationToken,
        on_progress: &mut (dyn FnMut(tool::ToolProgress) + Send),
        request_id: Option<&Arc<AtomicU64>>,
        retry_safe: bool,
    ) -> Result<Value, McpError> {
        let payload = json!({"name": name, "arguments": arguments});
        let client = self.ensure_connected().await.map_err(McpError::Transport)?;
        match client
            .request_with(
                "tools/call",
                payload.clone(),
                Some(cancel),
                Some(&mut *on_progress),
                request_id,
            )
            .await
        {
            Ok(result) => Ok(result),
            Err(McpError::Cancelled) => Err(McpError::Cancelled),
            Err(McpError::Timeout(message)) => Err(McpError::Timeout(message)),
            Err(error) if error.is_transport() => {
                tracing::warn!(
                    server = %self.server_name,
                    tool = %name,
                    %error,
                    "MCP transport error, reconnecting{}",
                    if retry_safe { " and retrying once" } else { "" }
                );
                self.reset().await;
                if !retry_safe {
                    return Err(error);
                }
                let client = self.ensure_connected().await.map_err(McpError::Transport)?;
                client
                    .request_with(
                        "tools/call",
                        payload,
                        Some(cancel),
                        Some(&mut *on_progress),
                        request_id,
                    )
                    .await
            }
            Err(error) => Err(error),
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.public_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters.clone()
    }

    fn output_schema(&self) -> Option<Value> {
        self.output_schema.clone()
    }

    fn risk_profile(&self) -> tool::RiskProfile {
        self.risk
    }

    fn accesses(&self, _arguments: &Value) -> tool::ToolAccesses {
        self.accesses.clone()
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        Some(MCP_REQUEST_TIMEOUT)
    }

    fn exposure(&self) -> tool::ToolExposure {
        if self.server.defer_tools() {
            tool::ToolExposure::Deferred
        } else {
            tool::ToolExposure::Direct
        }
    }

    async fn execute(
        &self,
        ctx: &::tool::ToolContext,
        arguments: Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        let request_id = Arc::new(AtomicU64::new(0));
        let done = CancellationToken::new();
        let watcher_done = done.clone();
        let watcher_server = Arc::clone(&self.server);
        let watcher_id = Arc::clone(&request_id);
        let watcher_cancel = ctx.cancel_token.clone();
        let watcher = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = watcher_cancel.cancelled() => {
                    let id = watcher_id.load(Ordering::Relaxed);
                    if id != 0 {
                        watcher_server.cancel_request(id).await;
                    }
                }
                _ = watcher_done.cancelled() => {}
            }
        });

        let mut progress = |progress: tool::ToolProgress| ctx.emit_progress(progress);
        let result = self
            .server
            .call_tool(
                &self.original_name,
                arguments,
                &ctx.cancel_token,
                &mut progress,
                Some(&request_id),
                self.retry_safe,
            )
            .await;
        done.cancel();
        watcher.abort();

        match result {
            Ok(result) => Ok(render_tool_result(
                &result,
                &format!(
                    "tool `{}` on MCP server `{}`",
                    self.original_name,
                    self.server.name()
                ),
            )),
            Err(McpError::Cancelled) => Ok(tool::ToolOutput::Error {
                message: format!("MCP tool `{}` was cancelled", self.public_name),
                detail: Some(
                    "The server was sent notifications/cancelled and may still be stopping."
                        .to_string(),
                ),
            }),
            Err(error) => Err(::tool::ToolError::execution(
                self.public_name.clone(),
                error.to_string(),
            )),
        }
    }
}

pub async fn load_mcp_tools(configs: &HashMap<String, McpServerConfig>) -> McpLoadResult {
    let mut tools = Vec::new();
    let mut statuses = Vec::new();
    let mut servers = Vec::new();
    let mut names = configs.keys().cloned().collect::<Vec<_>>();
    names.sort();

    let mut taken = HashSet::new();
    for name in names {
        let config = &configs[&name];
        if !config.enabled {
            statuses.push((name, McpConnectionStatus::Disabled));
            continue;
        }

        let loaded = timeout(MCP_CONNECT_TIMEOUT, async {
            let server = Arc::new(McpServer::new(
                config.clone(),
                name.clone(),
                config.read_only,
            ));
            let server_tools = build_tools_from_server(&server, &mut taken).await?;
            Ok::<_, String>((server, server_tools))
        })
        .await;

        match loaded {
            Ok(Ok((server, server_tools))) => {
                let count = server_tools.len();
                tools.extend(server_tools);
                statuses.push((name, McpConnectionStatus::Connected { tools: count }));
                servers.push(server);
            }
            Ok(Err(error)) => statuses.push((name, McpConnectionStatus::Failed(error))),
            Err(_) => statuses.push((
                name,
                McpConnectionStatus::Failed("connection timed out".to_string()),
            )),
        }
    }

    McpLoadResult {
        tools,
        statuses,
        servers,
    }
}

/// Reconnect to each server and re-discover its tools. Returns the refreshed
/// tool list and statuses (same shape as [`load_mcp_tools`], but reusing
/// existing server handles). Called by the background refresh loop.
pub async fn refresh_mcp_tools(
    servers: &[Arc<McpServer>],
) -> (Vec<Arc<dyn Tool>>, Vec<(String, McpConnectionStatus)>) {
    let mut tools = Vec::new();
    let mut statuses = Vec::new();

    let mut taken = HashSet::new();
    for server in servers {
        // Reset the connection so ensure_connected opens a fresh one.
        server.reset().await;
        match timeout(MCP_CONNECT_TIMEOUT, async {
            build_tools_from_server(server, &mut taken).await
        })
        .await
        {
            Ok(Ok(server_tools)) => {
                let count = server_tools.len();
                tools.extend(server_tools);
                statuses.push((
                    server.name().to_string(),
                    McpConnectionStatus::Connected { tools: count },
                ));
            }
            Ok(Err(error)) => {
                statuses.push((
                    server.name().to_string(),
                    McpConnectionStatus::Failed(error),
                ));
            }
            Err(_) => statuses.push((
                server.name().to_string(),
                McpConnectionStatus::Failed("connection timed out".to_string()),
            )),
        }
    }

    (tools, statuses)
}

/// Connect a single server from its config and discover its tools. Returns the
/// reconnect-capable handle alongside its tool adapters, or a failure string.
/// Used to bring a server online on demand (e.g. the `/mcp` modal re-enabling a
/// previously disabled server, which never went through [`load_mcp_tools`]).
pub async fn connect_server(
    name: &str,
    config: &McpServerConfig,
) -> Result<(Arc<McpServer>, Vec<Arc<dyn Tool>>), String> {
    timeout(MCP_CONNECT_TIMEOUT, async {
        let server = Arc::new(McpServer::new(
            config.clone(),
            name.to_string(),
            config.read_only,
        ));
        let mut taken = HashSet::new();
        let tools = build_tools_from_server(&server, &mut taken).await?;
        Ok::<_, String>((server, tools))
    })
    .await
    .map_err(|_| "connection timed out".to_string())?
}

/// Re-run `tools/list` over the existing connection without dropping it.
///
/// This is the list_changed path: a catalog edit should not fail an unrelated
/// in-flight tool call. If the live request fails, the caller can fall back to
/// [`reconnect_server`], which resets the transport.
pub async fn refresh_server_tools(
    server: &Arc<McpServer>,
) -> (Vec<Arc<dyn Tool>>, McpConnectionStatus) {
    let mut taken = HashSet::new();
    match timeout(
        MCP_CONNECT_TIMEOUT,
        build_tools_from_server(server, &mut taken),
    )
    .await
    {
        Ok(Ok(tools)) => {
            let count = tools.len();
            (tools, McpConnectionStatus::Connected { tools: count })
        }
        Ok(Err(error)) => (Vec::new(), McpConnectionStatus::Failed(error)),
        Err(_) => (
            Vec::new(),
            McpConnectionStatus::Failed("connection timed out".to_string()),
        ),
    }
}

/// Reset and re-establish one server's connection, re-discovering its tools.
/// The single-server analogue of [`refresh_mcp_tools`]; returns the refreshed
/// tools and the connection status. Used by the `/mcp` modal's reconnect action.
pub async fn reconnect_server(
    server: &Arc<McpServer>,
) -> (Vec<Arc<dyn Tool>>, McpConnectionStatus) {
    server.reset().await;
    let mut taken = HashSet::new();
    match timeout(
        MCP_CONNECT_TIMEOUT,
        build_tools_from_server(server, &mut taken),
    )
    .await
    {
        Ok(Ok(tools)) => {
            let count = tools.len();
            (tools, McpConnectionStatus::Connected { tools: count })
        }
        Ok(Err(error)) => (Vec::new(), McpConnectionStatus::Failed(error)),
        Err(_) => (
            Vec::new(),
            McpConnectionStatus::Failed("connection timed out".to_string()),
        ),
    }
}

/// Build [`McpTool`] adapters from a connected server's `tools/list` response.
/// Each tool holds the [`McpServer`] handle so it can auto-reconnect on
/// failure.
async fn build_tools_from_server(
    server: &Arc<McpServer>,
    taken: &mut HashSet<String>,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    let client = server.ensure_connected().await?;
    let result = client.request("tools/list", json!({})).await?;
    let definitions = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP tools/list response has no tools array".to_string())?;
    // Probe the optional resource surface once, with the connect budget, so a
    // server that does not implement it does not get two permanently-failing
    // tools in the deferred catalog.
    let resources_supported = server.config.enable_resources
        && timeout(
            MCP_CONNECT_TIMEOUT,
            client.request("resources/list", json!({})),
        )
        .await
        .is_ok_and(|result| result.is_ok());

    // `sanitize_name` is a lossy, many-to-one mapping (every non-`[A-Za-z0-9_]`
    // char folds to `_`), so distinct server-side tool names can collapse to the
    // same public name (e.g. `read-file` and `read.file` both become
    // `read_file`). Left unchecked, the second adapter would be silently dropped
    // by the first-wins `ToolSet`, making that tool unreachable. `taken` is
    // shared across every server in one load/refresh so the names are globally
    // unique, and the suffix is a stable hash of the raw identity. `original_name`
    // is untouched, so `tools/call` still targets the correct server-side tool.
    let mut tools: Vec<Arc<dyn Tool>> = definitions
        .iter()
        // Config-time scoping (follow-up): `allow_tools`/`deny_tools`
        // filter on the *original* server-declared name, before sanitization
        // can collapse distinct names. Denied tools are never adapted, so the
        // model never sees them and cannot call them.
        .filter(|definition| {
            definition
                .get("name")
                .and_then(Value::as_str)
                .map(|name| server.admits_tool(name))
                .unwrap_or(true)
        })
        .map(|definition| {
            let original_name = definition
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "MCP tool has no name".to_string())?
                .to_string();
            let public_name = unique_name(
                format!(
                    "mcp__{}__{}",
                    sanitize_name(server.name()),
                    sanitize_name(&original_name)
                ),
                &format!("{}\0{}", server.name(), original_name),
                taken,
            );
            let description = definition
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("MCP tool")
                .to_string();
            let parameters = definition
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object"}));
            let output_schema = definition.get("outputSchema").cloned();
            let annotations = definition.get("annotations");
            let hint = |name: &str| {
                annotations
                    .and_then(|annotations| annotations.get(name))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            let read_only = hint("readOnlyHint") || server.read_only();
            let destructive = hint("destructiveHint");
            let idempotent = hint("idempotentHint");
            let open_world = hint("openWorldHint");
            // Scheduling uses the same annotation: a read-only MCP call cannot
            // corrupt a concurrent write, while anything else is treated as a
            // global-exclusive effect because the server does not expose a
            // locatable path.
            let (risk, accesses) = if read_only {
                (tool::RiskProfile::ReadOnly, tool::ToolAccesses::none())
            } else if destructive {
                (tool::RiskProfile::Destructive, tool::ToolAccesses::all())
            } else if idempotent {
                (
                    tool::RiskProfile::IdempotentMutation,
                    tool::ToolAccesses::all(),
                )
            } else if open_world || annotations.is_none() {
                (tool::RiskProfile::NetworkAccess, tool::ToolAccesses::all())
            } else {
                (
                    tool::RiskProfile::IdempotentMutation,
                    tool::ToolAccesses::all(),
                )
            };
            Ok(Arc::new(McpTool {
                public_name,
                original_name,
                description,
                parameters,
                output_schema,
                risk,
                retry_safe: read_only || idempotent,
                accesses,
                server: Arc::clone(server),
            }) as Arc<dyn Tool>)
        })
        .collect::<Result<Vec<Arc<dyn Tool>>, String>>()?;

    if resources_supported {
        let list_name = unique_name(
            format!("mcp__{}__list_resources", sanitize_name(server.name())),
            &format!("{}\0__list_resources", server.name()),
            taken,
        );
        let read_name = unique_name(
            format!("mcp__{}__read_resource", sanitize_name(server.name())),
            &format!("{}\0__read_resource", server.name()),
            taken,
        );
        tools.push(Arc::new(McpResourceTool {
            public_name: list_name,
            operation: ResourceOp::List,
            server: Arc::clone(server),
        }));
        tools.push(Arc::new(McpResourceTool {
            public_name: read_name,
            operation: ResourceOp::Read,
            server: Arc::clone(server),
        }));
    }
    Ok(tools)
}

/// Which MCP resource endpoint one adapter exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceOp {
    List,
    Read,
}

/// A model-facing adapter for `resources/list` / `resources/read`.
///
/// MCP resources are a first-class part of the protocol, not a tool-server
/// extension. Exposing them as two read-only tools keeps the model's discovery
/// surface uniform: `tool_search` can activate them, the permission chain sees
/// a normal `ReadOnly` hazard, and `read` remains the only place a filesystem
/// path is resolved.
struct McpResourceTool {
    public_name: String,
    operation: ResourceOp,
    server: Arc<McpServer>,
}

#[async_trait]
impl Tool for McpResourceTool {
    fn name(&self) -> &str {
        &self.public_name
    }

    fn description(&self) -> &str {
        match self.operation {
            ResourceOp::List => "List the MCP server's resources and resource templates.",
            ResourceOp::Read => "Read one MCP resource by URI.",
        }
    }

    fn parameters_schema(&self) -> Value {
        match self.operation {
            ResourceOp::List => json!({
                "type": "object",
                "properties": {
                    "cursor": {"type": "string", "description": "Opaque pagination cursor"}
                },
                "additionalProperties": false
            }),
            ResourceOp::Read => json!({
                "type": "object",
                "properties": {
                    "uri": {"type": "string", "description": "Resource URI returned by list_resources"}
                },
                "required": ["uri"],
                "additionalProperties": false
            }),
        }
    }

    fn risk_profile(&self) -> tool::RiskProfile {
        tool::RiskProfile::ReadOnly
    }

    /// A resource read touches remote state, not the local filesystem. The
    /// empty declaration is explicit: it is parallelizable, and it never claims
    /// a path the scheduler would have to serialize against.
    fn accesses(&self, _arguments: &Value) -> tool::ToolAccesses {
        tool::ToolAccesses::none()
    }

    fn exposure(&self) -> tool::ToolExposure {
        if self.server.defer_tools() {
            tool::ToolExposure::Deferred
        } else {
            tool::ToolExposure::Direct
        }
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        Some(MCP_REQUEST_TIMEOUT)
    }

    async fn execute(
        &self,
        ctx: &::tool::ToolContext,
        arguments: Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        let request_id = Arc::new(AtomicU64::new(0));
        let done = CancellationToken::new();
        let watcher_done = done.clone();
        let watcher_server = Arc::clone(&self.server);
        let watcher_id = Arc::clone(&request_id);
        let watcher_cancel = ctx.cancel_token.clone();
        let watcher = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = watcher_cancel.cancelled() => {
                    let id = watcher_id.load(Ordering::Relaxed);
                    if id != 0 {
                        watcher_server.cancel_request(id).await;
                    }
                }
                _ = watcher_done.cancelled() => {}
            }
        });
        let mut progress = |progress: tool::ToolProgress| ctx.emit_progress(progress);
        let result = match self.operation {
            ResourceOp::List => {
                let mut params = json!({});
                if let Some(cursor) = arguments
                    .get("cursor")
                    .and_then(Value::as_str)
                    .filter(|cursor| !cursor.is_empty())
                {
                    params["cursor"] = json!(cursor);
                }
                self.server
                    .request_endpoint(
                        "resources/list",
                        params,
                        &ctx.cancel_token,
                        &mut progress,
                        Some(&request_id),
                    )
                    .await
                    .map(|result| {
                        let content = serde_json::to_string_pretty(&result).unwrap_or_default();
                        tool::ToolOutput::Structured {
                            content: tool::untrusted::frame_with_note(
                                tool::untrusted::Origin::Mcp,
                                &format!("resources/list on MCP server `{}`", self.server.name()),
                                &content,
                            ),
                            value: result,
                            failed: false,
                        }
                    })
            }
            ResourceOp::Read => {
                let uri = arguments
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        tool::ToolError::invalid_args(self.public_name.clone(), "missing `uri`")
                    })?;
                self.server
                    .request_endpoint(
                        "resources/read",
                        json!({"uri": uri}),
                        &ctx.cancel_token,
                        &mut progress,
                        Some(&request_id),
                    )
                    .await
                    .map(|result| render_resource_result(&result))
            }
        };
        done.cancel();
        watcher.abort();
        result.map_err(|error| match error {
            McpError::Cancelled => tool::ToolError::cancelled(self.public_name.clone()),
            other => tool::ToolError::execution(self.public_name.clone(), other.to_string()),
        })
    }
}

/// Project one `resources/read` response into a model-facing output.
///
/// Text resources are third-party bytes served by an external process, so they
/// are framed as an `<untrusted origin="mcp">` region (ADR-0228). A web page gets
/// that framing today; an MCP resource reaches the model through the same door
/// and must not be the one door without it.
fn render_resource_result(result: &Value) -> tool::ToolOutput {
    use tool::tool_output::MediaPart;

    let mut parts = Vec::new();
    if let Some(contents) = result.get("contents").and_then(Value::as_array) {
        for item in contents {
            let uri = item.get("uri").and_then(Value::as_str).unwrap_or_default();
            let mime = item.get("mimeType").and_then(Value::as_str);
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                parts.push(MediaPart::text(tool::untrusted::frame_with_note(
                    tool::untrusted::Origin::Mcp,
                    &format!("source: {uri}"),
                    text,
                )));
            } else if let Some(blob) = item.get("blob").and_then(Value::as_str) {
                match mime {
                    Some(mime) if mime.starts_with("image/") => {
                        parts.push(mcp_image_part(mime, blob));
                    }
                    _ => parts.push(MediaPart::Resource {
                        uri: uri.to_string(),
                        mime: mime.map(str::to_string),
                        name: item.get("name").and_then(Value::as_str).map(str::to_string),
                        text: None,
                    }),
                }
            }
        }
    }
    if parts.is_empty() {
        return tool::ToolOutput::text(tool::untrusted::frame(
            tool::untrusted::Origin::Mcp,
            &result.to_string(),
        ));
    }
    tool::ToolOutput::media(parts)
}

async fn write_message(stdin: &mut ChildStdin, message: &Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    if line.len().saturating_add(1) > MAX_MCP_MESSAGE_BYTES {
        return Err(format!(
            "MCP JSON-RPC message exceeds {} MiB",
            MAX_MCP_MESSAGE_BYTES / (1024 * 1024)
        ));
    }
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|error| error.to_string())?;
    stdin.flush().await.map_err(|error| error.to_string())
}

async fn read_bounded_line<R>(reader: &mut R, limit: usize) -> Result<Vec<u8>, String>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let (consumed, newline) = {
            let available = reader.fill_buf().await.map_err(|error| error.to_string())?;
            if available.is_empty() {
                return Err("MCP server closed stdout".to_string());
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let end = newline.map_or(available.len(), |position| position + 1);
            if line.len().saturating_add(end) > limit {
                return Err(format!(
                    "MCP JSON-RPC message exceeds {} MiB",
                    limit / (1024 * 1024)
                ));
            }
            line.extend_from_slice(&available[..end]);
            (end, newline.is_some())
        };
        reader.consume(consumed);
        if newline {
            return Ok(line);
        }
    }
}

async fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value, String> {
    loop {
        let line = read_bounded_line(stdout, MAX_MCP_MESSAGE_BYTES).await?;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        return serde_json::from_slice(&line)
            .map_err(|error| format!("invalid MCP JSON-RPC message: {error}"));
    }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Provider function-name ceiling (OpenAI's limit is 64 characters).
const MCP_MAX_TOOL_NAME: usize = 64;

/// Reserve a collision-free, length-bounded public name within `taken`.
///
/// The base name is kept when it is unique and fits. Any collision or overflow
/// gets a **stable** suffix derived from the raw `(server, tool)` identity, not
/// the lowest free integer: a numeric suffix depends on the order a server
/// happens to return `tools/list`, so a reconnect could silently re-point a
/// historical call at a different tool. A hash suffix is order-independent and
/// survives restarts.
fn unique_name(name: String, raw_key: &str, taken: &mut HashSet<String>) -> String {
    let hash = tool::vfs_short_hash(raw_key.as_bytes());
    let fit = |base: &str| {
        if base.len() + 1 + hash.len() <= MCP_MAX_TOOL_NAME {
            format!("{base}_{hash}")
        } else {
            let keep = MCP_MAX_TOOL_NAME - 1 - hash.len();
            let mut end = keep;
            while !base.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}_{hash}", &base[..end])
        }
    };
    let base = if name.len() > MCP_MAX_TOOL_NAME {
        fit(&name)
    } else {
        name
    };
    if taken.insert(base.clone()) {
        return base;
    }
    let mut candidate = fit(&base);
    let mut attempt = 1u32;
    while !taken.insert(candidate.clone()) {
        let suffix = format!("{hash}{attempt}");
        let keep = MCP_MAX_TOOL_NAME - 1 - suffix.len();
        let mut end = keep.min(base.len());
        while !base.is_char_boundary(end) {
            end -= 1;
        }
        candidate = format!("{}_{suffix}", &base[..end]);
        attempt += 1;
    }
    candidate
}

const MCP_MAX_OUTPUT_BYTES: usize = 16_384;
const MCP_MAX_OUTPUT_LINES: usize = 200;
const MCP_PREVIEW_HEAD_LINES: usize = 50;
const MCP_PREVIEW_TAIL_LINES: usize = 20;

pub(crate) fn compact_mcp_output(raw: &str) -> String {
    let line_count = raw.lines().count();
    if raw.len() <= MCP_MAX_OUTPUT_BYTES && line_count <= MCP_MAX_OUTPUT_LINES {
        return raw.to_string();
    }

    // Spill full raw output to a temp file in $TMPDIR per [INV-MCP-05].
    let temp_dir = std::env::temp_dir();
    let file_name = format!(
        "nuo-mcp-output-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let temp_path = temp_dir.join(file_name);
    if let Err(err) = std::fs::write(&temp_path, raw) {
        tracing::warn!(
            error = %err,
            path = %temp_path.display(),
            "failed to spill oversized MCP output to temp file"
        );
    }

    let lines: Vec<&str> = raw.lines().collect();
    let head = lines
        .iter()
        .take(MCP_PREVIEW_HEAD_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let tail_start = lines.len().saturating_sub(MCP_PREVIEW_TAIL_LINES);
    let tail = lines[tail_start..].join("\n");
    let skipped = lines
        .len()
        .saturating_sub(MCP_PREVIEW_HEAD_LINES + MCP_PREVIEW_TAIL_LINES);

    format!(
        "[MCP Output truncated: {line_count} lines, {} bytes. Exceeded 16KB/200-line safety boundary [INV-MCP-05]]\n\
         [Full output saved to: {}]\n\n\
         --- Output preview (head) ---\n\
         {head}\n\
         ... [truncated {skipped} lines] ...\n\
         --- Output preview (tail) ---\n\
         {tail}",
        raw.len(),
        temp_path.display()
    )
}

/// Base64 image ceiling for one MCP content part. The image normalizer handles
/// files the agent reads; a remote server can still return an arbitrarily large
/// inline image, so the same boundary applies here.
const MAX_MCP_IMAGE_BASE64_BYTES: usize = 8 * 1024 * 1024;

fn mcp_image_part(mime: &str, data: &str) -> tool::tool_output::MediaPart {
    if data.len() > MAX_MCP_IMAGE_BASE64_BYTES {
        tool::tool_output::MediaPart::text(format!(
            "[image omitted: {mime}, {} bytes exceeds the inline media ceiling]",
            data.len()
        ))
    } else {
        tool::tool_output::MediaPart::image(mime, data)
    }
}

/// Project a `tools/call` result into a tool output.
///
/// A tool result carries the peer's bytes, so every text it returns is framed
/// as untrusted data. The note names the tool and server the model is talking
/// to; framing is presentation, the tool-result header still owns the call
/// identity (ADR-0228).
fn render_tool_result(result: &Value, note: &str) -> tool::ToolOutput {
    use tool::tool_output::MediaPart;

    let frame =
        |text: &str| tool::untrusted::frame_with_note(tool::untrusted::Origin::Mcp, note, text);

    let failed = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let structured = result.get("structuredContent").cloned();
    let mut parts = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for item in content {
            match item.get("type").and_then(Value::as_str).unwrap_or("text") {
                "image" => {
                    if let (Some(mime), Some(data)) = (
                        item.get("mimeType").and_then(Value::as_str),
                        item.get("data").and_then(Value::as_str),
                    ) {
                        parts.push(mcp_image_part(mime, data));
                    }
                }
                "resource" => {
                    let resource = item.get("resource").unwrap_or(item);
                    parts.push(MediaPart::Resource {
                        uri: resource
                            .get("uri")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        mime: resource
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        name: resource
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        text: resource.get("text").and_then(Value::as_str).map(|text| {
                            tool::untrusted::frame_with_note(
                                tool::untrusted::Origin::Mcp,
                                &format!(
                                    "resource: {}",
                                    resource
                                        .get("uri")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                ),
                                text,
                            )
                        }),
                    });
                }
                _ => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        parts.push(MediaPart::text(frame(&compact_mcp_output(text))));
                    }
                }
            }
        }
    }
    let has_media = parts
        .iter()
        .any(|part| !matches!(part, MediaPart::Text { .. }));
    if has_media {
        return tool::ToolOutput::Media {
            parts,
            structured,
            failed,
        };
    }
    let text = parts
        .into_iter()
        .filter_map(|part| match part {
            MediaPart::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(value) = structured {
        return tool::ToolOutput::Structured {
            content: if text.is_empty() {
                frame(&value.to_string())
            } else {
                text
            },
            value,
            failed,
        };
    }
    if text.is_empty() {
        return tool::ToolOutput::text(frame(&result.to_string()));
    }
    if failed {
        return tool::ToolOutput::Error {
            message: text,
            detail: None,
        };
    }
    tool::ToolOutput::text(text)
}

#[cfg(test)]
mod tests;
