//! Transport-agnostic wire codec and stream abstractions (ADR-0158).
//!
//! Provides length-prefixed framing over native asynchronous byte streams
//! (Unix Domain Sockets, Named Pipes) as well as conversion helpers to adapt
//! WebSocket streams into the same [`BoxWireStream`] and [`BoxWireSink`] interfaces.

use bytes::{Buf, BufMut, BytesMut};
use futures::{Sink, SinkExt, Stream, StreamExt};
use nuo_client::wire::Wire;
use std::io;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Decoder, Encoder, Framed};

/// Maximum wire frame payload length: 16 MB.
pub const MAX_WIRE_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Length-delimited JSON codec for native IPC streams.
///
/// Frames consist of a 4-byte big-endian length prefix followed by
/// the UTF-8 JSON-encoded payload.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeWireCodec;

impl Decoder for NativeWireCodec {
    type Item = Wire;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }

        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_be_bytes(length_bytes) as usize;

        if length > MAX_WIRE_FRAME_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Wire frame length {length} exceeds maximum limit {MAX_WIRE_FRAME_SIZE}"),
            ));
        }

        if src.len() < 4 + length {
            src.reserve(4 + length - src.len());
            return Ok(None);
        }

        src.advance(4);
        let payload = src.split_to(length);

        serde_json::from_slice(&payload).map(Some).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize Wire payload: {e}"),
            )
        })
    }
}

impl Encoder<Wire> for NativeWireCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Wire, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let serialized = serde_json::to_vec(&item).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to serialize Wire payload: {e}"),
            )
        })?;

        if serialized.len() > MAX_WIRE_FRAME_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Serialized Wire frame size {} exceeds limit {}",
                    serialized.len(),
                    MAX_WIRE_FRAME_SIZE
                ),
            ));
        }

        dst.reserve(4 + serialized.len());
        dst.put_u32(serialized.len() as u32);
        dst.put_slice(&serialized);
        Ok(())
    }
}

/// Unified sink type for sending [`Wire`] messages.
pub type BoxWireSink = Pin<Box<dyn Sink<Wire, Error = String> + Send>>;

/// Unified stream type for receiving [`Wire`] messages.
pub type BoxWireStream = Pin<Box<dyn Stream<Item = Result<Wire, String>> + Send>>;

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
                Some(serde_json::from_str(&text).map_err(|e| format!("JSON decode error: {e}")))
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
