//! Agent extension point for projecting the next model-visible context.

use async_trait::async_trait;
use tool::Message;

/// Mid-turn model-context projection hook.
///
/// After each ReAct turn, when context pressure crosses the configured budget,
/// the agent hands the live message list to the gate and asks it to produce the
/// next model-visible window. A replacement swaps the live list; `None` leaves
/// it untouched. The implementation owns durability policy and archives
/// original content before returning a replacement.
#[async_trait]
pub trait ContextProjectionGate: Send + Sync {
    async fn project_context(&self, messages: Vec<Message>) -> Option<Vec<Message>>;

    /// Relieve a request whose **serialized body** is over its byte ceiling.
    ///
    /// `media_target_bytes` is the number of attachment bytes the request may
    /// still carry; the caller has already subtracted everything media eviction
    /// cannot touch (instructions, tool schemas, message text). `0` means no
    /// attachment fits.
    ///
    /// Returns a replacement window when it offloaded anything. The default is
    /// `None`: a gate that only knows how to fold tokens must not pretend it
    /// can answer a transport-byte verdict.
    async fn project_payload(
        &self,
        _messages: Vec<Message>,
        _media_target_bytes: usize,
    ) -> Option<Vec<Message>> {
        None
    }
}
