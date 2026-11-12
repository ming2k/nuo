//! One-shot control verbs and their typed JSON payloads.

use super::current_posture;
use super::lifecycle::CONTROL_REPLY_TIMEOUT;
use super::*;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

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
    let action = AttachAction::Control(request);

    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_control((sink, source), action, timeout).await;
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
    finish_control((sink, source), action, timeout).await
}

pub(super) async fn finish_control(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
    timeout: Duration,
) -> Result<ControlOutcome, String> {
    let (mut wire_sink, mut wire_source) = parts;
    let select = Wire::Select {
        action,
        project: None,
        posture: current_posture(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol: PROTOCOL_VERSION,
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send control select: {e}"))?;

    tokio::time::timeout(timeout, async {
        loop {
            match wire_source.next().await {
                Some(Ok(WireFrame::Wire(wire))) => match wire {
                    Wire::ControlReply {
                        ok,
                        thread_id,
                        error,
                        payload,
                    } => {
                        return if ok {
                            Ok(ControlOutcome { thread_id, payload })
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
