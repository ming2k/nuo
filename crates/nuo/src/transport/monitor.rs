//! Monitor handshake and event stream.

use super::lifecycle::MONITOR_FIRST_FRAME_TIMEOUT;
use super::*;
use nuo_protocol::MonitorAction;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

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

pub(super) async fn finish_monitor(
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
        protocol: PROTOCOL_VERSION,
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
