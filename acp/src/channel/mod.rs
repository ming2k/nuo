//! Channel standard: the many-to-many communication surface.
//!
//! Two communication modes exist in this protocol and they are deliberately
//! distinct:
//!
//! | Mode | Shape | Mechanism |
//! |------|-------|-----------|
//! | Agent-to-agent | 1:1 request/reply | `Room::request` — correlated reply |
//! | Agent-to-channel | N:M publish/read | [`Channel`] — ordered log + subscriptions |
//!
//! Conflating them is what makes multi-agent systems either lossy or noisy. An
//! agent-to-agent call needs *this* answer delivered to *this* caller; a channel
//! needs an ordered record that any number of readers can consume at their own
//! pace, without the author knowing or caring who is listening.

pub mod id;
pub mod message;
pub mod surface;

pub use id::{ChannelId, MAX_CHANNEL_ID_LEN};
pub use message::{
    ChannelMessage, NotificationDecision, NotifyReason, Subscription, SubscriptionFilter,
    SubscriptionMode,
};
pub use surface::{Channel, DEFAULT_CHANNEL_CAPACITY};
