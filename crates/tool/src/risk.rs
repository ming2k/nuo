use serde::{Deserialize, Serialize};

/// Risk and capability profile of a tool.
///
/// Enables policy engines and sandboxes to reason deterministically about side effects
/// without resorting to fragile string blacklists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RiskProfile {
    /// Safe, read-only inspection with no side effects on external state.
    #[default]
    ReadOnly,

    /// Mutates external state, but operations are idempotent and safely repeatable.
    IdempotentMutation,

    /// Destructive or non-reversible mutations (e.g. deletion, overwriting, wiping).
    Destructive,

    /// Communicates over external network interfaces or services.
    NetworkAccess,

    /// Executes arbitrary processes, scripts, or operating system shells.
    ArbitraryExecution,
}

impl RiskProfile {
    /// Returns true if this profile involves potential high-risk side effects.
    pub fn is_high_risk(&self) -> bool {
        matches!(self, Self::Destructive | Self::ArbitraryExecution)
    }

    /// Returns true if this tool only performs read-only operations.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}
