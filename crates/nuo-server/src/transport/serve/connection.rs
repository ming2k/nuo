//! Connection spawning, wire framing, delta coalescing, and protocol routing (control, monitor, attach).
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::{self, handshake::server::Request};
use tokio_util::sync::CancellationToken;

use super::auth::{check_credentials, validate_origin};
use super::conn_table::ConnTable;
use super::options::{ServeExpose, protocol_mismatch_error};
use super::sync_buffer::workspace_trust_notice;
use super::tcp::{TcpManager, TcpTransport, classify};
use crate::runtime::shutdown::ShutdownGate;
use crate::transport::wire_channel::{
    BoxWireSink, BoxWireStream, native_framed_split, websocket_split,
};
use nuo_protocol::{
    AgentRequest, AgentResponse, ControlRequest, ERR_PROTOCOL_MISMATCH, ERR_SERVER_DRAINING,
    ERR_SERVER_FAILED, ERR_SERVER_NOT_READY, MonitorAction, MonitorEvent, ServerPhase,
    ThreadTarget, Wire, WireFrame, protocol_accepts,
};

/// Shared server accept context: the session registry, connection table,
/// shutdown gate, and cancellation token every accepted socket needs. One
/// instance is cloned per accept loop; `spawn_connection` takes it plus the
/// per-socket specifics (stream, token, exposure, peer).
#[derive(Clone)]
pub(crate) struct ServeCtx {
    pub(crate) registry: Arc<crate::runtime::registry::ThreadRegistry>,
    pub(crate) conns: Arc<ConnTable>,
    pub(crate) gate: Arc<ShutdownGate>,
    pub(crate) listeners: CancellationToken,
    pub(crate) tcp_manager: Option<Arc<TcpManager>>,
}

pub(crate) fn spawn_tcp_connection(
    stream: tokio::net::TcpStream,
    ctx: ServeCtx,
    token: Option<String>,
    expose: ServeExpose,
    peer: String,
) {
    let ServeCtx {
        registry,
        conns,
        gate,
        listeners,
        tcp_manager,
    } = ctx;
    tokio::spawn(async move {
        let expected_token = token.as_deref();
        match classify(&stream).await {
            Ok(TcpTransport::Http) => {
                // Health responses are momentary and stateless; they stay out
                // of the drain-tracked connection table by design.
                let phase = registry.server_phase();
                let persistence_health = registry.persistence_health_snapshot();
                if let Err(e) = crate::transport::health_http::serve(
                    stream,
                    gate.version_of_server(),
                    registry.server_id(),
                    &phase,
                    persistence_health.as_ref(),
                    expected_token,
                )
                .await
                {
                    tracing::debug!(%peer, error=%e, "nuo server: http error");
                }
            }
            Ok(TcpTransport::WebSocket) => {
                spawn_connection(
                    stream,
                    ServeCtx {
                        registry,
                        conns,
                        gate,
                        listeners,
                        tcp_manager,
                    },
                    token,
                    expose,
                    peer,
                );
            }
            Err(e) => {
                tracing::debug!(%peer, error=%e, "nuo server: transport classify failed");
            }
        }
    });
}

/// Spawn one connection task, registered in the connection table for the
/// drain phase. Every accepted socket funnels through here so the table can
/// never miss one; the guard unregisters on every exit path.
pub(crate) fn spawn_local_connection<S>(stream: S, ctx: ServeCtx, peer: String)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let ServeCtx {
        registry,
        conns,
        gate,
        listeners,
        tcp_manager,
    } = ctx;
    let (id, conn_cancel) = conns.register();
    let conns_for_guard = conns.clone();
    tokio::spawn(async move {
        let _guard = ConnGuard {
            conns: conns_for_guard.clone(),
            id,
        };
        let (wire_sink, wire_source) = native_framed_split(stream);
        let result = tokio::select! {
            r = handle_wire_stream(wire_sink, wire_source, registry, gate, listeners, conns_for_guard, tcp_manager) => r,
            _ = conn_cancel.cancelled() => {
                tracing::debug!(%peer, "nuo server: closing local connection for drain");
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::warn!(%peer, error=%e, "nuo server: local connection ended");
        }
    });
}

pub(crate) fn spawn_connection<S>(
    stream: S,
    ctx: ServeCtx,
    token: Option<String>,
    expose: ServeExpose,
    peer: String,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let ServeCtx {
        registry,
        conns,
        gate,
        listeners,
        tcp_manager,
    } = ctx;
    let (id, conn_cancel) = conns.register();
    let conns_for_guard = conns.clone();
    tokio::spawn(async move {
        // RAII deregistration: even a panic in handle_connection unregisters.
        let _guard = ConnGuard {
            conns: conns_for_guard.clone(),
            id,
        };
        let result = tokio::select! {
            r = handle_connection(ConnectionParams {
                stream,
                registry,
                token,
                expose,
                gate,
                listeners,
                conns: conns_for_guard,
                tcp_manager,
            }) => r,
            // Draining server: cancel the connection's future.
            // The socket drops with it, closing the TCP stream; clients
            // treat the disconnect exactly like a Close frame — reconnect
            // with backoff. (Sending a graceful Close frame from *here* is
            // not possible: the WS sink is owned by the inner future.)
            _ = conn_cancel.cancelled() => {
                tracing::debug!(%peer, "nuo server: closing connection for drain");
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::warn!(%peer, error=%e, "nuo server: connection ended");
        }
    });
}

/// Unregisters a connection id on drop.
struct ConnGuard {
    conns: Arc<ConnTable>,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.conns.unregister(self.id);
    }
}

/// Check whether the current [`ServerPhase`] admits domain work (`[INV-SRVPHASE-03]`).
fn non_ready_refusal(phase: &ServerPhase) -> Option<(&'static str, String)> {
    match phase {
        ServerPhase::Ready | ServerPhase::Reloading => None,
        ServerPhase::Initializing { stage, detail, .. } => {
            let suffix = match detail {
                Some(nuo_protocol::StorageInitDetail::BackingUp { target_version }) => {
                    format!("backing up database before v{target_version}")
                }
                Some(nuo_protocol::StorageInitDetail::Migrating { step, total, name }) => {
                    format!("migrating database {step}/{total}: {name}")
                }
                Some(nuo_protocol::StorageInitDetail::Verifying) => {
                    "verifying database integrity".to_string()
                }
                None => stage.as_str().to_string(),
            };
            Some((
                ERR_SERVER_NOT_READY,
                format!("server is initializing ({suffix})"),
            ))
        }
        ServerPhase::Draining { reason } => Some((
            ERR_SERVER_DRAINING,
            match reason {
                Some(r) => format!("server is shutting down ({r})"),
                None => "server is shutting down".to_string(),
            },
        )),
        ServerPhase::Failed { message, .. } => {
            Some((ERR_SERVER_FAILED, format!("server failed: {message}")))
        }
    }
}

