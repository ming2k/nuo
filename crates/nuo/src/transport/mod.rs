//! Client transport, server connection, lifecycle discovery, and control channel for Nuo.

// Child modules share this module's vocabulary through `use super::*`.
#![allow(unused_imports)]

#[allow(unused_imports)]
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::{Sink, SinkExt, Stream, StreamExt};
use protocol::{
    AgentRequest, AgentResponse, MonitorAction, MonitorEvent, MonitoredTask, MonitoredThread,
    ThreadOverview, WireFrame,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

pub use protocol::discovery::{Discovery, ServerInfo};
pub use protocol::version::{
    ERR_PROTOCOL_MISMATCH, ERR_SERVER_DRAINING, ERR_SERVER_FAILED, ERR_SERVER_NOT_READY,
    ERR_VERSION_MISMATCH, protocol_accepts, protocol_mismatch, version_mismatch,
};
pub use protocol::{
    ControlRequest, NativeWireCodec, PROTOCOL_VERSION, ServerEndpointsReport, ServerInitStage,
    ServerPhase, StorageInitDetail, ThreadInitOptions, ThreadTarget, Wire,
};
#[cfg(unix)]
pub use server::api::serve_discovery::default_uds_path;
pub use server::api::serve_discovery::{
    self as discovery, default_local_endpoint, global_discovery_path, global_lock_path,
    global_spawn_lock_path, instance_dir,
};

/// Initialize client logging guard.
pub fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    init_tracing_for("client.log", "client")
}

/// Initialize server logging guard.
pub fn init_server_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    init_tracing_for("nuo.log", "server")
}

fn init_tracing_for(
    file_name: &str,
    role: &str,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let level = std::env::var("NUO_LOG").unwrap_or_else(|_| String::from("info"));
    if level.eq_ignore_ascii_case("off") {
        return None;
    }
    let dir = host::paths::get().log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let file_appender = tracing_appender::rolling::daily(&dir, file_name);
    let (writer, guard) = tracing_appender::non_blocking(file_appender);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .with_thread_ids(true)
        .with_target(true)
        .try_init();
    tracing::info!(pid = std::process::id(), role, "tracing initialized");
    Some(guard)
}

pub type BoxWireSink = Pin<Box<dyn Sink<Wire, Error = io::Error> + Send>>;
pub type BoxWireStream = Pin<Box<dyn Stream<Item = Result<WireFrame, io::Error>> + Send>>;

pub fn native_framed_split<T>(stream: T) -> (BoxWireSink, BoxWireStream)
where
    T: AsyncRead + AsyncWrite + Send + 'static,
{
    let framed = Framed::new(stream, NativeWireCodec);
    let (sink, stream) = framed.split();
    (Box::pin(sink), Box::pin(stream))
}

/// Connect timeout for client WebSocket links to local or remote Nuo servers
/// (ADR-0224).
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// TCP keepalive idle interval for client WebSocket sockets (ADR-0224).
#[cfg(unix)]
const WS_TCP_KEEPALIVE_IDLE: Duration = Duration::from_secs(30);

fn ws_client() -> &'static netune::Client {
    static CLIENT: OnceLock<netune::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let socket = netune::SocketConfig::default();
        #[cfg(unix)]
        let socket = socket.with_keepalive(WS_TCP_KEEPALIVE_IDLE);
        netune::Client::builder()
            .connect_timeout(Some(WS_CONNECT_TIMEOUT))
            .socket_config(socket)
            .build()
    })
}

/// Dial a Nuo server over WebSocket using `netune`'s owned client codec and
/// `WsSession` heartbeat liveness (ADR-0224 `[INV-RESILIENCE-03]`).
///
/// Long-lived channels (`attach`, `remote`, `monitor`) pass
/// [`netune::WsSessionConfig::default`] (20 s Ping / 10 s Pong deadline);
/// one-shot control verbs disable client pings and rely on their outer reply
/// timeout.
pub(super) async fn connect_ws_wire(
    url: &str,
    token: Option<&str>,
    heartbeat: netune::WsSessionConfig,
) -> Result<(BoxWireSink, BoxWireStream), String> {
    let (target, path) =
        netune::Target::from_url(url).map_err(|e| format!("bad ws url {url}: {e}"))?;
    let mut handshake = netune::WsHandshake::default();
    if let Some(token) = token {
        handshake = handshake.with_extra_header("authorization", format!("Bearer {token}"));
    }
    let ws = ws_client()
        .connect_ws_with(&target, &path, handshake)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    Ok(ws_session_split(netune::WsSession::new(ws, heartbeat)))
}

enum WsOut {
    Send(Box<Wire>),
    Close(tokio::sync::oneshot::Sender<()>),
}

