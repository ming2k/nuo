//! Tiered instruction model for system-level agent directives.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

/// The route/role facts an instruction selector is evaluated against.
///
/// A missing fact never matches a set selector: callers that cannot name the
/// route must not accidentally admit route-scoped content.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstructionRoute<'a> {
    pub role: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub connection: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// Scope selectors for one project-authored instruction.
///
/// All set selectors must match. Unset selectors are wildcards. `role`,
/// `provider`, and `connection` are exact (case-insensitive) because they name
/// finite, configured entities; `model` is always a glob pattern, and an exact
/// model id is simply a pattern without wildcards. There is deliberately no
/// numeric priority: matching entries compose in scope order and then in
/// declaration (path) order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstructionSelector {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// Glob matched against the wire model id (`*` and `?`; `/` is ordinary
    /// text, so `*` spans a vendor prefix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl InstructionSelector {
    /// Whether the selector applies to every route.
    pub fn is_global(&self) -> bool {
        self.role.is_none()
            && self.provider.is_none()
            && self.connection.is_none()
            && self.model.is_none()
    }

    /// Scope rank: broader scopes sort earlier, the model pattern sorts last.
    pub fn scope_rank(&self) -> u8 {
        if self.model.is_some() {
            4
        } else if self.connection.is_some() {
            3
        } else if self.provider.is_some() {
            2
        } else if self.role.is_some() {
            1
        } else {
            0
        }
    }

    /// Number of set selectors; the tie-breaker within one scope rank.
    pub fn set_count(&self) -> usize {
        usize::from(self.role.is_some())
            + usize::from(self.provider.is_some())
            + usize::from(self.connection.is_some())
            + usize::from(self.model.is_some())
    }

    /// Deterministic composition key: broad scopes first, then fewer selectors,
    /// then declaration order (the caller's stable sort keeps that).
    pub fn order_key(&self) -> (u8, usize) {
        (self.scope_rank(), self.set_count())
    }

    /// Human-readable selector summary, used as a user-instruction heading.
    pub fn display_label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(role) = &self.role {
            parts.push(format!("role {role}"));
        }
        if let Some(provider) = &self.provider {
            parts.push(format!("provider {provider}"));
        }
        if let Some(connection) = &self.connection {
            parts.push(format!("connection {connection}"));
        }
        if let Some(model) = &self.model {
            parts.push(format!("model {model}"));
        }
        if parts.is_empty() {
            "global".to_string()
        } else {
            parts.join(" | ")
        }
    }

    /// Reject empty selector values and malformed model patterns at load time.
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("role", &self.role),
            ("provider", &self.provider),
            ("connection", &self.connection),
        ] {
            if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                return Err(format!("instruction selector `{field}` is empty"));
            }
        }
        if let Some(model) = &self.model {
            if model.trim().is_empty() {
                return Err("instruction selector `model` is empty".to_string());
            }
            glob::Pattern::new(model)
                .map_err(|error| format!("invalid `model` pattern '{model}': {error}"))?;
        }
        Ok(())
    }

    /// Whether this selector applies to `route`.
    pub fn matches(&self, route: &InstructionRoute<'_>) -> bool {
        let exact = |wanted: &Option<String>, actual: Option<&str>| match wanted {
            None => true,
            Some(wanted) => actual.is_some_and(|actual| actual.eq_ignore_ascii_case(wanted)),
        };
        if !exact(&self.role, route.role)
            || !exact(&self.provider, route.provider)
            || !exact(&self.connection, route.connection)
        {
            return false;
        }
        match &self.model {
            None => true,
            Some(pattern) => {
                let Ok(glob) = glob::Pattern::new(pattern) else {
                    return false;
                };
                route.model.is_some_and(|model| {
                    glob.matches_with(
                        &model.to_ascii_lowercase(),
                        glob::MatchOptions {
                            case_sensitive: false,
                            require_literal_separator: false,
                            require_literal_leading_dot: false,
                        },
                    )
                })
            }
        }
    }
}

/// Where a scoped instruction was authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionSource {
    /// User configuration (`[[instructions.user]]`), cross-project route tuning.
    User,
    /// Repository files (`.nuo/instructions/**/*.md`), trusted and attested.
    Project,
}

/// One scoped instruction with its scope and provenance.
///
/// `id` is the stable provenance (`user:<n>` or the repository-relative path);
/// `ordinal` is the declaration order within its source (config array index or
/// sorted path order), used as the deterministic tie-breaker after scope rank.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopedInstruction {
    pub id: String,
    pub source: InstructionSource,
    pub ordinal: usize,
    #[serde(default)]
    pub selector: InstructionSelector,
    pub text: String,
}

impl ScopedInstruction {
    /// The attribution heading rendered above the entry.
    pub fn heading(&self) -> String {
        match self.source {
            InstructionSource::Project => self.id.clone(),
            InstructionSource::User => {
                format!("user | {}", self.selector.display_label())
            }
        }
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

    fn route<'a>(
        role: Option<&'a str>,
        provider: Option<&'a str>,
        connection: Option<&'a str>,
        model: &'a str,
    ) -> InstructionRoute<'a> {
        InstructionRoute {
            role,
            provider,
            connection,
            model: Some(model),
        }
    }

    #[test]
    fn instruction_selector_matches_route_facts_and_model_patterns() {
        let global = InstructionSelector::default();
        assert!(global.is_global());
        assert!(global.matches(&route(None, None, None, "any/model")));

        let scoped = InstructionSelector {
            role: Some("Reviewer".into()),
            provider: Some("commandcode-goat".into()),
            connection: Some("cc-fox".into()),
            model: Some("*deepseek*".into()),
        };
        assert!(scoped.matches(&route(
            Some("reviewer"),
            Some("COMMANDCODE-GOAT"),
            Some("cc-FOX"),
            "deepseek/deepseek-v4-flash-fast"
        )));
        // A set selector requires the fact to exist and agree.
        assert!(!scoped.matches(&route(
            None,
            Some("commandcode-goat"),
            Some("cc-fox"),
            "deepseek/x"
        )));
        assert!(!scoped.matches(&route(
            Some("reviewer"),
            Some("commandcode-goat"),
            Some("cc-fox"),
            "gpt-6-sol"
        )));
    }

    #[test]
    fn instruction_selector_ordering_is_broad_to_narrow() {
        let rank = |selector: InstructionSelector| selector.order_key();
        assert_eq!(rank(InstructionSelector::default()), (0, 0));
        assert_eq!(
            rank(InstructionSelector {
                role: Some("ops".into()),
                ..Default::default()
            }),
            (1, 1)
        );
        assert_eq!(
            rank(InstructionSelector {
                provider: Some("openai".into()),
                ..Default::default()
            }),
            (2, 1)
        );
        assert_eq!(
            rank(InstructionSelector {
                connection: Some("cc-fox".into()),
                ..Default::default()
            }),
            (3, 1)
        );
        // The model pattern is the most specific dimension even when combined.
        assert_eq!(
            rank(InstructionSelector {
                connection: Some("cc-fox".into()),
                model: Some("*deepseek*".into()),
                ..Default::default()
            }),
            (4, 2)
        );
    }

    #[test]
    fn instruction_selector_validation_rejects_empty_values_and_bad_patterns() {
        assert!(InstructionSelector::default().validate().is_ok());
        assert!(
            InstructionSelector {
                role: Some("  ".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            InstructionSelector {
                model: Some("[".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
