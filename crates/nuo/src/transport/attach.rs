//! Attach handshake: errors, welcome parsing, and the duplex link.

use super::current_posture;
use super::lifecycle::ATTACH_ACK_TIMEOUT;
use super::*;
use nuo_protocol::ThreadOverview;

/// A failed attach.
///
/// `[INV-ATTACH-01]`: `code` is the server's machine-readable taxonomy, carried
/// verbatim from `Wire::Error`. Downgrading a refusal to a bare `String` is what
/// made an expected outcome (a tab whose session is gone) indistinguishable from
/// a transport fault, so the client had no choice but to die on it (ADR-0123).
///
/// `code: None` covers both "transport failure" (the server never spoke) and
/// "refusal with no declared taxonomy". Both are unrecoverable by contract: a
/// client may not invent a recovery the server did not authorize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachError {
    pub code: Option<String>,
    pub message: String,
}

impl AttachError {
    /// A transport-level failure: no server taxonomy exists.
    pub(super) fn transport(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }

    /// The refusal code, without the `Option` ceremony at every call site.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Whether the target session provably no longer exists
    /// (`ERR_SESSION_GONE`). The tab naming it must be retired.
    pub fn is_session_gone(&self) -> bool {
        self.code() == Some(nuo_protocol::ERR_SESSION_GONE)
    }

    /// Whether the target session exists durably but could not be assembled
    /// (`ERR_SESSION_UNRESUMABLE`). The tab must be kept — its history is real.
    pub fn is_session_unresumable(&self) -> bool {
        self.code() == Some(nuo_protocol::ERR_SESSION_UNRESUMABLE)
    }
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The message alone: diagnostics must not grow a code prefix into prose
        // a human reads (and a caller must never parse it back out).
        f.write_str(&self.message)
    }
}

impl std::error::Error for AttachError {}

impl From<AttachError> for String {
    fn from(error: AttachError) -> Self {
        error.message
    }
}

struct Welcome {
    session_id: String,
    phase: nuo_protocol::ThreadPhase,
}

enum Reply {
    Welcome(Welcome),
    Pick(Vec<ThreadOverview>),
}

pub type AttachResult = Result<Handshake, AttachError>;

/// Where an attached reader records the *transport* failure that ended it.
///
/// Deliberately not an [`AgentResponse`]: a socket death is not something the
/// server said, and encoding it as one made a decode fault indistinguishable
/// from a server instruction to stop (ADR-0136). The consumer reads this after
/// the response stream closes — `None` means the connection ended cleanly.
pub type LinkLostMarker = Arc<Mutex<Option<String>>>;

#[allow(clippy::large_enum_variant)]
pub enum Handshake {
    Attached {
        req_tx: mpsc::UnboundedSender<AgentRequest>,
        resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
        /// Where the reader's *transport* failure lands, when there is one.
        ///
        /// Separate from `resp_rx` by construction: a socket death is not an
        /// `AgentResponse`, and spelling it `Exit` made a client click look
        /// like a server instruction (ADR-0136). `None` when the reader ended
        /// because the connection closed cleanly.
        link_lost: LinkLostMarker,
        session_id: String,
        /// Readiness at the moment of the handshake. Informational only:
        /// `[INV-ATTACH-04]` (ADR-0133) makes the *post-snapshot*
        /// `AgentResponse::ThreadPhase(Ready)` the gate for input.
        phase: nuo_protocol::ThreadPhase,
    },
    Pick(Vec<ThreadOverview>),
}

pub async fn connect(info: &ServerInfo, action: AttachAction) -> AttachResult {
    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_handshake((sink, source), action).await;
    }
    if info.port == 0 {
        return Err(AttachError::transport(
            "server runs in client-bound mode (UDS only) and local endpoint is not reachable",
        ));
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| AttachError::transport(format!("bad ws url {url}: {e}")))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| AttachError::transport(format!("bad bearer token: {e}")))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| AttachError::transport(format!("ws connect to {url}: {e}")))?;
    let (sink, source) = websocket_split(ws);
    finish_handshake((sink, source), action).await
}

async fn finish_handshake(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
) -> AttachResult {
    finish_handshake_with_project(parts, action, std::env::current_dir().ok()).await
}

