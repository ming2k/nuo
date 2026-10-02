//! Built-in agent-to-agent and agent-to-channel collaboration capabilities.
//!
//! This module is the *only* place where the agent runtime touches the
//! multi-agent protocol. It is wired in exactly when an agent is configured
//! with a room, and contributes nothing otherwise — an agent without peers has
//! no collaboration tools, no peer directory, and no protocol surface in its
//! prompt.
//!
//! Purity is maintained by construction, not by convention:
//!
//! - Collaboration reaches the model as **ordinary tools** with ordinary JSON
//!   schemas. The cognitive loop has no protocol branches, so it cannot behave
//!   differently depending on whether peers exist.
//! - Capability and advertisement are kept in lockstep: tools are registered
//!   for precisely the peers that exist, and the prompt sections are rendered
//!   from the same membership snapshot.
//! - Delegation depth is bounded by the protocol's hop budget, so agents cannot
//!   ping-pong tasks indefinitely.
//!
//! # Two modes
//!
//! The tool set mirrors the protocol's two communication shapes rather than
//! blending them:
//!
//! | Mode | Tools | Shape |
//! |------|-------|-------|
//! | Agent-to-agent | [`DelegateToPeerTool`], [`ListPeersTool`] | 1:1, awaits an answer |
//! | Agent-to-channel | [`PublishToChannelTool`] and friends | N:M, expects no reply |
//!
//! Keeping them distinct is what prevents the common failure where an agent
//! "broadcasts" a task and then waits forever for an answer, or conversely
//! delegates a status update nobody asked for.

pub mod context;
pub mod tools;

use crate::tools::ToolRegistry;
use acp::{AgentAddress, Fabric, MailboxHandle};
use std::sync::Arc;
use std::time::Duration;

pub use context::DelegationContext;
pub use tools::{
    DelegateToPeerTool, ListChannelsTool, ListPeersTool, OpenChannelTool, PublishToChannelTool,
    ReadChannelTool, SubscribeChannelTool,
};

/// Tools contributed by collaboration.
///
/// Two families, matching the two communication modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollaborationTool {
    // Agent-to-agent: 1:1, awaits an answer.
    DelegateToPeer,
    ListPeers,
    // Agent-to-channel: N:M, fire-and-record.
    PublishToChannel,
    ReadChannel,
    ListChannels,
    OpenChannel,
    SubscribeChannel,
}

impl CollaborationTool {
    /// Every collaboration tool, used to assert advertisement parity.
    pub const ALL: [CollaborationTool; 7] = [
        CollaborationTool::DelegateToPeer,
        CollaborationTool::ListPeers,
        CollaborationTool::PublishToChannel,
        CollaborationTool::ReadChannel,
        CollaborationTool::ListChannels,
        CollaborationTool::OpenChannel,
        CollaborationTool::SubscribeChannel,
    ];

    /// The agent-to-agent subset.
    pub const DIRECT: [CollaborationTool; 2] = [
        CollaborationTool::DelegateToPeer,
        CollaborationTool::ListPeers,
    ];

    /// The agent-to-channel subset.
    pub const CHANNEL: [CollaborationTool; 5] = [
        CollaborationTool::PublishToChannel,
        CollaborationTool::ReadChannel,
        CollaborationTool::ListChannels,
        CollaborationTool::OpenChannel,
        CollaborationTool::SubscribeChannel,
    ];

    /// Tool name as exposed to the model.
    pub fn name(self) -> &'static str {
        match self {
            Self::DelegateToPeer => "delegate_to_peer",
            Self::ListPeers => "list_peers",
            Self::PublishToChannel => "publish_to_channel",
            Self::ReadChannel => "read_channel",
            Self::ListChannels => "list_channels",
            Self::OpenChannel => "open_channel",
            Self::SubscribeChannel => "subscribe_to_channel",
        }
    }

    /// Which mode this tool belongs to, for prompts and diagnostics.
    pub fn mode(self) -> CollaborationMode {
        match self {
            Self::DelegateToPeer | Self::ListPeers => CollaborationMode::AgentToAgent,
            _ => CollaborationMode::AgentToChannel,
        }
    }
}

/// The two communication modes collaboration exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollaborationMode {
    /// 1:1 request/reply, awaiting a specific peer's answer.
    AgentToAgent,
    /// N:M publish/read over a named channel.
    AgentToChannel,
}

impl CollaborationMode {
    pub fn describe(self) -> &'static str {
        match self {
            Self::AgentToAgent => "delegate a task to one peer and await its answer",
            Self::AgentToChannel => "publish to and read from named channels",
        }
    }
}

/// Collaboration capabilities installed on an agent, produced by
/// [`install_collaboration_tools`].
#[derive(Clone)]
pub struct Collaboration {
    fabric: Option<Fabric>,
    address: AgentAddress,
    handle: MailboxHandle,
    direct_peers: Vec<acp::AgentManifest>,
}

impl Collaboration {
    /// Address this collaboration is bound to.
    pub fn address(&self) -> &AgentAddress {
        &self.address
    }

    /// The room this agent participates in, if joined to one.
    pub fn fabric(&self) -> Option<&Fabric> {
        self.fabric.as_ref()
    }

    /// Mailbox handle for sending envelopes outside the tool interface.
    pub fn mailbox(&self) -> &MailboxHandle {
        &self.handle
    }

