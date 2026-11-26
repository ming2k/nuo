//! Unified `Hello` → `ServerHello` transport link, multiplexed thread
//! subscriptions, and same-connection control verbs (ADR-0133, ADR-0251,
//! ADR-0260).

use super::control::ControlOutcome;
use super::current_posture;
use super::lifecycle::{ATTACH_ACK_TIMEOUT, CONTROL_REPLY_TIMEOUT};
use super::ready;
use super::*;
use protocol::{
    ControlRequest, MonitorAction, MonitorEvent, PersistenceHealth, ServerPhase, ThreadOverview,
    ThreadTarget,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::oneshot;

/// A failed attach, subscription, or control verb.
///
/// `[INV-ATTACH-01]`: `code` is the server's machine-readable taxonomy, carried
/// verbatim from `Wire::Error` / `Wire::ControlReply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachError {
    pub code: Option<String>,
    pub message: String,
    /// `true` when the failure originated at the transport/framing layer
    /// (connect, timeout, EOF, or undecodable handshake), `false` when the
    /// server spoke an explicit refusal frame.
    is_transport: bool,
}

impl AttachError {
    /// A transport-level failure: no server taxonomy exists.
    pub fn from_transport(message: impl Into<String>) -> Self {
        Self::transport(message)
    }

    /// An explicit server refusal from `Wire::Error` / `Wire::ControlReply`.
    pub fn from_server_refusal(code: Option<String>, message: impl Into<String>) -> Self {
        Self::server_refusal(code, message)
    }

    /// A transport-level failure: no server taxonomy exists.
    pub(super) fn transport(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
            is_transport: true,
        }
    }

    /// An explicit server refusal from `Wire::Error`.
    pub(super) fn server_refusal(code: Option<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            is_transport: false,
        }
    }

    /// The refusal code, without the `Option` ceremony at every call site.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Whether this failure originated in transport/framing rather than an
    /// explicit `Wire::Error` refusal from the server (ADR-0224).
    pub fn is_transport_failure(&self) -> bool {
        self.is_transport
    }

    /// Whether the target session provably no longer exists (`ERR_SESSION_GONE`).
    pub fn is_session_gone(&self) -> bool {
        self.code() == Some(protocol::ERR_SESSION_GONE)
    }

    /// Whether the target session exists durably but could not be assembled
    /// (`ERR_SESSION_UNRESUMABLE`).
    pub fn is_session_unresumable(&self) -> bool {
        self.code() == Some(protocol::ERR_SESSION_UNRESUMABLE)
    }

    /// Whether the server refused because it is still initializing.
    pub fn is_server_not_ready(&self) -> bool {
        self.code() == Some(protocol::ERR_SERVER_NOT_READY)
    }

    /// Whether the server refused because it is shutting down.
    pub fn is_server_draining(&self) -> bool {
        self.code() == Some(protocol::ERR_SERVER_DRAINING)
    }

    /// Whether the server refused because it entered a fatal failed state.
    pub fn is_server_failed(&self) -> bool {
        self.code() == Some(protocol::ERR_SERVER_FAILED)
    }
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AttachError {}

impl From<AttachError> for String {
    fn from(error: AttachError) -> Self {
        error.message
    }
}

/// Server metadata and initial lifecycle phase returned by `Wire::ServerHello`
/// (ADR-0260 `[INV-SRVPHASE-02]`).
#[derive(Debug, Clone)]
pub struct ServerHelloInfo {
    pub server_id: String,
    pub protocol: u32,
    pub version: String,
    pub started_at: u64,
    pub phase: ServerPhase,
    pub persistence_health: Option<PersistenceHealth>,
    pub monitor: Option<protocol::MonitorSnapshot>,
}

/// Where an attached reader records the *transport* or *drain* failure that ended it.
pub type LinkLostMarker = Arc<Mutex<Option<String>>>;

