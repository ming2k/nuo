use crate::address::AgentAddress;
use crate::channel::id::ChannelId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single message recorded on a channel.
///
/// Messages are immutable and totally ordered within a channel by `seq`, which
/// is what allows any subscriber to read exactly what it missed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Monotonic sequence number within the channel, starting at 1.
    pub seq: u64,
    /// Channel this message belongs to.
    pub channel: ChannelId,
    /// Author of the message.
    pub from: AgentAddress,
    /// Message text.
    pub body: String,
    /// Publication time.
    pub at: DateTime<Utc>,
    /// Optional mention targets, used by `Mention`-mode subscriptions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<AgentAddress>,
    /// Optional thread correlation, for organizing replies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

impl ChannelMessage {
    /// Whether this message mentions `agent`.
    pub fn mentions_agent(&self, agent: &AgentAddress) -> bool {
        self.mentions.iter().any(|m| m == agent)
    }

    /// Renders the message as a single line for prompt injection.
    pub fn to_prompt_line(&self) -> String {
        let mentions = if self.mentions.is_empty() {
            String::new()
        } else {
            format!(
                " (mentions: {})",
                self.mentions
                    .iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        format!("[{}] {}{}: {}", self.seq, self.from, mentions, self.body)
    }
}

/// Rules for filtering unaddressed channel messages to claim tasks autonomously.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionFilter {
    /// Keywords or tags that trigger waking when present in message body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

impl SubscriptionFilter {
    pub fn new(keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            keywords: keywords.into_iter().map(Into::into).collect(),
        }
    }

    /// Checks whether the message body matches any of the filter keywords.
    pub fn matches(&self, message: &ChannelMessage) -> bool {
        if self.keywords.is_empty() {
            return false;
        }
        let lower = message.body.to_lowercase();
        self.keywords
            .iter()
            .any(|k| lower.contains(&k.to_lowercase()))
    }
}

/// When a subscriber wants to be woken by channel traffic.
///
/// This is the control that keeps channels from being either lossy or noisy:
/// publishing always appends to the log, but only *notification* is filtered by
/// this policy.
///
/// Policy is applied once, at publication time, by the owning room: only
/// subscribers whose mode opts in receive a notification. A reader therefore
/// never has to re-derive whether it should have been woken, which is what makes
/// the three modes observably different rather than three names for one
/// behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionMode {
    /// Wake on every message. Appropriate for low-traffic channels of record,
    /// such as `incidents`.
    All,
    /// Wake only when this agent is explicitly mentioned. The default choice for
    /// busy channels where most traffic is not addressed to you.
    MentionsOnly,
    /// Never wake; read on demand via the log or a read tool. Use for passive
    /// monitoring and audit channels.
    Manual,
    /// Wake when mentioned, OR when an unaddressed message matches the filter keywords.
    Filtered(SubscriptionFilter),
}

/// Defaults to [`SubscriptionMode::MentionsOnly`].
///
/// Deriving `Default` would have selected the first variant, `All` — "wake on
/// every message" — which is the one mode the docs warn against for busy
/// channels. A default is what an unconfigured caller gets, so it must be the
/// conservative choice.
impl Default for SubscriptionMode {
    fn default() -> Self {
        Self::MentionsOnly
    }
}

impl SubscriptionMode {
    /// Why `message` wakes this subscriber, if it does.
    ///
    /// This is the single authority on notification policy: the room calls it
    /// once per subscriber at publication time and records the answer in the
    /// notification. Nothing downstream re-derives it, so a subscriber can never
    /// disagree with the decision that actually woke it.
    ///
    /// Returned as a reason rather than a `bool` because the *why* is what the
    /// receiver needs: a mention obliges a reply, a plain subscription does not.
    pub fn notify_reason(
        &self,
        message: &ChannelMessage,
        subscriber: &AgentAddress,
    ) -> Option<NotifyReason> {
        match self {
            Self::All => Some(NotifyReason::Subscribed),
            Self::MentionsOnly if message.mentions_agent(subscriber) => {
                Some(NotifyReason::Mentioned)
            }
            Self::Filtered(filter) => {
                if message.mentions_agent(subscriber) {
                    Some(NotifyReason::Mentioned)
                } else if message.mentions.is_empty() && filter.matches(message) {
                    Some(NotifyReason::MatchedFilter)
                } else {
                    None
                }
            }
            Self::MentionsOnly | Self::Manual => None,
        }
    }

    /// Human-readable description, used in prompts and tool output.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::All => "notified on every message",
            Self::MentionsOnly => "notified only when mentioned",
            Self::Manual => "reads on demand, never notified",
            Self::Filtered(_) => {
                "notified when mentioned or when unaddressed message matches filter"
            }
        }
    }
}

/// Why a subscriber was woken.
///
/// Carried in the notification because the receiver cannot reconstruct it: by the
/// time a notification is handled, the message that caused it may have been
/// evicted by retention, and a scan of the log would be both racy and bounded.
/// Deciding once, at publication, is what makes the three subscription modes
/// observably different.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyReason {
    /// The subscriber's policy is [`SubscriptionMode::All`]: it asked to hear
    /// about every message, not only those addressed to it.
    Subscribed,
    /// The message names this subscriber explicitly, so the author is asking for
    /// its attention.
    Mentioned,
    /// The message was unaddressed and matched the subscriber's filter keywords.
    MatchedFilter,
}

impl NotifyReason {
    /// Whether this reason obliges the agent to *act*, rather than merely to be
    /// aware.
    ///
    /// A mention or matched filter is a request for attention and is worth a cognitive round. Mere
    /// subscription is not: waking on every message of a busy channel would cost
    /// one round per member per post for chatter that is usually irrelevant, so
    /// the caller is free to let it wait for the reader's next turn.
    pub fn expects_attention(self) -> bool {
        matches!(self, Self::Mentioned | Self::MatchedFilter)
    }

    /// Human-readable description, used in diagnostics and prompts.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Subscribed => "subscribed to every message",
            Self::Mentioned => "mentioned",
            Self::MatchedFilter => "matched filter",
        }
    }
}

/// A member's registration on a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// The subscribed agent.
    pub agent: AgentAddress,
    /// Notification policy.
    pub mode: SubscriptionMode,
    /// How many messages this agent has already seen.
    ///
    /// The cursor makes catch-up idempotent: reading never replays a message and
    /// never skips one.
    pub cursor: u64,
    /// When the subscription was created.
    pub joined_at: DateTime<Utc>,
}

impl Subscription {
    pub fn new(agent: AgentAddress, mode: SubscriptionMode) -> Self {
        Self {
            agent,
            mode,
            cursor: 0,
            joined_at: Utc::now(),
        }
    }

    /// Advances the cursor to cover `seq`, never moving backwards.
    pub fn advance_to(&mut self, seq: u64) {
        if seq > self.cursor {
            self.cursor = seq;
        }
    }
}

/// How a message was handled for a given subscriber, used for diagnostics and
/// for tests that assert notification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationDecision {
    /// Subscriber should be woken, for the stated reason.
    Notify(NotifyReason),
    /// Subscriber should not be woken, but the message is in its log.
    Suppress,
    /// Subscriber is not registered on this channel.
    NotSubscribed,
}

impl NotificationDecision {
    /// The reason to wake, if this is a `Notify`.
    pub fn reason(self) -> Option<NotifyReason> {
        match self {
            Self::Notify(reason) => Some(reason),
            Self::Suppress | Self::NotSubscribed => None,
        }
    }
}
