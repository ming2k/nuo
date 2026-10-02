//! Minimal MCP client support for local stdio servers.
//!
//! Each configured server is initialized once at startup. Its advertised tools
//! are adapted to muta's `Tool` trait and use the same agent execution path
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
use nuo_contracts::Tool;
use nuo_contracts::mcp::{McpConnectionStatus, McpServerConfig};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};

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

/// Pluggable universal asset attestation verifier (ADR-0243, ADR-0252).
pub type McpAttestationVerifier = Arc<
    dyn Fn(&nuo_contracts::security::AssetLocator, &nuo_contracts::security::AssetSpec) -> bool
        + Send
        + Sync,
>;

static ATTESTATION_VERIFIER: OnceLock<McpAttestationVerifier> = OnceLock::new();

/// Configure the universal attestation verifier used across all MCP scopes (ADR-0243, ADR-0252).
pub fn set_attestation_verifier(verifier: McpAttestationVerifier) {
    let _ = ATTESTATION_VERIFIER.set(verifier);
}

pub fn is_asset_attested(
    locator: &nuo_contracts::security::AssetLocator,
    spec: &nuo_contracts::security::AssetSpec,
) -> bool {
    ATTESTATION_VERIFIER
        .get()
        .map(|v| v(locator, spec))
        .unwrap_or(true)
}
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};

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

