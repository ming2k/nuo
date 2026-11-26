//! Agent extension point for projecting the next model-visible context.

use async_trait::async_trait;
use tool::Message;

/// Result of a mid-turn model-context projection.
///
/// `messages` replaces the live model window; `committed_len` is the exact
/// prefix length of `messages` that is already durably persisted in the thread
/// store (everything at `messages[committed_len..]` is still uncommitted and
/// will be appended by the next turn commit). When Tier-2 compaction ran,
/// `compacted` carries its checkpoint so the live client renders the card at
/// the exact turn seam (`[INV-CMP-10]`).
#[derive(Debug, Clone)]
pub struct ContextProjectionOutcome {
    pub messages: Vec<Message>,
    pub committed_len: usize,
    pub compacted: Option<protocol::ContextProjectionCheckpoint>,
}

/// Mid-turn model-context projection hook.
///
/// After each ReAct turn, when context pressure crosses the configured budget,
/// the agent hands the live message list and its committed prefix length to
/// the gate and asks it to produce the next model-visible window. A
/// replacement swaps the live list and updates the committed cursor; `None`
/// leaves both untouched. Projections are disposable request views and never
/// cause a subsequent turn commit to rewrite durable history
/// (`[INV-THREADSSOT-02]`, `[INV-CMP-08]`).
#[async_trait]
pub trait ContextProjectionGate: Send + Sync {
    async fn project_context(
        &self,
        messages: Vec<Message>,
        committed_len: usize,
    ) -> Option<ContextProjectionOutcome>;

    /// Relieve a request whose **serialized body** is over its byte ceiling.
    ///
    /// `media_target_bytes` is the number of attachment bytes the request may
    /// still carry; the caller has already subtracted everything media eviction
    /// cannot touch (instructions, tool schemas, message text). `0` means no
    /// attachment fits.
    ///
    /// Returns a replacement window when it offloaded anything.
    ///
    /// **Contract**: payload relief modifies attachments strictly **in place**
    /// and MUST preserve `messages.len()` so the caller's `committed_len`
    /// cursor remains aligned with the live window. A replacement whose length
    /// differs is rejected by the caller rather than desynchronizing delta
    /// commits.
    ///
    /// The default is `None`: a gate that only knows how to fold tokens must
    /// not pretend it can answer a transport-byte verdict.
    async fn project_payload(
        &self,
        _messages: Vec<Message>,
        _media_target_bytes: usize,
    ) -> Option<Vec<Message>> {
        None
    }
}
