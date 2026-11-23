//! Request-context declaration vocabulary.
//!
//! A context element that reaches the model declares five facts (ADR-0215):
//! which lifecycle moment creates it, which [`Topic`] it is about, which
//! [`Rank`] it carries, how [`Stability`]-volatile it is, and what [`Budget`] it
//! may spend. Region, band position, and rank are *derived* from those
//! declarations rather than declared separately, so an illegal combination
//! ("untrusted but cached") is unrepresentable.
//!
//! The vocabulary lives in the contract layer because embedders construct
//! sources through the public builder API; the registry that enforces the
//! invariants lives in `nuo-engine`.
pub mod plan;
pub mod view;

pub use plan::{
    Budget, ContextNotice, ContextPlan, Elision, NoticeKind, Overflow, PlannedBlock,
    PlannedBlockRecord, PrefixDrift, Resume,
};
pub use view::{ContextMeasurement, ContextPlanView, PlannedBlockView};

use serde::{Deserialize, Serialize};

/// When a value changes, and therefore how much of the cached prefix it can
/// invalidate.
///
/// Ordering is meaningful and is the band order of the rendered request:
/// ascending stability. A block may not be placed in an earlier band than its
/// most volatile input, which is why stability is derived from declared inputs
/// rather than claimed by the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    /// Never changes for a given build. A compiled-in contract.
    Frozen,
    /// Changes when identity, role, or provider route changes.
    Route,
    /// Changes on a session-scoped decision (`/model`, `/role`, a trust grant).
    Session,
    /// Changes when a workspace file changes (`AGENTS.md`, a manifest, a skill).
    Workspace,
}

impl Stability {
    /// Short label for the inspection table.
    pub fn label(self) -> &'static str {
        match self {
            Self::Frozen => "frozen",
            Self::Route => "route",
            Self::Session => "session",
            Self::Workspace => "workspace",
        }
    }
}

/// What a context element is *about*. One active owner per topic.
///
/// Closed on purpose: adding a topic is an edit to the contract crate, which is
/// the review gate that stops `tool_discipline` and `tool.discipline` from being
/// two owners of one concern ([`INV-CTXSRC-01]`, ADR-0215).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// What the agent is. The first frame block.
    Identity,
    /// How conflicting instructions resolve. The second frame block.
    Precedence,
    /// Which tool to reach for, and when.
    ToolDiscipline,
    /// How to explore an unfamiliar workspace with the admitted tools.
    ExplorationProtocol,
    /// That credentials and secrets are never echoed.
    Secrets,
    /// User-authored scoped instructions.
    UserInstructions,
    /// Which language the answer is written in.
    ///
    /// Its own topic because it is not style tuning: it is the one rule that
    /// decides which language the model *thinks out loud in* when the prompt,
    /// the tool output, and the injected notes are all in another one. Folding
    /// it into `user_instructions` would make it opt-in prose a user has to
    /// write; a response language is a property of the session, not a tip.
    OutputLanguage,
    /// Repository-authored scoped instructions.
    ProjectInstructions,
    /// An unscoped project rules document (`AGENTS.md`).
    ProjectAuthority,
    /// The discovered skill catalog.
    SkillCapability,
    /// Which directories are admitted, and what that permits.
    WorkspaceBoundary,
    /// A deterministic architecture summary of the workspace.
    WorkspaceTopology,
    /// How to treat data that must never be read as an instruction.
    UntrustedPolicy,
    /// Dialect-specific output rules from the active provider.
    ProviderDialect,
}

