//! Unified communication fabric providing transport-agnostic message exchange,
//! monotonic timeline logging, directory discovery, and zero-trust peer routing.

use crate::address::AgentAddress;
use crate::channel::id::ChannelId;
use crate::channel::message::{ChannelMessage, Subscription, SubscriptionMode};
use crate::channel::surface::Channel;
use crate::envelope::{AgentEnvelope, DelegationBudget};
use crate::error::{ProtocolError, Result};
use crate::intent::{DelegationOutcome, MessageIntent};
use crate::manifest::AgentManifest;
use crate::routing::mailbox::MailboxHandle;
use crate::routing::policy::RoutingPolicy;
use crate::routing::router::AgentRouter;
use crate::routing::tracker::PresenceTracker;
use crate::transport::TransportSender;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Unified communication fabric: the single substrate connecting agents,
/// monotonic timelines, and remote gateways.
#[derive(Clone)]
pub struct Fabric {
    id: String,
    router: AgentRouter,
    manifests: Arc<RwLock<HashMap<AgentAddress, AgentManifest>>>,
    channels: Arc<RwLock<HashMap<ChannelId, Channel>>>,
    replicators: Arc<RwLock<Vec<Arc<dyn TransportSender>>>>,
    tracker: PresenceTracker,
}

impl Fabric {
    /// Creates a new communication fabric with a designated identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            router: AgentRouter::new(),
            manifests: Arc::new(RwLock::new(HashMap::new())),
            channels: Arc::new(RwLock::new(HashMap::new())),
            replicators: Arc::new(RwLock::new(Vec::new())),
            tracker: PresenceTracker::new(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Access to the presence and heartbeat tracker ledger.
    pub fn tracker(&self) -> &PresenceTracker {
        &self.tracker
    }

    /// Sets the dynamic routing policy for topological access control.
    pub async fn set_routing_policy(&self, policy: Arc<dyn RoutingPolicy>) {
        self.router.set_policy(policy).await;
    }

    /// Access to the underlying router for attaching transport gateways.
    pub fn router(&self) -> &AgentRouter {
        &self.router
    }

    // -----------------------------------------------------------------
    // Directory & Discovery
    // -----------------------------------------------------------------

    /// Admits an agent by registering its manifest and returning its dedicated mailbox.
    pub async fn join(&self, manifest: AgentManifest, buffer: usize) -> crate::routing::Mailbox {
        let address = manifest.address.clone();
        {
            let mut guard = self.manifests.write().await;
            guard.insert(address.clone(), manifest);
        }
        self.router.register(address, buffer).await
    }

    /// Connects an agent address directly without a prior manifest card.
    pub async fn register(&self, address: AgentAddress, buffer: usize) -> crate::routing::Mailbox {
        self.router.register(address, buffer).await
    }

    /// Removes an agent from the fabric.
    pub async fn leave(&self, address: &AgentAddress) {
        {
            let mut guard = self.manifests.write().await;
            guard.remove(address);
        }
        let channels = self.channels().await;
        for c in channels {
            c.unsubscribe(address).await;
        }
    }

    /// Retrieves an agent's capability manifest by address.
    pub async fn manifest_of(&self, address: &AgentAddress) -> Option<AgentManifest> {
        let guard = self.manifests.read().await;
        guard.get(address).cloned()
    }

    /// Lists manifests of other reachable agents on this fabric.
    pub async fn peers_of(&self, caller: &AgentAddress) -> Vec<AgentManifest> {
        let guard = self.manifests.read().await;
        let mut peers: Vec<AgentManifest> = guard
            .values()
            .filter(|m| &m.address != caller)
            .cloned()
            .collect();
        peers.sort_by(|a, b| a.address.cmp(&b.address));
        peers
    }

    /// Renders the peer directory as prompt text for a given agent.
    pub async fn render_peer_directory(&self, current_agent: &AgentAddress) -> Option<String> {
        let peers = self.peers_of(current_agent).await;
        if peers.is_empty() {
            return None;
        }
        let lines: Vec<String> = peers.iter().map(|m| m.to_prompt_summary()).collect();
        Some(lines.join("\n"))
    }

    // -----------------------------------------------------------------
    // Timelines & Channels (Monotonic Ordered Log Primitives)
    // -----------------------------------------------------------------

    /// Opens or retrieves an existing timeline/channel.
    pub async fn open_timeline(&self, id: ChannelId) -> Channel {
        self.open_channel(id).await
    }

    /// Opens or retrieves an existing channel.
    pub async fn open_channel(&self, id: ChannelId) -> Channel {
        let mut channels = self.channels.write().await;
        if let Some(existing) = channels.get(&id) {
            return existing.clone();
        }
        let channel = Channel::new(id.clone());
        channels.insert(id, channel.clone());
        channel
    }

    /// Opens a channel with a bounded retention capacity.
    pub async fn open_channel_with_capacity(&self, id: ChannelId, capacity: usize) -> Channel {
        let mut channels = self.channels.write().await;
        if let Some(existing) = channels.get(&id) {
            return existing.clone();
        }
        let channel = Channel::with_capacity(id.clone(), capacity);
        channels.insert(id, channel.clone());
        channel
    }

    /// Accesses an existing timeline by identifier.
    pub async fn timeline(&self, id: &ChannelId) -> Option<Channel> {
        self.channel(id).await
    }

    /// Accesses an existing channel by identifier.
    pub async fn channel(&self, id: &ChannelId) -> Option<Channel> {
        let guard = self.channels.read().await;
        guard.get(id).cloned()
    }

    /// Lists all active timelines / channels.
    pub async fn timelines(&self) -> Vec<Channel> {
        self.channels().await
    }

    /// Lists all active channels.
    pub async fn channels(&self) -> Vec<Channel> {
        let guard = self.channels.read().await;
        guard.values().cloned().collect()
    }

    /// Retrieves all subscriptions held by an agent across channels on this fabric.
    pub async fn subscriptions_of(&self, agent: &AgentAddress) -> Vec<(Channel, Subscription)> {
        let channels = self.channels().await;
        let mut out = Vec::new();
        for channel in channels {
            if let Some(sub) = channel.subscription_of(agent).await {
                out.push((channel, sub));
            }
        }
        out
    }

    /// Subscribes an agent to a channel.
    pub async fn subscribe(
        &self,
        subscriber: &AgentAddress,
        channel_id: &ChannelId,
        mode: SubscriptionMode,
    ) -> Result<Subscription> {
        if self.get_member(subscriber).await.is_none() {
            return Err(ProtocolError::Unreachable(format!(
                "`{subscriber}` is not a member of room `{}` and cannot subscribe to `{channel_id}`",
                self.id
            )));
        }

        let channel = self.channel(channel_id).await.ok_or_else(|| {
            ProtocolError::ChannelNotFound(format!(
                "channel `{channel_id}` is not open in room `{}`",
                self.id
            ))
        })?;

        Ok(channel.subscribe(subscriber.clone(), mode).await)
    }

    /// Ingests a replicated channel event from a remote fabric.
    pub async fn ingest_channel_sync(
        &self,
        payload: &crate::intent::ChannelSyncPayload,
    ) -> Result<()> {
        let channel = self.open_channel(payload.channel.clone()).await;
        channel.record_replicated(payload.message.clone()).await;
        Ok(())
    }

    /// Attaches an outbound transport sender to replicate local channel publications to a remote room.
    pub async fn add_channel_replicator(&self, sender: Arc<dyn TransportSender>) {
        self.attach_replicator(sender).await;
    }

    /// Publishes a message to a channel, advancing its monotonic log.
    pub async fn publish(
        &self,
        from: &AgentAddress,
        channel_id: &ChannelId,
        body: impl Into<String>,
        mentions: Vec<AgentAddress>,
    ) -> Result<(ChannelMessage, Vec<AgentAddress>)> {
        let channel = self.channel(channel_id).await.ok_or_else(|| {
            ProtocolError::ChannelNotFound(format!(
                "channel `{channel_id}` is not open in room `{}`",
                self.id
            ))
        })?;

        for mention in &mentions {
            if mention != from && self.get_member(mention).await.is_none() {
                return Err(ProtocolError::Unreachable(format!(
                    "mentioned agent `{mention}` is not a member of fabric `{}`",
                    self.id
                )));
            }
        }

        let message = channel
            .publish(from.clone(), body, mentions.clone(), None)
            .await;

        // Replicate to remote fabrics/rooms if replicators are attached.
        let replicators = self.replicators.read().await.clone();
        if !replicators.is_empty() {
            let sync_intent = MessageIntent::channel_sync(channel_id.clone(), message.clone());
            let envelope = AgentEnvelope::new(
                from.clone(),
                AgentAddress::parse(&format!("agent://{}/channel-sync", self.id))
                    .unwrap_or_else(|_| from.clone()),
                sync_intent,
            );
            for rep in replicators {
                let _ = rep.send(envelope.clone()).await;
            }
        }

        let mut notified = Vec::new();
        for sub in channel.subscribers().await {
            if sub.agent == *from {
                continue;
            }
            let Some(reason) = channel
                .notification_for(&sub.agent, &message)
                .await
                .reason()
            else {
                continue;
            };

            let envelope = AgentEnvelope::new(
                from.clone(),
                sub.agent.clone(),
                MessageIntent::channel_notify(
                    channel_id.clone(),
                    message.seq,
                    from.clone(),
                    reason,
                    preview_of(&message.body),
                ),
            );
            if self.dispatch(envelope).await.is_ok() {
                notified.push(sub.agent);
            }
        }

        Ok((message, notified))
    }

    // -----------------------------------------------------------------
    // Point-to-Point Messaging & Delegation
    // -----------------------------------------------------------------

    /// Dispatches an envelope into the fabric routing network.
    pub async fn dispatch(&self, envelope: AgentEnvelope) -> Result<()> {
        self.router.route(envelope).await
    }

    /// Sends a delegation without awaiting the outcome.
    pub async fn delegate(
        &self,
        sender: &AgentAddress,
        target: &AgentAddress,
        task: impl Into<String>,
        budget: Option<DelegationBudget>,
    ) -> Result<uuid::Uuid> {
        let envelope = self
            .build_delegation(sender, target, task.into(), budget)
            .await?;
        let id = envelope.id;
        self.dispatch(envelope).await?;
        Ok(id)
    }

    /// Delegates a task and awaits the peer's terminal outcome.
    pub async fn request(
        &self,
        handle: &MailboxHandle,
        sender: &AgentAddress,
        target: &AgentAddress,
        task: impl Into<String>,
        budget: Option<DelegationBudget>,
        timeout: Duration,
    ) -> Result<DelegationOutcome> {
        let envelope = self
            .build_delegation(sender, target, task.into(), budget)
            .await?;
        let request_id = envelope.id;

        let reply_rx = handle.register_reply(request_id).await;

        if let Err(err) = self.dispatch(envelope).await {
            handle.cancel_reply(request_id).await;
            return Err(err);
        }

        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(reply)) => reply.outcome().ok_or_else(|| {
                ProtocolError::DelegationError(format!(
                    "peer `{target}` replied with a non-terminal intent"
                ))
            }),
            Ok(Err(_)) => Err(ProtocolError::DelegationError(format!(
                "reply channel for request {request_id} to `{target}` was closed"
            ))),
            Err(_) => {
                handle.cancel_reply(request_id).await;
                Err(ProtocolError::RequestTimeout(timeout.as_millis() as u64))
            }
        }
    }

    /// Builds and validates a delegation envelope, without dispatching it.
    pub async fn build_delegation(
        &self,
        sender: &AgentAddress,
        target: &AgentAddress,
        task: String,
        budget: Option<DelegationBudget>,
    ) -> Result<AgentEnvelope> {
        if self.get_member(target).await.is_none() {
            return Err(ProtocolError::Unreachable(format!(
                "target agent `{target}` is not a member of fabric `{}`",
                self.id
            )));
        }

        let mut envelope = AgentEnvelope::new(
            sender.clone(),
            target.clone(),
            MessageIntent::delegate(task),
        );
        if let Some(budget) = budget {
            envelope = envelope.with_budget(budget);
        }
        Ok(envelope)
    }

    /// Sends a natural language query and awaits the peer's informational reply.
    pub async fn ask(
        &self,
        handle: &MailboxHandle,
        sender: &AgentAddress,
        target: &AgentAddress,
        prompt: impl Into<String>,
        timeout: Duration,
    ) -> Result<String> {
        if self.get_member(target).await.is_none() {
            return Err(ProtocolError::Unreachable(format!(
                "target agent `{target}` is not a member of fabric `{}`",
                self.id
            )));
        }

        let envelope = AgentEnvelope::new(
            sender.clone(),
            target.clone(),
            MessageIntent::query(prompt.into()),
        );
        let request_id = envelope.id;
        let reply_rx = handle.register_reply(request_id).await;

        if let Err(err) = self.dispatch(envelope).await {
            handle.cancel_reply(request_id).await;
            return Err(err);
        }

        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(reply)) => match reply.intent {
                MessageIntent::Inform(payload) => Ok(payload.reply),
                MessageIntent::Resolve(payload) => Ok(payload.output),
                MessageIntent::Reject(payload) => {
                    Err(ProtocolError::DelegationError(payload.reason))
                }
                other => Err(ProtocolError::DelegationError(format!(
                    "peer replied with unexpected intent: {}",
                    other.summary()
                ))),
            },
            Ok(Err(_)) => Err(ProtocolError::DelegationError(format!(
                "reply channel for query {request_id} to `{target}` was closed"
            ))),
            Err(_) => {
                handle.cancel_reply(request_id).await;
                Err(ProtocolError::RequestTimeout(timeout.as_millis() as u64))
            }
        }
    }

    // -----------------------------------------------------------------
    // Gateways and Replicators
    // -----------------------------------------------------------------

    /// Attaches an outbound transport replicator.
    pub async fn attach_replicator(&self, sender: Arc<dyn TransportSender>) {
        let mut guard = self.replicators.write().await;
        guard.push(sender);
    }

    /// Attaches a remote gateway forwarding a prefix to an external transport sender.
    pub async fn add_remote_gateway(&self, prefix: String, sender: Arc<dyn TransportSender>) {
        self.router.add_remote_gateway(prefix, sender).await;
    }

    pub async fn get_member(&self, address: &AgentAddress) -> Option<FabricMember> {
        let guard = self.manifests.read().await;
        let manifest = guard.get(address)?.clone();
        let mailbox = self.router.mailbox_handle(address).await?;
        Some(FabricMember { manifest, mailbox })
    }
}

/// Member handle resolved on the fabric.
pub struct FabricMember {
    pub manifest: AgentManifest,
    pub mailbox: MailboxHandle,
}

fn preview_of(body: &str) -> String {
    const LIMIT: usize = 120;
    if body.chars().count() <= LIMIT {
        return body.to_string();
    }
    let truncated: String = body.chars().take(LIMIT).collect();
    format!("{truncated}…")
}
