//! Canonical ACP collaboration and channel tools for cognitive agents.
//!
//! Exposes agent-to-agent delegation (`delegate_to_peer`, `list_peers`) and
//! agent-to-channel communication (`publish_to_channel`, `read_channel`,
//! `list_channels`, `open_channel`, `subscribe_to_channel`) conforming to
//! [`nuo_tool::Tool`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nuo_tool::{RiskProfile, Tool, ToolContext, ToolError, ToolOutput, ToolScope};
use serde_json::{Value, json};

use crate::{
    AgentAddress, AgentManifest, ChannelId, DelegationBudget, Fabric, MailboxHandle,
    MessageIntent, ProtocolError, SubscriptionMode,
};

tokio::task_local! {
    /// Ambient context for the currently-executing delegated task.
    ///
    /// Set by the runtime around a delegated task and read by the collaboration
    /// tools when issuing nested delegations. Using a task-local keeps this
    /// scoped to one task, so concurrent sessions cannot leak budgets into each
    /// other, and it avoids adding protocol parameters to the cognitive loop.
    static CURRENT_DELEGATION: DelegationContext;
}

/// Who asked for the task currently being executed, and with what budget.
#[derive(Debug, Clone)]
pub struct DelegationContext {
    /// Peer that delegated the in-flight task.
    pub requester: AgentAddress,
    /// Budget granted to this request, including the hop allowance already
    /// decremented for this agent.
    pub budget: DelegationBudget,
}

impl DelegationContext {
    pub fn new(requester: AgentAddress, budget: DelegationBudget) -> Self {
        Self { requester, budget }
    }

    /// Runs `future` with this delegation in scope.
    pub async fn scope<F, T>(self, future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        CURRENT_DELEGATION.scope(self, future).await
    }

    /// The budget of the in-flight delegated task, if any.
    pub fn current_budget() -> Option<DelegationBudget> {
        CURRENT_DELEGATION.try_with(|ctx| ctx.budget).ok()
    }

    /// The peer currently awaiting our answer, if any.
    pub fn current_requester() -> Option<AgentAddress> {
        CURRENT_DELEGATION
            .try_with(|ctx| ctx.requester.clone())
            .ok()
    }
}

/// Shared wiring for ACP collaboration tools.
///
/// Holds what a tool needs to act: local address, room fabric, mailbox handle,
/// round-trip timeout, and optional direct peers.
pub struct AcpToolContext {
    pub address: AgentAddress,
    pub fabric: Option<Fabric>,
    pub handle: MailboxHandle,
    pub timeout: Duration,
    pub direct_peers: Vec<AgentManifest>,
}

impl AcpToolContext {
    pub fn new(
        address: AgentAddress,
        fabric: Option<Fabric>,
        handle: MailboxHandle,
        timeout: Duration,
        direct_peers: Vec<AgentManifest>,
    ) -> Self {
        Self {
            address,
            fabric,
            handle,
            timeout,
            direct_peers,
        }
    }

    pub fn for_fabric(
        address: AgentAddress,
        fabric: Fabric,
        handle: MailboxHandle,
        timeout: Duration,
    ) -> Self {
        Self::new(address, Some(fabric), handle, timeout, Vec::new())
    }

    pub fn for_direct_peers(
        address: AgentAddress,
        handle: MailboxHandle,
        direct_peers: Vec<AgentManifest>,
        timeout: Duration,
    ) -> Self {
        Self::new(address, None, handle, timeout, direct_peers)
    }

