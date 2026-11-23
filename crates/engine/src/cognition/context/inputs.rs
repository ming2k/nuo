//! Declared context inputs.
//!
//! Every value a context source may read is declared here as an [`InputKey`].
//! The key carries a static [`Stability`], so a source's cache claim is *derived*
//! from what it reads rather than asserted by the source
//! (`[INV-CTXSRC-03]`, ADR-0215).
use std::collections::BTreeSet;
use std::hash::Hash;

use protocol::Stability;
use tool::BuiltinTool;

/// One declared input a context source may read.
///
/// Stability is a property of the key, not of the value: project rules are
/// `Workspace`-stable whether or not they are present, because their *presence*
/// can change with a workspace file.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InputKey {
    /// The composed identity line.
    IdentityPreamble,
    /// User-authored scoped instructions matching this turn's route.
    UserInstructions,
    /// Repository-authored scoped instructions matching this turn's route.
    ProjectInstructions,
    /// The unscoped project rules document.
    ProjectRules,
    /// Additional admitted workspace roots.
    AdditionalWorkspaceRoots,
    /// The deterministic workspace topology summary.
    WorkspaceTopology,
    /// The discovered skill catalog.
    SkillCatalog,
    /// The declared answer language for this session.
    ResponseLanguage,
    /// Provider dialect guidance.
    ProviderGuidance,
    /// The declared tool surface.
    ToolSurface,
}

impl InputKey {
    /// Every key, for exhaustive tests and the `[INV-CTXSRC-06]` reachability
    /// check.
    pub const ALL: &'static [InputKey] = &[
        InputKey::IdentityPreamble,
        InputKey::UserInstructions,
        InputKey::ProjectInstructions,
        InputKey::ProjectRules,
        InputKey::AdditionalWorkspaceRoots,
        InputKey::WorkspaceTopology,
        InputKey::SkillCatalog,
        InputKey::ResponseLanguage,
        InputKey::ProviderGuidance,
        InputKey::ToolSurface,
    ];

    /// How volatile this input is. A source's stability is the maximum over the
    /// keys it declares.
    pub fn stability(self) -> Stability {
        match self {
            // Provider/protocol dialect guidance follows the route.
            Self::ProviderGuidance | Self::ToolSurface => Stability::Route,
            // Replaced by `/role` or the embedding's persona; both are route
            // decisions for the request the assembler is building.
            Self::IdentityPreamble => Stability::Route,
            // Loaded per session (`agent.toml`, the skill scan, the trust grant).
            Self::UserInstructions
            | Self::ResponseLanguage
            | Self::SkillCatalog
            | Self::AdditionalWorkspaceRoots => Stability::Session,
            // Derived from workspace files.
            Self::ProjectRules | Self::ProjectInstructions | Self::WorkspaceTopology => {
                Stability::Workspace
            }
        }
    }

    /// Short label for the inspection table.
    pub fn label(self) -> &'static str {
        match self {
            Self::IdentityPreamble => "identity_preamble",
            Self::UserInstructions => "user_instructions",
            Self::ProjectInstructions => "project_instructions",
            Self::ProjectRules => "project_rules",
            Self::AdditionalWorkspaceRoots => "additional_workspace_roots",
            Self::WorkspaceTopology => "workspace_topology",
            Self::SkillCatalog => "skill_catalog",
            Self::ResponseLanguage => "response_language",
            Self::ProviderGuidance => "provider_guidance",
            Self::ToolSurface => "tool_surface",
        }
    }
}

/// The declared tool surface.
///
/// Prompt policy reads declared *capability*, never a tool name (ADR-0216): a
/// rename or alias cannot delete guidance, and activating a deferred tool cannot
/// move the instruction prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ToolSurface {
    native: BTreeSet<BuiltinTool>,
    parallel: bool,
}

impl ToolSurface {
    /// Build the surface from the admitted toolset.
    ///
    /// There is deliberately no `deferred_active` counter and no
    /// `spawns_subagent` flag. Both were declared here and read by nobody; a
    /// capability set that also carries non-identity state is the shape that put
    /// the deferred-tool count into the source memo key in the first place
    /// (`[INV-TOOLSURFACE-03]`, ADR-0229). The deferred count and the subagent
    /// capability are both readable from the tool manager and the snapshot, which
    /// is where their consumers live.
    pub fn new<'a>(tools: impl IntoIterator<Item = &'a dyn tool::Tool>, parallel: bool) -> Self {
        let native = tools
            .into_iter()
            .filter_map(tool::Tool::builtin)
            .collect::<BTreeSet<_>>();
        Self { native, parallel }
    }

    /// Whether a declared native capability is admitted this request.
    pub fn has_native(&self, tool: BuiltinTool) -> bool {
        self.native.contains(&tool)
    }

    /// Whether any of these native capabilities is admitted.
    pub fn has_any_native(&self, tools: &[BuiltinTool]) -> bool {
        tools.iter().any(|tool| self.native.contains(tool))
    }

    /// Whether the active channel supports parallel tool calls.
    pub fn supports_parallel(&self) -> bool {
        self.parallel
    }
}

/// Everything a context source may read, built once per request.
///
/// Plain data with no `&Agent`, which keeps a source's `render` signature free of
/// lifetime parameters and makes the whole context content-addressable
/// (`[INV-CTXSRC-08]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ContextInputs {
    pub identity_preamble: String,
    pub tool_surface: ToolSurface,
    pub user_instructions: Vec<protocol::ScopedInstruction>,
    pub provider_guidance: &'static str,
    pub project_rules: String,
    pub additional_workspace_roots: Vec<String>,
    pub workspace_root: Option<String>,
    pub workspace_topology: Option<String>,
    pub project_instructions: Vec<protocol::ScopedInstruction>,
    pub skill_catalog: Vec<(String, String)>,
    /// The declared answer language, validated at the config boundary
    /// ([`protocol::ResponseLanguage`]).
    ///
    /// `None` means "no declared language", which is deliberately *not* the same
    /// as "English": an undeclared session renders no block at all rather than a
    /// default that would silently outrank the user's own words. See
    /// [`protocol::context::Topic::OutputLanguage`].
    pub response_language: Option<protocol::ResponseLanguage>,
}

impl ContextInputs {
    /// An all-empty context for registry-mechanics tests.
    pub fn empty() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hasher;

    #[test]
    fn stability_is_a_property_of_the_key_not_of_the_value() {
        // Project rules are Workspace-stable even when the string is empty.
        assert_eq!(InputKey::ProjectRules.stability(), Stability::Workspace);
        assert_eq!(InputKey::ToolSurface.stability(), Stability::Route);
        assert_eq!(InputKey::ResponseLanguage.stability(), Stability::Session);
    }

    #[test]
    fn every_key_has_a_unique_label() {
        let mut labels: Vec<&str> = InputKey::ALL.iter().map(|key| key.label()).collect();
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count);
    }

    /// The surface is a capability set and the channel's parallel support, and
    /// nothing else — there is no field a mid-session activation could move
    /// (`[INV-TOOLSURFACE-03]`, ADR-0229).
    #[test]
    fn the_surface_identity_is_a_capability_set() {
        let surface = ToolSurface {
            native: BTreeSet::from([BuiltinTool::Read]),
            parallel: false,
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        surface.hash(&mut hasher);
        let first = hasher.finish();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        surface.clone().hash(&mut hasher);
        assert_eq!(first, hasher.finish());
        assert!(surface.has_native(BuiltinTool::Read));
        assert!(!surface.has_native(BuiltinTool::WriteFile));
    }
}