/// Execute one session-management control verb on the unified connection
/// (ADR-0260). Returns `Ok(true)` if `Shutdown` was triggered (caller should
/// close the connection after sending the reply).
async fn execute_control(
    wire_sink: &mut BoxWireSink,
    registry: &Arc<crate::runtime::registry::ThreadRegistry>,
    gate: &Arc<ShutdownGate>,
    listeners: &CancellationToken,
    tcp_manager: &Option<Arc<TcpManager>>,
    request_id: u64,
    request: ControlRequest,
) -> Result<bool, String> {
    if matches!(request, ControlRequest::Shutdown) {
        // Acknowledge on the wire first — the gate fires the same drain as any
        // signal, which would otherwise cancel this connection before the reply
        // lands. Shutdown is admitted in every ServerPhase.
        let reply = Wire::ControlReply {
            request_id,
            ok: true,
            thread_id: None,
            error: None,
            code: None,
            payload: None,
        };
        wire_sink
            .send(reply)
            .await
            .map_err(|e| format!("send control reply: {e}"))?;
        let _ = wire_sink.close().await;
        registry.set_server_phase(ServerPhase::Draining {
            reason: Some(crate::runtime::shutdown::ShutdownReason::ControlVerb.to_string()),
        });
        gate.request(crate::runtime::shutdown::ShutdownReason::ControlVerb, false);
        listeners.cancel();
        return Ok(true);
    }

    // Read-only endpoint inspection works even during startup; every other
    // control verb requires a ready/reloading server (`[INV-SRVPHASE-03]`).
    if !matches!(request, ControlRequest::GetServerEndpoints)
        && let Some((code, message)) = non_ready_refusal(&registry.server_phase())
    {
        let reply = Wire::ControlReply {
            request_id,
            ok: false,
            thread_id: None,
            error: Some(message),
            code: Some(code.to_string()),
            payload: None,
        };
        wire_sink
            .send(reply)
            .await
            .map_err(|e| format!("send control reply: {e}"))?;
        return Ok(false);
    }

    let (ok, session_id, error, payload) = match request {
        ControlRequest::Shutdown => unreachable!("handled above"),
        ControlRequest::Reload => {
            // Surface `Reloading` immediately to the calling connection before
            // awaiting `reload_runtime()` inline (which transitions
            // `Reloading -> Ready` before `run_session_loop` polls `phase_rx`).
            send_monitor(
                wire_sink,
                MonitorEvent::ServerPhaseChanged(ServerPhase::Reloading),
            )
            .await?;
            match registry.reload_runtime().await {
                Ok(()) => (true, None, None, None),
                Err(error) => (false, None, Some(error), None),
            }
        }
        ControlRequest::CreateThread {
            project,
            prompt,
            init_options,
        } => {
            match registry
                .create_session_with_options(
                    std::path::PathBuf::from(&project),
                    init_options.unwrap_or_default(),
                )
                .await
            {
                Ok(id) => {
                    if let Some(text) = prompt
                        && let Err(e) = registry.send_prompt(&id, text).await
                    {
                        tracing::warn!(session=%id,error=%e,"nuo server: create-thread prompt failed");
                    }
                    (true, Some(id), None, None)
                }
                Err(e) => (false, None, Some(e), None),
            }
        }
        ControlRequest::SendPrompt { thread_id, text } => {
            match registry.send_prompt(&thread_id, text).await {
                Ok(()) => (true, Some(thread_id), None, None),
                Err(e) => (false, None, Some(e), None),
            }
        }
        ControlRequest::Interrupt { thread_id } => match registry.interrupt(&thread_id).await {
            Ok(()) => (true, Some(thread_id), None, None),
            Err(e) => (false, None, Some(e), None),
        },
        ControlRequest::ResolvePermission {
            thread_id,
            request_id: perm_req_id,
            decision,
        } => match registry
            .resolve_permission(&thread_id, perm_req_id, decision)
            .await
        {
            Ok(()) => (true, Some(thread_id), None, None),
            Err(e) => (false, None, Some(e), None),
        },
        ControlRequest::KillThread { thread_id } => match registry.kill_session(&thread_id).await {
            Ok(()) => (true, Some(thread_id), None, None),
            Err(e) => (false, None, Some(e), None),
        },
        ControlRequest::ListThreads { filter } => match registry.history_rows(&filter).await {
            Ok(rows) => match serde_json::to_string(&rows) {
                Ok(payload) => (true, None, None, Some(payload)),
                Err(error) => (
                    false,
                    None,
                    Some(format!("encode thread listing: {error}")),
                    None,
                ),
            },
            Err(error) => (false, None, Some(error), None),
        },
        ControlRequest::StopThreads { filter, ids } => {
            let report = registry.stop_threads(filter.as_ref(), ids).await;
            match serde_json::to_string(&report) {
                Ok(payload) => (true, None, None, Some(payload)),
                Err(error) => (
                    false,
                    None,
                    Some(format!("encode stop report: {error}")),
                    None,
                ),
            }
        }
        ControlRequest::ForkBranch {
            thread_id,
            name,
            from_fact_id,
        } => {
            if let Some(hosted) = registry.get(&thread_id).await {
                let from = from_fact_id.map(nuo_protocol::FactId::from_raw);
                match hosted.session.fork_branch(name, from).await {
                    Ok(branch) => (
                        true,
                        Some(thread_id),
                        None,
                        Some(serde_json::json!({ "branch_id": branch.0 }).to_string()),
                    ),
                    Err(error) => (false, Some(thread_id), Some(error), None),
                }
            } else {
                (
                    false,
                    Some(thread_id),
                    Some("thread is not live".to_string()),
                    None,
                )
            }
        }
        ControlRequest::ListBranches { thread_id } => {
            if let Some(hosted) = registry.get(&thread_id).await {
                match hosted.session.branches().await {
                    Ok(branches) => (
                        true,
                        Some(thread_id),
                        None,
                        Some(serde_json::to_string(&branches).unwrap_or_else(|_| "[]".to_string())),
                    ),
                    Err(error) => (false, Some(thread_id), Some(error), None),
                }
            } else {
                (
                    false,
                    Some(thread_id),
                    Some("thread is not live".to_string()),
                    None,
                )
            }
        }
        ControlRequest::SwitchBranch {
            thread_id,
            branch_id,
        } => {
            if let Some(hosted) = registry.get(&thread_id).await {
                match hosted
                    .session
                    .switch_branch(nuo_protocol::BranchId::new(branch_id))
                    .await
                {
                    Ok(()) => (true, Some(thread_id), None, None),
                    Err(error) => (false, Some(thread_id), Some(error), None),
                }
            } else {
                (
                    false,
                    Some(thread_id),
                    Some("thread is not live".to_string()),
                    None,
                )
            }
        }
        ControlRequest::DeleteBranch {
            thread_id,
            branch_id,
        } => {
            if let Some(hosted) = registry.get(&thread_id).await {
                match hosted
                    .session
                    .delete_branch(nuo_protocol::BranchId::new(branch_id))
                    .await
                {
                    Ok(()) => (true, Some(thread_id), None, None),
                    Err(error) => (false, Some(thread_id), Some(error), None),
                }
            } else {
                (
                    false,
                    Some(thread_id),
                    Some("thread is not live".to_string()),
                    None,
                )
            }
        }
        ControlRequest::PurgeThreads { filter, ids } => {
            let report = registry.purge_threads(filter.as_ref(), ids).await;
            match serde_json::to_string(&report) {
                Ok(payload) => (true, None, None, Some(payload)),
                Err(error) => (
                    false,
                    None,
                    Some(format!("encode purge report: {error}")),
                    None,
                ),
            }
        }
        ControlRequest::SuspendThread { thread_id } => {
            match registry.suspend_session_control(&thread_id).await {
                Ok(()) => (true, Some(thread_id), None, None),
                Err(e) => (false, None, Some(e), None),
            }
        }
        ControlRequest::AskArchivist { text } => {
            let answer = registry.ask_archivist(text).await;
            match answer.status {
                crate::runtime::archivist_service::ArchivistTurnStatus::Failed(e) => {
                    (false, None, Some(e), None)
                }
                _ => (true, None, None, Some(answer.text)),
            }
        }
        ControlRequest::OpenTcpListener { port } => {
            if let Some(mgr) = tcp_manager {
                match mgr.open_tcp(port).await {
                    Ok(rep) => {
                        let payload = serde_json::to_string(&rep).unwrap_or_default();
                        (true, None, None, Some(payload))
                    }
                    Err(e) => (false, None, Some(e), None),
                }
            } else {
                (
                    false,
                    None,
                    Some("TCP manager not available".to_string()),
                    None,
                )
            }
        }
        ControlRequest::CloseTcpListener => {
            if let Some(mgr) = tcp_manager {
                match mgr.close_tcp().await {
                    Ok(rep) => {
                        let payload = serde_json::to_string(&rep).unwrap_or_default();
                        (true, None, None, Some(payload))
                    }
                    Err(e) => (false, None, Some(e), None),
                }
            } else {
                (
                    false,
                    None,
                    Some("TCP manager not available".to_string()),
                    None,
                )
            }
        }
        ControlRequest::GetServerEndpoints => {
            if let Some(mgr) = tcp_manager {
                let rep = mgr.get_endpoints().await;
                let payload = serde_json::to_string(&rep).unwrap_or_default();
                (true, None, None, Some(payload))
            } else {
                (
                    false,
                    None,
                    Some("TCP manager not available".to_string()),
                    None,
                )
            }
        }
        ControlRequest::GetUsageStats { event_cap } => {
            let report = tokio::task::spawn_blocking(move || {
                nuo_persistence::usage_stats::UsageStatsStore::new().report(event_cap)
            })
            .await
            .unwrap_or_default();
            match serde_json::to_string(&report) {
                Ok(payload) => (true, None, None, Some(payload)),
                Err(e) => (false, None, Some(format!("encode usage report: {e}")), None),
            }
        }
    };
    let reply = Wire::ControlReply {
        request_id,
        ok,
        thread_id: session_id,
        error,
        code: None,
        payload,
    };
    wire_sink
        .send(reply)
        .await
        .map_err(|e| format!("send control reply: {e}"))?;
    Ok(false)
}

