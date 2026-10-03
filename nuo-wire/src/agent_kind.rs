//! Agent archetypes: Root and Subagent.
//!
//! Two agent archetypes ([`AgentKind`]) — `Root` (the driving brain) and
//! `Subagent` (the mission worker). Pure domain vocabulary, owned by the
//! contract layer (`[INV-WIRE-04]` single source of truth); relocated here from
//! `nuo-agent` per ADR-0009 to remove the `nuo-wire → nuo-agent` inversion.

use serde::{Deserialize, Serialize};

/// Archetype / classification of an agent entity.
///
/// In the homogeneous agent model (ADR-0183), an agent entity runs either in a
/// top-level root posture or a delegated child posture:
/// - [`AgentKind::Root`]: Full cognitive loop, tool execution, session/daemon orchestrator.
/// - [`AgentKind::Subagent`]: Isolated, sandboxed, short-lived task execution worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// Root agent: full cognitive loop, intent driver, conversation & tool authority.
    Root,
    /// Subagent: mission-scoped worker, isolated/sandboxed, single-task lifecycle.
    Subagent,
}

impl AgentKind {
    pub const ALL: &'static [AgentKind] = &[AgentKind::Root, AgentKind::Subagent];

    pub fn is_root(self) -> bool {
        matches!(self, Self::Root)
    }

    pub fn is_subagent(self) -> bool {
        matches!(self, Self::Subagent)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Subagent => "subagent",
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_kind_serde() {
        for kind in AgentKind::ALL {
            let s = serde_json::to_string(kind).unwrap();
            let back: AgentKind = serde_json::from_str(&s).unwrap();
            assert_eq!(back, *kind);
        }
    }
}