    /// Peer directory rendered for this agent, or `None` when it has no peers.
    pub async fn peer_directory(&self) -> Option<String> {
        if let Some(room) = &self.fabric {
            room.render_peer_directory(&self.address).await
        } else if !self.direct_peers.is_empty() {
            let lines: Vec<String> = self
                .direct_peers
                .iter()
                .map(|p| format!("- {}: {} — {}", p.address, p.name, p.description))
                .collect();
            Some(lines.join("\n"))
        } else {
            None
        }
    }

    /// Whether any peer is currently reachable.
    pub async fn has_peers(&self) -> bool {
        if let Some(room) = &self.fabric {
            !room.peers_of(&self.address).await.is_empty()
        } else {
            !self.direct_peers.is_empty()
        }
    }

    /// Unread channel traffic relevant to a specific conversation.
    ///
    /// Scoped deliberately: a channel conversation sees that channel's unread
    /// traffic, while a 1:1 conversation sees none. Injecting group chatter into
    /// private exchanges would contaminate them; duplicating it into every 1:1
    /// session would pay the same token cost repeatedly for context that belongs
    /// to one surface.
    pub async fn unread_channels_for(
        &self,
        key: &crate::session::SessionKey,
        per_channel_limit: usize,
    ) -> Option<String> {
        let room = self.fabric.as_ref()?;
        match key.channel() {
            Some(channel_id) => {
                let channel = room.channel(channel_id).await?;
                channel
                    .render_unread(&self.address, per_channel_limit)
                    .await
            }
            None => None,
        }
    }

    /// Unread traffic across every subscribed channel.
    ///
    /// Use when an agent is reasoning outside any single conversation, such as
    /// during startup or a scheduled sweep.
    pub async fn unread_channels(&self, per_channel_limit: usize) -> Option<String> {
        let room = self.fabric.as_ref()?;
        let subscriptions = room.subscriptions_of(&self.address).await;
        let mut sections = Vec::new();
        for (channel, _) in subscriptions {
            if let Some(rendered) = channel
                .render_unread(&self.address, per_channel_limit)
                .await
            {
                sections.push(rendered);
            }
        }
        if sections.is_empty() {
            return None;
        }
        Some(sections.join("\n\n"))
    }

    /// Whether this agent is subscribed to a channel.
    pub async fn is_subscribed_to(&self, channel: &acp::ChannelId) -> bool {
        let Some(room) = &self.fabric else {
            return false;
        };
        match room.channel(channel).await {
            Some(channel) => channel.is_subscribed(&self.address).await,
            None => false,
        }
    }

    /// Channels this agent is subscribed to, with its policy and cursor.
    pub async fn subscriptions(&self) -> Vec<(String, String, u64)> {
        let Some(room) = &self.fabric else {
            return Vec::new();
        };
        room.subscriptions_of(&self.address)
            .await
            .into_iter()
            .map(|(channel, sub)| {
                (
                    channel.id().to_string(),
                    sub.mode.describe().to_string(),
                    sub.cursor,
                )
            })
            .collect()
    }
}

/// Registers the built-in collaboration tools for a room, returning the binding handle.
///
/// `timeout` bounds a single delegation round trip; channel operations are local
/// and need no timeout.
pub fn install_collaboration_tools(
    tools: &mut ToolRegistry,
    address: AgentAddress,
    fabric: Fabric,
    handle: MailboxHandle,
    timeout: Duration,
) -> Collaboration {
    let ctx = Arc::new(acp::AcpToolContext::for_fabric(
        address.clone(),
        fabric.clone(),
        handle.clone(),
        timeout,
    ));

    acp::register_acp_tools(tools, ctx);

    Collaboration {
        fabric: Some(fabric),
        address,
        handle,
        direct_peers: Vec::new(),
    }
}

/// Registers direct 1:1 agent-to-agent delegation tools without channel or room tools.
pub fn install_direct_delegation_tools(
    tools: &mut ToolRegistry,
    address: AgentAddress,
    handle: MailboxHandle,
    direct_peers: Vec<acp::AgentManifest>,
    timeout: Duration,
) -> Collaboration {
    let ctx = Arc::new(acp::AcpToolContext::for_direct_peers(
        address.clone(),
        handle.clone(),
        direct_peers.clone(),
        timeout,
    ));

    acp::register_acp_direct_tools(tools, ctx);

    Collaboration {
        fabric: None,
        address,
        handle,
        direct_peers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_unique_and_stable() {
        let mut names: Vec<&str> = CollaborationTool::ALL.iter().map(|t| t.name()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            count,
            "collaboration tool names must be unique"
        );

        // Names are part of the model-facing contract and must not drift.
        for expected in [
            "delegate_to_peer",
            "list_peers",
            "publish_to_channel",
            "read_channel",
            "list_channels",
            "open_channel",
            "subscribe_to_channel",
        ] {
            assert!(names.contains(&expected), "missing tool `{expected}`");
        }
    }

    #[test]
    fn everyone_belongs_to_exactly_one_mode() {
        let mut direct = 0;
        let mut channel = 0;
        for tool in CollaborationTool::ALL {
            match tool.mode() {
                CollaborationMode::AgentToAgent => direct += 1,
                CollaborationMode::AgentToChannel => channel += 1,
            }
        }

        assert_eq!(direct, CollaborationTool::DIRECT.len());
        assert_eq!(channel, CollaborationTool::CHANNEL.len());
        assert_eq!(direct + channel, CollaborationTool::ALL.len());
    }
}
