//! Monitor handshake and event stream over the unified `Hello` → `ServerHello`
//! protocol (ADR-0260).

use super::current_posture;
use super::lifecycle::MONITOR_FIRST_FRAME_TIMEOUT;
use super::*;
use nuo_protocol::MonitorAction;

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
    let (sink, source) = connect_ws_wire(
        &url,
        info.token.as_deref(),
        netune::WsSessionConfig::default(),
    )
    .await?;
    finish_monitor((sink, source), action, &url).await
}

pub(super) async fn finish_monitor(
    parts: (BoxWireSink, BoxWireStream),
    action: MonitorAction,
    target: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    let (mut wire_sink, mut wire_source) = parts;

    let hello = Wire::Hello {
        protocol: PROTOCOL_VERSION,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        posture: current_posture(),
        monitor: Some(action),
    };
    wire_sink
        .send(hello)
        .await
        .map_err(|e| format!("wire send hello: {e}"))?;

    let first = tokio::time::timeout(MONITOR_FIRST_FRAME_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(WireFrame::Wire(wire))) => match wire {
                    Wire::ServerHello {
                        monitor: Some(snapshot),
                        ..
                    } => return Ok(MonitorEvent::Snapshot(snapshot)),
                    Wire::ServerHello { monitor: None, .. } => {
                        return Err("server returned ServerHello without monitor snapshot".into());
                    }
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
    if !action.watch {
        let _ = wire_sink.close().await;
        return Ok(rx);
    }
    tokio::spawn(async move {
        let _keep_sink_alive = wire_sink;
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
