use crate::address::AgentAddress;
use crate::channel::id::ChannelId;
use crate::channel::message::{
    ChannelMessage, NotificationDecision, Subscription, SubscriptionMode,
};
use crate::error::{ProtocolError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Default number of messages retained per channel.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 512;

/// A named, ordered, many-to-many conversation surface.
///
/// A channel is the second of the two communication modes:
///
/// | Mode | Shape | Mechanism |
/// |------|-------|-----------|
/// | Agent-to-agent | 1:1, request/reply | `Room::request` — correlated reply |
/// | Agent-to-channel | N:M, publish/read | `Channel` — ordered log + subscriptions |
///
/// The log is the source of truth: publishing always appends, and reading is
/// cursor-based. Subscriptions control only *notification*, never visibility, so
/// a member that was busy or absent loses nothing.
#[derive(Clone)]
pub struct Channel {
    id: ChannelId,
    state: Arc<RwLock<ChannelState>>,
}

struct ChannelState {
    messages: Vec<ChannelMessage>,
    next_seq: u64,
    subscriptions: HashMap<AgentAddress, Subscription>,
    capacity: usize,
    consecutive_agent_turns: u32,
    circuit_breaker_limit: Option<u32>,
    circuit_breaker_tripped: bool,
}

impl Channel {
    /// Creates an empty channel with the default retention.
    pub fn new(id: ChannelId) -> Self {
        Self::with_capacity(id, DEFAULT_CHANNEL_CAPACITY)
    }

    /// Creates an empty channel retaining at most `capacity` messages.
    pub fn with_capacity(id: ChannelId, capacity: usize) -> Self {
        Self {
            id,
            state: Arc::new(RwLock::new(ChannelState {
                messages: Vec::new(),
                next_seq: 1,
                subscriptions: HashMap::new(),
                capacity: capacity.max(1),
                consecutive_agent_turns: 0,
                circuit_breaker_limit: None,
                circuit_breaker_tripped: false,
            })),
        }
    }

    pub fn id(&self) -> &ChannelId {
        &self.id
    }

    /// Adds or updates a subscriber, returning the resulting subscription.
    ///
    /// Crate-private, and deliberately so: a bare [`Channel`] has no directory and
    /// therefore cannot check that the subscriber exists, so exposing this would
    /// let a caller create a subscription that can be notified but can never read
    /// what it was notified about. [`Room::subscribe`](crate::routing::Room::subscribe)
    /// is the public entry point and enforces membership.
    ///
    /// Re-subscribing changes only the notification mode; the read cursor is
    /// preserved so a member never re-reads or skips messages by changing how it
    /// wants to be woken.
    pub(crate) async fn subscribe(
        &self,
        agent: AgentAddress,
        mode: SubscriptionMode,
    ) -> Subscription {
        let mut state = self.state.write().await;
        let entry = state
            .subscriptions
            .entry(agent.clone())
            .or_insert_with(|| Subscription::new(agent.clone(), mode.clone()));
        entry.mode = mode;
        entry.clone()
    }

    /// Removes a subscriber. Returns whether it was subscribed.
    pub async fn unsubscribe(&self, agent: &AgentAddress) -> bool {
        self.state
            .write()
            .await
            .subscriptions
            .remove(agent)
            .is_some()
    }

    /// Whether `agent` is subscribed.
    pub async fn is_subscribed(&self, agent: &AgentAddress) -> bool {
        self.state.read().await.subscriptions.contains_key(agent)
    }

    /// Current subscribers.
    pub async fn subscribers(&self) -> Vec<Subscription> {
        self.state
            .read()
            .await
            .subscriptions
            .values()
            .cloned()
            .collect()
    }

    /// A specific member's subscription, if any.
    pub async fn subscription_of(&self, agent: &AgentAddress) -> Option<Subscription> {
        self.state.read().await.subscriptions.get(agent).cloned()
    }

    /// Publishes a message, returning the stored record.
    ///
    /// Crate-private because publication is what triggers notification, and only
    /// the room can decide who is eligible to be notified and verify that the
    /// author is a member. [`Room::publish`](crate::routing::Room::publish) is the
    /// public entry point.
    ///
    /// Publishing never fails for lack of subscribers: the channel is a record,
    /// not a delivery guarantee to a live audience.
    pub(crate) async fn publish(
        &self,
        from: AgentAddress,
        body: impl Into<String>,
        mentions: Vec<AgentAddress>,
        thread: Option<String>,
    ) -> ChannelMessage {
        let mut state = self.state.write().await;

        let message = ChannelMessage {
            seq: state.next_seq,
            channel: self.id.clone(),
            from,
            body: body.into(),
            at: chrono::Utc::now(),
            mentions,
            thread,
        };
        state.next_seq += 1;

        state.consecutive_agent_turns += 1;
        if let Some(limit) = state.circuit_breaker_limit
            && state.consecutive_agent_turns > limit
        {
            state.circuit_breaker_tripped = true;
            tracing::warn!(
                channel = %self.id,
                consecutive = state.consecutive_agent_turns,
                limit,
                "Channel circuit breaker tripped; suppressing further automated agent turns"
            );
        }

        state.messages.push(message.clone());

        // Bound retention; sequence numbers keep advancing so a cursor pointing
        // at an evicted message correctly reads as "nothing new to replay".
        while state.messages.len() > state.capacity {
            state.messages.remove(0);
        }

        message
    }

    /// Records a message that was replicated from another host's channel.
    /// Preserves the original monotonic sequence number without generating a new one locally.
    pub async fn record_replicated(&self, message: ChannelMessage) {
        let mut state = self.state.write().await;
        if message.seq >= state.next_seq {
            state.next_seq = message.seq + 1;
        }
        state.messages.push(message);
        while state.messages.len() > state.capacity {
            state.messages.remove(0);
        }
    }

    /// Sets an anti-storm circuit breaker limit for consecutive automated agent messages.
    pub async fn set_circuit_breaker(&self, max_consecutive_turns: Option<u32>) {
        let mut state = self.state.write().await;
        state.circuit_breaker_limit = max_consecutive_turns;
    }

    /// Resets a tripped circuit breaker, allowing automated agent turns to resume.
    pub async fn reset_circuit_breaker(&self) {
        let mut state = self.state.write().await;
        state.consecutive_agent_turns = 0;
        state.circuit_breaker_tripped = false;
    }

    /// Whether the circuit breaker is currently tripped.
    pub async fn is_circuit_breaker_tripped(&self) -> bool {
        self.state.read().await.circuit_breaker_tripped
    }

    /// Decides whether `agent` should be woken for `message`, and why.
    ///
    /// Returns [`NotifyReason`](crate::channel::NotifyReason) rather than a bool
    /// so the caller can record the justification in the notification: the
    /// receiver must not have to re-derive it later against a log that may have
    /// moved on.
    pub async fn notification_for(
        &self,
        agent: &AgentAddress,
        message: &ChannelMessage,
    ) -> NotificationDecision {
        let state = self.state.read().await;
        if state.circuit_breaker_tripped {
            return NotificationDecision::Suppress;
        }
        match state.subscriptions.get(agent) {
            Some(sub) => match sub.mode.notify_reason(message, agent) {
                Some(reason) => NotificationDecision::Notify(reason),
                None => NotificationDecision::Suppress,
            },
            None => NotificationDecision::NotSubscribed,
        }
    }

    /// Messages after `cursor`, oldest first, up to `limit`.
    ///
    /// Reading is explicit and non-destructive: only
    /// [`Channel::advance_cursor`] moves a reader forward.
    pub async fn messages_after(&self, cursor: u64, limit: usize) -> Vec<ChannelMessage> {
        let state = self.state.read().await;
        state
            .messages
            .iter()
            .filter(|m| m.seq > cursor)
            .take(limit.max(1))
            .cloned()
            .collect()
    }

    /// The most recent messages, oldest first.
    pub async fn recent(&self, limit: usize) -> Vec<ChannelMessage> {
        let state = self.state.read().await;
        let skip = state.messages.len().saturating_sub(limit.max(1));
        state.messages.iter().skip(skip).cloned().collect()
    }

    /// Reads everything this subscriber has not yet seen, advancing its cursor.
    ///
    /// This is the catch-up operation: idempotent, so calling it twice in a row
    /// returns nothing the second time.
    ///
    /// At most `limit` messages are returned. When more were waiting, the rest
    /// stay unread — the cursor advances only over what was actually returned, so
    /// a caller that drains again continues where it left off. That distinction
    /// matters: advancing past messages nobody was shown would drop them
    /// permanently, since the cursor is the only record of what has been seen.
    pub async fn drain_for(
        &self,
        agent: &AgentAddress,
        limit: usize,
    ) -> Result<Vec<ChannelMessage>> {
        let from_cursor = {
            let state = self.state.read().await;
            let sub = state.subscriptions.get(agent).ok_or_else(|| {
                ProtocolError::NotSubscribed(format!(
                    "`{agent}` is not subscribed to channel `{}`",
                    self.id
                ))
            })?;
            sub.cursor
        };

        let mut state = self.state.write().await;
        let messages: Vec<ChannelMessage> = state
            .messages
            .iter()
            .filter(|m| m.seq > from_cursor)
            .take(limit.max(1))
            .cloned()
            .collect();

        if let (Some(last), Some(sub)) = (messages.last(), state.subscriptions.get_mut(agent)) {
            sub.advance_to(last.seq);
        }

        Ok(messages)
    }

    /// Advances a subscriber's cursor without returning the skipped messages.
    ///
    /// Used to mark traffic as seen without spending context on it.
    pub async fn advance_cursor(&self, agent: &AgentAddress, seq: u64) -> Result<u64> {
        let mut state = self.state.write().await;
        let sub = state.subscriptions.get_mut(agent).ok_or_else(|| {
            ProtocolError::NotSubscribed(format!(
                "`{agent}` is not subscribed to channel `{}`",
                self.id
            ))
        })?;
        sub.advance_to(seq);
        Ok(sub.cursor)
    }

    /// Highest sequence recorded, usable as a cursor meaning "only future".
    pub async fn head_seq(&self) -> u64 {
        self.state.read().await.next_seq - 1
    }

    /// Number of retained messages.
    pub async fn len(&self) -> usize {
        self.state.read().await.messages.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.state.read().await.messages.is_empty()
    }

    /// Renders recent messages as prompt text, or `None` when empty.
    pub async fn render(&self, limit: usize) -> Option<String> {
        let messages = self.recent(limit).await;
        if messages.is_empty() {
            return None;
        }
        let lines: Vec<String> = messages
            .iter()
            .map(ChannelMessage::to_prompt_line)
            .collect();
        Some(format!("#{}:\n{}", self.id, lines.join("\n")))
    }

    /// Renders only what `agent` has not seen, without advancing its cursor.
    pub async fn render_unread(&self, agent: &AgentAddress, limit: usize) -> Option<String> {
        let cursor = self.subscription_of(agent).await?.cursor;
        let messages = self.messages_after(cursor, limit).await;
        if messages.is_empty() {
            return None;
        }
        let lines: Vec<String> = messages
            .iter()
            .map(ChannelMessage::to_prompt_line)
            .collect();
        Some(format!("#{}:\n{}", self.id, lines.join("\n")))
    }
}
