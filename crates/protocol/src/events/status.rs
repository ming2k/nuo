//! Loop-status and harness-snapshot types.

use super::*;

/// Coarse, display-level status of a session's round lifecycle, mirrored to
/// the TUI activity bar. This is a badge, not the protocol state: the round
/// lifecycle itself (`RoundLifecycle` in `nuo-harness`) is binary — no active
/// round, or an active round identified by a generation.
/// Awaiting-permission / awaiting-input are overlays derived from the
/// parked-request tables, not values here: they carry
/// no lifecycle meaning (interrupt behaves identically) and there is no
/// user-level pause/resume for them to describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoopStatus {
    Idle,
    Running,
}

impl LoopStatus {
    pub fn is_idle(self) -> bool {
        matches!(self, Self::Idle)
    }

    /// The wire string, also used directly by the TUI's activity bar.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
        }
    }
}

impl std::fmt::Display for LoopStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSnapshot {
    pub loop_status: LoopStatus,
    /// Monotonic session round counter. For a running snapshot this is the
    /// admitted active round; for an idle snapshot it is the most recently
    /// admitted round. Frontends must use this instead of counting visible
    /// transcript messages, which may have been compacted.
    #[serde(default)]
    pub round_counter: u64,
    /// Whether the agent runs in unattended execution mode
    /// (`--unattended` / `/unattended on`). The TUI surfaces this as the `auto`
    /// posture token (ADR-0062 `[INV-HEAD-07]`); frontends may present it however
    /// their own vocabulary dictates.
    #[serde(default)]
    pub unattended: bool,
    /// Whether workspace filesystem confinement is enforced this session (default true).
    /// (`--no-confinement` / `/confinement off`). The TUI surfaces `false` as the
    /// `open` posture token (ADR-0062 `[INV-HEAD-07]`).
    #[serde(default = "default_confined")]
    pub confined: bool,
    /// Workspace authority is independent from the attended/unattended posture.
    /// Frontends surface this state continuously so authority is never implicit.
    #[serde(default)]
    pub workspace_security: crate::WorkspaceSecuritySnapshot,
    /// Whether a stopped round is parked for `/retry`: the previous round
    /// ended before completing (terminal provider error or an interrupt that
    /// left committed content) and its durable resume point is still armed.
    /// A round that completed naturally leaves this `false` forever — `/retry`
    /// is a no-op for it. Frontends use this to offer the `/retry` affordance
    /// instead of scanning the transcript for error notices.
    #[serde(default)]
    pub retry_pending: bool,
    /// Active staffing role for this session (e.g. "developer", "philosophist").
    #[serde(default)]
    pub role: Option<String>,
    /// Bound workspace path for this session, or None for workspace-free sessions.
    #[serde(default)]
    pub workspace: Option<String>,
}

const fn default_confined() -> bool {
    true
}
