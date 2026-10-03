//! Agent identity: who an `Agent` (re-exported by the agent
//! crate) is and what it is for. Pure domain vocabulary — three strings and a
//! formatter — with no agent-layer dependencies, so it lives in core alongside
//! the role vocabulary (`AgentRoleProfile` in `agent_role.rs`).
//!
//! Kept identity-agnostic: nothing here hardcodes "muta" or "coding". The
//! embedding (a CLI, a server) supplies the fields, so the same engine can be
//! repurposed as a different persona or for a different mission (research, ops,
//! writing) by passing different values.

use serde::{Deserialize, Serialize};

/// Who an agent is and what it is for. Identity-agnostic: it does not hardcode
/// "muta" or "coding". The embedding (the CLI, a future frontend) supplies
/// the fields so the same engine can be repurposed as a different persona or
/// for a different mission (research, ops, writing) by passing different
/// values. Everything else in the system prompt (tone, todo/ask_user guidance)
/// is mission-neutral and stays in the agent crate.
///
/// Supplying an identity is optional: the shipped coding CLI supplies none, so
/// its prompt opens at the host environment. Nothing in the harness reads the
/// model's self-name — an identity line earns its tokens only where it changes
/// behaviour (a subagent's task prompt, a `/role` directive).
///
/// The three fields compose the opening line:
/// - [`AgentIdentity::name`] — what the agent is called (e.g. `"hypervisor"`
///   for the daemon's coordinator). Empty means "unnamed".
/// - [`AgentIdentity::mission`] — what the agent is for (e.g. a research
///   frontend's mission; empty means no mission framing).
/// - [`AgentIdentity::directive`] — optional full-text override of the opening.
///   When set, [`AgentIdentity::preamble`] returns it verbatim and ignores
///   `name`/`mission`. Subagents use this to inject their role's full task
///   prompt as the identity; focused roles use it for their imperative role
///   directive.
///
/// [`AgentIdentity::default`] yields empty fields (no preamble — the system
/// prompt opens straight at the host-environment section); tests and the
/// shipped CLI use it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// What this agent is called, e.g. `"hypervisor"`. Empty means "unnamed" —
    /// the preamble then opens with the mission alone.
    pub name: String,
    /// What this agent is for, e.g. `"a meticulous research assistant"`. Empty
    /// means "no mission framing".
    pub mission: String,
    /// Optional full-text identity directive override. When non-empty, `preamble`
    /// returns this verbatim (used by subagents whose identity *is* their
    /// role's full task prompt). None/empty → compose from name + mission.
    pub directive: Option<String>,
}

impl AgentIdentity {
    /// Build a structured identity from a name and a mission.
    pub fn new(name: impl Into<String>, mission: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mission: mission.into(),
            directive: None,
        }
    }

    /// Build an identity from a mission alone, leaving the agent unnamed. The
    /// preamble then reads `"You are {mission}."` — a compact self-description
    /// for an embedding that wants one. (The coding CLI ships no identity at
    /// all: its prompt opens at the host environment.)
    pub fn from_mission(mission: impl Into<String>) -> Self {
        Self {
            name: String::new(),
            mission: mission.into(),
            directive: None,
        }
    }

    /// Build an identity whose preamble is a full role directive / prompt string,
    /// ignoring name/mission composition. Used by subagents and focused roles: their
    /// identity is the role's complete task prompt or directive.
    pub fn from_directive(directive: impl Into<String>) -> Self {
        Self {
            name: String::new(),
            mission: String::new(),
            directive: Some(directive.into()),
        }
    }

    /// Render the opening system-prompt sentence. A `directive` override returns
    /// it verbatim; otherwise `"You are {name}, {mission}."` when both are set,
    /// `"You are {name}."` / `"You are {mission}."` when one is set, and the
    /// empty string when neither is (tests / identity-less agents).
    pub fn preamble(&self) -> String {
        if let Some(directive) = &self.directive
            && !directive.is_empty()
        {
            return directive.clone();
        }
        match (self.name.is_empty(), self.mission.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!("You are {}.", self.name),
            (true, false) => format!("You are {}.", self.mission),
            (false, false) => format!("You are {}, {}.", self.name, self.mission),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentIdentity;

    #[test]
    fn default_identity_renders_no_preamble() {
        assert_eq!(AgentIdentity::default().preamble(), "");
    }

    #[test]
    fn mission_only_identity_opens_with_the_mission() {
        assert_eq!(
            AgentIdentity::from_mission("an expert AI coding assistant").preamble(),
            "You are an expert AI coding assistant."
        );
    }

    #[test]
    fn named_identity_composes_name_and_mission() {
        assert_eq!(
            AgentIdentity::new("hypervisor", "the daemon-level coordinator").preamble(),
            "You are hypervisor, the daemon-level coordinator."
        );
    }

    #[test]
    fn directive_override_is_returned_verbatim() {
        assert_eq!(
            AgentIdentity::from_directive("Role: code reviewer. Report findings; never apply.")
                .preamble(),
            "Role: code reviewer. Report findings; never apply."
        );
    }
}