    async fn resolve_peer(&self, raw: &str) -> std::result::Result<AgentAddress, String> {
        let target = AgentAddress::parse(raw)
            .map_err(|err| format!("invalid peer address `{raw}`: {err}"))?;

        if target == self.address {
            return Err("refusing to delegate to self: this would recurse without progress".into());
        }

        if let Some(room) = &self.fabric {
            if room.get_member(&target).await.is_none() {
                let known = room
                    .peers_of(&self.address)
                    .await
                    .iter()
                    .map(|m| m.address.as_str().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let known = if known.is_empty() {
                    "none".to_string()
                } else {
                    known
                };
                return Err(format!(
                    "`{target}` is not a member of this room. Known peers: {known}"
                ));
            }
        } else if !self.direct_peers.iter().any(|p| p.address == target) {
            let known = self
                .direct_peers
                .iter()
                .map(|p| p.address.as_str().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let known = if known.is_empty() {
                "none".to_string()
            } else {
                known
            };
            return Err(format!(
                "`{target}` is not a registered direct peer. Known peers: {known}"
            ));
        }

        Ok(target)
    }

    fn nested_budget(&self) -> std::result::Result<Option<DelegationBudget>, String> {
        match DelegationContext::current_budget() {
            Some(budget) => budget
                .descend()
                .map(Some)
                .map_err(|err| format!("cannot delegate further: {err}. Perform the task directly instead.")),
            None => Ok(None),
        }
    }
}

/// Delegates a natural-language task to a peer agent and awaits its outcome.
pub struct DelegateToPeerTool {
    ctx: Arc<AcpToolContext>,
}

impl DelegateToPeerTool {
    pub fn new(ctx: Arc<AcpToolContext>) -> Self {
        Self { ctx }
    }

    async fn execute_internal(&self, arguments: Value) -> std::result::Result<String, String> {
        let peer_raw = arguments
            .get("peer")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required `peer`".to_string())?;
        let task = arguments
            .get("task")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required `task`".to_string())?;
        let thread_opt = arguments
            .get("thread")
            .and_then(Value::as_str)
            .map(str::to_string);

        let target = self.ctx.resolve_peer(peer_raw).await?;
        let budget = self.ctx.nested_budget()?;

        let outcome = if let Some(room) = &self.ctx.fabric {
            let mut envelope = room
                .build_delegation(&self.ctx.address, &target, task.to_string(), budget)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(t) = &thread_opt
                && let MessageIntent::Delegate(p) = &mut envelope.intent
            {
                p.thread = Some(t.clone());
            }

            let request_id = envelope.id;
            let reply_rx = self.ctx.handle.register_reply(request_id).await;

            if let Err(err) = room.dispatch(envelope).await {
                self.ctx.handle.cancel_reply(request_id).await;
                return Err(err.to_string());
            }

            match tokio::time::timeout(self.ctx.timeout, reply_rx).await {
                Ok(Ok(reply)) => reply.outcome().ok_or_else(|| {
                    ProtocolError::DelegationError(format!(
                        "peer `{target}` replied with a non-terminal intent"
                    ))
                }),
                Ok(Err(_)) => Err(ProtocolError::DelegationError(format!(
                    "reply channel for request {request_id} to `{target}` was closed"
                ))),
                Err(_) => {
                    self.ctx.handle.cancel_reply(request_id).await;
                    Err(ProtocolError::RequestTimeout(
                        self.ctx.timeout.as_millis() as u64,
                    ))
                }
            }
        } else {
            let mut intent = MessageIntent::delegate(task);
            if let (Some(t), MessageIntent::Delegate(p)) = (&thread_opt, &mut intent) {
                p.thread = Some(t.clone());
            }

            let mut envelope =
                crate::AgentEnvelope::new(self.ctx.address.clone(), target.clone(), intent);
            if let Some(b) = budget {
                envelope = envelope.with_budget(b);
            }

            let request_id = envelope.id;
            let reply_rx = self.ctx.handle.register_reply(request_id).await;

            if let Err(err) = self.ctx.handle.send(envelope).await {
                self.ctx.handle.cancel_reply(request_id).await;
                return Err(err.to_string());
            }

            match tokio::time::timeout(self.ctx.timeout, reply_rx).await {
                Ok(Ok(reply)) => reply.outcome().ok_or_else(|| {
                    ProtocolError::DelegationError(format!(
                        "peer `{target}` replied with a non-terminal intent"
                    ))
                }),
                Ok(Err(_)) => Err(ProtocolError::DelegationError(format!(
                    "reply channel for request {request_id} to `{target}` was closed"
                ))),
                Err(_) => {
                    self.ctx.handle.cancel_reply(request_id).await;
                    Err(ProtocolError::RequestTimeout(
                        self.ctx.timeout.as_millis() as u64,
                    ))
                }
            }
        }
        .map_err(|err| format!("delegation to `{target}` failed: {err}"))?;

        if outcome.is_resolved() {
            let text = outcome.to_natural_language();
            if let Some(sid) = outcome.session_id() {
                Ok(format!("{text}\n\n[Session thread: '{sid}']"))
            } else {
                Ok(text)
            }
        } else {
            Err(format!(
                "peer `{target}` did not complete the task: {}",
                outcome.to_natural_language()
            ))
        }
    }
}

#[async_trait]
impl Tool for DelegateToPeerTool {
    fn name(&self) -> &str {
        "delegate_to_peer"
    }

    fn description(&self) -> &str {
        "Delegates a task or question to another agent in this room, then waits for its reply. \
         Prefer this over doing the work yourself when a peer's described specialty covers the \
         request (for example issue tracking, research, or deployment). The task must be phrased \
         in natural language and must be self-contained: the peer does not share your conversation."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "peer": {
                    "type": "string",
                    "description": "Address of the peer agent, e.g. `agent://local/kanban`. Use `list_peers` to see available peers."
                },
                "task": {
                    "type": "string",
                    "description": "Self-contained natural language instructions for the peer, including any context it needs."
                },
                "thread": {
                    "type": "string",
                    "description": "Optional thread/session ID to continue an ongoing conversation with this peer. Omit to start fresh."
                }
            },
            "required": ["peer", "task"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::NetworkAccess
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Collaboration]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        match self.execute_internal(arguments).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Err(ToolError::execution(self.name(), err)),
        }
    }
}