/// Consecutive streamed deltas are coalesced for this long before framing.
///
/// A provider emits a delta per SSE event — a few characters at a time. One
/// frame per delta means one JSON encode, one length prefix and one write
/// syscall per few characters. Eight milliseconds is below the perceptual
/// threshold and cuts the frame count by an order of magnitude on a normal
/// answer.
const STREAM_COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeltaKind {
    Text,
    Reasoning,
}

/// One run of streamed deltas waiting to be framed.
#[derive(Debug)]
pub(crate) struct PendingDelta {
    pub(crate) thread_id: String,
    pub(crate) generation: u64,
    pub(crate) context: nuo_protocol::TurnContext,
    pub(crate) kind: DeltaKind,
    pub(crate) text: String,
}

impl PendingDelta {
    pub(crate) fn into_response(self) -> AgentResponse {
        let event = match self.kind {
            DeltaKind::Text => nuo_protocol::RoundEvent::StreamDelta(self.text),
            DeltaKind::Reasoning => nuo_protocol::RoundEvent::StreamReasoningDelta(self.text),
        };
        AgentResponse::Round {
            thread_id: self.thread_id,
            scope: nuo_protocol::RoundScope::Turn {
                context: self.context,
                event,
            },
        }
    }
}

/// Split a response into its streamable delta, or hand it back untouched.
///
/// `Err` means "not a delta" — the caller flushes the pending run first so
/// ordering is never reordered against a non-delta event.
#[allow(clippy::result_large_err)] // handing the untouched response back is the point
pub(crate) fn take_delta(
    response: AgentResponse,
) -> Result<(String, nuo_protocol::TurnContext, DeltaKind, String), AgentResponse> {
    match response {
        AgentResponse::Round {
            thread_id,
            scope: nuo_protocol::RoundScope::Turn { context, event },
        } => match event {
            nuo_protocol::RoundEvent::StreamDelta(text) => {
                Ok((thread_id, context, DeltaKind::Text, text))
            }
            nuo_protocol::RoundEvent::StreamReasoningDelta(text) => {
                Ok((thread_id, context, DeltaKind::Reasoning, text))
            }
            event => Err(AgentResponse::Round {
                thread_id,
                scope: nuo_protocol::RoundScope::Turn { context, event },
            }),
        },
        AgentResponse::Round {
            thread_id,
            scope: nuo_protocol::RoundScope::Session { event },
        } => Err(AgentResponse::Round {
            thread_id,
            scope: nuo_protocol::RoundScope::Session { event },
        }),
        other => Err(other),
    }
}

/// Append `text` to the pending run when it continues the same channel;
/// otherwise hand back the displaced run for the caller to send first.
pub(crate) fn queue_delta(
    pending: &mut Option<PendingDelta>,
    thread_id: String,
    generation: u64,
    context: nuo_protocol::TurnContext,
    kind: DeltaKind,
    text: String,
) -> Option<PendingDelta> {
    if let Some(current) = pending.as_mut()
        && current.kind == kind
        && current.thread_id == thread_id
        && current.generation == generation
        && current.context == context
    {
        current.text.push_str(&text);
        return None;
    }
    pending.replace(PendingDelta {
        thread_id,
        generation,
        context,
        kind,
        text,
    })
}