/// Structured link-level events delivered from a live unified connection
/// alongside `AgentResponse` and `MonitorEvent` (ADR-0260 `[INV-ATTACH-01]`).
#[derive(Debug, Clone)]
pub enum LinkEvent {
    /// Server explicitly refused a `Wire::Subscribe` (or emitted a non-control
    /// `Wire::Error`) with its machine-readable refusal code preserved.
    SubscribeRefused(AttachError),
    /// Server answered an ambiguous auto-attach `Wire::Subscribe` with
    /// candidate threads (`Wire::Pick`).
    PickRequired(Vec<ThreadOverview>),
}

/// One outbound frame sent from the client over a unified multiplexed
/// connection (ADR-0251 `[INV-MUX-04]`, ADR-0260 `[INV-SRVPHASE-04]`).
#[derive(Debug)]
pub enum ClientOutbound {
    /// Route an [`AgentRequest`] to `thread_id` (`None` targets the
    /// connection's primary thread).
    Request {
        thread_id: Option<String>,
        request: AgentRequest,
    },
    /// Subscribe this connection to a thread (create new, attach, or picker)
    /// without disturbing existing subscriptions.
    Subscribe {
        target: ThreadTarget,
        project: Option<std::path::PathBuf>,
    },
    /// Detach a subscribed thread from this connection.
    Unsubscribe { thread_id: String },
    /// Execute a session-free [`ControlRequest`] over the existing unified
    /// connection — works identically for local and `--remote` clients.
    Control {
        request: ControlRequest,
        reply_tx: oneshot::Sender<Result<ControlOutcome, AttachError>>,
    },
}

impl ClientOutbound {
    /// Extract the inner [`AgentRequest`] when this frame is a domain request.
    pub fn into_request(self) -> Option<AgentRequest> {
        match self {
            Self::Request { request, .. } => Some(request),
            Self::Subscribe { .. } | Self::Unsubscribe { .. } | Self::Control { .. } => None,
        }
    }

    /// Borrow the inner [`AgentRequest`] when this frame is a domain request.
    pub fn as_request(&self) -> Option<&AgentRequest> {
        match self {
            Self::Request { request, .. } => Some(request),
            Self::Subscribe { .. } | Self::Unsubscribe { .. } | Self::Control { .. } => None,
        }
    }
}

impl From<AgentRequest> for ClientOutbound {
    fn from(request: AgentRequest) -> Self {
        Self::Request {
            thread_id: None,
            request,
        }
    }
}

/// Issue a [`ControlRequest`] over an existing unified client link (`req_tx`)
/// with a bounded reply deadline (ADR-0260 `[INV-SRVPHASE-04]`).
pub async fn send_control_over_link(
    tx: &mpsc::UnboundedSender<ClientOutbound>,
    request: ControlRequest,
) -> Result<ControlOutcome, String> {
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(ClientOutbound::Control { request, reply_tx })
        .map_err(|_| "server connection is closed".to_string())?;
    tokio::time::timeout(CONTROL_REPLY_TIMEOUT, reply_rx)
        .await
        .map_err(|_| "timed out waiting for control reply from server".to_string())?
        .map_err(|_| "server closed connection before answering control request".to_string())?
        .map_err(|e| e.message)
}

/// A live session-free connection established via `Wire::Hello` → `Wire::ServerHello`
/// (ADR-0260 `[INV-SRVPHASE-02]`, `[INV-SRVPHASE-04]`).
pub struct ServerLink {
    pub hello: ServerHelloInfo,
    pub req_tx: mpsc::UnboundedSender<ClientOutbound>,
    pub resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
    pub monitor_rx: mpsc::UnboundedReceiver<MonitorEvent>,
    pub link_event_rx: mpsc::UnboundedReceiver<LinkEvent>,
    /// Resolves immediately when the underlying connection reader ends:
    /// `None` after an explicit `AgentResponse::Exit`, or `Some(reason)` on a
    /// transport drop or server drain.
    pub closed_rx: oneshot::Receiver<Option<String>>,
    pub link_lost: LinkLostMarker,
}

