use super::mailbox::{Mailbox, MailboxCore, MailboxHandle};
use crate::address::AgentAddress;
use crate::envelope::AgentEnvelope;
use crate::error::{ProtocolError, Result};
use crate::transport::TransportSender;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

/// A gateway forwarding addresses with a given prefix to a remote host.
#[derive(Clone)]
struct RemoteGateway {
    prefix: String,
    sender: Arc<dyn TransportSender>,
}

/// Message routing hub delivering envelopes between registered agent mailboxes
/// and forwarding remote-addressed traffic through attached gateways.
///
/// The router owns each mailbox's delivery core, so it can deliver correlated
/// replies and unsolicited traffic without holding the consuming receiver.
#[derive(Clone)]
pub struct AgentRouter {
    endpoints: Arc<RwLock<HashMap<AgentAddress, Arc<MailboxCore>>>>,
    gateways: Arc<RwLock<Vec<RemoteGateway>>>,
    policy: Arc<RwLock<Arc<dyn super::policy::RoutingPolicy>>>,
}

impl Default for AgentRouter {
    fn default() -> Self {
        Self {
            endpoints: Arc::new(RwLock::new(HashMap::new())),
            gateways: Arc::new(RwLock::new(Vec::new())),
            policy: Arc::new(RwLock::new(Arc::new(super::policy::AllowAllPolicy))),
        }
    }
}

impl AgentRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the dynamic routing policy for access control and topology supervision.
    pub async fn set_policy(&self, policy: Arc<dyn super::policy::RoutingPolicy>) {
        let mut guard = self.policy.write().await;
        *guard = policy;
    }

    /// Attaches a remote gateway: targets starting with `prefix` are forwarded
    /// to `sender` rather than resolved against local members. Longest prefix
    /// wins when several gateways overlap.
    pub async fn add_remote_gateway(&self, prefix: String, sender: Arc<dyn TransportSender>) {
        let mut guard = self.gateways.write().await;
        guard.retain(|g| g.prefix != prefix);
        guard.push(RemoteGateway { prefix, sender });
    }

    /// Detaches a previously attached gateway.
    pub async fn remove_remote_gateway(&self, prefix: &str) {
        let mut guard = self.gateways.write().await;
        guard.retain(|g| g.prefix != prefix);
    }

    /// Resolves the gateway responsible for `address`, if any.
    async fn gateway_for(&self, address: &AgentAddress) -> Option<Arc<dyn TransportSender>> {
        let guard = self.gateways.read().await;
        guard
            .iter()
            .filter(|g| address.as_str().starts_with(&g.prefix))
            .max_by_key(|g| g.prefix.len())
            .map(|g| g.sender.clone())
    }

    /// Registers an agent address, returning its `Mailbox` for the serve loop.
    ///
    /// Outbound traffic from the mailbox is routed back through this router.
    pub async fn register(&self, address: AgentAddress, buffer: usize) -> Mailbox {
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<AgentEnvelope>(buffer);
        let (mailbox, core) = Mailbox::new(buffer, outbound_tx);

        {
            let mut guard = self.endpoints.write().await;
            guard.insert(address.clone(), core);
        }

        // Forward outbound traffic from this mailbox into the routing network.
        let router = self.clone();
        tokio::spawn(async move {
            while let Some(env) = outbound_rx.recv().await {
                if let Err(err) = router.route(env).await {
                    tracing::warn!(error = %err, "router failed to deliver envelope");
                }
            }
        });

        mailbox
    }

    /// Unregisters an agent from the router.
    pub async fn unregister(&self, address: &AgentAddress) {
        let mut guard = self.endpoints.write().await;
        guard.remove(address);
    }

    /// Whether an address is currently registered locally.
    pub async fn is_registered(&self, address: &AgentAddress) -> bool {
        self.endpoints.read().await.contains_key(address)
    }

    /// Retrieves the delivery core for a registered address.
    pub async fn endpoint_for(&self, address: &AgentAddress) -> Option<Arc<MailboxCore>> {
        let guard = self.endpoints.read().await;
        guard.get(address).cloned()
    }

    /// Obtains a sending mailbox handle for a registered address.
    pub async fn mailbox_handle(&self, address: &AgentAddress) -> Option<MailboxHandle> {
        let core = self.endpoint_for(address).await?;
        Some(core.handle())
    }

    /// All locally registered addresses.
    pub async fn registered_addresses(&self) -> Vec<AgentAddress> {
        self.endpoints.read().await.keys().cloned().collect()
    }

    /// Routes an incoming envelope to its addressee.
    ///
    /// Local members take precedence; otherwise a matching gateway forwards the
    /// envelope off-host. Unroutable envelopes produce an explicit error rather
    /// than being silently dropped.
    ///
    /// Channel traffic does not appear here. A publication is recorded in its
    /// channel's log and the room points subscribers at it, so fan-out is
    /// resolved where subscriptions live rather than by the router.
    pub async fn route(&self, envelope: AgentEnvelope) -> Result<()> {
        let decision = {
            let guard = self.policy.read().await;
            guard.evaluate(&envelope)
        };
        if let super::policy::RoutingDecision::Deny { reason } = decision {
            return Err(ProtocolError::PolicyDenied(reason));
        }

        let endpoints: Vec<(AgentAddress, Arc<MailboxCore>)> = {
            let guard = self.endpoints.read().await;
            guard.iter().map(|(a, c)| (a.clone(), c.clone())).collect()
        };

        let target_addr = &envelope.target;

        if let Some((_, core)) = endpoints.iter().find(|(addr, _)| addr == target_addr) {
            return core.deliver(envelope).await;
        }

        if let Some(gateway) = self.gateway_for(target_addr).await {
            return gateway.send(envelope).await;
        }

        Err(ProtocolError::Unreachable(format!(
            "agent `{target_addr}` is neither a local member nor covered by a remote gateway"
        )))
    }
}
