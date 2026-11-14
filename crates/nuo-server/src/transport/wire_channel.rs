//! Transport-agnostic wire codec and stream abstractions.
//!
//! Provides length-prefixed framing over native asynchronous byte streams
//! (Unix Domain Sockets, Named Pipes) as well as conversion helpers to adapt
//! WebSocket streams into the same [`BoxWireStream`] and [`BoxWireSink`] interfaces.
use futures::{Sink, SinkExt, Stream, StreamExt};
use nuo_protocol::{NativeWireCodec, Wire, WireFrame};
use std::io;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

// The length-prefixed codec lives in `nuo-protocol` — one implementation, one
// frame limit, one decode contract for both peers. This module owns transport
// adaptation only; a second codec here was a copy that had to be kept in step
// by hand.

/// Unified sink type for sending [`Wire`] messages.
pub type BoxWireSink = Pin<Box<dyn Sink<Wire, Error = String> + Send>>;

/// Unified stream type for receiving [`nuo_protocol::WireFrame`]s.
pub type BoxWireStream = Pin<Box<dyn Stream<Item = Result<WireFrame, String>> + Send>>;

/// Convert a native async byte stream (e.g. `UnixStream` or Windows named pipe)
/// into a pair of unified `(BoxWireSink, BoxWireStream)`.
pub fn native_framed_split<S>(stream: S) -> (BoxWireSink, BoxWireStream)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let framed = Framed::new(stream, NativeWireCodec);
    let (sink, source) = framed.split();

    let sink = Box::pin(sink.sink_map_err(|e| format!("IPC write error: {e}")));
    let source = Box::pin(source.map(|res| res.map_err(|e| format!("IPC read error: {e}"))));

    (sink, source)
}

/// Convert a WebSocket stream into a pair of unified `(BoxWireSink, BoxWireStream)`.
pub fn websocket_split<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
) -> (BoxWireSink, BoxWireStream)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (sink, source) = ws_stream.split();

    let sink = Box::pin(
        sink.with(|wire: Wire| async move {
            match serde_json::to_string(&wire) {
                Ok(text) => Ok(tokio_tungstenite::tungstenite::Message::Text(text.into())),
                Err(e) => Err(tokio_tungstenite::tungstenite::Error::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    e.to_string(),
                ))),
            }
        })
        .sink_map_err(|e| format!("WebSocket write error: {e}")),
    );

    let source = Box::pin(source.filter_map(|res| async move {
        match res {
            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                Some(Ok(nuo_protocol::decode_wire_payload(text.as_bytes())))
            }
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => None,
            Ok(tokio_tungstenite::tungstenite::Message::Ping(_))
            | Ok(tokio_tungstenite::tungstenite::Message::Pong(_)) => None,
            Ok(other) => Some(Err(format!("Unexpected WebSocket message: {other:?}"))),
            Err(e) => Some(Err(format!("WebSocket read error: {e}"))),
        }
    }));

    (sink, source)
}
