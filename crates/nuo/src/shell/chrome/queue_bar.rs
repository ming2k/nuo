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
/// One queued outbox item projected for the [`QueueBarProps`] / queue modal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueItemProps {
    pub queued_at_ms: u64,
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
    pub paused: bool,
    pub blocked: bool,
    /// The chord that actually opens the queue panel, resolved from the
    /// command registry. `None` — the command was unbound or the
    /// user remapped it away — renders no keycap: the bar advertises the
    /// binding that fires, and nothing when none does.
    pub expand_key: Option<crate::shell::keymap::Key>,
}
