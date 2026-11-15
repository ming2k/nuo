//! Transport layer: the seam that carries envelopes between hosts.
//!
//! Transports are deliberately split into a [`TransportSender`] (shared,
//! object-safe, held by routers) and a [`TransportReceiver`] (owned by a bridge
//! task). Cross-host implementations — Unix sockets, named pipes, WebSocket,
//! NATS — implement these two traits and attach via
//! [`AgentRouter::add_remote_gateway`](crate::routing::AgentRouter::add_remote_gateway).

pub mod bridge;
pub mod memory;

use crate::envelope::AgentEnvelope;
use crate::error::Result;
use async_trait::async_trait;

pub use bridge::{TransportBridge, attach_outbound};
pub use memory::{ChannelReceiver, ChannelSender, TransportLink, in_memory_pair};

/// Sending half of a transport link. Cheaply cloneable and shared.
///
/// The router holds senders for remote gateways, so this trait must be
/// object-safe and usable from behind an `Arc`.
#[async_trait]
pub trait TransportSender: Send + Sync {
    /// Sends an envelope over this link.
    async fn send(&self, envelope: AgentEnvelope) -> Result<()>;
}

/// Receiving half of a transport link, driven by a bridge task.
#[async_trait]
pub trait TransportReceiver: Send {
    /// Waits for the next inbound envelope.
    async fn recv(&mut self) -> Result<AgentEnvelope>;
}