/// Lists the peer agents currently reachable in the room.
pub struct ListPeersTool {
    ctx: Arc<AcpToolContext>,
}

impl ListPeersTool {
    pub fn new(ctx: Arc<AcpToolContext>) -> Self {
        Self { ctx }
    }

    async fn execute_internal(&self, _arguments: Value) -> std::result::Result<String, String> {
        let peers: Vec<AgentManifest> = if let Some(room) = &self.ctx.fabric {
            room.peers_of(&self.ctx.address).await
        } else {
            self.ctx.direct_peers.clone()
        };
        if peers.is_empty() {
            return Ok("No peer agents are currently available.".into());
        }

        let mut lines = vec![format!("{} peer agent(s) available:", peers.len())];
        for manifest in peers {
            lines.push(manifest.to_prompt_summary());
            if !manifest.skills.is_empty() {
                lines.push(format!("  skills: {}", manifest.skills.join(", ")));
            }
        }
        Ok(lines.join("\n"))
    }
}

#[async_trait]
impl Tool for ListPeersTool {
    fn name(&self) -> &str {
        "list_peers"
    }

    fn description(&self) -> &str {
        "Lists the other agents currently in this room and what each one specializes in. \
         Call this before delegating if you are unsure which peer handles a task."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Collaboration]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        match self.execute_internal(arguments).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Err(ToolError::execution(self.name(), err)),
        }
    }
}

/// Publishes a message to a named channel.
pub struct PublishToChannelTool {
    ctx: Arc<AcpToolContext>,
}

impl PublishToChannelTool {
    pub fn new(ctx: Arc<AcpToolContext>) -> Self {
        Self { ctx }
    }

    async fn execute_internal(&self, arguments: Value) -> std::result::Result<String, String> {
        let raw_channel = arguments
            .get("channel")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required `channel`".to_string())?;
        let message = arguments
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required `message`".to_string())?;

        let channel_id = ChannelId::new(raw_channel)
            .map_err(|err| format!("invalid channel `{raw_channel}`: {err}"))?;

        let mut mentions = Vec::new();
        if let Some(list) = arguments.get("mention").and_then(Value::as_array) {
            for entry in list {
                let raw = entry
                    .as_str()
                    .ok_or_else(|| "`mention` entries must be strings".to_string())?;
                mentions.push(
                    AgentAddress::parse(raw)
                        .map_err(|err| format!("invalid mention `{raw}`: {err}"))?,
                );
            }
        }

        let room = self.ctx.fabric.as_ref().ok_or_else(|| {
            "publishing to a channel requires room membership".to_string()
        })?;

        let (stored, notified) = room
            .publish(&self.ctx.address, &channel_id, message, mentions)
            .await
            .map_err(|err| format!("publish to `{channel_id}` failed: {err}"))?;

        Ok(format!(
            "Posted to #{channel_id} as message {}; {} subscriber(s) notified.",
            stored.seq,
            notified.len()
        ))
    }
}

#[async_trait]
impl Tool for PublishToChannelTool {
    fn name(&self) -> &str {
        "publish_to_channel"
    }

    fn description(&self) -> &str {
        "Posts a message to a named channel, where it is recorded for all subscribers. \
         Use this to share progress or information with a group. Unlike `delegate_to_peer`, \
         publishing expects no reply and does not wait: it is how you keep several agents \
         informed at once. Mention a peer by address to make sure they are notified."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel to publish to, e.g. `ops` or `dev/frontend`."
                },
                "message": {
                    "type": "string",
                    "description": "The message body to post."
                },
                "mention": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional peer addresses to explicitly notify."
                }
            },
            "required": ["channel", "message"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::IdempotentMutation
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Collaboration]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        match self.execute_internal(arguments).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Err(ToolError::execution(self.name(), err)),
        }
    }
}