impl Topic {
    /// Every topic, for exhaustive tests and the inspection column.
    pub const ALL: &'static [Topic] = &[
        Topic::Identity,
        Topic::Precedence,
        Topic::ToolDiscipline,
        Topic::ExplorationProtocol,
        Topic::Secrets,
        Topic::UserInstructions,
        Topic::OutputLanguage,
        Topic::ProjectInstructions,
        Topic::ProjectAuthority,
        Topic::SkillCapability,
        Topic::WorkspaceBoundary,
        Topic::WorkspaceTopology,
        Topic::UntrustedPolicy,
        Topic::ProviderDialect,
    ];

    /// Short label for the inspection table. Matches the serde wire name so a
    /// value has exactly one spelling.
    pub fn label(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Precedence => "precedence",
            Self::ToolDiscipline => "tool_discipline",
            Self::ExplorationProtocol => "exploration_protocol",
            Self::Secrets => "secrets",
            Self::UserInstructions => "user_instructions",
            Self::OutputLanguage => "output_language",
            Self::ProjectInstructions => "project_instructions",
            Self::ProjectAuthority => "project_authority",
            Self::SkillCapability => "skill_capability",
            Self::WorkspaceBoundary => "workspace_boundary",
            Self::WorkspaceTopology => "workspace_topology",
            Self::UntrustedPolicy => "untrusted_policy",
            Self::ProviderDialect => "provider_dialect",
        }
    }

    /// Placement class, derived from the topic so a source cannot mislabel it.
    pub fn rank(self) -> Rank {
        match self {
            // Data the model may consult but must not obey.
            Self::UntrustedPolicy => Rank::Untrusted,
            // Data the model may consult; never a behavioral rule.
            Self::SkillCapability | Self::WorkspaceTopology => Rank::Reference,
            // Everything else shapes behavior.
            _ => Rank::Directive,
        }
    }

    /// Where the block sits relative to the stability bands.
    ///
    /// Identity and the conflict-resolution contract are not "the most stable
    /// body blocks"; they are what the request *is*, so they lead in topic order
    /// regardless of how volatile their own bytes are.
    pub fn position(self) -> Position {
        match self {
            Self::Identity | Self::Precedence => Position::Frame,
            _ => Position::Body,
        }
    }
}

/// Where a block sits relative to the stability bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    /// Emitted first, in topic order. A frame block may declare only
    /// `Route`-or-more-stable inputs, or the whole prefix is invalidated on
    /// every workspace edit.
    Frame,
    /// Ordered by ascending [`Stability`], then by declared order.
    Body,
}

impl Position {
    pub fn label(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Body => "body",
        }
    }
}

/// Placement class derived from [`Topic`].
///
/// `Directive` shapes behavior and never becomes conversation content;
/// `Untrusted` never enters the cacheable prefix. Both are asserted over the
/// rendered request ([`INV-CTXSRC-04]`), so the class is a routing decision and
/// not a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rank {
    /// Shapes model behavior. Always instruction text, never a message.
    Directive,
    /// Data the model may consult.
    Reference,
    /// Data that must never be interpretable as an instruction.
    Untrusted,
}

impl Rank {
    pub fn label(self) -> &'static str {
        match self {
            Self::Directive => "directive",
            Self::Reference => "reference",
            Self::Untrusted => "untrusted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_orders_from_frozen_to_workspace() {
        let mut bands = vec![
            Stability::Workspace,
            Stability::Session,
            Stability::Route,
            Stability::Frozen,
        ];
        bands.sort();
        assert_eq!(
            bands,
            vec![
                Stability::Frozen,
                Stability::Route,
                Stability::Session,
                Stability::Workspace,
            ]
        );
    }

    #[test]
    fn identity_and_precedence_are_the_frame() {
        assert_eq!(Topic::Identity.position(), Position::Frame);
        assert_eq!(Topic::Precedence.position(), Position::Frame);
        for topic in Topic::ALL {
            if matches!(topic, Topic::Identity | Topic::Precedence) {
                continue;
            }
            assert_eq!(topic.position(), Position::Body, "{}", topic.label());
        }
    }

    #[test]
    fn untrusted_rank_is_derived_from_the_topic_not_declared() {
        assert_eq!(Topic::UntrustedPolicy.rank(), Rank::Untrusted);
        assert_eq!(Topic::ProjectAuthority.rank(), Rank::Directive);
        assert_eq!(Topic::SkillCapability.rank(), Rank::Reference);
    }

    #[test]
    fn every_topic_label_is_unique_and_matches_its_wire_name() {
        let mut labels: Vec<&str> = Topic::ALL.iter().map(|topic| topic.label()).collect();
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count, "two topics share a label");
        for topic in Topic::ALL {
            let wire = serde_json::to_string(topic).unwrap();
            assert_eq!(
                wire,
                format!("\"{}\"", topic.label()),
                "label and wire name disagree for {}",
                topic.label()
            );
        }
    }

    #[test]
    fn domain_label_uses_the_same_snake_case_vocabulary() {
        assert!(
            Topic::ALL.iter().all(|topic| !topic.label().contains('-')),
            "a hyphenated label would be a second spelling"
        );
    }
}
