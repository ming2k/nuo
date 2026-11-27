//! Thread identity, lineage, and detail records.

use super::*;

/// A row in the threads picker: enough to identify, describe and order a past
/// thread without leaking the full transcript to the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadOverview {
    pub id: String,
    /// Authentic AI/manual thread title, when established (ADR-0050).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub overview: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub active: bool,
    /// The thread this one was forked from, when it is a branch rather
    /// than a trunk (`/fork`). `None` for a trunk thread
    /// (`/new` or the very first thread). Lineage is what the dashboard
    /// groups by: one trunk row per conversation, its branches nested
    /// beneath — there is always exactly one *main* line, the trunk the
    /// user is driving; branches are derived views that never replace it.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// How this thread came to exist: a user thread or a subagent run.
    #[serde(default)]
    pub origin: ThreadOrigin,
    /// Structured digest (intent + history checklist), if
    /// the thread has generated one.
    #[serde(default)]
    pub digest: Option<crate::cognitive::ThreadDigest>,
}

/// The provenance of a thread relative to its lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThreadOrigin {
    /// A user thread (`/new`, resume, or the first-ever thread).
    #[default]
    Trunk,
    /// A subagent run's own durable thread: spawned by a subagent tool call in
    /// a parent thread, never surfaced in the picker.
    Subagent,
}

/// Full detail for one thread, requested on demand (the thread-info
/// sub-view, `i` from the threads picker). Unlike [`ThreadOverview`], which
/// carries a truncated preview, this carries the *complete* last effective user
/// prompt so the info view can show it in full. Built from the same deferred
/// header parse as the picker rows (no full-transcript deserialize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ThreadDetail {
    pub id: String,
    /// Stored title (AI or manual), if any.
    pub title: Option<String>,
    /// The parent thread for a subagent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Structured digest (intent + history checklist), if
    /// the thread has generated one — the resume-time working-memory view.
    #[serde(default)]
    pub digest: Option<crate::cognitive::ThreadDigest>,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub active: bool,
    /// The complete, untruncated text of the last non-echo user prompt, or
    /// `None` when the thread holds no user prompts.
    pub last_prompt: Option<String>,
}