/// Reads recent messages from a channel.
pub struct ReadChannelTool {
    ctx: Arc<AcpToolContext>,
}

impl ReadChannelTool {
    pub fn new(ctx: Arc<AcpToolContext>) -> Self {
        Self { ctx }
    }

    async fn execute_internal(&self, arguments: Value) -> std::result::Result<String, String> {
        let raw_channel = arguments
            .get("channel")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required `channel`".to_string())?;
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;

        let channel_id = ChannelId::new(raw_channel)
            .map_err(|err| format!("invalid channel `{raw_channel}`: {err}"))?;

        let room = self.ctx.fabric.as_ref().ok_or_else(|| {
            "reading a channel requires room membership".to_string()
        })?;

        let channel = room.channel(&channel_id).await.ok_or_else(|| {
            format!("channel `{channel_id}` does not exist in this room")
        })?;

        let messages = channel.recent(limit).await;
        if messages.is_empty() {
            return Ok(format!("#{channel_id} has no messages yet."));
        }

        let lines: Vec<String> = messages.iter().map(|m| m.to_prompt_line()).collect();
        Ok(format!(
            "#{channel_id} ({} message(s)):\n{}",
            messages.len(),
            lines.join("\n")
        ))
    }
}

#[async_trait]
impl Tool for ReadChannelTool {
    fn name(&self) -> &str {
        "read_channel"
    }

    fn description(&self) -> &str {
        "Reads recent messages from a channel, including anything posted while you were \
         busy. Use this to catch up on a discussion before responding or delegating."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel to read, e.g. `ops`."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum messages to return, newest last. Defaults to 20."
                }
            },
            "required": ["channel"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Collaboration]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        match self.execute_internal(arguments).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Err(ToolError::execution(self.name(), err)),
        }
    }
}

/// Lists the channels available in this room.
pub struct ListChannelsTool {
    ctx: Arc<AcpToolContext>,
}

impl ListChannelsTool {
    pub fn new(ctx: Arc<AcpToolContext>) -> Self {
        Self { ctx }
    }

    async fn execute_internal(&self, _arguments: Value) -> std::result::Result<String, String> {
        let room = self.ctx.fabric.as_ref().ok_or_else(|| {
            "listing channels requires room membership".to_string()
        })?;

        let channels = room.channels().await;
        if channels.is_empty() {
            return Ok("No channels are open in this room.".into());
        }

        let mut lines = vec![format!("{} channel(s):", channels.len())];
        for channel in channels {
            let count = channel.len().await;
            let subscription = channel.subscription_of(&self.ctx.address).await;
            let status = match subscription {
                Some(sub) => format!("subscribed, {}", sub.mode.describe()),
                None => "not subscribed".to_string(),
            };
            lines.push(format!(
                "#{} — {} message(s), {}",
                channel.id(),
                count,
                status
            ));
        }
        Ok(lines.join("\n"))
    }
}

#[async_trait]
impl Tool for ListChannelsTool {
    fn name(&self) -> &str {
        "list_channels"
    }

    fn description(&self) -> &str {
        "Lists the channels in this room, showing how many messages each holds and whether \
         you are subscribed. Call this to find where a discussion is happening."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Collaboration]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        match self.execute_internal(arguments).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Err(ToolError::execution(self.name(), err)),
        }
    }
}

/// Creates a channel in this room.
pub struct OpenChannelTool {
    ctx: Arc<AcpToolContext>,
}

impl OpenChannelTool {
    pub fn new(ctx: Arc<AcpToolContext>) -> Self {
        Self { ctx }
    }

    async fn execute_internal(&self, arguments: Value) -> std::result::Result<String, String> {
        let raw = arguments
            .get("channel")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required `channel`".to_string())?;

        let channel_id = ChannelId::new(raw)
            .map_err(|err| format!("invalid channel `{raw}`: {err}"))?;

        let room = self.ctx.fabric.as_ref().ok_or_else(|| {
            "opening a channel requires room membership".to_string()
        })?;

        let existed = room.channel(&channel_id).await.is_some();
        room.open_channel(channel_id.clone()).await;

        Ok(if existed {
            format!("#{channel_id} already exists.")
        } else {
            format!("Opened #{channel_id}.")
        })
    }
}

#[async_trait]
impl Tool for OpenChannelTool {
    fn name(&self) -> &str {
        "open_channel"
    }

