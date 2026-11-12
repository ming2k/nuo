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
    /// How this thread came to exist, so the dashboard can badge rows
    /// without string-matching ids: `fork` (an explicit `/fork` branch),
    /// or `trunk` (no parent).
    #[serde(default)]
    pub fork_kind: ThreadForkKind,
    /// Where this branch diverged from its parent (ADR-0120), persisted at
    /// fork time by the one fork primitive. `None` on a trunk and on branches
    /// whose provenance predates the field — never inferred from a title, a
    /// round counter, or a filtered snapshot (`[INV-LINEAGE-01]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_point: Option<ForkPoint>,
    /// Structured digest (intent + history checklist), if
    /// the thread has generated one.
    #[serde(default)]
    pub digest: Option<crate::cognitive::ThreadDigest>,
}

/// The provenance of a thread relative to its lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThreadForkKind {
    /// A root thread: `/new` or the first-ever thread. The main line.
    #[default]
    Trunk,
    /// An explicit `/fork` branch (the active pointer moved here).
    Fork,
    /// A subagent run's own durable thread: spawned by a
    /// subagent tool call in a parent thread, never surfaced in the picker.
    Subagent,
}

/// Where a branch diverged from its parent (ADR-0120).
///
/// The address is exactly the one the `/fork` grammar accepts, so the recorded
/// value round-trips through the gesture that created the branch:
///
/// ```text
/// /fork        → ForkPoint { round: <parent's round_counter>, turn: None }
/// /fork 2      → ForkPoint { round: 2, turn: None }
/// /fork 2.1    → ForkPoint { round: 2, turn: Some(1) }
/// ```
///
/// One type rather than two independent options: a `turn` cannot exist without
/// the `round` it indexes into, so the invalid combination is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkPoint {
    /// The parent's 1-indexed user round at the cut.
    pub round: u64,
    /// The 0-indexed turn inside that round; `None` when the branch diverged at
    /// the round boundary itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
}

impl ForkPoint {
    /// A cut at the conclusion of `round`.
    pub fn at_round(round: u64) -> Self {
        Self { round, turn: None }
    }

    /// A cut after turn `turn` of `round`.
    pub fn at_turn(round: u64, turn: u32) -> Self {
        Self {
            round,
            turn: Some(turn),
        }
    }

    /// The browser-facing address: `r2` or `r2.1`.
    pub fn label(&self) -> String {
        match self.turn {
            Some(turn) => format!("r{}.{}", self.round, turn),
            None => format!("r{}", self.round),
        }
    }
}

impl std::fmt::Display for ForkPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
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
    /// Lineage (ADR-0120): the thread this one was forked from and the address
    /// it diverged at, so the info sub-view can state where a branch came from
    /// without consulting the picker's row set (which may not contain its
    /// parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_point: Option<ForkPoint>,
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
