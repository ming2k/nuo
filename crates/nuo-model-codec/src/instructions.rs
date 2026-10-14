//! Tiered instruction model for system-level agent directives (ADR-0160).
//!
//! Replaces legacy flat system-prompt strings with a structured, cache-tiered
//! instruction manifest. Slices are classified by lifetime volatility, enabling
//! wire protocol projectors to place optimal KV-cache breakpoints (Anthropic),
//! map static tiers to top-level `instructions` and dynamic tiers to developer
//! items (OpenAI Responses), or format leading `Role::System` messages (Chat
//! Completions).

use serde::{Deserialize, Serialize};

/// Cache tier and lifetime volatility of an instruction slice.
///
/// Order determines placement priority when flattening or setting cache breakpoints:
/// lower numerical value = more static / earlier prefix.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum InstructionTier {
    /// Immutable base identity, persona, safety ethos, host environment & tool discipline.
    /// Globally cacheable across turns and sessions.
    Base = 0,
    /// Session-stable context: project rules (AGENTS.md), workspace roots, static tool category guidelines.
    /// Cacheable for the duration of a project / session.
    Session = 1,
    /// Task-stable mission guidance: subagent role, subagent task framing.
    Task = 2,
    /// Ephemeral / turn-dynamic modifiers: runtime execution mode (Delegated mode), recency nudges.
    /// Volatile per-round state that should not invalidate static prefixes.
    Ephemeral = 3,
}

impl InstructionTier {
    /// Returns true if this tier represents static/system-level instructions
    /// suitable for top-level instructions or global system blocks.
    pub fn is_static(&self) -> bool {
        !matches!(self, Self::Ephemeral)
    }
}

/// A self-contained rendered instruction slice with stable identity and tier classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSlice {
    pub id: String,
    pub tier: InstructionTier,
    pub content: String,
}

impl InstructionSlice {
    pub fn new(id: impl Into<String>, tier: InstructionTier, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            tier,
            content: content.into(),
        }
    }
}

/// A structured manifest of instruction slices assembled for a model request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionBundle {
    pub slices: Vec<InstructionSlice>,
}

impl InstructionBundle {
    pub fn new(slices: Vec<InstructionSlice>) -> Self {
        Self { slices }
    }

    pub fn from_single(
        id: impl Into<String>,
        tier: InstructionTier,
        content: impl Into<String>,
    ) -> Self {
        let content_str = content.into();
        if content_str.trim().is_empty() {
            Self::default()
        } else {
            Self {
                slices: vec![InstructionSlice::new(id, tier, content_str)],
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.slices.is_empty() || self.slices.iter().all(|s| s.content.trim().is_empty())
    }

    pub fn len(&self) -> usize {
        self.slices.len()
    }

    pub fn push(&mut self, slice: InstructionSlice) {
        if !slice.content.trim().is_empty() {
            self.slices.push(slice);
        }
    }

    /// Iterator over slices in a specific tier.
    pub fn slices_by_tier(&self, tier: InstructionTier) -> impl Iterator<Item = &InstructionSlice> {
        self.slices
            .iter()
            .filter(move |s| s.tier == tier && !s.content.trim().is_empty())
    }

    /// Non-ephemeral slices (Base, Session, Task) suitable for top-level instructions.
    pub fn static_slices(&self) -> impl Iterator<Item = &InstructionSlice> {
        self.slices
            .iter()
            .filter(|s| s.tier.is_static() && !s.content.trim().is_empty())
    }

    /// Ephemeral slices (turn-dynamic nudges, delegated mode flags).
    pub fn ephemeral_slices(&self) -> impl Iterator<Item = &InstructionSlice> {
        self.slices
            .iter()
            .filter(|s| s.tier == InstructionTier::Ephemeral && !s.content.trim().is_empty())
    }

    pub fn has_ephemeral(&self) -> bool {
        self.slices
            .iter()
            .any(|s| s.tier == InstructionTier::Ephemeral && !s.content.trim().is_empty())
    }

    /// Render all active non-empty slices into a single string.
    /// Sections are joined with a single newline; sections that require
    /// a paragraph break carry their own leading `\n` to preserve exact formatting.
    pub fn render_combined(&self) -> String {
        let mut out = String::new();
        for slice in &self.slices {
            let trimmed = slice.content.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&slice.content);
        }
        out
    }

    /// Render static (Base, Session, Task) slices into a top-level system instruction string.
    pub fn render_system_instructions(&self) -> String {
        let mut out = String::new();
        for slice in self.static_slices() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&slice.content);
        }
        out
    }

    /// Render ephemeral slices into a combined ephemeral instruction string.
    pub fn render_ephemeral(&self) -> String {
        let mut out = String::new();
        for slice in self.ephemeral_slices() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&slice.content);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_bundle_partitions_tiers_correctly() {
        let mut bundle = InstructionBundle::default();
        bundle.push(InstructionSlice::new(
            "base.id",
            InstructionTier::Base,
            "You are an AI assistant.",
        ));
        bundle.push(InstructionSlice::new(
            "session.rules",
            InstructionTier::Session,
            "Follow project rules.",
        ));
        bundle.push(InstructionSlice::new(
            "task.subagent",
            InstructionTier::Task,
            "Analyze code carefully.",
        ));
        bundle.push(InstructionSlice::new(
            "ephem.delegated",
            InstructionTier::Ephemeral,
            "Delegated mode active.",
        ));

        assert_eq!(bundle.len(), 4);
        assert!(bundle.has_ephemeral());

        let static_text = bundle.render_system_instructions();
        assert_eq!(
            static_text,
            "You are an AI assistant.\nFollow project rules.\nAnalyze code carefully."
        );

        let ephem_text = bundle.render_ephemeral();
        assert_eq!(ephem_text, "Delegated mode active.");

        let combined_text = bundle.render_combined();
        assert_eq!(
            combined_text,
            "You are an AI assistant.\nFollow project rules.\nAnalyze code carefully.\nDelegated mode active."
        );
    }

    #[test]
    fn empty_slices_are_skipped() {
        let mut bundle = InstructionBundle::default();
        bundle.push(InstructionSlice::new("empty", InstructionTier::Base, "   "));
        assert!(bundle.is_empty());
        assert_eq!(bundle.len(), 0);
    }
}
