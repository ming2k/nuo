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
use protocol::{
    AgentRequest, AgentResponse, AttachAction, ControlRequest, ERR_PROTOCOL_MISMATCH,
    MonitorAction, MonitorEvent, Wire, WireFrame, protocol_accepts,
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
                if let Err(e) = crate::transport::health_http::serve(
                    stream,
                    gate.version_of_server(),
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

/// Execute a session-management verb and reply once. The control
/// channel is request/response (unlike the streaming monitor/attach roles):
/// one `ControlRequest` in, one `ControlReply` out, then the connection may
/// close or issue another verb.
async fn run_control(
    mut wire_sink: BoxWireSink,
    registry: Arc<crate::runtime::registry::ThreadRegistry>,
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
    tcp_manager: Option<Arc<TcpManager>>,
    request: ControlRequest,
) -> Result<(), String> {
    let (ok, session_id, error, payload) = match request {
        ControlRequest::Reload => {
            // ADR-0034 Level 1 (soft reload): refresh the server's config
            // snapshot and re-sync MCP + skills across every hosted session.
            // No connection is dropped and no round is disturbed.
            let reply = match registry.reload_runtime().await {
                Ok(()) => Wire::ControlReply {
                    ok: true,
                    thread_id: None,
                    error: None,
                    payload: None,
                },
                Err(error) => Wire::ControlReply {
                    ok: false,
                    thread_id: None,
                    error: Some(error),
                    payload: None,
                },
            };
            wire_sink
                .send(reply)
                .await
                .map_err(|e| format!("send control reply: {e}"))?;
            return Ok(());
        }
        ControlRequest::Shutdown => {
            // The remote stop verb: acknowledge on the wire first
            // — the gate fires the same drain as any signal, which would
            // otherwise cancel this very connection before the reply lands.
            let reply = Wire::ControlReply {
                ok: true,
                thread_id: None,
                error: None,
                payload: None,
            };
            wire_sink
                .send(reply)
                .await
                .map_err(|e| format!("send control reply: {e}"))?;
            let _ = wire_sink.close().await;
            // The verb is the same trigger as a signal: latch the gate (so
            // monitors stream ServerDraining and the run loop drains), and
            // stop the accept loops immediately — even when the serve layer
            // is driven standalone (no host::run), the listeners must stop.
            gate.request(crate::runtime::shutdown::ShutdownReason::ControlVerb, false);
            listeners.cancel();
            return Ok(());
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
            request_id,
            decision,
        } => match registry
            .resolve_permission(&thread_id, request_id, decision)
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
            let Some(hosted) = registry.get(&thread_id).await else {
                return Err("thread is not live".to_string());
            };
            let from = from_fact_id.map(protocol::FactId::from_raw);
            match hosted.session.fork_branch(name, from).await {
                Ok(branch) => (
                    true,
                    Some(thread_id),
                    None,
                    Some(serde_json::json!({ "branch_id": branch.0 }).to_string()),
                ),
                Err(error) => (false, Some(thread_id), Some(error), None),
            }
        }
        ControlRequest::ListBranches { thread_id } => {
            let Some(hosted) = registry.get(&thread_id).await else {
                return Err("thread is not live".to_string());
            };
            match hosted.session.branches().await {
                Ok(branches) => (
                    true,
                    Some(thread_id),
                    None,
                    Some(serde_json::to_string(&branches).unwrap_or_else(|_| "[]".to_string())),
                ),
                Err(error) => (false, Some(thread_id), Some(error), None),
            }
        }
        ControlRequest::SwitchBranch {
            thread_id,
            branch_id,
        } => {
            let Some(hosted) = registry.get(&thread_id).await else {
                return Err("thread is not live".to_string());
            };
            match hosted
                .session
                .switch_branch(protocol::BranchId::new(branch_id))
                .await
            {
                Ok(()) => (true, Some(thread_id), None, None),
                Err(error) => (false, Some(thread_id), Some(error), None),
            }
        }
        ControlRequest::DeleteBranch {
            thread_id,
            branch_id,
        } => {
            let Some(hosted) = registry.get(&thread_id).await else {
                return Err("thread is not live".to_string());
            };
            match hosted
                .session
                .delete_branch(protocol::BranchId::new(branch_id))
                .await
            {
                Ok(()) => (true, Some(thread_id), None, None),
                Err(error) => (false, Some(thread_id), Some(error), None),
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
            // The Archivist's answer is a successful query result, so it rides
            // the reply payload. A failed round keeps ok=false with the error.
            let answer = registry.ask_archivist(text).await;
            match answer.status {
                crate::runtime::archivist_service::ArchivistTurnStatus::Failed(e) => {
                    (false, None, Some(e), None)
                }
                _ => (true, None, None, Some(answer.text)),
            }
        }
        ControlRequest::OpenTcpListener { port } => {
            if let Some(mgr) = &tcp_manager {
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
            if let Some(mgr) = &tcp_manager {
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
            if let Some(mgr) = &tcp_manager {
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
            // ADR-0129 [INV-USAGE-01]: session-free model usage query. The
            // aggregate folds every day blob, so it runs on the blocking pool —
            // a control connection must never stall the accept loop.
            let report = tokio::task::spawn_blocking(move || {
                persistence::usage_stats::UsageStatsStore::new().report(event_cap)
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
        ok,
        thread_id: session_id,
        error,
        payload,
    };
    wire_sink
        .send(reply)
        .await
        .map_err(|e| format!("send control reply: {e}"))?;
    let _ = wire_sink.close().await;
    Ok(())
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
    pub(crate) context: protocol::TurnContext,
    pub(crate) kind: DeltaKind,
    pub(crate) text: String,
}

impl PendingDelta {
    pub(crate) fn into_response(self) -> AgentResponse {
        let event = match self.kind {
            DeltaKind::Text => protocol::RoundEvent::StreamDelta(self.text),
            DeltaKind::Reasoning => protocol::RoundEvent::StreamReasoningDelta(self.text),
        };
        AgentResponse::Round {
            thread_id: self.thread_id,
            scope: protocol::RoundScope::Turn {
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
) -> Result<(String, protocol::TurnContext, DeltaKind, String), AgentResponse> {
    match response {
        AgentResponse::Round {
            thread_id,
            scope: protocol::RoundScope::Turn { context, event },
        } => match event {
            protocol::RoundEvent::StreamDelta(text) => {
                Ok((thread_id, context, DeltaKind::Text, text))
            }
            protocol::RoundEvent::StreamReasoningDelta(text) => {
                Ok((thread_id, context, DeltaKind::Reasoning, text))
            }
            event => Err(AgentResponse::Round {
                thread_id,
                scope: protocol::RoundScope::Turn { context, event },
            }),
        },
        AgentResponse::Round {
            thread_id,
            scope: protocol::RoundScope::Session { event },
        } => Err(AgentResponse::Round {
            thread_id,
            scope: protocol::RoundScope::Session { event },
        }),
        other => Err(other),
    }
}

/// Append `text` to the pending run when it continues the same channel;
/// otherwise hand back the displaced run for the caller to send first.
pub(crate) fn queue_delta(
    pending: &mut Option<PendingDelta>,
    thread_id: String,
    context: protocol::TurnContext,
    kind: DeltaKind,
    text: String,
) -> Option<PendingDelta> {
    if let Some(current) = pending.as_mut()
        && current.kind == kind
        && current.thread_id == thread_id
        && current.context == context
    {
        current.text.push_str(&text);
        return None;
    }
    pending.replace(PendingDelta {
        thread_id,
        context,
        kind,
        text,
    })
}

async fn send_delta(wire_sink: &mut BoxWireSink, delta: PendingDelta) -> Result<(), String> {
    wire_sink
        .send(Wire::Response {
            response: delta.into_response(),
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
    let (action, project, client_posture, client_version, client_protocol) =
        match wire_source.next().await {
            Some(Ok(WireFrame::Wire(Wire::Select {
                action,
                project,
                posture,
                version,
                protocol,
            }))) => (action, project, posture, version, protocol),
            // A first frame we cannot model is still a first frame: answer it
            // with the taxonomy rather than a bare disconnect, so the peer's
            // disclosure names the skew instead of a transport fault.
            Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                send_error_with_code(
                    &mut wire_sink,
                    &format!(
                        "could not decode the first frame{}{detail}",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    ),
                    Some(protocol::ERR_PROTOCOL_MISMATCH),
                )
                .await?;
                return Ok(());
            }
            Some(Ok(WireFrame::Wire(_))) => {
                send_error(&mut wire_sink, "expected Select as the first frame").await?;
                return Ok(());
            }
            Some(Err(error)) => {
                send_error(&mut wire_sink, &format!("bad first frame: {error}")).await?;
                return Ok(());
            }
            None => return Ok(()),
        };
    // Protocol negotiation, enforced before any session work.
    //
    // `[INV-WIRE-04]` — the protocol number is the **only** admission authority.
    // The server serves any number in `[MIN_PROTOCOL_VERSION, PROTOCOL_VERSION]`
    // regardless of the product build, which is what lets a pinned client keep
    // talking to a newer server across additive changes. There is no second
    // lane: the field is required, so a peer that omits it fails to decode
    // above (and is answered with the protocol taxonomy) instead of being judged
    // by a different rule. The product version is advisory identity, used only
    // to make the refusal name both builds.
    if !protocol_accepts(client_protocol) {
        send_error_with_code(
            &mut wire_sink,
            &protocol_mismatch_error(client_protocol, client_version.as_deref()),
            Some(ERR_PROTOCOL_MISMATCH),
        )
        .await?;
        return Ok(());
    }
    match action {
        AttachAction::Monitor(action) => {
            return run_monitor(wire_sink, wire_source, registry, action, gate).await;
        }
        AttachAction::Control(request) => {
            return run_control(wire_sink, registry, gate, listeners, tcp_manager, request).await;
        }
        AttachAction::New(_) | AttachAction::Attach(_) | AttachAction::Picker(_) => {}
    }
    let _interactive_guard = conns.register_interactive();
    // The caller's project scopes creation / lazy resume. Attach
    // clients declare their working directory in the Select frame's optional
    // `project`; a client predating that field sends none and the server
    // falls back to its own process cwd — which is whatever the first client
    // that spawned the server happened to use, so it is only correct by
    // coincidence.
    let raw_project = project.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    let caller_project = host::paths::find_project_root(&raw_project);
    // A modern client *declared* its project; a legacy client did not and the
    // server is guessing from its own cwd. Auto-binding a lone cross-project
    // session is the "launched in project A, working in project B" trap, but
    // only the declared case can know it is cross-project — so the guard
    // applies there, and the legacy path keeps its historical behaviour.
    let declared_project = project.is_some();
    let handle = match registry
        .resolve_with_declaration(action, &caller_project, declared_project)
        .await
    {
        crate::runtime::registry::ResolveOutcome::Welcome(handle) => handle,
        crate::runtime::registry::ResolveOutcome::Pick { sessions } => {
            wire_sink
                .send(Wire::Pick { threads: sessions })
                .await
                .map_err(|e| format!("send pick: {e}"))?;
            return Ok(());
        }
        crate::runtime::registry::ResolveOutcome::Refused { message, code } => {
            // `[INV-ATTACH-01]`: the taxonomy must survive the wire. A code-less
            // send here is what forced clients to sniff prose (ADR-0123).
            send_error_with_code(&mut wire_sink, &message, code).await?;
            return Ok(());
        }
    };
    run_attach_multiplexed(
        wire_sink,
        wire_source,
        registry,
        caller_project,
        client_posture,
        handle,
    )
    .await
}

/// One thread subscribed on a multiplexed client connection (ADR-0251).
struct SubscribedThread {
    handle: crate::runtime::registry::AttachHandle,
    last_snapshot: Option<protocol::ThreadSnapshot>,
    forwarder: tokio::task::JoinHandle<()>,
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

/// Subscribe a resolved thread handle onto `wire_sink`, sending either
/// `Wire::Welcome` (initial handshake) or `Wire::Subscribed` (multiplexed
/// attach), accounting the human channel posture, and emitting the
/// authoritative snapshot + readiness transition (`[INV-ATTACH-04]`).
async fn hydrate_subscribed_thread(
    wire_sink: &mut BoxWireSink,
    handle: &crate::runtime::registry::AttachHandle,
    client_posture: protocol::human_request::HumanChannelPosture,
    is_initial: bool,
) -> Result<
    (
        String,
        broadcast::Receiver<AgentResponse>,
        Option<protocol::ThreadSnapshot>,
    ),
    String,
> {
    // Subscribe before reading any cached state: anything the build task or the
    // live driver publishes from here on is buffered for this connection.
    // `[INV-ATTACH-04]` (ADR-0133): the authoritative snapshot precedes `Ready`.
    let rx = handle.events().subscribe();
    let session_id = handle.session_id().await;
    let phase = handle.phase();

    let ack = if is_initial {
        Wire::Welcome {
            thread_id: session_id.clone(),
            phase: phase.clone(),
        }
    } else {
        Wire::Subscribed {
            thread_id: session_id.clone(),
            phase: phase.clone(),
        }
    };
    wire_sink
        .send(ack)
        .await
        .map_err(|e| format!("send thread ack: {e}"))?;

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
    let last_snapshot = handle.snapshot().await;
    if let Some(snapshot) = &last_snapshot {
        wire_sink
            .send(Wire::Response {
                response: AgentResponse::ThreadSnapshot(Box::new(snapshot.clone())),
            })
            .await
            .map_err(|e| format!("send thread snapshot: {e}"))?;
        // A client cannot answer a gate from a pipe; surface the trust posture
        // passively for non-interactive attaches (ADR-0127).
        if after_attach != protocol::human_request::HumanChannelPosture::Interactive
            && let Some(harness) = &snapshot.harness_state
            && harness.workspace_security.needs_review()
        {
            let response = workspace_trust_notice(&session_id, &harness.workspace_security);
            wire_sink
                .send(Wire::Response { response })
                .await
                .map_err(|e| format!("send trust notice: {e}"))?;
        }
        // `Ready` is a *transition* the client gates on; it is deliberately not
        // inferred from Welcome's phase cell, so the snapshot can never race
        // past it.
        wire_sink
            .send(Wire::Response {
                response: AgentResponse::ThreadPhase(protocol::ThreadPhaseUpdate {
                    thread_id: session_id.clone(),
                    phase: protocol::ThreadPhase::Ready,
                }),
            })
            .await
            .map_err(|e| format!("send ready phase: {e}"))?;
    } else if phase != protocol::ThreadPhase::Binding {
        // A second attacher joining an in-flight build learns where it is.
        wire_sink
            .send(Wire::Response {
                response: AgentResponse::ThreadPhase(protocol::ThreadPhaseUpdate {
                    thread_id: session_id.clone(),
                    phase,
                }),
            })
            .await
            .map_err(|e| format!("send thread phase: {e}"))?;
    }

    Ok((session_id, rx, last_snapshot))
}

/// Tracks every thread subscribed on one multiplexed connection, with flat
/// path-compressed aliases for in-place `/new`, `/resume`, and `/fork` re-keying.
struct SubscriptionTable {
    entries: std::collections::HashMap<String, SubscribedThread>,
    /// Flat map from historical `sub_key` -> current live key in `entries`.
    aliases: std::collections::HashMap<String, String>,
    primary_thread_id: String,
}

impl SubscriptionTable {
    fn new(initial_id: String, initial_sub: SubscribedThread) -> Self {
        let mut entries = std::collections::HashMap::new();
        entries.insert(initial_id.clone(), initial_sub);
        Self {
            entries,
            aliases: std::collections::HashMap::new(),
            primary_thread_id: initial_id,
        }
    }

    fn primary(&self) -> &str {
        &self.primary_thread_id
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
        if let Some(sub) = self.entries.remove(previous_id) {
            self.entries.insert(next_id.clone(), sub);
            for target in self.aliases.values_mut() {
                if target == previous_id {
                    *target = next_id.clone();
                }
            }
            self.aliases.remove(&next_id);
            self.aliases
                .insert(previous_id.to_string(), next_id.clone());
            if self.primary_thread_id == previous_id {
                self.primary_thread_id = next_id;
            }
        }
    }

    /// Remove a subscription and prune all aliases targeting it.
    fn remove(&mut self, thread_id: &str) -> Option<SubscribedThread> {
        let removed = self.entries.remove(thread_id);
        if removed.is_some() {
            self.aliases.retain(|_, target| target != thread_id);
            if self.primary_thread_id == thread_id
                && let Some(next_primary) = self.entries.keys().next().cloned()
            {
                self.primary_thread_id = next_primary;
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
            let resolved_key = subs.resolve_key(&sub_key);
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
                AgentResponse::ThreadSnapshot(snapshot) => {
                    if let Some(key) = &resolved_key
                        && let Some(sub) = subs.get_mut(key)
                    {
                        sub.last_snapshot = Some((**snapshot).clone());
                    }
                }
                _ => {}
            }
            match take_delta(response) {
                Ok((thread_id, context, kind, text)) => {
                    if let Some(displaced) =
                        queue_delta(pending_delta, thread_id, context, kind, text)
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
                    if let Err(e) = wire_sink.send(Wire::Response { response }).await {
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
                if let Some(snapshot) = sub.handle.snapshot().await {
                    sub.last_snapshot = Some(snapshot.clone());
                    let _ = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadSnapshot(Box::new(snapshot)),
                        })
                        .await;
                    let _ = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadPhase(protocol::ThreadPhaseUpdate {
                                thread_id: key,
                                phase: protocol::ThreadPhase::Ready,
                            }),
                        })
                        .await;
                } else if let Some(snapshot) = &sub.last_snapshot {
                    let _ = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadSnapshot(Box::new(snapshot.clone())),
                        })
                        .await;
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
            if subs.is_empty() {
                MuxControl::Stop(Ok(()))
            } else {
                MuxControl::Continue
            }
        }
        None => {
            let _ = flush_delta(wire_sink, pending_delta).await;
            MuxControl::Stop(Ok(()))
        }
    }
}

struct MuxEnv<'a> {
    caller_project: &'a std::path::Path,
    client_posture: protocol::human_request::HumanChannelPosture,
    registry: &'a crate::runtime::registry::ThreadRegistry,
    mux_tx: &'a tokio::sync::mpsc::Sender<MuxEvent>,
}

async fn handle_subscribe_frame(
    sub_action: protocol::AttachAction,
    sub_project: Option<std::path::PathBuf>,
    env: &MuxEnv<'_>,
    subs: &mut SubscriptionTable,
    wire_sink: &mut BoxWireSink,
) -> MuxControl {
    let sub_caller_project = match &sub_project {
        Some(p) => host::paths::find_project_root(p),
        None => env.caller_project.to_path_buf(),
    };
    match env
        .registry
        .resolve_with_declaration(sub_action, &sub_caller_project, sub_project.is_some())
        .await
    {
        crate::runtime::registry::ResolveOutcome::Welcome(new_handle) => {
            let tid = new_handle.session_id().await;
            if let Some(existing) = subs.get_mut(&tid) {
                // Idempotent re-subscribe: re-send acknowledgement + snapshot
                // without double-attaching the human channel.
                let phase = existing.handle.phase();
                if let Err(e) = wire_sink
                    .send(Wire::Subscribed {
                        thread_id: tid.clone(),
                        phase,
                    })
                    .await
                {
                    return MuxControl::Stop(Err(format!("wire send subscribed: {e}")));
                }
                if let Some(snapshot) = existing.handle.snapshot().await {
                    existing.last_snapshot = Some(snapshot.clone());
                    let _ = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadSnapshot(Box::new(snapshot)),
                        })
                        .await;
                    let _ = wire_sink
                        .send(Wire::Response {
                            response: AgentResponse::ThreadPhase(protocol::ThreadPhaseUpdate {
                                thread_id: tid,
                                phase: protocol::ThreadPhase::Ready,
                            }),
                        })
                        .await;
                }
            } else {
                match hydrate_subscribed_thread(wire_sink, &new_handle, env.client_posture, false)
                    .await
                {
                    Ok((new_tid, new_rx, new_snapshot)) => {
                        let forwarder =
                            spawn_thread_forwarder(new_tid.clone(), new_rx, env.mux_tx.clone());
                        subs.insert(
                            new_tid,
                            SubscribedThread {
                                handle: new_handle,
                                last_snapshot: new_snapshot,
                                forwarder,
                            },
                        );
                    }
                    Err(e) => return MuxControl::Stop(Err(e)),
                }
            }
        }
        crate::runtime::registry::ResolveOutcome::Pick { sessions } => {
            if let Err(e) = wire_sink.send(Wire::Pick { threads: sessions }).await {
                return MuxControl::Stop(Err(format!("send pick: {e}")));
            }
        }
        crate::runtime::registry::ResolveOutcome::Refused { message, code } => {
            if let Err(e) = send_error_with_code(wire_sink, &message, code).await {
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
    let forwarder = if let Some(sub) = subs.remove(&target_id) {
        let _ = sub.handle.human_channel().detach();
        drop(sub.handle);
        Some(sub.forwarder)
    } else {
        None
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

    if subs.is_empty() {
        MuxControl::Stop(Ok(()))
    } else {
        MuxControl::Continue
    }
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

async fn run_attach_multiplexed(
    mut wire_sink: BoxWireSink,
    mut wire_source: BoxWireStream,
    registry: Arc<crate::runtime::registry::ThreadRegistry>,
    caller_project: std::path::PathBuf,
    client_posture: protocol::human_request::HumanChannelPosture,
    initial_handle: crate::runtime::registry::AttachHandle,
) -> Result<(), String> {
    let (session_id, initial_rx, initial_snapshot) =
        hydrate_subscribed_thread(&mut wire_sink, &initial_handle, client_posture, true).await?;

    let (mux_tx, mut mux_rx) = tokio::sync::mpsc::channel::<MuxEvent>(256);
    let initial_forwarder = spawn_thread_forwarder(session_id.clone(), initial_rx, mux_tx.clone());
    let mut subs = SubscriptionTable::new(
        session_id,
        SubscribedThread {
            handle: initial_handle,
            last_snapshot: initial_snapshot,
            forwarder: initial_forwarder,
        },
    );

    // Consecutive streamed deltas ride one frame per coalescing window. Any
    // non-delta event, a channel lag, a channel close, or the client's end
    // frame flushes the pending run first, so ordering is never reordered.
    let mut pending_delta: Option<PendingDelta> = None;
    let mut flush_deadline: Option<tokio::time::Instant> = None;

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
                Some(Ok(WireFrame::Wire(Wire::Subscribe {
                    action: sub_action,
                    project: sub_project,
                }))) => {
                    if let Err(e) = flush_delta(&mut wire_sink, &mut pending_delta).await {
                        break Err(e);
                    }
                    flush_deadline = None;
                    let env = MuxEnv {
                        caller_project: &caller_project,
                        client_posture,
                        registry: &registry,
                        mux_tx: &mux_tx,
                    };
                    if let MuxControl::Stop(res) = handle_subscribe_frame(
                        sub_action,
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
                    let target_id = thread_id
                        .as_deref()
                        .unwrap_or_else(|| subs.primary())
                        .to_string();
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
                    let target_id = thread_id
                        .as_deref()
                        .unwrap_or_else(|| subs.primary())
                        .to_string();
                    forward_thread_request(&target_id, request, &subs, &mut wire_sink).await;
                }
                // A frame this build cannot model: report the skew on the
                // server's own log and keep the connection. The peer still
                // gets its turn to be understood [ADR-0136].
                Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                    tracing::warn!(
                        session = %subs.primary(),
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

/// Serve a host-observability client: send one snapshot, then —
/// while `watch` holds — stream diffs until the client hangs up. The channel
/// is strictly server → client; monitor clients never steer sessions, so any
/// inbound frame other than a close is ignored. The subscription is taken
/// *before* the snapshot is composed so an event published between the two
/// cannot be lost (it arrives as a redundant diff, which consumers tolerate —
/// updates are idempotent whole-row replacements).
async fn run_monitor(
    mut wire_sink: BoxWireSink,
    mut wire_source: BoxWireStream,
    registry: Arc<crate::runtime::registry::ThreadRegistry>,
    action: MonitorAction,
    gate: Arc<ShutdownGate>,
) -> Result<(), String> {
    let mut rx = registry.subscribe_monitor();
    let snapshot = registry.monitor_snapshot(action).await;
    send_monitor(&mut wire_sink, MonitorEvent::Snapshot(snapshot)).await?;
    if !action.watch {
        let _ = wire_sink.close().await;
        return Ok(());
    }
    let drain = gate.triggered();
    tokio::pin!(drain);
    loop {
        tokio::select! {
            // Server draining: say so on the stream, then close.
            // Watch clients get an explicit terminal signal instead of an
            // abrupt disconnect they might misread as a network fault.
            _ = &mut drain => {
                send_monitor(&mut wire_sink, MonitorEvent::ServerDraining).await?;
                let _ = wire_sink.close().await;
                return Ok(());
            }
            // Keep polling the inbound stream so WebSocket Ping frames are
            // answered with Pong immediately (`tungstenite` flushes its
            // auto-Pong on `read()`) and client disconnects end the task
            // promptly instead of leaking until the next broadcast (ADR-0224).
            inbound = wire_source.next() => match inbound {
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "nuo server: monitor client read ended");
                    return Ok(());
                }
                None => return Ok(()),
            },
            received = rx.recv() => {
                let event = match received {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // The client fell behind; rows are whole-row replacements, so
                        // the safest resync is a fresh snapshot.
                        tracing::warn!(
                            skipped = n,
                            "nuo server: monitor client lagged, resyncing"
                        );
                        let snapshot = registry.monitor_snapshot(action).await;
                        send_monitor(&mut wire_sink, MonitorEvent::Snapshot(snapshot)).await?;
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                };
                let filtered = match &event {
                    MonitorEvent::ThreadAdded(row) | MonitorEvent::ThreadUpdated(row) => {
                        if action.include_idle || row.status.is_active() {
                            Some(event.clone())
                        } else {
                            None
                        }
                    }
                    MonitorEvent::Snapshot(_)
                    | MonitorEvent::TaskUpdated(_)
                    | MonitorEvent::TaskRemoved { .. }
                    | MonitorEvent::PersistenceHealth(_)
                    | MonitorEvent::ThreadRemoved { .. }
                    | MonitorEvent::ServerDraining => Some(event.clone()),
                };
                if let Some(event) = filtered {
                    send_monitor(&mut wire_sink, event).await?;
                }
            }
        }
    }
}

/// Send one monitor stream frame. Frames ride the `Wire::Monitor` envelope
/// so the client sees the same wire shape for the initial
/// snapshot and every diff.
async fn send_monitor(wire_sink: &mut BoxWireSink, event: MonitorEvent) -> Result<(), String> {
    wire_sink
        .send(Wire::Monitor { event })
        .await
        .map_err(|e| format!("send monitor event: {e}"))
}

async fn send_error(wire_sink: &mut BoxWireSink, message: &str) -> Result<(), String> {
    send_error_with_code(wire_sink, message, None).await
}

/// `send_error` with a stable machine-readable `code` so clients
/// can branch on the reason instead of string-sniffing the message.
async fn send_error_with_code(
    wire_sink: &mut BoxWireSink,
    message: &str,
    code: Option<&str>,
) -> Result<(), String> {
    wire_sink
        .send(Wire::Error {
            message: message.to_string(),
            code: code.map(str::to_string),
        })
        .await
        .map_err(|e| format!("send error: {e}"))
}