struct WsChannelSink {
    tx: Option<mpsc::UnboundedSender<WsOut>>,
    closing: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl Sink<Wire> for WsChannelSink {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match &self.tx {
            Some(tx) if !tx.is_closed() => Poll::Ready(Ok(())),
            _ => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "WebSocket session driver closed",
            ))),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Wire) -> Result<(), Self::Error> {
        let Some(tx) = &self.tx else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "WebSocket session sink already closed",
            ));
        };
        tx.send(WsOut::Send(Box::new(item))).map_err(|_| {
            io::Error::new(io::ErrorKind::BrokenPipe, "WebSocket session driver closed")
        })
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(tx) = self.tx.take() {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            if tx.send(WsOut::Close(done_tx)).is_ok() {
                self.closing = Some(done_rx);
            }
        }
        if let Some(rx) = self.closing.as_mut() {
            match Pin::new(rx).poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(_) => self.closing = None,
            }
        }
        Poll::Ready(Ok(()))
    }
}

/// Adapt a full-duplex [`netune::WsSession`] onto the crate's `(BoxWireSink, BoxWireStream)`
/// pair using one cancel-safe driver task (`[INV-WS-09]`, ADR-0224).
pub(super) fn ws_session_split<S>(mut session: netune::WsSession<S>) -> (BoxWireSink, BoxWireStream)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<WsOut>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<Result<WireFrame, io::Error>>();

    tokio::spawn(async move {
        let mut out_closed = false;
        loop {
            tokio::select! {
                biased;
                _ = in_tx.closed() => {
                    // Reader half was dropped by the consumer; close cleanly so
                    // half-dropped read-only streams (like `monitor`) never leak
                    // the background driver task.
                    let _ = session.close(netune::CloseCode::NORMAL, "").await;
                    break;
                }
                outbound = out_rx.recv(), if !out_closed => {
                    match outbound {
                        Some(WsOut::Send(wire)) => match serde_json::to_string(&wire) {
                            Ok(json) => {
                                if let Err(err) = session.send_text(json).await {
                                    let _ = in_tx.send(Err(io::Error::new(
                                        io::ErrorKind::ConnectionReset,
                                        err.to_string(),
                                    )));
                                    break;
                                }
                            }
                            Err(err) => {
                                let _ = in_tx.send(Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    format!("Failed to serialize Wire to JSON: {err}"),
                                )));
                                break;
                            }
                        },
                        Some(WsOut::Close(done)) => {
                            let _ = session.close(netune::CloseCode::NORMAL, "").await;
                            let _ = done.send(());
                            break;
                        }
                        // Dropping the sink without an explicit `close()` means
                        // the caller is read-only (e.g. `finish_monitor` drops
                        // `wire_sink` after sending `Select`). Keep the session
                        // and its heartbeat alive while `in_tx` is still open.
                        None => {
                            out_closed = true;
                        }
                    }
                }
                inbound = session.next() => match inbound {
                    // A WebSocket text/binary frame is self-delimiting, so an
                    // undecodable payload is reported per frame and the socket
                    // stays usable [ADR-0136]. `Err` remains reserved for the
                    // socket or heartbeat liveness deadline itself.
                    Ok(Some(netune::WsMessage::Text(text))) => {
                        if in_tx
                            .send(Ok(protocol::decode_wire_payload(text.as_bytes())))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(Some(netune::WsMessage::Binary(bin))) => {
                        if in_tx
                            .send(Ok(protocol::decode_wire_payload(&bin)))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(Some(netune::WsMessage::Close(_))) | Ok(None) => break,
                    Ok(Some(netune::WsMessage::Ping(_) | netune::WsMessage::Pong(_))) => {}
                    Err(err) => {
                        let _ = in_tx.send(Err(io::Error::new(
                            io::ErrorKind::ConnectionReset,
                            err.to_string(),
                        )));
                        break;
                    }
                },
            }
        }
    });

    let sink: BoxWireSink = Box::pin(WsChannelSink {
        tx: Some(out_tx),
        closing: None,
    });
    let stream: BoxWireStream = Box::pin(futures::stream::unfold(in_rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }));
    (sink, stream)
}

mod attach;
mod control;
mod lifecycle;
mod monitor;
mod ready;
mod remote;
mod rows;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use self::attach::*;
#[allow(unused_imports)]
pub use self::control::*;
#[allow(unused_imports)]
pub use self::lifecycle::*;
#[allow(unused_imports)]
pub use self::monitor::*;
#[allow(unused_imports)]
pub use self::ready::format_init_phase;
#[allow(unused_imports)]
pub use self::remote::*;
#[allow(unused_imports)]
pub use self::rows::*;
