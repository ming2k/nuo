//! Canonical inter-agent communication, addressing, envelopes, and collaborative channels.
//!
//! The crate defines *how agents find each other, address each other, communicate, and
//! delegate work in natural language* — and nothing else. It carries no
//! cognitive logic, no model client, and no I/O beyond in-process channels.
//!
//! # Layers
//!
//! - [`address`] — URI identity (`agent://authority/path`) and selectors.
//! - [`manifest`] — self-describing capability cards used for discovery and
//!   for LLM-side peer selection.
//! - [`intent`] + [`envelope`] — typed communicative intents and the envelope
//!   that carries them, including correlation and delegation budgets.
//! - [`routing`] — mailboxes, the router, and collaborative [`routing::Room`]s.
//! - [`transport`] — the seam for moving envelopes between hosts.
//!
//! # Correlation model
//!
//! Every request/reply exchange is correlated by envelope id. A
//! [`routing::Mailbox`] keeps in-flight request subscriptions separate from
//! unsolicited traffic, so awaiting a reply can never consume — or discard — an
//! unrelated delegation arriving at the same agent.
//!
//! # Delegation depth
//!
//! [`DelegationBudget::remaining_hops`] is decremented on each hand-off, so
//! agents delegating to agents cannot recurse without bound.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod address;
pub mod channel;
pub mod envelope;
pub mod error;
pub mod intent;
pub mod manifest;
pub mod routing;
pub mod signature;
pub mod tools;
pub mod transport;

pub use address::{AddressSelector, AgentAddress};
pub use channel::{
    Channel, ChannelId, ChannelMessage, MAX_CHANNEL_ID_LEN, NotificationDecision, NotifyReason,
    Subscription, SubscriptionFilter, SubscriptionMode,
};
pub use envelope::{AgentEnvelope, DEFAULT_MAX_HOPS, DelegationBudget, PendingRequest};
pub use error::{ProtocolError, Result};
pub use intent::{
    ChannelNotifyPayload, ChannelSyncPayload, DelegatePayload, DelegationOutcome, ExecutionMetrics,
    HandshakeAckPayload, HandshakePayload, InformPayload, MessageIntent, ProgressPayload,
    QueryPayload, RejectPayload, ResolvePayload, SessionIntent, SignalKind, SignalPayload,
    SteerAction, SteerInstruction, SteerPayload, TaskAckPayload,
};
pub use manifest::AgentManifest;
pub use routing::{
    AgentPresence, AgentRouter, AllowAllPolicy, Fabric, FabricMember, FnRoutingPolicy, Mailbox,
    MailboxCore, MailboxHandle, PresenceTracker, RoutingDecision, RoutingPolicy,
};
pub type Timeline = Channel;
pub use signature::{
    AdmissionRejection, EnvelopeSignature, EnvelopeSigner, EnvelopeVerifier, HmacSigner,
    SignatureEnforcement, canonical_bytes,
};
pub use tools::{
    AcpToolContext, DelegateToPeerTool, DelegationContext, ListChannelsTool, ListPeersTool,
    OpenChannelTool, PublishToChannelTool, ReadChannelTool, SubscribeChannelTool,
    create_acp_direct_tools, create_acp_tools, register_acp_direct_tools, register_acp_tools,
};
pub use transport::{
    ChannelReceiver, ChannelSender, TransportBridge, TransportLink, TransportReceiver,
    TransportSender, attach_outbound, in_memory_pair,
};
