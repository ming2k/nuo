//! Client transport, server connection, lifecycle discovery, and control channel for Nuo.

// Child modules share this module's vocabulary through `use super::*`.
#![allow(unused_imports)]

#[allow(unused_imports)]
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{Sink, SinkExt, Stream, StreamExt, TryStreamExt};
use nuo_protocol::{
    AgentRequest, AgentResponse, MonitorAction, MonitorEvent, MonitoredTask, MonitoredThread,
    ThreadOverview, WireFrame,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_util::codec::Framed;

pub use nuo_protocol::discovery::{Discovery, ServerInfo};
pub use nuo_protocol::version::{
    ERR_PROTOCOL_MISMATCH, ERR_VERSION_MISMATCH, VersionRelation, compare_versions,
    incompatibility_error, protocol_accepts, protocol_mismatch, server_image_is_current,
    version_mismatch, versions_compatible,
};
pub use nuo_protocol::{
    AttachAction, ControlRequest, MIN_PROTOCOL_VERSION, NativeWireCodec, PROTOCOL_VERSION,
    ServerEndpointsReport, ThreadInitOptions, Wire,
};
#[cfg(unix)]
pub use nuo_server::serve_discovery::default_uds_path;
pub use nuo_server::serve_discovery::{
    self as discovery, default_local_endpoint, global_discovery_path, global_lock_path,
    global_spawn_lock_path, instance_dir,
};
pub type SessionInitOptions = ThreadInitOptions;

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
    let dir = nuo_host::paths::get().log_dir();
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

pub fn websocket_split<T>(ws_stream: T) -> (BoxWireSink, BoxWireStream)
where
    T: Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Send
        + 'static,
{
    let (ws_sink, ws_source) = ws_stream.split();

    let sink = ws_sink
        .sink_map_err(|e| io::Error::new(io::ErrorKind::ConnectionReset, e))
        .with(|wire: Wire| async move {
            let json = serde_json::to_string(&wire).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Failed to serialize Wire to JSON: {e}"),
                )
            })?;
            Ok(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        });

    let stream = ws_source
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionReset, e))
        .filter_map(|msg_res| async move {
            match msg_res {
                // A WebSocket text frame is self-delimiting, so an undecodable
                // payload is reported per frame and the socket stays usable
                // [ADR-0136]. `Err` remains reserved for the socket itself.
                Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                    Some(Ok(nuo_protocol::decode_wire_payload(text.as_bytes())))
                }
                Ok(tokio_tungstenite::tungstenite::Message::Binary(bin)) => {
                    Some(Ok(nuo_protocol::decode_wire_payload(&bin)))
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => None,
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            }
        });

    (Box::pin(sink), Box::pin(stream))
}

mod attach;
mod control;
mod lifecycle;
mod monitor;
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
pub use self::remote::*;
#[allow(unused_imports)]
pub use self::rows::*;