async fn send_delta(wire_sink: &mut BoxWireSink, delta: PendingDelta) -> Result<(), String> {
    let generation = delta.generation;
    wire_sink
        .send(Wire::Response {
            response: delta.into_response(),
            generation,
        })
        .await
        .map_err(|e| format!("wire send: {e}"))
}

async fn flush_delta(
    wire_sink: &mut BoxWireSink,
    pending: &mut Option<PendingDelta>,
) -> Result<(), String> {
    match pending.take() {
        Some(delta) => send_delta(wire_sink, delta).await,
        None => Ok(()),
    }
}

#[allow(clippy::result_large_err)]
async fn handle_wire_stream(
    mut wire_sink: BoxWireSink,
    mut wire_source: BoxWireStream,
    registry: Arc<crate::runtime::registry::ThreadRegistry>,
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
    conns: Arc<ConnTable>,
    tcp_manager: Option<Arc<TcpManager>>,
) -> Result<(), String> {
    let (client_protocol, client_version, client_posture, monitor_action) =
        match wire_source.next().await {
            Some(Ok(WireFrame::Wire(Wire::Hello {
                protocol,
                version,
                posture,
                monitor,
            }))) => (protocol, version, posture, monitor),
            Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                send_error_with_code(
                    &mut wire_sink,
                    None,
                    &format!(
                        "could not decode the first frame{}{detail}",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    ),
                    Some(nuo_protocol::ERR_PROTOCOL_MISMATCH),
                )
                .await?;
                return Ok(());
            }
            Some(Ok(WireFrame::Wire(_))) => {
                send_error(&mut wire_sink, None, "expected Hello as the first frame").await?;
                return Ok(());
            }
            Some(Err(error)) => {
                send_error(&mut wire_sink, None, &format!("bad first frame: {error}")).await?;
                return Ok(());
            }
            None => return Ok(()),
        };

    // `[INV-WIRE-04]` — exact `PROTOCOL_VERSION` match is the sole admission rule.
    if !protocol_accepts(client_protocol) {
        send_error_with_code(
            &mut wire_sink,
            None,
            &protocol_mismatch_error(client_protocol, client_version.as_deref()),
            Some(ERR_PROTOCOL_MISMATCH),
        )
        .await?;
        return Ok(());
    }

    // Subscribe to server lifecycle and (optionally) monitor diffs *before*
    // composing `ServerHello`, so no transition between snapshot and loop can
    // be missed (ADR-0260 `[INV-SRVPHASE-01]`, `[INV-SRVPHASE-02]`).
    let mut phase_rx = registry.subscribe_server_phase();
    let monitor_rx = if monitor_action.is_some_and(|m| m.watch) {
        Some(registry.subscribe_monitor())
    } else {
        None
    };

    let current_phase = phase_rx.borrow_and_update().clone();
    let monitor_snapshot = match monitor_action {
        Some(action) => Some(registry.monitor_snapshot(action).await),
        None => None,
    };
    let hello = Wire::ServerHello {
        server_id: registry.server_id().to_string(),
        protocol: nuo_protocol::PROTOCOL_VERSION,
        version: gate.version_of_server().to_string(),
        started_at: registry.server_started_at().await,
        phase: current_phase,
        persistence_health: registry.persistence_health_snapshot(),
        monitor: monitor_snapshot,
    };
    wire_sink
        .send(hello)
        .await
        .map_err(|e| format!("send server hello: {e}"))?;

    run_session_loop(SessionLoopParams {
        wire_sink,
        wire_source,
        registry,
        gate,
        listeners,
        conns,
        tcp_manager,
        client_posture,
        monitor_action,
        phase_rx,
        monitor_rx,
    })
    .await
}

/// One thread subscribed on a multiplexed client connection (ADR-0251, ADR-0253).
struct SubscribedThread {
    handle: crate::runtime::registry::AttachHandle,
    forwarder: tokio::task::JoinHandle<()>,
    /// Monotonic epoch for this subscription: starts at `1`, incremented on
    /// in-place re-key (`/new`, `/resume`, `/fork`) and re-subscription (ADR-0253 §2).
    generation: u64,
}

/// Multiplexed event delivered from any subscribed thread's broadcast stream
/// to the connection writer loop.
enum MuxEvent {
    Response {
        sub_key: String,
        response: Box<AgentResponse>,
    },
    Lagged {
        sub_key: String,
        skipped: u64,
    },
    Closed {
        sub_key: String,
    },
}

fn spawn_thread_forwarder(
    sub_key: String,
    mut rx: broadcast::Receiver<AgentResponse>,
    mux_tx: tokio::sync::mpsc::Sender<MuxEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(response) => {
                    if mux_tx
                        .send(MuxEvent::Response {
                            sub_key: sub_key.clone(),
                            response: Box::new(response),
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    if mux_tx
                        .send(MuxEvent::Lagged {
                            sub_key: sub_key.clone(),
                            skipped,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    let _ = mux_tx.send(MuxEvent::Closed { sub_key }).await;
                    break;
                }
            }
        }
    })
}

/// Subscribe a resolved thread handle onto `wire_sink`, sending
/// `Wire::Subscribed`, accounting the human channel posture, and emitting the
/// authoritative snapshot + readiness transition (`[INV-ATTACH-04]`).
async fn hydrate_subscribed_thread(
    wire_sink: &mut BoxWireSink,
    handle: &crate::runtime::registry::AttachHandle,
    client_posture: nuo_protocol::human_request::HumanChannelPosture,
    request_id: u64,
) -> Result<(String, broadcast::Receiver<AgentResponse>), String> {
    // Subscribe before reading any cached state: anything the build task or the
    // live driver publishes from here on is buffered for this connection.
    // `[INV-ATTACH-04]` (ADR-0133): the authoritative snapshot precedes `Ready`.
    let rx = handle.events().subscribe();
    let session_id = handle.session_id().await;
    let phase = handle.phase();

    wire_sink
        .send(Wire::Subscribed {
            request_id,
            thread_id: session_id.clone(),
            generation: 1,
            phase,
        })
        .await
        .map_err(|e| format!("send thread subscribed: {e}"))?;

    // Fold this client's declared posture into the session's human channel
    // before publishing the snapshot, so the trust gate sees the effective
    // posture rather than a per-connection guess.
    let after_attach = handle.human_channel().attach(client_posture);
    tracing::info!(
        session_id = %session_id,
        effective = ?after_attach,
        "nuo server: human channel accounted"
    );

    // Hydrate, then announce readiness. A hosted session builds the snapshot
    // on demand (local reads only); a building session returns the snapshot
    // cached at promotion, or receives it on the phase stream below.
    if let Some(snapshot) = handle.snapshot().await {
        let trust_notice = (after_attach
            != nuo_protocol::human_request::HumanChannelPosture::Interactive)
            .then(|| {
                snapshot
                    .harness_state
                    .as_ref()
                    .filter(|harness| harness.workspace_security.needs_review())
                    .map(|harness| workspace_trust_notice(&session_id, &harness.workspace_security))
            })
            .flatten();
        wire_sink
            .send(Wire::Response {
                response: AgentResponse::ThreadSnapshot(Box::new(snapshot)),
                generation: 1,
            })
            .await
            .map_err(|e| format!("send thread snapshot: {e}"))?;
        // A client cannot answer a gate from a pipe; surface the trust posture
        // passively for non-interactive attaches (ADR-0127).
        if let Some(response) = trust_notice {
            wire_sink
                .send(Wire::Response {
                    response,
                    generation: 1,
                })
                .await
                .map_err(|e| format!("send trust notice: {e}"))?;
        }
        // `Ready` is a *transition* the client gates on; it is deliberately not
        // inferred from `Subscribed`'s phase cell, so the snapshot can never
        // race past it.
        wire_sink
            .send(Wire::Response {
                response: AgentResponse::ThreadPhase(nuo_protocol::ThreadPhaseUpdate {
                    thread_id: session_id.clone(),
                    phase: nuo_protocol::ThreadPhase::Ready,
                }),
                generation: 1,
            })
            .await
            .map_err(|e| format!("send ready phase: {e}"))?;
    }

    Ok((session_id, rx))
}

