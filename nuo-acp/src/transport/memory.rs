use super::{TransportReceiver, TransportSender};
use crate::envelope::AgentEnvelope;
use crate::error::{ProtocolError, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

/// In-process transport backed by a Tokio MPSC channel.
///
/// Used for co-located agents and as the backbone for tests. Cross-host
/// transports (IPC, WebSocket, NATS) implement the same two traits.
pub struct ChannelSender {
    sender: mpsc::Sender<AgentEnvelope>,
}

impl ChannelSender {
    pub fn new(sender: mpsc::Sender<AgentEnvelope>) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl TransportSender for ChannelSender {
    async fn send(&self, envelope: AgentEnvelope) -> Result<()> {
        self.sender
            .send(envelope)
            .await
            .map_err(|e| ProtocolError::Transport(format!("channel send failed: {e}")))
    }
}

/// Receiving end of a [`ChannelSender`].
pub struct ChannelReceiver {
    receiver: mpsc::Receiver<AgentEnvelope>,
}

impl ChannelReceiver {
    pub fn new(receiver: mpsc::Receiver<AgentEnvelope>) -> Self {
        Self { receiver }
    }
}

#[async_trait]
impl TransportReceiver for ChannelReceiver {
    async fn recv(&mut self) -> Result<AgentEnvelope> {
        self.receiver
            .recv()
            .await
            .ok_or_else(|| ProtocolError::Transport("channel closed by peer".into()))
    }
}

/// A split transport link: a sender for the router and a receiver for a bridge.
///
/// The sender is `Arc` so it can be handed directly to
/// [`AgentRouter::add_remote_gateway`](crate::routing::AgentRouter::add_remote_gateway),
/// while the receiver is owned by the inbound pump.
pub struct TransportLink {
    pub sender: Arc<dyn TransportSender>,
    pub receiver: Box<dyn TransportReceiver>,
}

/// Creates a connected in-process pair: `a` sends to `b`, `b` sends to `a`.
pub fn in_memory_pair(buffer: usize) -> (TransportLink, TransportLink) {
    // a -> b
    let (tx_ab, rx_ab) = mpsc::channel(buffer);
    // b -> a
    let (tx_ba, rx_ba) = mpsc::channel(buffer);

    let link_a = TransportLink {
        sender: Arc::new(ChannelSender::new(tx_ab)),
        receiver: Box::new(ChannelReceiver::new(rx_ba)),
    };

    let link_b = TransportLink {
        sender: Arc::new(ChannelSender::new(tx_ba)),
        receiver: Box::new(ChannelReceiver::new(rx_ab)),
    };

    (link_a, link_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::AgentAddress;
    use crate::intent::MessageIntent;

    #[tokio::test]
    async fn in_memory_pair_round_trip() {
        let (mut a, mut b) = in_memory_pair(8);

        let envelope = AgentEnvelope::new(
            AgentAddress::parse("agent://local/alice").unwrap(),
            AgentAddress::parse("agent://local/bob").unwrap(),
            MessageIntent::delegate("sum two numbers"),
        );
        let id = envelope.id;

        a.sender.send(envelope).await.unwrap();
        let received = b.receiver.recv().await.unwrap();
        assert_eq!(received.id, id);

        // Reverse direction is independent of the first.
        let reply = received.reply(
            AgentAddress::parse("agent://local/bob").unwrap(),
            MessageIntent::resolve("4"),
        );
        let reply_id = reply.id;
        b.sender.send(reply).await.unwrap();
        assert_eq!(a.receiver.recv().await.unwrap().id, reply_id);
    }
}