    fn description(&self) -> &str {
        "Creates a channel for a group discussion that does not exist yet. Opening an \
         existing channel is harmless. Channel names are lowercase, may use `-`, `_` and \
         `/` for hierarchy, and must start with a letter or digit."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel name, e.g. `incidents` or `dev/frontend`."
                }
            },
            "required": ["channel"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::IdempotentMutation
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Collaboration]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        match self.execute_internal(arguments).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Err(ToolError::execution(self.name(), err)),
        }
    }
}

/// Subscribes this agent to a channel.
pub struct SubscribeChannelTool {
    ctx: Arc<AcpToolContext>,
}

impl SubscribeChannelTool {
    pub fn new(ctx: Arc<AcpToolContext>) -> Self {
        Self { ctx }
    }

    async fn execute_internal(&self, arguments: Value) -> std::result::Result<String, String> {
        let raw = arguments
            .get("channel")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required `channel`".to_string())?;

        let mode = match arguments.get("mode").and_then(Value::as_str) {
            None => SubscriptionMode::default(),
            Some("all") => SubscriptionMode::All,
            Some("mentions_only") => SubscriptionMode::MentionsOnly,
            Some("manual") => SubscriptionMode::Manual,
            Some("filtered") => {
                let keywords: Vec<String> = arguments
                    .get("keywords")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                SubscriptionMode::Filtered(crate::SubscriptionFilter::new(keywords))
            }
            Some(other) => {
                return Err(format!(
                    "unknown mode `{other}`; expected all, mentions_only, manual, or filtered"
                ));
            }
        };

        let channel_id = ChannelId::new(raw)
            .map_err(|err| format!("invalid channel `{raw}`: {err}"))?;

        let room = self.ctx.fabric.as_ref().ok_or_else(|| {
            "subscribing to a channel requires room membership".to_string()
        })?;

        let subscription = room
            .subscribe(&self.ctx.address, &channel_id, mode)
            .await
            .map_err(|err| format!("cannot subscribe to `{channel_id}`: {err}"))?;

        Ok(format!(
            "Subscribed to #{channel_id}: {}.",
            subscription.mode.describe()
        ))
    }
}

#[async_trait]
impl Tool for SubscribeChannelTool {
    fn name(&self) -> &str {
        "subscribe_to_channel"
    }

    fn description(&self) -> &str {
        "Subscribes to a channel so its traffic reaches you. \
         `mode` controls how often you are woken: \
         `all` (every message — use sparingly), \
         `mentions_only` (only when you are mentioned — the usual choice), \
         `manual` (never woken, read on demand via `read_channel`)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel to subscribe to."
                },
                "mode": {
                    "type": "string",
                    "enum": ["all", "mentions_only", "manual"],
                    "description": "Notification policy. Defaults to `mentions_only`."
                }
            },
            "required": ["channel"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::IdempotentMutation
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Collaboration]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        match self.execute_internal(arguments).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Err(ToolError::execution(self.name(), err)),
        }
    }
}

/// Creates the full set of ACP collaboration tools (agent-to-agent and agent-to-channel).
pub fn create_acp_tools(ctx: Arc<AcpToolContext>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(DelegateToPeerTool::new(ctx.clone())),
        Arc::new(ListPeersTool::new(ctx.clone())),
        Arc::new(PublishToChannelTool::new(ctx.clone())),
        Arc::new(ReadChannelTool::new(ctx.clone())),
        Arc::new(ListChannelsTool::new(ctx.clone())),
        Arc::new(OpenChannelTool::new(ctx.clone())),
        Arc::new(SubscribeChannelTool::new(ctx)),
    ]
}

/// Creates the direct 1:1 agent-to-agent delegation tools only (excluding channels).
pub fn create_acp_direct_tools(ctx: Arc<AcpToolContext>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(DelegateToPeerTool::new(ctx.clone())),
        Arc::new(ListPeersTool::new(ctx)),
    ]
}

/// Registers the full set of ACP collaboration tools into a ToolRegistry.
pub fn register_acp_tools(registry: &mut nuo_tool::ToolRegistry, ctx: Arc<AcpToolContext>) {
    for tool in create_acp_tools(ctx) {
        registry.register_arc(tool);
    }
}

/// Registers direct delegation tools into a ToolRegistry.
pub fn register_acp_direct_tools(registry: &mut nuo_tool::ToolRegistry, ctx: Arc<AcpToolContext>) {
    for tool in create_acp_direct_tools(ctx) {
        registry.register_arc(tool);
    }
}