/// Tracks every thread subscribed on one multiplexed connection, with flat
/// path-compressed aliases for in-place `/new`, `/resume`, and `/fork` re-keying.
struct SubscriptionTable {
    entries: std::collections::HashMap<String, SubscribedThread>,
    /// Flat map from historical `sub_key` -> current live key in `entries`.
    aliases: std::collections::HashMap<String, String>,
    primary_thread_id: Option<String>,
}

impl SubscriptionTable {
    fn empty() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            aliases: std::collections::HashMap::new(),
            primary_thread_id: None,
        }
    }

    fn primary(&self) -> Option<&str> {
        self.primary_thread_id.as_deref()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn get(&self, thread_id: &str) -> Option<&SubscribedThread> {
        self.entries.get(thread_id)
    }

    fn get_mut(&mut self, thread_id: &str) -> Option<&mut SubscribedThread> {
        self.entries.get_mut(thread_id)
    }

    fn insert(&mut self, thread_id: String, sub: SubscribedThread) {
        self.aliases.remove(&thread_id);
        if self.primary_thread_id.is_none() {
            self.primary_thread_id = Some(thread_id.clone());
        }
        self.entries.insert(thread_id, sub);
    }

    /// Resolve a forwarder's `sub_key` to its current live subscription key in O(1).
    fn resolve_key(&self, key: &str) -> Option<String> {
        if self.entries.contains_key(key) {
            return Some(key.to_string());
        }
        self.aliases
            .get(key)
            .filter(|target| self.entries.contains_key(*target))
            .cloned()
    }

    /// Re-key an existing subscription after an in-place reset (`/new`,
    /// `/resume`, `/fork`), flattening all prior aliases to point directly to
    /// `next_id` so alias resolution is always O(1) with no hop limit.
    fn rekey(&mut self, previous_id: &str, next_id: String) {
        if previous_id == next_id {
            return;
        }
        if let Some(mut sub) = self.entries.remove(previous_id) {
            sub.generation = sub.generation.saturating_add(1);
            self.entries.insert(next_id.clone(), sub);
            for target in self.aliases.values_mut() {
                if target == previous_id {
                    *target = next_id.clone();
                }
            }
            self.aliases.remove(&next_id);
            self.aliases
                .insert(previous_id.to_string(), next_id.clone());
            if self.primary_thread_id.as_deref() == Some(previous_id) {
                self.primary_thread_id = Some(next_id);
            }
        }
    }

    /// Remove a subscription and prune all aliases targeting it.
    fn remove(&mut self, thread_id: &str) -> Option<SubscribedThread> {
        let removed = self.entries.remove(thread_id);
        if removed.is_some() {
            self.aliases.retain(|_, target| target != thread_id);
            if self.primary_thread_id.as_deref() == Some(thread_id) {
                self.primary_thread_id = self.entries.keys().next().cloned();
            }
        }
        removed
    }

    fn teardown(&mut self) {
        for (session_id, sub) in self.entries.drain() {
            sub.forwarder.abort();
            let after_detach = sub.handle.human_channel().detach();
            tracing::info!(
                session_id = %session_id,
                effective = ?after_detach,
                "nuo server: human channel released"
            );
        }
        self.aliases.clear();
        self.primary_thread_id = None;
    }
}

enum MuxControl {
    Continue,
    Stop(Result<(), String>),
}

async fn handle_mux_event(
    event: Option<MuxEvent>,
    subs: &mut SubscriptionTable,
    wire_sink: &mut BoxWireSink,
    pending_delta: &mut Option<PendingDelta>,
    flush_deadline: &mut Option<tokio::time::Instant>,
) -> MuxControl {
    match event {
        Some(MuxEvent::Response { sub_key, response }) => {
            let response = *response;
            // Track in-place session re-keying (`/new`, `/resume`, `/fork`).
            match &response {
                AgentResponse::ConversationCleared {
                    previous_id,
                    thread_id,
                }
                | AgentResponse::ConversationReplaced {
                    previous_id,
                    thread_id,
                    ..
                } if previous_id != thread_id => {
                    subs.rekey(previous_id, thread_id.clone());
                }
                _ => {}
            }
            let generation = subs
                .resolve_key(&sub_key)
                .and_then(|k| subs.get(&k))
                .map_or(1, |s| s.generation);
            match take_delta(response) {
                Ok((thread_id, context, kind, text)) => {
                    if let Some(displaced) =
                        queue_delta(pending_delta, thread_id, generation, context, kind, text)
                        && let Err(e) = send_delta(wire_sink, displaced).await
                    {
                        return MuxControl::Stop(Err(e));
                    }
                    if flush_deadline.is_none() {
                        *flush_deadline =
                            Some(tokio::time::Instant::now() + STREAM_COALESCE_WINDOW);
                    }
                }
                Err(response) => {
                    if let Err(e) = flush_delta(wire_sink, pending_delta).await {
                        return MuxControl::Stop(Err(e));
                    }
                    *flush_deadline = None;
                    if let Err(e) = wire_sink
                        .send(Wire::Response {
                            response,
                            generation,
                        })
                        .await
                    {
                        return MuxControl::Stop(Err(format!("wire send: {e}")));
                    }
                }
            }
            MuxControl::Continue
        }
        Some(MuxEvent::Lagged { sub_key, skipped }) => {
            if let Err(e) = flush_delta(wire_sink, pending_delta).await {
                return MuxControl::Stop(Err(e));
            }
            *flush_deadline = None;
            let Some(key) = subs.resolve_key(&sub_key) else {
                return MuxControl::Continue;
            };
            tracing::warn!(
                thread_id = %key,
                skipped,
                "nuo server: client lagged; re-anchoring from snapshot"
            );
            if let Some(sub) = subs.get_mut(&key) {
                let generation = sub.generation;
                if let Some(snapshot) = sub.handle.snapshot().await {
                    if let Err(e) = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadSnapshot(Box::new(snapshot)),
                            generation,
                        })
                        .await
                    {
                        return MuxControl::Stop(Err(format!("wire send lag snapshot: {e}")));
                    }
                    if let Err(e) = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadPhase(nuo_protocol::ThreadPhaseUpdate {
                                thread_id: key,
                                phase: nuo_protocol::ThreadPhase::Ready,
                            }),
                            generation,
                        })
                        .await
                    {
                        return MuxControl::Stop(Err(format!("wire send lag ready: {e}")));
                    }
                }
            }
            MuxControl::Continue
        }
        Some(MuxEvent::Closed { sub_key }) => {
            if let Err(e) = flush_delta(wire_sink, pending_delta).await {
                return MuxControl::Stop(Err(e));
            }
            *flush_deadline = None;
            if let Some(key) = subs.resolve_key(&sub_key)
                && let Some(sub) = subs.remove(&key)
            {
                sub.forwarder.abort();
                let after_detach = sub.handle.human_channel().detach();
                tracing::info!(
                    session_id = %key,
                    effective = ?after_detach,
                    "nuo server: subscribed thread closed; human channel released"
                );
            }
            MuxControl::Continue
        }
        None => {
            let _ = flush_delta(wire_sink, pending_delta).await;
            MuxControl::Stop(Ok(()))
        }
    }
}

