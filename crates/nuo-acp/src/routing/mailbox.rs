use crate::envelope::AgentEnvelope;
use crate::error::{ProtocolError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;

/// Delivery core shared between an agent's mailbox handles and the router.
///
/// Traffic is split by correlation semantics:
///
/// - **Correlated replies** (`Resolve` / `Reject` / `Inform` carrying a
///   `correlation_id`) go to the `oneshot` channel registered for that request.
/// - **Everything else** (delegations, queries, broadcasts, signals) goes to
///   the unsolicited inbox drained by [`Mailbox::recv`].
///
/// Keeping these paths separate is what makes concurrent delegation safe:
/// waiting for a reply can never consume — or discard — an unrelated peer task.
pub struct MailboxCore {
    replies: Mutex<HashMap<Uuid, oneshot::Sender<AgentEnvelope>>>,
    inbox_tx: mpsc::Sender<AgentEnvelope>,
    outbound_tx: mpsc::Sender<AgentEnvelope>,
}

impl MailboxCore {
    /// Delivers an envelope into this mailbox, honouring correlation semantics.
    pub async fn deliver(&self, envelope: AgentEnvelope) -> Result<()> {
        if envelope.settles_request()
            && let Some(request_id) = envelope.correlation_id
        {
            let waiting = {
                let mut guard = self.replies.lock().await;
                guard.remove(&request_id)
            };

            match waiting {
                Some(tx) => {
                    // The awaiting caller may have timed out; a closed receiver
                    // is not a delivery failure.
                    let _ = tx.send(envelope);
                }
                None => {
                    // Late reply for an expired or unknown request. Dropping it
                    // is deliberate: leaking it into the unsolicited inbox would
                    // let a stale response masquerade as a fresh peer task.
                    tracing::debug!(%request_id, "dropping reply for unknown or expired request");
                }
            }
            return Ok(());
        }

        self.inbox_tx
            .send(envelope)
            .await
            .map_err(|_| ProtocolError::Transport("inbox channel closed".into()))
    }

    /// Registers interest in the reply to `request_id`.
    pub async fn register_reply(&self, request_id: Uuid) -> oneshot::Receiver<AgentEnvelope> {
        let (tx, rx) = oneshot::channel();
        let mut guard = self.replies.lock().await;
        guard.insert(request_id, tx);
        rx
    }

    /// Abandons interest in a reply, used when a request times out.
    pub async fn cancel_reply(&self, request_id: Uuid) {
        let mut guard = self.replies.lock().await;
        guard.remove(&request_id);
    }

    /// Number of in-flight requests currently awaiting replies.
    pub async fn pending_reply_count(&self) -> usize {
        self.replies.lock().await.len()
    }

    /// Creates a cloneable sending handle from this delivery core.
    pub fn handle(self: &Arc<Self>) -> MailboxHandle {
        MailboxHandle { core: self.clone() }
    }
}

/// Cloneable sending half of an agent mailbox.
///
/// Shared freely with tools and background tasks; it cannot consume the inbox,
/// so it is always safe to call from inside a running cognitive loop.
#[derive(Clone)]
pub struct MailboxHandle {
    core: Arc<MailboxCore>,
}

impl MailboxHandle {
    pub fn new(core: Arc<MailboxCore>) -> Self {
        Self { core }
    }
    /// Emits an envelope from this agent onto the network.
    pub async fn send(&self, envelope: AgentEnvelope) -> Result<()> {
        self.core
            .outbound_tx
            .send(envelope)
            .await
            .map_err(|e| ProtocolError::Transport(format!("failed to send from mailbox: {e}")))
    }

    /// Dispatches an envelope onto the network and awaits its correlated reply up to `timeout`.
    pub async fn request(
        &self,
        envelope: AgentEnvelope,
        timeout: std::time::Duration,
    ) -> Result<AgentEnvelope> {
        let request_id = envelope.id;
        let reply_rx = self.register_reply(request_id).await;

        if let Err(err) = self.send(envelope).await {
            self.cancel_reply(request_id).await;
            return Err(err);
        }

        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(ProtocolError::Transport(format!(
                "reply channel for request {request_id} was closed by peer"
            ))),
            Err(_) => {
                self.cancel_reply(request_id).await;
                Err(ProtocolError::RequestTimeout(timeout.as_millis() as u64))
            }
        }
    }

    /// Registers interest in the reply to `request_id`.
    ///
    /// Register *before* dispatching the request to avoid racing the response.
    pub async fn register_reply(&self, request_id: Uuid) -> oneshot::Receiver<AgentEnvelope> {
        self.core.register_reply(request_id).await
    }

    /// Abandons interest in a reply, used when a request times out.
    pub async fn cancel_reply(&self, request_id: Uuid) {
        self.core.cancel_reply(request_id).await
    }

    /// Number of in-flight requests currently awaiting replies.
    pub async fn pending_reply_count(&self) -> usize {
        self.core.pending_reply_count().await
    }

    /// Shared delivery core, for routers and transports.
    pub fn core(&self) -> Arc<MailboxCore> {
        self.core.clone()
    }
}

/// Receiving half of an agent mailbox, owned by the agent's serve loop.
pub struct Mailbox {
    handle: MailboxHandle,
    inbox_rx: mpsc::Receiver<AgentEnvelope>,
}

impl Mailbox {
    /// Creates a mailbox and its shared delivery core.
    ///
    /// The core is returned so the owning router can deliver without access to
    /// the receiver. Prefer [`crate::routing::AgentRouter::register`].
    pub fn new(
        inbox_capacity: usize,
        outbound_tx: mpsc::Sender<AgentEnvelope>,
    ) -> (Self, Arc<MailboxCore>) {
        let (inbox_tx, inbox_rx) = mpsc::channel(inbox_capacity);
        let core = Arc::new(MailboxCore {
            replies: Mutex::new(HashMap::new()),
            inbox_tx,
            outbound_tx,
        });
        let mailbox = Self {
            handle: MailboxHandle { core: core.clone() },
            inbox_rx,
        };
        (mailbox, core)
    }

    /// Cloneable sending half, safe to share with tools and tasks.
    pub fn handle(&self) -> MailboxHandle {
        self.handle.clone()
    }

    /// Asynchronously receives the next unsolicited envelope.
    ///
    /// Correlated replies are never returned here.
    pub async fn recv(&mut self) -> Option<AgentEnvelope> {
        self.inbox_rx.recv().await
    }

    /// Attempts to receive an unsolicited envelope without blocking.
    pub fn try_recv(&mut self) -> std::result::Result<AgentEnvelope, mpsc::error::TryRecvError> {
        self.inbox_rx.try_recv()
    }

    /// Emits an envelope from this agent onto the network.
    pub async fn send(&self, envelope: AgentEnvelope) -> Result<()> {
        self.handle.send(envelope).await
    }
}