#[allow(clippy::large_enum_variant)]
pub enum Handshake {
    Attached {
        req_tx: mpsc::UnboundedSender<ClientOutbound>,
        resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
        monitor_rx: mpsc::UnboundedReceiver<MonitorEvent>,
        server_hello: ServerHelloInfo,
        link_lost: LinkLostMarker,
        session_id: String,
        /// Readiness at the moment of subscription acknowledgement.
        /// `[INV-ATTACH-04]` makes the post-snapshot `ThreadPhase::Ready`
        /// the gate for input.
        phase: protocol::ThreadPhase,
    },
    Pick(Vec<ThreadOverview>),
}

pub type AttachResult = Result<Handshake, AttachError>;
pub type ServerLinkResult = Result<ServerLink, AttachError>;

/// Open a raw wire transport stream to a local server (IPC first, TCP fallback).
pub(super) async fn dial_local_wire(
    info: &ServerInfo,
) -> Result<(BoxWireSink, BoxWireStream), AttachError> {
    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = host::ipc::connect(&endpoint).await
    {
        return Ok(native_framed_split(stream));
    }
    if info.port == 0 {
        return Err(AttachError::transport(
            "server runs in client-bound mode (UDS only) and local endpoint is not reachable",
        ));
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    connect_ws_wire(
        &url,
        info.token.as_deref(),
        netune::WsSessionConfig::default(),
    )
    .await
    .map_err(AttachError::transport)
}

/// Establish a session-free [`ServerLink`] to a local server (`Wire::Hello` →
/// `Wire::ServerHello`). Succeeds immediately across all `ServerPhase` values.
pub async fn connect_server_local(
    info: &ServerInfo,
    monitor: Option<MonitorAction>,
) -> ServerLinkResult {
    let parts = dial_local_wire(info).await?;
    finish_server_hello(parts, monitor).await
}

/// Complete `Wire::Hello` → `Wire::ServerHello` and spawn the unified full-duplex
/// reader/writer tasks for a [`ServerLink`].
pub(super) async fn finish_server_hello(
    parts: (BoxWireSink, BoxWireStream),
    monitor: Option<MonitorAction>,
) -> ServerLinkResult {
    let (mut wire_sink, mut wire_source) = parts;
    let hello_info = exchange_hello(&mut wire_sink, &mut wire_source, monitor).await?;

    let (req_out_tx, req_out_rx) = mpsc::unbounded_channel::<ClientOutbound>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();
    let (mon_in_tx, mon_in_rx) = mpsc::unbounded_channel::<MonitorEvent>();
    let (link_ev_tx, link_ev_rx) = mpsc::unbounded_channel::<LinkEvent>();
    let (closed_tx, closed_rx) = oneshot::channel::<Option<String>>();

    if let Some(snap) = hello_info.monitor.clone() {
        let _ = mon_in_tx.send(MonitorEvent::Snapshot(snap));
    }

    let link_lost_marker = Arc::new(Mutex::new(None::<String>));
    spawn_duplex_tasks(
        wire_sink,
        wire_source,
        req_out_rx,
        resp_in_tx,
        mon_in_tx,
        Some(link_ev_tx),
        Some(closed_tx),
        Arc::clone(&link_lost_marker),
        None,
        1,
    );

    Ok(ServerLink {
        hello: hello_info,
        req_tx: req_out_tx,
        resp_rx: resp_in_rx,
        monitor_rx: mon_in_rx,
        link_event_rx: link_ev_rx,
        closed_rx,
        link_lost: link_lost_marker,
    })
}

/// Connect to a local server, wait for `ServerPhase::Ready` if initializing,
/// and subscribe to `target`.
pub async fn connect(info: &ServerInfo, target: ThreadTarget) -> AttachResult {
    let parts = dial_local_wire(info).await?;
    finish_handshake(parts, target).await
}

async fn finish_handshake(
    parts: (BoxWireSink, BoxWireStream),
    target: ThreadTarget,
) -> AttachResult {
    finish_handshake_with_project(parts, target, std::env::current_dir().ok()).await
}

pub(super) async fn finish_handshake_with_project(
    parts: (BoxWireSink, BoxWireStream),
    target: ThreadTarget,
    project: Option<std::path::PathBuf>,
) -> AttachResult {
    finish_handshake_with_options(parts, target, project, None).await
}

pub(super) async fn finish_handshake_with_options(
    parts: (BoxWireSink, BoxWireStream),
    target: ThreadTarget,
    project: Option<std::path::PathBuf>,
    monitor: Option<MonitorAction>,
) -> AttachResult {
    let (mut wire_sink, mut wire_source) = parts;

    let mut hello_info = exchange_hello(&mut wire_sink, &mut wire_source, monitor).await?;
    let mut buffered_monitor: Vec<MonitorEvent> = Vec::new();
    if let Some(snap) = hello_info.monitor.clone() {
        buffered_monitor.push(MonitorEvent::Snapshot(snap));
    }

    // If the server is still completing bounded local initialization, wait on
    // its live `ServerPhaseChanged` stream via the shared progress-aware
    // watchdog before subscribing (ADR-0260 `[INV-SRVPHASE-03]`, ADR-0266).
    if hello_info.phase.is_initializing() {
        hello_info.phase = ready::await_server_ready(
            &mut wire_source,
            hello_info.phase,
            Some(&mut buffered_monitor),
            ready::INIT_HARD_CAP,
        )
        .await?;
    }

    match &hello_info.phase {
        ServerPhase::Failed { code, message } => {
            return Err(AttachError::server_refusal(
                code.clone()
                    .or_else(|| Some(protocol::ERR_SERVER_FAILED.to_string())),
                format!("server startup failed: {message}"),
            ));
        }
        ServerPhase::Draining { reason } => {
            let msg = match reason {
                Some(r) => format!("server is shutting down ({r})"),
                None => "server is shutting down".to_string(),
            };
            return Err(AttachError::server_refusal(
                Some(protocol::ERR_SERVER_DRAINING.to_string()),
                msg,
            ));
        }
        _ => {}
    }

    let request_id = 1;
    wire_sink
        .send(Wire::Subscribe {
            request_id,
            target,
            project,
        })
        .await
        .map_err(|e| AttachError::transport(format!("wire send subscribe: {e}")))?;

    enum SubscribeReply {
        Subscribed {
            session_id: String,
            generation: u64,
            phase: protocol::ThreadPhase,
        },
        Pick(Vec<ThreadOverview>),
    }

    let reply = tokio::time::timeout(ATTACH_ACK_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(WireFrame::Wire(wire))) => match wire {
                    Wire::Subscribed {
                        request_id: rid,
                        thread_id,
                        generation,
                        phase,
                    } if rid == request_id => {
                        return Ok(SubscribeReply::Subscribed {
                            session_id: thread_id,
                            generation,
                            phase,
                        });
                    }
                    Wire::Pick {
                        request_id: rid,
                        threads,
                    } if rid == request_id => {
                        return Ok(SubscribeReply::Pick(threads));
                    }
                    Wire::Error {
                        request_id: rid,
                        message,
                        code,
                    } if rid.is_none() || rid == Some(request_id) => {
                        return Err(AttachError::server_refusal(
                            code,
                            format!("server rejected the attach: {message}"),
                        ));
                    }
                    Wire::Monitor { event } => {
                        buffered_monitor.push(event);
                    }
                    other => {
                        return Err(AttachError::transport(format!(
                            "unexpected frame before Subscribed: {other:?}"
                        )));
                    }
                },
                Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                    return Err(AttachError::transport(format!(
                        "server sent a frame this client cannot decode{}: {detail}",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    )));
                }
                Some(Err(error)) => {
                    return Err(AttachError::transport(format!(
                        "wire recv during subscribe: {error}"
                    )));
                }
                None => return Err(AttachError::transport("server closed the connection")),
            }
        }
    })
    .await
    .map_err(|_| {
        AttachError::transport(
            "server did not acknowledge the thread subscription within the liveness window",
        )
    })??;

    let (session_id, generation, phase) = match reply {
        SubscribeReply::Subscribed {
            session_id,
            generation,
            phase,
        } => (session_id, generation, phase),
        SubscribeReply::Pick(sessions) => {
            let _ = wire_sink.close().await;
            return Ok(Handshake::Pick(sessions));
        }
    };

    let (req_out_tx, req_out_rx) = mpsc::unbounded_channel::<ClientOutbound>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();
    let (mon_in_tx, mon_in_rx) = mpsc::unbounded_channel::<MonitorEvent>();
    for event in buffered_monitor {
        let _ = mon_in_tx.send(event);
    }

    let link_lost_marker = Arc::new(Mutex::new(None::<String>));
    spawn_duplex_tasks(
        wire_sink,
        wire_source,
        req_out_rx,
        resp_in_tx,
        mon_in_tx,
        None,
        None,
        Arc::clone(&link_lost_marker),
        Some((session_id.clone(), generation)),
        2,
    );

    Ok(Handshake::Attached {
        req_tx: req_out_tx,
        resp_rx: resp_in_rx,
        monitor_rx: mon_in_rx,
        server_hello: hello_info,
        link_lost: link_lost_marker,
        session_id,
        phase,
    })
}