struct MuxEnv<'a> {
    client_posture: nuo_protocol::human_request::HumanChannelPosture,
    registry: &'a crate::runtime::registry::ThreadRegistry,
    mux_tx: &'a tokio::sync::mpsc::Sender<MuxEvent>,
}

async fn handle_subscribe_frame(
    request_id: u64,
    target: ThreadTarget,
    sub_project: Option<std::path::PathBuf>,
    env: &MuxEnv<'_>,
    subs: &mut SubscriptionTable,
    wire_sink: &mut BoxWireSink,
) -> MuxControl {
    // Explicit non-ready admission (ADR-0260 `[INV-SRVPHASE-03]`): never queue
    // `Subscribe` silently behind opaque timeouts.
    if let Some((code, message)) = non_ready_refusal(&env.registry.server_phase()) {
        if let Err(e) =
            send_error_with_code(wire_sink, Some(request_id), &message, Some(code)).await
        {
            return MuxControl::Stop(Err(e));
        }
        return MuxControl::Continue;
    }

    let raw_project = sub_project.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    let sub_caller_project = nuo_host::paths::find_project_root(&raw_project);
    let declared_project = sub_project.is_some();

    match env
        .registry
        .resolve_with_declaration(target, &sub_caller_project, declared_project)
        .await
    {
        crate::runtime::registry::ResolveOutcome::Welcome(new_handle) => {
            let tid = new_handle.session_id().await;
            if let Some(existing) = subs.get_mut(&tid) {
                // Idempotent re-subscribe: upgrade any `Building` handle to the
                // live `Hosted` handle once promoted, advance generation, and
                // re-send acknowledgement + live snapshot without double-attaching
                // the human channel.
                if matches!(
                    new_handle,
                    crate::runtime::registry::AttachHandle::Hosted(_)
                ) {
                    existing.handle = new_handle;
                }
                existing.generation = existing.generation.saturating_add(1);
                let generation = existing.generation;
                let phase = existing.handle.phase();
                if let Err(e) = wire_sink
                    .send(Wire::Subscribed {
                        request_id,
                        thread_id: tid.clone(),
                        generation,
                        phase,
                    })
                    .await
                {
                    return MuxControl::Stop(Err(format!("wire send subscribed: {e}")));
                }
                if let Some(snapshot) = existing.handle.snapshot().await {
                    if let Err(e) = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadSnapshot(Box::new(snapshot)),
                            generation,
                        })
                        .await
                    {
                        return MuxControl::Stop(Err(format!(
                            "wire send resubscribe snapshot: {e}"
                        )));
                    }
                    if let Err(e) = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadPhase(nuo_protocol::ThreadPhaseUpdate {
                                thread_id: tid,
                                phase: nuo_protocol::ThreadPhase::Ready,
                            }),
                            generation,
                        })
                        .await
                    {
                        return MuxControl::Stop(Err(format!("wire send resubscribe ready: {e}")));
                    }
                }
            } else {
                match hydrate_subscribed_thread(
                    wire_sink,
                    &new_handle,
                    env.client_posture,
                    request_id,
                )
                .await
                {
                    Ok((new_tid, new_rx)) => {
                        let forwarder =
                            spawn_thread_forwarder(new_tid.clone(), new_rx, env.mux_tx.clone());
                        subs.insert(
                            new_tid,
                            SubscribedThread {
                                handle: new_handle,
                                forwarder,
                                generation: 1,
                            },
                        );
                    }
                    Err(e) => return MuxControl::Stop(Err(e)),
                }
            }
        }
        crate::runtime::registry::ResolveOutcome::Pick { sessions } => {
            if let Err(e) = wire_sink
                .send(Wire::Pick {
                    request_id,
                    threads: sessions,
                })
                .await
            {
                return MuxControl::Stop(Err(format!("send pick: {e}")));
            }
        }
        crate::runtime::registry::ResolveOutcome::Refused { message, code } => {
            if let Err(e) = send_error_with_code(wire_sink, Some(request_id), &message, code).await
            {
                return MuxControl::Stop(Err(e));
            }
        }
    }
    MuxControl::Continue
}

