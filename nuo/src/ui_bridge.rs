//! Frontend capability abstraction for the slash-command dispatcher.
//!
//! The slash handlers live in `muta-runtime` and are frontend-agnostic, but
//! one command (`/export`) needs to interact with the user's clipboard — a
//! capability that only the running frontend possesses (the TUI uses
//! arboard/osc52; a browser frontend would use the navigator.clipboard API).
//!
//! Rather than reach into a frontend's clipboard module, the dispatcher takes a
//! `&dyn UiBridge` and calls [`UiBridge::copy_to_clipboard`]. Each frontend
//! supplies its own implementation.
//!
//! This is deliberately minimal — one method — and grows only when another
//! slash command genuinely needs a frontend-side side effect. See ADR-0037.

pub use nuo_host::clipboard::CopyOutcome;

/// Frontend-side capabilities the slash-command dispatcher needs. Implemented
/// by the TUI (real clipboard) and any future frontend.
#[async_trait::async_trait]
pub trait UiBridge: Send + Sync {
    /// Copy `text` to the user's clipboard, returning the mechanism used (or
    /// an error message). Must be non-blocking from the dispatcher's
    /// perspective — the TUI impl runs the actual copy in a background task
    /// because arboard/wl-copy can hang.
    async fn copy_to_clipboard(&self, text: &str) -> Result<CopyOutcome, String>;
}