pub(super) async fn finish_handshake_with_project(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
    project: Option<std::path::PathBuf>,
) -> AttachResult {
    let (mut wire_sink, mut wire_source) = parts;

    let select = Wire::Select {
        action,
        project,
        posture: current_posture(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol: PROTOCOL_VERSION,
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| AttachError::transport(format!("wire send select: {e}")))?;

    let reply = tokio::time::timeout(ATTACH_ACK_TIMEOUT, async {
        match wire_source.next().await {
            Some(Ok(WireFrame::Wire(wire))) => match wire {
                Wire::Welcome { thread_id, phase } => Ok(Reply::Welcome(Welcome {
                    session_id: thread_id,
                    phase,
                })),
                Wire::Pick { threads } => Ok(Reply::Pick(threads)),
                Wire::Error { message, code } => {
                    // `[INV-ATTACH-01]`: the code travels with the message.
                    // This is the hop that used to discard the taxonomy
                    // with a `..` and leave the client to sniff prose.
                    Err(AttachError {
                        code,
                        message: format!("server rejected the attach: {message}"),
                    })
                }
                other => {
                    // `[INV-ATTACH-03]` (ADR-0133): Welcome is the first
                    // frame by contract. Silently dropping an earlier state
                    // frame hid a protocol violation and lost hydration, so
                    // this is now a hard, diagnosable error.
                    Err(AttachError::transport(format!(
                        "unexpected frame before Welcome: {other:?}"
                    )))
                }
            },
            // The handshake is the one place where a frame we cannot model is
            // still an attach refusal: no thread has been claimed yet, and a
            // Welcome we cannot read is exactly the disagreement the protocol
            // number exists to prevent. Refusing with the taxonomy lets the
            // attach loop disclose it instead of dying on it.
            Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                Err(AttachError::transport(format!(
                    "server sent a frame this client cannot decode{}: {detail}",
                    envelope
                        .map(|tag| format!(" (envelope `{tag}`)"))
                        .unwrap_or_default()
                )))
            }
            Some(Err(error)) => Err(AttachError::transport(format!(
                "wire recv during handshake: {error}"
            ))),
            None => Err(AttachError::transport("server closed the connection")),
        }
    })
    .await
    .map_err(|_| {
        AttachError::transport(
            "server did not acknowledge the attach within the liveness window \
             (the server may be starting, wedged, or speaking a different protocol)",
        )
    })??;

    let welcome = match reply {
        Reply::Welcome(w) => w,
        Reply::Pick(sessions) => {
            let _ = wire_sink.close().await;
            return Ok(Handshake::Pick(sessions));
        }
    };

    let (req_out_tx, mut req_out_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();
    // Shared with the reader task: after `resp_rx` closes, this is *how* the
    // consumer learns whether the link failed and why, without guessing.
    let link_lost_marker = Arc::new(Mutex::new(None::<String>));
    let link_lost_inner = Arc::clone(&link_lost_marker);
    let link_lost_for_writer = Arc::clone(&link_lost_marker);
    let exit_seen = Arc::new(Mutex::new(false));
    let exit_seen_inner = Arc::clone(&exit_seen);

    tokio::spawn(async move {
        let mut end_pending = false;
        while let Some(request) = req_out_rx.recv().await {
            if matches!(request, AgentRequest::EndThread) {
                end_pending = true;
            }
            if let Err(error) = wire_sink.send(Wire::Request { request }).await {
                tracing::warn!(%error, "attach: wire send failed");
                let mut marker = link_lost_for_writer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if marker.is_none() {
                    *marker = Some(format!("wire send failed: {error}"));
                }
                break;
            }
        }
        if end_pending {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let _ = wire_sink.close().await;
    });

    tokio::spawn(async move {
        while let Some(frame) = wire_source.next().await {
            match frame {
                Ok(WireFrame::Wire(Wire::Response { response })) => {
                    // A server instruction to stop is recorded as such, so the
                    // close that follows is not mistaken for a fault.
                    if matches!(response, AgentResponse::Exit) {
                        *exit_seen_inner.lock().unwrap_or_else(|e| e.into_inner()) = true;
                    }
                    if resp_in_tx.send(response).is_err() {
                        return;
                    }
                }
                // One frame this build cannot model. Both transports are
                // self-delimiting, so the reader stays aligned and the session
                // is untouched — disclose the skew and keep reading
                // [ADR-0136]. Before this arm existed the frame arrived as
                // `Err`, the loop ended, and the synthesized `Exit` closed the
                // window mid-round.
                Ok(WireFrame::Undecodable { envelope, detail }) => {
                    tracing::warn!(
                        envelope = envelope.as_deref().unwrap_or("<none>"),
                        %detail,
                        "attach: server sent a frame this client cannot decode; \
                         connection retained"
                    );
                    let notice = AgentResponse::Error(format!(
                        "The server sent a message this client does not understand{}. \
                         The connection is still live — run `nuo stop` so both \
                         sides come up on the same build.",
                        envelope
                            .map(|tag| format!(" (envelope `{tag}`)"))
                            .unwrap_or_default()
                    ));
                    if resp_in_tx.send(notice).is_err() {
                        return;
                    }
                }
                Ok(WireFrame::Wire(_)) => {
                    tracing::warn!("attach: unexpected post-handshake frame, ignored")
                }
                // The socket itself failed. This is *not* a response and must
                // never be spelled `Exit`; it is recorded where the consumer
                // can read the real reason after `resp_rx` closes [ADR-0136].
                Err(error) => {
                    tracing::warn!(%error, "attach: wire recv failed; link is down");
                    *link_lost_inner.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(error.to_string());
                    break;
                }
            }
        }
        let exit_seen = *exit_seen_inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut marker = link_lost_inner.lock().unwrap_or_else(|e| e.into_inner());
        if !exit_seen && marker.is_none() {
            tracing::warn!("attach: connection closed by server unexpectedly without Exit frame");
            *marker = Some("server closed connection unexpectedly".to_string());
        }
    });

    Ok(Handshake::Attached {
        req_tx: req_out_tx,
        resp_rx: resp_in_rx,
        link_lost: link_lost_marker,
        session_id: welcome.session_id,
        phase: welcome.phase,
    })
}