async fn handle_end_thread(
    target_id: String,
    registry: &crate::runtime::registry::ThreadRegistry,
    subs: &mut SubscriptionTable,
    mux_rx: &mut tokio::sync::mpsc::Receiver<MuxEvent>,
    wire_sink: &mut BoxWireSink,
    pending_delta: &mut Option<PendingDelta>,
    flush_deadline: &mut Option<tokio::time::Instant>,
) -> MuxControl {
    let killed = match registry.kill_session(&target_id).await {
        Ok(()) => {
            tracing::info!(session = %target_id, "nuo server: client declared session end");
            true
        }
        Err(e) => {
            if registry.cancel_building(&target_id).await {
                tracing::info!(
                    session = %target_id,
                    "nuo server: client cancelled an in-flight session build"
                );
            } else {
                tracing::debug!(
                    session = %target_id,
                    error = %e,
                    "nuo server: session already gone on client end"
                );
            }
            false
        }
    };
    if let Err(e) = flush_delta(wire_sink, pending_delta).await {
        return MuxControl::Stop(Err(e));
    }
    *flush_deadline = None;

    // Detach and drop the handle first so this connection does not keep the
    // broadcast channel open, while keeping the forwarder alive long enough to
    // deliver the terminal `Exit` frame broadcast by `kill_session`.
    let (forwarder, ended_generation) = if let Some(sub) = subs.remove(&target_id) {
        let _ = sub.handle.human_channel().detach();
        drop(sub.handle);
        (Some(sub.forwarder), sub.generation)
    } else {
        (None, 1)
    };

    if killed && forwarder.is_some() {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(250);
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, mux_rx.recv()).await {
            match ev {
                MuxEvent::Response { response, .. } => {
                    let is_exit = matches!(*response, AgentResponse::Exit);
                    if wire_sink
                        .send(Wire::Response {
                            response: *response,
                            generation: ended_generation,
                        })
                        .await
                        .is_err()
                        || is_exit
                    {
                        break;
                    }
                }
                MuxEvent::Closed { sub_key } if sub_key == target_id => break,
                MuxEvent::Lagged { .. } | MuxEvent::Closed { .. } => {}
            }
        }
    }

    if let Some(forwarder) = forwarder {
        forwarder.abort();
    }

    MuxControl::Continue
}

async fn forward_thread_request(
    target_id: &str,
    request: AgentRequest,
    subs: &SubscriptionTable,
    wire_sink: &mut BoxWireSink,
) {
    let Some(sub) = subs.get(target_id) else {
        tracing::warn!(
            target = %target_id,
            "nuo server: request addressed to unsubscribed thread"
        );
        return;
    };
    let req_tx = sub.handle.req_tx();
    match req_tx.try_send(request) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("nuo server: session request queue full, shedding load");
            let err = Wire::Error {
                request_id: None,
                message: "server session request queue is full (server busy)".to_string(),
                code: Some("server_busy".to_string()),
            };
            let _ = wire_sink.send(err).await;
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!(
                target = %target_id,
                "nuo server: session driver channel closed"
            );
        }
    }
}

struct SessionLoopParams {
    wire_sink: BoxWireSink,
    wire_source: BoxWireStream,
    registry: Arc<crate::runtime::registry::ThreadRegistry>,
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
    conns: Arc<ConnTable>,
    tcp_manager: Option<Arc<TcpManager>>,
    client_posture: nuo_protocol::human_request::HumanChannelPosture,
    monitor_action: Option<MonitorAction>,
    phase_rx: tokio::sync::watch::Receiver<ServerPhase>,
    monitor_rx: Option<broadcast::Receiver<MonitorEvent>>,
}

async fn recv_monitor_opt(
    rx: &mut Option<broadcast::Receiver<MonitorEvent>>,
) -> Option<Result<MonitorEvent, broadcast::error::RecvError>> {
    match rx {
        Some(rx) => Some(rx.recv().await),
        None => std::future::pending().await,
    }
}

