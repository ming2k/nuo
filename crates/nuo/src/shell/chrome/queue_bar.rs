//! The queue/outbox **projection types** consumed by the consolidated
//! activity strip and the queue modal.
//!
//! There used to be a standalone one-row queue bar renderer here
//! (`draw_queue_bar`) with its own keycap legend and preview logic. It had no
//! production caller: ADR-0062 `[INV-FOOTER-02]` merged the queue summary into
//! the single-row `ActivityStrip`, so a second renderer would have been a
//! parallel implementation of the same row — the exact drift the invariant
//! exists to prevent. The renderer is deleted; these types remain because they
//! are the *data* the live surfaces consume.
//!
//! Only the fields a surface actually renders live here. A projection field
//! nobody reads is worse than a missing one: it looks like the row states
//! something it does not. The old `paused` was exactly that — it was `true`
//! whenever any item merely existed, so it carried no information and was read
//! by nobody; it is gone. What the row does state is `blocked`, which is a real
//! condition (the driver is holding the queue).
/// One queued outbox item projected for the [`QueueBarProps`] / queue modal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueItemProps {
    /// When the item was staged, or `None` when the peer did not report one.
    /// Optional rather than defaulted: a fabricated epoch stamp would render as
    /// a real wall-clock (`08:00` in UTC+8), which is a lie about when the user
    /// queued the message.
    pub queued_at_ms: Option<u64>,
    pub text: String,
    /// The item is a pending steer (in-round course correction), not a
    /// follow-up. Surfaces prefix it `steer` and cancel it via `CancelSteer`
    /// instead of treating it as next-round dialogue (ADR-0137).
    pub is_steer: bool,
}

/// Inputs for the outbox summary the activity strip and the queue modal render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueBarProps<'a> {
    pub items: &'a [QueueItemProps],
    /// The viewed session's queue is paused: the driver ships nothing until it
    /// is resumed. Rendered by the activity strip, which is the row that can
    /// say *why* the queue is not moving.
    pub blocked: bool,
    /// The chord that actually opens the queue panel, resolved from the
    /// command registry. `None` — the command was unbound or the
    /// user remapped it away — renders no keycap: the bar advertises the
    /// binding that fires, and nothing when none does.
    pub expand_key: Option<crate::shell::keymap::Key>,
}