pub(super) async fn exchange_hello(
    wire_sink: &mut BoxWireSink,
    wire_source: &mut BoxWireStream,
    monitor: Option<MonitorAction>,
) -> Result<ServerHelloInfo, AttachError> {
    let hello = Wire::Hello {
        protocol: PROTOCOL_VERSION,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        posture: current_posture(),
        monitor,
    };
    wire_sink
        .send(hello)
        .await
        .map_err(|e| AttachError::transport(format!("wire send hello: {e}")))?;

    tokio::time::timeout(ATTACH_ACK_TIMEOUT, async {
        match wire_source.next().await {
            Some(Ok(WireFrame::Wire(Wire::ServerHello {
                server_id,
                protocol,
                version,
                started_at,
                phase,
                persistence_health,
                monitor,
            }))) => Ok(ServerHelloInfo {
                server_id,
                protocol,
                version,
                started_at,
                phase,
                persistence_health,
                monitor,
            }),
            Some(Ok(WireFrame::Wire(Wire::Error { message, code, .. }))) => {
                Err(AttachError::server_refusal(
                    code,
                    format!("server rejected the handshake: {message}"),
                ))
            }
            Some(Ok(WireFrame::Wire(other))) => Err(AttachError::transport(format!(
                "unexpected frame before ServerHello: {other:?}"
            ))),
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
            "server did not acknowledge Hello within the liveness window \
             (the server may be starting, wedged, or speaking a different protocol)",
        )
    })?
}

