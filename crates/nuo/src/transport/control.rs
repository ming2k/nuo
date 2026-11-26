//! Control-plane verbs over the unified `Hello` → `ServerHello` wire contract
//! (ADR-0260).

use super::current_posture;
use super::lifecycle::CONTROL_REPLY_TIMEOUT;
use super::ready;
use super::*;
use std::time::Duration;

/// Typed result of one control verb. `payload` is the verb's JSON-encoded
/// result (`ListThreads`, usage/endpoint reports, Archivist answers); verbs
/// without a result leave it `None`.
#[derive(Debug, Clone, Default)]
pub struct ControlOutcome {
    pub thread_id: Option<String>,
    pub payload: Option<String>,
}

pub async fn control(info: &ServerInfo, request: ControlRequest) -> Result<(), String> {
    control_outcome(info, request).await.map(|_| ())
}

/// Control verb returning the reply payload, if any. This is the data-bearing
/// form; `control` is the fire-and-check form.
pub async fn control_with_reply(
    info: &ServerInfo,
    request: ControlRequest,
) -> Result<String, String> {
    control_with_reply_with_timeout(info, request, CONTROL_REPLY_TIMEOUT).await
}

pub(super) async fn control_with_reply_with_timeout(
    info: &ServerInfo,
    request: ControlRequest,
    timeout: Duration,
) -> Result<String, String> {
    control_outcome_with_timeout(info, request, timeout)
        .await?
        .payload
        .ok_or_else(|| "control verb returned no payload".to_string())
}

pub async fn control_outcome(
    info: &ServerInfo,
    request: ControlRequest,
) -> Result<ControlOutcome, String> {
    control_outcome_with_timeout(info, request, CONTROL_REPLY_TIMEOUT).await
}

async fn control_outcome_with_timeout(
    info: &ServerInfo,
    request: ControlRequest,
    timeout: Duration,
) -> Result<ControlOutcome, String> {
    let connect_deadline = tokio::time::Instant::now() + timeout;

    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(Ok(stream)) =
            tokio::time::timeout_at(connect_deadline, host::ipc::connect(&endpoint)).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_control_with_budgets((sink, source), request, ready::INIT_HARD_CAP, timeout)
            .await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let (sink, source) = tokio::time::timeout_at(
        connect_deadline,
        connect_ws_wire(
            &url,
            info.token.as_deref(),
            netune::WsSessionConfig::default().with_ping_interval(Duration::ZERO),
        ),
    )
    .await
    .map_err(|_| "timed out connecting control channel to server".to_string())??;
    finish_control_with_budgets((sink, source), request, ready::INIT_HARD_CAP, timeout).await
}

#[cfg(test)]
pub(super) async fn finish_control(
    parts: (BoxWireSink, BoxWireStream),
    request: ControlRequest,
    timeout: Duration,
) -> Result<ControlOutcome, String> {
    finish_control_with_budgets(parts, request, timeout, timeout).await
}

async fn finish_control_with_budgets(
    parts: (BoxWireSink, BoxWireStream),
    request: ControlRequest,
    init_cap: Duration,
    reply_timeout: Duration,
) -> Result<ControlOutcome, String> {
    let (mut wire_sink, mut wire_source) = parts;
    let hello_deadline = tokio::time::Instant::now() + reply_timeout;
    let hello = Wire::Hello {
        protocol: PROTOCOL_VERSION,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        posture: current_posture(),
        monitor: None,
    };
    tokio::time::timeout_at(hello_deadline, wire_sink.send(hello))
        .await
        .map_err(|_| "timed out sending control hello to server".to_string())?
        .map_err(|e| format!("wire send control hello: {e}"))?;

    // Wait for `ServerHello`; if the server is still initializing and the verb
    // requires a ready server, wait via the shared progress-aware watchdog
    // before applying the verb's reply timeout (ADR-0266).
    let needs_ready = !matches!(
        request,
        ControlRequest::Shutdown | ControlRequest::GetServerEndpoints
    );
    let mut server_phase = tokio::time::timeout_at(hello_deadline, async {
        match wire_source.next().await {
            Some(Ok(WireFrame::Wire(Wire::ServerHello { phase, .. }))) => Ok(phase),
            Some(Ok(WireFrame::Wire(Wire::Error { message, .. }))) => Err(message),
            Some(Ok(WireFrame::Undecodable { envelope, detail })) => Err(format!(
                "server sent a frame this client cannot decode{}: {detail}",
                envelope
                    .map(|tag| format!(" (envelope `{tag}`)"))
                    .unwrap_or_default()
            )),
            Some(Ok(other)) => Err(format!("unexpected frame before ServerHello: {other:?}")),
            Some(Err(error)) => Err(format!("wire recv control hello: {error}")),
            None => Err("server closed the connection before ServerHello".to_string()),
        }
    })
    .await
    .map_err(|_| "timed out waiting for server hello".to_string())??;

    if needs_ready {
        if server_phase.is_initializing() {
            server_phase =
                ready::await_server_ready(&mut wire_source, server_phase, None, init_cap)
                    .await
                    .map_err(|e| e.message)?;
        }
        match &server_phase {
            ServerPhase::Failed { message, .. } => {
                return Err(format!("server startup failed: {message}"));
            }
            ServerPhase::Draining { reason } => {
                return Err(match reason {
                    Some(r) => format!("server is shutting down ({r})"),
                    None => "server is shutting down".to_string(),
                });
            }
            _ => {}
        }
    }

    let deadline = tokio::time::Instant::now() + reply_timeout;

    let request_id = 1;
    tokio::time::timeout_at(
        deadline,
        wire_sink.send(Wire::Control {
            request_id,
            request,
        }),
    )
    .await
    .map_err(|_| "timed out sending control verb to server".to_string())?
    .map_err(|e| format!("wire send control verb: {e}"))?;

    tokio::time::timeout_at(deadline, async {
        loop {
            match wire_source.next().await {
                Some(Ok(WireFrame::Wire(Wire::ControlReply {
                    request_id: reply_id,
                    ok,
                    thread_id,
                    error,
                    payload,
                    ..
                }))) if reply_id == request_id => {
                    return if ok {
                        Ok(ControlOutcome { thread_id, payload })
                    } else {
                        Err(error.unwrap_or_else(|| "control verb rejected".to_string()))
                    };
                }
                Some(Ok(WireFrame::Wire(Wire::Error { message, .. }))) => return Err(message),
                Some(Ok(WireFrame::Wire(_))) => {}
                Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                    return Err(format!(
                        "server sent a frame this client cannot decode{}: {detail}",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    ));
                }
                Some(Err(e)) => return Err(format!("wire recv control reply: {e}")),
                None => return Err("server closed control connection without replying".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for control reply from server".to_string())?
}
