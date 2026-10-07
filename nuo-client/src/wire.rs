//! Transport-agnostic wire codec, protocol types, and stream abstractions for client communication.

use std::io;
use std::pin::Pin;

use bytes::{Buf, BufMut, BytesMut};
use futures::{Sink, SinkExt, Stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Decoder, Encoder, Framed};

pub use nuo_wire::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};

/// Stable machine-readable error codes.
pub const ERR_PROTOCOL_MISMATCH: &str = "protocol_mismatch";
pub const ERR_VERSION_MISMATCH: &str = "version_mismatch";

pub const fn protocol_accepts(client: u32) -> bool {
    matches!(client, MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION)
}

/// Initial options and postures when creating a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInitOptions {
    /// `--unattended` / unattended execution posture.
    #[serde(default)]
    pub unattended: bool,
    /// Whether workspace filesystem confinement is enforced (default true).
    #[serde(default = "default_confined")]
    pub confined: bool,
    /// Role id to staff this session with. `None` = the default
    /// workspace-scoped coding principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Resume the most recent matching session instead of creating a new one.
    /// With `role`, matches by role (and workspace when the role binds one).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume: bool,
}

const fn default_confined() -> bool {
    true
}

impl Default for SessionInitOptions {
    fn default() -> Self {
        Self {
            unattended: false,
            confined: true,
            role: None,
            resume: false,
        }
    }
}

impl SessionInitOptions {
    pub fn new(unattended: bool, confined: bool) -> Self {
        Self {
            unattended,
            confined,
            role: None,
            resume: false,
        }
    }

    pub fn with_role(mut self, role: Option<String>) -> Self {
        self.role = role;
        self
    }

    pub fn with_resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    pub fn is_default(&self) -> bool {
        !self.unattended && self.confined && self.role.is_none() && !self.resume
    }
}

/// What role the connection wants to assume.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachAction {
    New(Option<SessionInitOptions>),
    Attach(Option<String>),
    Picker(Option<SessionInitOptions>),
    Control(ControlRequest),
    Monitor(nuo_wire::MonitorAction),
}

impl Serialize for AttachAction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::New(None) => serializer.serialize_str("new"),
            Self::New(Some(opts)) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("new", opts)?;
                map.end()
            }
            Self::Attach(id) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("attach", id)?;
                map.end()
            }
            Self::Picker(None) => serializer.serialize_str("picker"),
            Self::Picker(Some(opts)) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("picker", opts)?;
                map.end()
            }
            Self::Control(req) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("control", req)?;
                map.end()
            }
            Self::Monitor(act) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("monitor", act)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for AttachAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum RawAttachAction {
            New(Option<SessionInitOptions>),
            Attach(Option<String>),
            Picker(Option<SessionInitOptions>),
            Control(ControlRequest),
            Monitor(nuo_wire::MonitorAction),
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireHelper {
            Str(String),
            Structured(RawAttachAction),
        }

        match WireHelper::deserialize(deserializer)? {
            WireHelper::Str(s) => match s.as_str() {
                "new" => Ok(AttachAction::New(None)),
                "picker" => Ok(AttachAction::Picker(None)),
                other => Err(serde::de::Error::unknown_variant(other, &["new", "picker"])),
            },
            WireHelper::Structured(raw) => Ok(match raw {
                RawAttachAction::New(opts) => AttachAction::New(opts),
                RawAttachAction::Attach(id) => AttachAction::Attach(id),
                RawAttachAction::Picker(opts) => AttachAction::Picker(opts),
                RawAttachAction::Control(c) => AttachAction::Control(c),
                RawAttachAction::Monitor(m) => AttachAction::Monitor(m),
            }),
        }
    }
}

/// Single-shot session-management verbs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum ControlRequest {
    Shutdown,
    /// ADR-0034 Level 1: re-read configuration and re-sync MCP + skills
    /// without dropping connections.
    Reload,
    CreateSession {
        project: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        init_options: Option<SessionInitOptions>,
    },
    SendPrompt {
        session_id: String,
        text: String,
    },
    Interrupt {
        session_id: String,
    },
    ResolvePermission {
        session_id: String,
        request_id: String,
        decision: nuo_wire::PermissionDecision,
    },
    KillSession {
        session_id: String,
    },
    SuspendSession {
        session_id: String,
    },
    AskArchivist {
        text: String,
    },
}

/// The unified wire envelope on every connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum Wire {
    /// Handshake frame declaring role, scope, and capabilities.
    Select {
        action: AttachAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<std::path::PathBuf>,
        #[serde(default)]
        posture: nuo_wire::human_request::HumanChannelPosture,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<u32>,
    },
    /// Server response welcoming an attached connection.
    Welcome {
        session_id: String,
        round_counter: u64,
        messages: Vec<nuo_wire::Message>,
        #[serde(default)]
        provider: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        round_interrupts: Vec<nuo_wire::RoundInterrupt>,
        #[serde(default)]
        retry_resolutions: Vec<nuo_wire::RetryResolution>,
        #[serde(default)]
        command_catalog: nuo_wire::CommandCatalog,
    },
    /// Server response to ambiguous attach / picker.
    Pick {
        sessions: Vec<nuo_wire::SessionOverview>,
    },
    /// Reply to single-shot control verb.
    ControlReply {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Full-duplex client agent request envelope.
    Request {
        #[serde(flatten)]
        request: nuo_wire::AgentRequest,
    },
    /// Full-duplex server agent response envelope.
    Response {
        #[serde(flatten)]
        response: nuo_wire::AgentResponse,
    },
    /// Server observability event envelope.
    Monitor {
        #[serde(flatten)]
        event: nuo_wire::MonitorEvent,
    },
    /// Connection-level error envelope.
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

/// Maximum wire frame payload length: 16 MB.
pub const MAX_WIRE_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Length-delimited JSON codec for native IPC streams.
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
                    "Serialized Wire payload length {} exceeds maximum limit {}",
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

pub type BoxWireSink = Pin<Box<dyn Sink<Wire, Error = io::Error> + Send>>;
pub type BoxWireStream = Pin<Box<dyn Stream<Item = Result<Wire, io::Error>> + Send>>;

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
    T: Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>>
        + Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
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
                Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                    match serde_json::from_str::<Wire>(&text) {
                        Ok(wire) => Some(Ok(wire)),
                        Err(e) => Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Failed to deserialize Wire from text: {e}"),
                        ))),
                    }
                }
                Ok(tokio_tungstenite::tungstenite::Message::Binary(bin)) => {
                    match serde_json::from_slice::<Wire>(&bin) {
                        Ok(wire) => Some(Ok(wire)),
                        Err(e) => Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Failed to deserialize Wire from binary: {e}"),
                        ))),
                    }
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => None,
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            }
        });

    (Box::pin(sink), Box::pin(stream))
}
