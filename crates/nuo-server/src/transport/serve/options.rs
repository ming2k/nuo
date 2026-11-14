//! Configuration options, startup handles, and version information for the server.
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::conn_table::ConnTable;
use crate::runtime::shutdown::ShutdownGate;
use nuo_protocol::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeExpose {
    Local,
    Public,
}

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub port: u16,
    pub expose: ServeExpose,
    pub token: Option<String>,
    /// Require a bearer token even on loopback: when `true` and no
    /// explicit `token` is set, the listener generates one and publishes it
    /// via the discovery record (owner-only, 0600). Defends the control plane
    /// against drive-by connections from other local processes and other
    /// users on a shared machine. The production server defaults this on via
    /// `[server] local_auth`; the field defaults off so existing
    /// `ServeOptions::default()` tests keep their unauthenticated loopback.
    pub local_auth: bool,
    /// When the requested `port` is taken, fall back to an OS-assigned
    /// ephemeral port (the discovery record then carries the real one) instead
    /// of failing startup. Used by the production server, whose CLI default
    /// port is fixed (9800); tests keep the strict default.
    pub port_fallback: bool,
    /// Native local control endpoint. Local IPC is exempt from the bearer
    /// token because its Unix permissions / Windows DACL are the auth boundary.
    pub local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
    /// [INV-SERVER-03] Zero TCP on Client-Bound Posture:
    /// When true, pure UDS/Local IPC mode — no TCP socket listener spawned.
    pub disable_tcp: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            port: 0,
            expose: ServeExpose::Local,
            token: None,
            local_auth: false,
            port_fallback: false,
            local_endpoint: None,
            disable_tcp: false,
        }
    }
}

/// What the server's startup resolved: the bound endpoints on success, or
/// the failure that must stop the process. Distinct from the pre-0101
/// `oneshot<u16>` (whose drop surfaced as a useless `RecvError` at the top
/// level): the actual `io::Error` travels with the value.
pub struct Startup {
    /// TCP bind result: the bound port, or why the listener could not bind.
    pub port: Option<tokio::sync::oneshot::Receiver<Result<u16, std::io::Error>>>,
    /// Native local-IPC bind result when enabled.
    pub local_ready: Option<
        tokio::sync::oneshot::Receiver<
            Result<Option<nuo_host::ipc::LocalEndpoint>, std::io::Error>,
        >,
    >,
}

/// The destructured [`Startup`] receivers (`Startup::take`).
pub struct StartupParts {
    pub port_rx: tokio::sync::oneshot::Receiver<Result<u16, std::io::Error>>,
    pub local_rx: tokio::sync::oneshot::Receiver<
        Result<Option<nuo_host::ipc::LocalEndpoint>, std::io::Error>,
    >,
}

impl Startup {
    /// Take the receivers out (the run loop awaits them as locals so the
    /// containing `ServeHandle` stays intact for the drain phases).
    pub fn take(&mut self) -> StartupParts {
        StartupParts {
            port_rx: self.port.take().unwrap_or_else(|| {
                tokio::sync::oneshot::channel::<Result<u16, std::io::Error>>().1
            }),
            local_rx: self.local_ready.take().unwrap_or_else(|| {
                tokio::sync::oneshot::channel::<
                    Result<Option<nuo_host::ipc::LocalEndpoint>, std::io::Error>,
                >()
                .1
            }),
        }
    }
}

pub struct ServeHandle {
    pub startup: Startup,
    /// The listeners' cancellation token. Cancelling stops the accept loops
    /// (the loops then exit and are joined through `tasks`).
    pub cancel: CancellationToken,
    /// The connection table: every live connection's cancel token, for the
    /// drain phase.
    pub conns: Arc<ConnTable>,
    /// Supervised accept tasks; `host::run` joins them during shutdown to
    /// *confirm* the loops exited (and to clean up native local-listener state
    /// deterministically, instead of racing the process end).
    pub tasks: Arc<crate::runtime::shutdown::TaskBook>,
    pub token: Option<String>,
    /// The server's shutdown gate: `ControlRequest::Shutdown` (the
    /// `nuo server stop` verb) funnels into it like any other trigger.
    pub gate: Arc<ShutdownGate>,
    /// This server build's version, echoed to clients during handshake
    /// version negotiation (rule 4).
    pub version: &'static str,
}

/// The server's own `CARGO_PKG_VERSION`, shared by the discovery record and
/// the handshake refusal message. Every server launch path embeds the same
/// workspace version.
pub fn server_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Refusal message for a client whose protocol version is outside the
/// supported range. The server closes the socket immediately after a single
/// message naming both the protocol window and the product builds. Sent
/// before any session work. Directional: a too-old client hears "update",
/// a too-new client hears "restart the server".
pub fn protocol_mismatch_error(client: u32, client_version: Option<&str>) -> String {
    let window = format!("{}..={}", MIN_PROTOCOL_VERSION, PROTOCOL_VERSION);
    let builds = match client_version {
        Some(v) => format!(" (client build {v}, server build {})", server_version()),
        None => format!(" (server build {})", server_version()),
    };
    if client < MIN_PROTOCOL_VERSION {
        format!(
            "client/server wire protocol mismatch: client protocol {client} is older \
             than the oldest this server serves ({window}). \
             Please update your nuo client to build {server} or newer{builds}.",
            server = server_version()
        )
    } else {
        format!(
            "client/server wire protocol mismatch: client protocol {client} is newer \
             than this server's protocol {current}. \
             Stop the server and let it restart on demand: `nuo server stop`, then rerun \
             this command (or `nuo server start` to bring it up explicitly){builds}.",
            current = PROTOCOL_VERSION
        )
    }
}