/// A typed MCP error: either a broken transport (retry-safe) or a protocol
/// result (never retry a protocol error — re-sending an already-applied
/// non-idempotent call would double a side effect).
#[derive(Debug)]
enum McpError {
    /// The stdio pipe broke: a failed write/read, the child exited, or a
    /// request timed out. The connection is unusable and a reconnect may help.
    Transport(String),
    /// The server replied with a JSON-RPC `error` object (or a malformed
    /// response). This is a deterministic server-side result — retrying the
    /// *same* call yields the *same* outcome, so never reconnect on it.
    Protocol(String),
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
            McpError::Transport(msg) | McpError::Protocol(msg) => f.write_str(msg),
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

struct McpTransport {
    process_tree: nuo_host::process::OwnedProcessTree,
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// One open connection to an MCP server: either a spawned stdio child or a
/// Streamable-HTTP endpoint. Both speak the same JSON-RPC message grammar, so
/// [`McpClient::request`] normalizes over this enum.
enum McpConnection {
    Stdio(McpTransport),
    Http {
        /// Absolute URL of the Streamable-HTTP endpoint.
        url: String,
        /// The owned transport (ADR-0200).
        client: netune::Client<netune::TlsConnector<netune::TcpConnector>>,
        /// Session id issued by the server (`Mcp-Session-Id` on initialize),
        /// echoed on every subsequent request. `None` until (and unless) the
        /// server assigns one.
        session_id: Option<String>,
    },
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
    async fn connect(config: &McpServerConfig) -> Result<Arc<Self>, String> {
        // Transport selection: a configured `url` reaches the server over
        // Streamable HTTP; otherwise `command` spawns a local stdio child.
        // (`url` wins when both are set, mirroring the common MCP client
        // configuration shape.)
        let connection = if let Some(url) = config.url.as_deref() {
            // Validate the URL up front so a misconfigured server fails at
            // connect time with a clear message, not on first request.
            netune::Target::from_url(url)
                .map_err(|error| format!("invalid MCP url '{url}': {error}"))?;
            let connector = netune::TlsConnector::platform(netune::TcpConnector::new())
                .map_err(|error| format!("failed to build MCP HTTP client: {error}"))?;
            McpConnection::Http {
                url: url.to_string(),
                client: netune::Client::new(
                    connector,
                    netune::Pool::default(),
                    netune::ClientConfig::default(),
                ),
                session_id: None,
            }
        } else {
            let (program, args) = config
                .command
                .split_first()
                .ok_or_else(|| "MCP command must not be empty".to_string())?;

            let mut command = if let Some(root) = &config.sandbox_root {
                // Project-authored servers are untrusted executable extensions.
                // Extension trust permits loading their exact declaration, but it
                // never grants ambient host or workspace-write/network authority.
                nuo_host::workspace_sandbox::command_with_environment(
                    program,
                    args,
                    &config.environment,
                    root,
                    nuo_host::workspace_sandbox::WorkspaceAccess::ReadOnly,
                    nuo_host::workspace_sandbox::NetworkAccess::Disabled,
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

            let (mut child, process_tree) = nuo_host::process::spawn_owned(&mut command)
                .map_err(|error| format!("failed to spawn '{}': {}", program, error))?;
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| "MCP server stdin is unavailable".to_string())?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| "MCP server stdout is unavailable".to_string())?;
            McpConnection::Stdio(McpTransport {
                process_tree,
                _child: child,
                stdin,
                stdout: BufReader::new(stdout),
            })
        };
        let client = Arc::new(Self {
            transport: Mutex::new(connection),
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
                        "name": "muta",
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

    async fn request(&self, method: &str, mut params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // The 2026-07-28 revision carries the protocol version, client
        // identity, and capabilities on every request via `params._meta`
        // under reserved `io.modelcontextprotocol/*` keys (SEP-2575); older
        // revisions send them once, in `initialize`.
        let params = if matches!(*self.dialect.read().await, McpDialect::Stateless) {
            let mut object = params
                .as_object_mut()
                .cloned()
                .unwrap_or_else(serde_json::Map::new);
            object.insert(
                "_meta".to_string(),
                json!({
                    "io.modelcontextprotocol/protocolVersion": MCP_STATELESS_VERSION,
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "muta",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "io.modelcontextprotocol/clientCapabilities": {}
                }),
            );
            Value::Object(object)
        } else {
            params
        };
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut connection = self.transport.lock().await;
        let response = match &mut *connection {
            McpConnection::Stdio(transport) => {
                write_message(&mut transport.stdin, &payload)
                    .await
                    .map_err(|msg| {
                        McpError::Transport(format!("MCP {method} write failed: {msg}"))
                    })?;
                // Bound how long a single request can hang the agent: a server that
                // keeps the pipe open without answering is released rather than
                // pinning the transport lock forever.
                timeout(MCP_REQUEST_TIMEOUT, async {
                    loop {
                        let response =
                            read_message(&mut transport.stdout).await.map_err(|msg| {
                                McpError::Transport(format!("MCP {method} read failed: {msg}"))
                            })?;
                        if response.get("id").and_then(Value::as_u64) != Some(id) {
                            // An unrelated notification/async reply: skip, keep reading
                            // for *our* id.
                            continue;
                        }
                        // Our reply — break out of the async block with it.
                        break Ok(response);
                    }
                })
                .await
                .map_err(|_| {
                    McpError::Transport(format!(
                        "MCP {method} timed out after {}s",
                        MCP_REQUEST_TIMEOUT.as_secs()
                    ))
                })??
            }
            McpConnection::Http {
                url,
                client,
                session_id,
            } => {
                let (target, path) = netune::Target::from_url(url).map_err(|error| {
                    McpError::Transport(format!("invalid MCP url '{url}': {error}"))
                })?;
                let mut head = netune::RequestHead::new(netune::Method::POST, path)
                    .with_header("accept", "application/json, text/event-stream")
                    .with_header("content-type", "application/json");
                if let Some(session) = session_id.as_deref() {
                    head = head.with_header("mcp-session-id", session);
                }
                let body = serde_json::to_vec(&payload)
                    .map_err(|error| McpError::Protocol(format!("MCP {method} encode: {error}")))?;
                // A connection-level failure (DNS, refused, TLS, reset) is
                // transport-class and safe to reconnect-retry, while any
                // delivered body is parsed below and classified as protocol.
                // The deadline covers the body too, as the previous client's
                // per-request timeout did.
                let exchange = tokio::time::timeout(
                    MCP_REQUEST_TIMEOUT,
                    client.request(&target, head, Some(body.into())),
                )
                .await
                .map_err(|_| {
                    McpError::Transport(format!(
                        "MCP {method} timed out after {}s",
                        MCP_REQUEST_TIMEOUT.as_secs()
                    ))
                })?
                .map_err(|error| {
                    McpError::Transport(format!("MCP {method} HTTP failed: {error}"))
                })?;
                let status = exchange.head.status;
                if !status.is_success() {
                    // 4xx/5xx: the server answered and refused — a protocol
                    // result, not a broken pipe.
                    let mut exchange = exchange;
                    let detail = exchange
                        .body
                        .read_to_end()
                        .await
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap_or_default();
                    return Err(McpError::Protocol(format!(
                        "MCP {method} HTTP {status}: {detail}"
                    )));
                }
                if session_id.is_none() {
                    *session_id = exchange
                        .head
                        .headers
                        .get("mcp-session-id")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                }
                let content_type = exchange
                    .head
                    .headers
                    .get("content-type")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let mut exchange = exchange;
                let body = exchange
                    .body
                    .read_to_end()
                    .await
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .map_err(|error| McpError::Transport(format!("MCP {method} body: {error}")))?;
                parse_http_message(&body, &content_type, method)?
            }
        };

        // At this point the response is well-formed and carries our id: a
        // JSON-RPC `error` object is a *protocol* result, not a transport
        // failure — the caller must NOT reconnect-and-retry it.
        if let Some(error) = response.get("error") {
            return Err(McpError::Protocol(format!("MCP {method} error: {error}")));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| McpError::Protocol(format!("MCP {method} response has no result")))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut connection = self.transport.lock().await;
        match &mut *connection {
            McpConnection::Stdio(transport) => write_message(&mut transport.stdin, &payload)
                .await
                .map_err(|msg| McpError::Transport(format!("MCP {method} notify failed: {msg}"))),
            McpConnection::Http {
                url,
                client,
                session_id,
            } => {
                let (target, path) = netune::Target::from_url(url).map_err(|error| {
                    McpError::Transport(format!("invalid MCP url '{url}': {error}"))
                })?;
                let mut head = netune::RequestHead::new(netune::Method::POST, path)
                    .with_header("accept", "application/json, text/event-stream")
                    .with_header("content-type", "application/json");
                if let Some(session) = session_id.as_deref() {
                    head = head.with_header("mcp-session-id", session);
                }
                let body = serde_json::to_vec(&payload)
                    .map_err(|error| McpError::Protocol(format!("MCP {method} encode: {error}")))?;
                // Notifications have no id and thus no reply to correlate:
                // spec-compliant servers answer 202 with (possibly) an empty
                // body. Transport failure still classifies as reconnect-safe.
                let exchange = tokio::time::timeout(
                    MCP_REQUEST_TIMEOUT,
                    client.request(&target, head, Some(body.into())),
                )
                .await
                .map_err(|_| {
                    McpError::Transport(format!(
                        "MCP {method} notify timed out after {}s",
                        MCP_REQUEST_TIMEOUT.as_secs()
                    ))
                })?
                .map_err(|error| {
                    McpError::Transport(format!("MCP {method} notify failed: {error}"))
                })?;
                let mut exchange = exchange;
                let _ = exchange.body.read_to_end().await;
                Ok(())
            }
        }
    }
}

/// Parse a Streamable-HTTP response body into a JSON-RPC reply for `method`.
/// A 200/201 may deliver either `application/json` (a single object) or
/// `text/event-stream` (SSE `data:` frames; per spec the *last* `message`
/// event carrying our id is the reply — but practical servers send exactly
/// one, so the first id-matching frame wins).
fn parse_http_message(body: &str, content_type: &str, method: &str) -> Result<Value, McpError> {
    if content_type.starts_with("text/event-stream") {
        // Collect every `data:` frame into candidate JSON-RPC messages.
        for frame in body.lines().filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .starts_with("data:")
                .then(|| trimmed["data:".len()..].trim())
        }) {
            if let Ok(value) = serde_json::from_str::<Value>(frame)
                && value.get("jsonrpc").is_some()
            {
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
    /// The reconnect-capable server handle. When a tool call fails with a
    /// connection error, [`call`](Tool::call) resets the connection and retries
    /// once — transparent crash recovery without waiting for the next refresh.
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
}

impl McpServer {
    pub fn new(config: McpServerConfig, server_name: String, read_only: bool) -> Self {
        Self {
            config,
            server_name,
            read_only,
            client: tokio::sync::Mutex::new(None),
        }
    }

    /// Connect (or reconnect) and return the live client. If a client is
    /// already held, it is reused; otherwise a fresh connection is established.
    async fn ensure_connected(&self) -> Result<Arc<McpClient>, String> {
        let spec = if let Some(url) = &self.config.url {
            nuo_contracts::security::AssetSpec::RemoteEndpoint {
                url: url.clone(),
                headers: self
                    .config
                    .environment
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            }
        } else {
            nuo_contracts::security::AssetSpec::Process {
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
            Some(root) => nuo_contracts::security::AssetLocator::WorkspaceMcp {
                workspace_root: root.to_string_lossy().to_string(),
                name: self.server_name.clone(),
            },
            None => nuo_contracts::security::AssetLocator::UserMcp {
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

    /// Whether a server-advertised tool (original name) passes this server's
    /// `allow_tools`/`deny_tools` configuration — see
    /// [`McpServerConfig::admits_tool`].
    pub fn admits_tool(&self, original_name: &str) -> bool {
        self.config.admits_tool(original_name)
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

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    fn hazard_level(&self) -> nuo_contracts::HazardLevel {
        nuo_contracts::HazardLevel::NetworkOrExternal
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let arguments: Value = serde_json::from_str(arguments)
            .map_err(|error| format!("invalid MCP tool arguments: {}", error))?;
        let payload = json!({
            "name": self.original_name,
            "arguments": arguments,
        });
        // Try with the current (possibly cached) connection. Only a *transport*
        // failure (the pipe broke — the server crashed) warrants a reconnect
        // and a single retry. A *protocol* error (a JSON-RPC `error` object) is
        // a deterministic server-side result: re-sending an already-applied
        // non-idempotent tool call would repeat its side effect, so it is
        // returned to the caller verbatim and never retried.
        let client = self.server.ensure_connected().await?;
        match client.request("tools/call", payload.clone()).await {
            Ok(result) => Ok(render_tool_result(&result)),
            Err(error) if error.is_transport() => {
                tracing::warn!(
                    server = %self.server.name(),
                    tool = %self.original_name,
                    %error,
                    "MCP transport error, reconnecting and retrying once"
                );
                self.server.reset().await;
                let client = self.server.ensure_connected().await?;
                let result = client.request("tools/call", payload).await?;
                Ok(render_tool_result(&result))
            }
            Err(error) => {
                // Protocol error: surface it to the caller; do not reconnect.
                Err(error.to_string())
            }
        }
    }
}

pub async fn load_mcp_tools(configs: &HashMap<String, McpServerConfig>) -> McpLoadResult {
    let mut tools = Vec::new();
    let mut statuses = Vec::new();
    let mut servers = Vec::new();
    let mut names = configs.keys().cloned().collect::<Vec<_>>();
    names.sort();

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
            let server_tools = build_tools_from_server(&server).await?;
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

    for server in servers {
        // Reset the connection so ensure_connected opens a fresh one.
        server.reset().await;
        match timeout(MCP_CONNECT_TIMEOUT, async {
            build_tools_from_server(server).await
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
        let tools = build_tools_from_server(&server).await?;
        Ok::<_, String>((server, tools))
    })
    .await
    .map_err(|_| "connection timed out".to_string())?
}

/// Reset and re-establish one server's connection, re-discovering its tools.
/// The single-server analogue of [`refresh_mcp_tools`]; returns the refreshed
/// tools and the connection status. Used by the `/mcp` modal's reconnect action.
pub async fn reconnect_server(
    server: &Arc<McpServer>,
) -> (Vec<Arc<dyn Tool>>, McpConnectionStatus) {
    server.reset().await;
    match timeout(MCP_CONNECT_TIMEOUT, build_tools_from_server(server)).await {
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
async fn build_tools_from_server(server: &Arc<McpServer>) -> Result<Vec<Arc<dyn Tool>>, String> {
    let client = server.ensure_connected().await?;
    let result = client.request("tools/list", json!({})).await?;
    let definitions = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP tools/list response has no tools array".to_string())?;

    // `sanitize_name` is a lossy, many-to-one mapping (every non-`[A-Za-z0-9_]`
    // char folds to `_`), so distinct server-side tool names can collapse to the
    // same public name (e.g. `read-file` and `read.file` both become
    // `read_file`). Left unchecked, the second adapter would be silently dropped
    // by the first-wins `ToolSet`, making that tool unreachable. We guarantee
    // uniqueness here, where the whole server's tool list is in scope, by
    // appending the lowest free numeric suffix on collision. `original_name` is
    // untouched, so `tools/call` still targets the correct server-side tool.
    let mut taken = HashSet::new();
    definitions
        .iter()
        // Config-time scoping (ADR-0085 follow-up): `allow_tools`/`deny_tools`
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
                &mut taken,
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
            Ok(Arc::new(McpTool {
                public_name,
                original_name,
                description,
                parameters,
                server: Arc::clone(server),
            }) as Arc<dyn Tool>)
        })
        .collect()
}

async fn write_message(stdin: &mut ChildStdin, message: &Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|error| error.to_string())?;
    stdin.flush().await.map_err(|error| error.to_string())
}

async fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = stdout
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?;
        if bytes == 0 {
            return Err("MCP server closed stdout".to_string());
        }
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(line.trim())
            .map_err(|error| format!("invalid MCP JSON-RPC message: {}", error));
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

/// Reserve a collision-free variant of `name` within `taken`, recording the
/// result. The first claimant keeps the bare name; each later duplicate gets the
/// lowest free `_2`, `_3`, … suffix. Deterministic and order-stable, so a server
/// returning the same `tools/list` always yields the same public names.
fn unique_name(name: String, taken: &mut HashSet<String>) -> String {
    if taken.insert(name.clone()) {
        return name;
    }
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{name}_{suffix}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        suffix += 1;
    }
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
        "muta-mcp-output-{}-{}.txt",
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
        "[MCP Output truncated: {line_count} lines, {} bytes. Exceeded 16KB/200-line safety boundary (ADR-0240 [INV-MCP-05])]\n\
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

fn render_tool_result(result: &Value) -> String {
    let raw = if let Some(content) = result.get("content").and_then(Value::as_array) {
        let rendered = content
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !rendered.is_empty() {
            rendered
        } else {
            result.to_string()
        }
    } else {
        result.to_string()
    };
    compact_mcp_output(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_for_provider_function_schemas() {
        assert_eq!(sanitize_name("git tools/read-file"), "git_tools_read_file");
    }

    #[test]
    fn colliding_public_names_are_disambiguated_deterministically() {
        // `read-file` and `read.file` both sanitize to the same public name;
        // the second (and third) must get a distinct, stable suffix.
        let mut taken = HashSet::new();
        let first = unique_name("mcp__s__read_file".to_string(), &mut taken);
        let second = unique_name("mcp__s__read_file".to_string(), &mut taken);
        let third = unique_name("mcp__s__read_file".to_string(), &mut taken);
        assert_eq!(first, "mcp__s__read_file");
        assert_eq!(second, "mcp__s__read_file_2");
        assert_eq!(third, "mcp__s__read_file_3");
        // A name that only differs after sanitization keeps its own identity.
        let other = unique_name("mcp__s__write_file".to_string(), &mut taken);
        assert_eq!(other, "mcp__s__write_file");
    }

    #[test]
    fn text_content_is_rendered_without_protocol_wrappers() {
        let result = json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ]
        });
        assert_eq!(render_tool_result(&result), "first\nsecond");
    }

    #[test]
    fn compact_mcp_output_preserves_small_outputs() {
        let small = "hello world\nline 2";
        assert_eq!(compact_mcp_output(small), small);
    }

    #[test]
    fn compact_mcp_output_compacts_and_spills_large_byte_payloads() {
        // Output exceeding 16KB
        let large = "a".repeat(20_000);
        let compacted = compact_mcp_output(&large);
        assert!(compacted.contains("[MCP Output truncated:"));
        assert!(compacted.contains("Exceeded 16KB/200-line safety boundary"));
        assert!(compacted.contains("[Full output saved to:"));
    }

    #[test]
    fn compact_mcp_output_compacts_and_spills_large_line_count() {
        // Output exceeding 200 lines
        let lines: Vec<String> = (1..=300).map(|i| format!("line {i}")).collect();
        let large = lines.join("\n");
        let compacted = compact_mcp_output(&large);
        assert!(compacted.contains("[MCP Output truncated: 300 lines"));
        assert!(compacted.contains("--- Output preview (head) ---"));
        assert!(compacted.contains("line 1"));
        assert!(compacted.contains("--- Output preview (tail) ---"));
        assert!(compacted.contains("line 300"));
    }
}