async fn run_session_loop(params: SessionLoopParams) -> Result<(), String> {
    let SessionLoopParams {
        mut wire_sink,
        mut wire_source,
        registry,
        gate,
        listeners,
        conns,
        tcp_manager,
        client_posture,
        monitor_action,
        mut phase_rx,
        mut monitor_rx,
    } = params;

    let (mux_tx, mut mux_rx) = tokio::sync::mpsc::channel::<MuxEvent>(1024);
    let mut subs = SubscriptionTable::empty();
    let mut interactive_guard = None;
    let mut pending_delta: Option<PendingDelta> = None;
    let mut flush_deadline: Option<tokio::time::Instant> = None;
    let mut drain_latched = false;

    let drain = gate.triggered();
    tokio::pin!(drain);

    let loop_result: Result<(), String> = loop {
        tokio::select! {
            _ = tokio::time::sleep_until(
                flush_deadline.unwrap_or_else(tokio::time::Instant::now),
            ), if flush_deadline.is_some() => {
                if let Err(e) = flush_delta(&mut wire_sink, &mut pending_delta).await {
                    break Err(e);
                }
                flush_deadline = None;
            }
            _ = &mut drain, if !drain_latched => {
                drain_latched = true;
                if !registry.server_phase().is_terminal() {
                    registry.set_server_phase(ServerPhase::Draining {
                        reason: gate.reason().map(|r| r.to_string()),
                    });
                }
            }
            changed = phase_rx.changed() => {
                if changed.is_err() {
                    break Ok(());
                }
                let phase = phase_rx.borrow_and_update().clone();
                if let Err(e) = flush_delta(&mut wire_sink, &mut pending_delta).await {
                    break Err(e);
                }
                flush_deadline = None;
                let is_failed = matches!(phase, ServerPhase::Failed { .. });
                let is_draining = matches!(phase, ServerPhase::Draining { .. });
                if let Err(e) = send_monitor(&mut wire_sink, MonitorEvent::ServerPhaseChanged(phase)).await {
                    break Err(e);
                }
                // On fatal startup failure, flush and close deterministically
                // so `conns.drain_with_budget` exits immediately without racing
                // socket teardown (ADR-0266 `[INV-SRVPHASE-06]`).
                if is_failed || (is_draining && subs.is_empty() && monitor_action.is_some_and(|m| m.watch)) {
                    let _ = wire_sink.close().await;
                    break Ok(());
                }
            }
            Some(mon) = recv_monitor_opt(&mut monitor_rx) => {
                match mon {
                    Ok(MonitorEvent::ServerPhaseChanged(_)) => {
                        // ADR-0266 `[INV-SRVPHASE-07]`: ServerPhaseChanged streams
                        // exclusively from `phase_rx` (watch channel).
                    }
                    Ok(event) => {
                        let include_idle = monitor_action.is_some_and(|m| m.include_idle);
                        let forward = match &event {
                            MonitorEvent::ThreadAdded(row) | MonitorEvent::ThreadUpdated(row) => {
                                include_idle || row.status.is_active()
                            }
                            _ => true,
                        };
                        if forward {
                            if let Err(e) = flush_delta(&mut wire_sink, &mut pending_delta).await {
                                break Err(e);
                            }
                            flush_deadline = None;
                            if let Err(e) = send_monitor(&mut wire_sink, event).await {
                                break Err(e);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "nuo server: monitor client lagged, resyncing");
                        if let Err(e) = flush_delta(&mut wire_sink, &mut pending_delta).await {
                            break Err(e);
                        }
                        flush_deadline = None;
                        if let Some(action) = monitor_action {
                            let snapshot = registry.monitor_snapshot(action).await;
                            if let Err(e) = send_monitor(&mut wire_sink, MonitorEvent::Snapshot(snapshot)).await {
                                break Err(e);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        monitor_rx = None;
                    }
                }
            }
            mux_event = mux_rx.recv() => {
                if let MuxControl::Stop(res) = handle_mux_event(
                    mux_event,
                    &mut subs,
                    &mut wire_sink,
                    &mut pending_delta,
                    &mut flush_deadline,
                ).await {
                    break res;
                }
            }
            msg = wire_source.next() => match msg {
                Some(Ok(WireFrame::Wire(Wire::Control { request_id, request }))) => {
                    if let Err(e) = flush_delta(&mut wire_sink, &mut pending_delta).await {
                        break Err(e);
                    }
                    flush_deadline = None;
                    match execute_control(
                        &mut wire_sink,
                        &registry,
                        &gate,
                        &listeners,
                        &tcp_manager,
                        request_id,
                        request,
                    )
                    .await
                    {
                        Ok(true) => break Ok(()),
                        Ok(false) => {}
                        Err(e) => break Err(e),
                    }
                }
                Some(Ok(WireFrame::Wire(Wire::Subscribe {
                    request_id,
                    target,
                    project: sub_project,
                }))) => {
                    if interactive_guard.is_none() {
                        interactive_guard = Some(conns.register_interactive());
                    }
                    if let Err(e) = flush_delta(&mut wire_sink, &mut pending_delta).await {
                        break Err(e);
                    }
                    flush_deadline = None;
                    let env = MuxEnv {
                        client_posture,
                        registry: &registry,
                        mux_tx: &mux_tx,
                    };
                    if let MuxControl::Stop(res) = handle_subscribe_frame(
                        request_id,
                        target,
                        sub_project,
                        &env,
                        &mut subs,
                        &mut wire_sink,
                    )
                    .await
                    {
                        break res;
                    }
                }
                Some(Ok(WireFrame::Wire(Wire::Unsubscribe { thread_id }))) => {
                    if let Err(e) = flush_delta(&mut wire_sink, &mut pending_delta).await {
                        break Err(e);
                    }
                    flush_deadline = None;
                    if let Some(sub) = subs.remove(&thread_id) {
                        sub.forwarder.abort();
                        let after_detach = sub.handle.human_channel().detach();
                        tracing::info!(
                            session_id = %thread_id,
                            effective = ?after_detach,
                            "nuo server: thread unsubscribed; human channel released"
                        );
                    }
                }
                Some(Ok(WireFrame::Wire(Wire::Request {
                    thread_id,
                    request: AgentRequest::EndThread,
                }))) => {
                    let Some(target_id) = thread_id
                        .as_deref()
                        .or_else(|| subs.primary())
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    if let MuxControl::Stop(res) = handle_end_thread(
                        target_id,
                        &registry,
                        &mut subs,
                        &mut mux_rx,
                        &mut wire_sink,
                        &mut pending_delta,
                        &mut flush_deadline,
                    ).await {
                        break res;
                    }
                }
                Some(Ok(WireFrame::Wire(Wire::Request { thread_id, request }))) => {
                    let Some(target_id) = thread_id
                        .as_deref()
                        .or_else(|| subs.primary())
                        .map(str::to_string)
                    else {
                        let _ = send_error(
                            &mut wire_sink,
                            None,
                            "cannot deliver request: connection has no subscribed thread",
                        )
                        .await;
                        continue;
                    };
                    forward_thread_request(&target_id, request, &subs, &mut wire_sink).await;
                }
                Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                    tracing::warn!(
                        session = subs.primary().unwrap_or("<none>"),
                        envelope = envelope.as_deref().unwrap_or("<none>"),
                        %detail,
                        "nuo server: undecodable frame from client; connection retained"
                    );
                }
                Some(Ok(WireFrame::Wire(_))) => {}
                Some(Err(e)) => tracing::warn!(error = %e, "nuo server: bad wire request"),
                None => {
                    let _ = flush_delta(&mut wire_sink, &mut pending_delta).await;
                    break Ok(());
                }
            },
        }
    };

    subs.teardown();
    loop_result
}

/// Everything one accepted TCP connection brings to its handshake: the socket,
/// the shared services it talks to, and the drain/cancel plumbing that owns it.
struct ConnectionParams<S> {
    stream: S,
    registry: Arc<crate::runtime::registry::ThreadRegistry>,
    token: Option<String>,
    expose: ServeExpose,
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
    conns: Arc<ConnTable>,
    tcp_manager: Option<Arc<TcpManager>>,
}

#[allow(clippy::result_large_err)] // One handshake path: the rejection carries the HTTP reason verbatim.
async fn handle_connection<S>(params: ConnectionParams<S>) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let ConnectionParams {
        stream,
        registry,
        token,
        expose,
        gate,
        listeners,
        conns,
        tcp_manager,
    } = params;
    // One handshake path for every TCP connection, token or not:
    // the callback is where browser-origin and Host validation live, and
    // those checks apply to unauthenticated loopback listeners too.
    let ws_stream = tokio_tungstenite::accept_hdr_async(
        stream,
        move |req: &Request, mut resp: tungstenite::handshake::server::Response| {
            validate_origin(req, expose)?;
            if let Some(expected) = token.as_deref() {
                match check_credentials(req, expected) {
                    Ok(Some(protocol)) => {
                        // The browser channel: the token arrives as
                        // a `bearer.<token>` subprotocol offer; the handshake
                        // must echo it or the browser aborts the connection.
                        if let Ok(value) = tungstenite::http::HeaderValue::from_str(&protocol) {
                            resp.headers_mut()
                                .insert(tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL, value);
                        }
                    }
                    Ok(None) => {}
                    Err(rejection) => return Err(rejection),
                }
            }
            Ok(resp)
        },
    )
    .await
    .map_err(|e| format!("ws handshake: {e}"))?;

    let (wire_sink, wire_source) = websocket_split(ws_stream);
    handle_wire_stream(
        wire_sink,
        wire_source,
        registry,
        gate,
        listeners,
        conns,
        tcp_manager,
    )
    .await
}

/// Send one server lifecycle / monitor stream frame.
async fn send_monitor(wire_sink: &mut BoxWireSink, event: MonitorEvent) -> Result<(), String> {
    wire_sink
        .send(Wire::Monitor { event })
        .await
        .map_err(|e| format!("send monitor event: {e}"))
}

async fn send_error(
    wire_sink: &mut BoxWireSink,
    request_id: Option<u64>,
    message: &str,
) -> Result<(), String> {
    send_error_with_code(wire_sink, request_id, message, None).await
}

/// `send_error` with a stable machine-readable `code` so clients
/// can branch on the reason instead of string-sniffing the message.
async fn send_error_with_code(
    wire_sink: &mut BoxWireSink,
    request_id: Option<u64>,
    message: &str,
    code: Option<&str>,
) -> Result<(), String> {
    wire_sink
        .send(Wire::Error {
            request_id,
            message: message.to_string(),
            code: code.map(str::to_string),
        })
        .await
        .map_err(|e| format!("send error: {e}"))
}
