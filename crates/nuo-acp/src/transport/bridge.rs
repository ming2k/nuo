use super::TransportLink;
use crate::address::AgentAddress;
use crate::routing::AgentRouter;
use std::sync::Arc;

/// Bridges a transport link to a router, making remote members reachable.
///
/// Two directions are wired:
///
/// - **Inbound**: envelopes arriving on the link are routed to local members.
///   The bridge records each already-forwarded envelope id so an envelope is
///   never forwarded twice, which would otherwise loop when two routers are
///   bridged to each other.
/// - **Outbound**: envelopes addressed to `remote_prefix` are handed to the
///   link's sender by the router's remote-forwarding hook.
///
/// This is the single seam where a new wire protocol plugs in: implement
/// [`TransportSender`](super::TransportSender) /
/// [`TransportReceiver`](super::TransportReceiver) and call this function.
pub struct TransportBridge {
    /// Address prefix considered remote by this bridge, e.g. `agent://node-b`.
    pub remote_prefix: String,
}

impl TransportBridge {
    pub fn new(remote_prefix: impl Into<String>) -> Self {
        Self {
            remote_prefix: remote_prefix.into(),
        }
    }

    /// Returns true when `address` belongs to the remote side of this bridge.
    pub fn is_remote(&self, address: &AgentAddress) -> bool {
        address.as_str().starts_with(&self.remote_prefix)
    }

    /// Spawns the inbound pump, forwarding link traffic into `router`.
    ///
    /// Note the deliberate absence of a loop guard: a bridge forwards inbound
    /// envelopes to its *local* router, and the local router only sends onward
    /// to a gateway when the target is remote. Two hosts bridged to each other
    /// therefore cannot echo an envelope back and forth, because each side
    /// resolves the other's addresses as remote rather than bouncing them home.
    /// The returned task ends when the link closes.
    pub fn spawn_inbound(
        self,
        router: AgentRouter,
        mut link: TransportLink,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match link.receiver.recv().await {
                    Ok(envelope) => {
                        if let Err(err) = router.route(envelope).await {
                            tracing::warn!(error = %err, "inbound bridge delivery failed");
                        }
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "inbound transport link closed");
                        break;
                    }
                }
            }
        })
    }
}

/// Registers a remote-forwarding sink on the router for `prefix`.
///
/// Envelopes whose target address starts with `prefix` are handed to the sink
/// instead of being resolved locally.
pub async fn attach_outbound(
    router: &AgentRouter,
    prefix: impl Into<String>,
    sender: Arc<dyn crate::transport::TransportSender>,
) {
    router.add_remote_gateway(prefix.into(), sender).await;
}