type PendingControlMap =
    Arc<Mutex<HashMap<u64, oneshot::Sender<Result<ControlOutcome, AttachError>>>>>;

#[allow(clippy::too_many_arguments)]
fn spawn_duplex_tasks(
    mut wire_sink: BoxWireSink,
    mut wire_source: BoxWireStream,
    mut req_out_rx: mpsc::UnboundedReceiver<ClientOutbound>,
    resp_in_tx: mpsc::UnboundedSender<AgentResponse>,
    mon_in_tx: mpsc::UnboundedSender<MonitorEvent>,
    link_ev_tx: Option<mpsc::UnboundedSender<LinkEvent>>,
    closed_tx: Option<oneshot::Sender<Option<String>>>,
    link_lost_marker: LinkLostMarker,
    initial_thread: Option<(String, u64)>,
    initial_request_id: u64,
) {
    let link_lost_inner = Arc::clone(&link_lost_marker);
    let link_lost_for_writer = Arc::clone(&link_lost_marker);
    let exit_seen = Arc::new(Mutex::new(false));
    let exit_seen_inner = Arc::clone(&exit_seen);
    let draining_reason = Arc::new(Mutex::new(None::<String>));
    let draining_reason_inner = Arc::clone(&draining_reason);
    let next_req_id = Arc::new(AtomicU64::new(initial_request_id));
    let pending_controls: PendingControlMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_controls_writer = Arc::clone(&pending_controls);

    tokio::spawn(async move {
        let mut end_pending = false;
        while let Some(outbound) = req_out_rx.recv().await {
            let wire = match outbound {
                ClientOutbound::Request { thread_id, request } => {
                    if matches!(request, AgentRequest::EndThread) {
                        end_pending = true;
                    }
                    Wire::Request { thread_id, request }
                }
                ClientOutbound::Subscribe { target, project } => {
                    let request_id = next_req_id.fetch_add(1, Ordering::Relaxed);
                    Wire::Subscribe {
                        request_id,
                        target,
                        project,
                    }
                }
                ClientOutbound::Unsubscribe { thread_id } => Wire::Unsubscribe { thread_id },
                ClientOutbound::Control { request, reply_tx } => {
                    let request_id = next_req_id.fetch_add(1, Ordering::Relaxed);
                    pending_controls_writer
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(request_id, reply_tx);
                    Wire::Control {
                        request_id,
                        request,
                    }
                }
            };
            if let Err(error) = wire_sink.send(wire).await {
                tracing::warn!(%error, "attach: wire send failed");
                let mut marker = link_lost_for_writer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if marker.is_none() {
                    *marker = Some(format!("wire send failed: {error}"));
                }
                break;
            }
        }
        if end_pending {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let _ = wire_sink.close().await;
    });

    tokio::spawn(async move {
        let mut active_generations: HashMap<String, u64> = HashMap::new();
        if let Some((tid, generation)) = initial_thread
            && !tid.is_empty()
            && generation > 0
        {
            active_generations.insert(tid, generation);
        }
        while let Some(frame) = wire_source.next().await {
            match frame {
                Ok(WireFrame::Wire(Wire::Response {
                    response,
                    generation,
                })) => {
                    if generation > 0
                        && let Some(tid) = response_thread_id(&response)
                    {
                        if active_generations
                            .get(tid)
                            .is_some_and(|&current| generation < current)
                        {
                            tracing::debug!(
                                thread_id = tid,
                                frame_generation = generation,
                                "attach: dropping stale-generation wire frame"
                            );
                            continue;
                        }
                        let entry = active_generations.entry(tid.to_string()).or_insert(0);
                        if generation > *entry {
                            *entry = generation;
                        }
                    }
                    if matches!(response, AgentResponse::Exit) {
                        *exit_seen_inner.lock().unwrap_or_else(|e| e.into_inner()) = true;
                    }
                    if resp_in_tx.send(response).is_err() {
                        break;
                    }
                }
                Ok(WireFrame::Wire(Wire::Subscribed {
                    thread_id,
                    generation,
                    phase,
                    ..
                })) => {
                    if generation > 0 {
                        let entry = active_generations.entry(thread_id.clone()).or_insert(0);
                        if generation > *entry {
                            *entry = generation;
                        }
                    }
                    if resp_in_tx
                        .send(AgentResponse::ThreadSubscribed { thread_id, phase })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(WireFrame::Wire(Wire::ControlReply {
                    request_id,
                    ok,
                    thread_id,
                    error,
                    code,
                    payload,
                })) => {
                    let waiter = pending_controls
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&request_id);
                    if let Some(tx) = waiter {
                        let res = if ok {
                            Ok(ControlOutcome { thread_id, payload })
                        } else {
                            Err(AttachError::server_refusal(
                                code,
                                error.unwrap_or_else(|| "control verb rejected".to_string()),
                            ))
                        };
                        let _ = tx.send(res);
                    }
                }
                Ok(WireFrame::Wire(Wire::Monitor { event })) => {
                    // Record draining explicitly without conflating it with a
                    // user-requested `AgentResponse::Exit`: when the socket
                    // closes after draining, the client sees the honest drain
                    // reason rather than a silent normal exit.
                    if let MonitorEvent::ServerPhaseChanged(ServerPhase::Draining { ref reason }) =
                        event
                    {
                        let msg = match reason {
                            Some(r) if !r.trim().is_empty() => {
                                format!("server is shutting down ({r})")
                            }
                            _ => "server is shutting down".to_string(),
                        };
                        *draining_reason_inner
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(msg);
                    }
                    let _ = mon_in_tx.send(event);
                }
                Ok(WireFrame::Wire(Wire::Pick { threads, .. })) => {
                    if let Some(ref ev_tx) = link_ev_tx {
                        let _ = ev_tx.send(LinkEvent::PickRequired(threads));
                    } else if resp_in_tx
                        .send(AgentResponse::ThreadsOverview(threads))
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(WireFrame::Wire(Wire::Error {
                    request_id,
                    code,
                    message,
                })) => {
                    if let Some(rid) = request_id
                        && let Some(tx) = pending_controls
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&rid)
                    {
                        let _ = tx.send(Err(AttachError::server_refusal(code, message)));
                        continue;
                    }
                    if let Some(ref ev_tx) = link_ev_tx {
                        let _ = ev_tx.send(LinkEvent::SubscribeRefused(
                            AttachError::server_refusal(code, message),
                        ));
                    } else if resp_in_tx.send(AgentResponse::Error(message)).is_err() {
                        break;
                    }
                }
                Ok(WireFrame::Undecodable { envelope, detail }) => {
                    tracing::warn!(
                        envelope = envelope.as_deref().unwrap_or("<none>"),
                        %detail,
                        "attach: server sent a frame this client cannot decode; \
                         connection retained"
                    );
                    let notice = AgentResponse::Error(format!(
                        "The server sent a message this client does not understand{}. \
                         The connection is still live — run `nuo server stop` so both \
                         sides come up on the same build.",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    ));
                    if resp_in_tx.send(notice).is_err() {
                        break;
                    }
                }
                Ok(WireFrame::Wire(_)) => {
                    tracing::warn!("attach: unexpected post-handshake frame, ignored")
                }
                Err(error) => {
                    tracing::warn!(%error, "attach: wire recv failed; link is down");
                    *link_lost_inner.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(error.to_string());
                    break;
                }
            }
        }

        let exit_seen = *exit_seen_inner.lock().unwrap_or_else(|e| e.into_inner());
        let drain_msg = draining_reason_inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let closed_reason = {
            let mut marker = link_lost_inner.lock().unwrap_or_else(|e| e.into_inner());
            if !exit_seen && marker.is_none() {
                if let Some(msg) = drain_msg {
                    *marker = Some(msg);
                } else {
                    tracing::warn!(
                        "attach: connection closed by server unexpectedly without Exit frame"
                    );
                    *marker = Some("server closed connection unexpectedly".to_string());
                }
            }
            marker.clone()
        };

        // Fail any in-flight control requests immediately so callers never hang.
        let err_msg = closed_reason
            .clone()
            .unwrap_or_else(|| "server connection closed".to_string());
        for (_, tx) in pending_controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            let _ = tx.send(Err(AttachError::transport(err_msg.clone())));
        }

        if let Some(tx) = closed_tx {
            let _ = tx.send(closed_reason);
        }
    });
}

/// Extract the owning thread id from a thread-scoped [`AgentResponse`] for
/// generation gating (`[INV-INSTANCE-03]`, ADR-0253 §2).
fn response_thread_id(response: &AgentResponse) -> Option<&str> {
    match response {
        AgentResponse::Round { thread_id, .. }
        | AgentResponse::ThreadSubscribed { thread_id, .. }
        | AgentResponse::ConversationCleared { thread_id, .. }
        | AgentResponse::ConversationReplaced { thread_id, .. } => Some(thread_id.as_str()),
        AgentResponse::ThreadSnapshot(snapshot) => Some(snapshot.thread_id.as_str()),
        AgentResponse::ThreadPhase(update) => Some(update.thread_id.as_str()),
        _ => None,
    }
}
