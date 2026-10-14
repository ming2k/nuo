//! Atomic harness extensions (ADR-0224): one primitive unifying tools and
//! ambient harness facets. A **tool** is an extension with a model-callable
//! surface; a **facet** is an extension with declared hook phases. Extensions
//! are instantiated per session and are the sole capability unit.

use crate::Tool;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// The fixed harness hook phases an extension may declare (ADR-0224). A closed
/// vocabulary owned by the harness: extensions declare participation, they do
/// not define phases, and the harness owns ordering and lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPhase {
    /// Contribute requested request-local temporary context (`E_n`,
    /// ADR-0213/ADR-0217). The result travels in `temporary_context`, never in
    /// durable history.
    ProjectTemporaryContext,
    /// Validate a pending file mutation before it reaches disk.
    InterceptFileMutation,
}

impl HookPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            HookPhase::ProjectTemporaryContext => "project_temporary_context",
            HookPhase::InterceptFileMutation => "intercept_file_mutation",
        }
    }
}

/// Context handed to [`Extension::run`] for one phase.
#[derive(Debug)]
pub struct HookContext<'a> {
    /// The session's workspace root, when a workspace is bound.
    pub workspace_root: Option<&'a Path>,
    /// The pending mutation `(path, content)` for [`HookPhase::InterceptFileMutation`].
    pub mutation: Option<(&'a Path, &'a str)>,
}

impl<'a> HookContext<'a> {
    pub fn temporary_context(workspace_root: Option<&'a Path>) -> Self {
        Self {
            workspace_root,
            mutation: None,
        }
    }

    pub fn mutation(path: &'a Path, content: &'a str) -> Self {
        Self {
            workspace_root: None,
            mutation: Some((path, content)),
        }
    }
}

/// Outcome of running one hook phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// No contribution; allow.
    None,
    /// A request-local temporary-context payload ([`HookPhase::ProjectTemporaryContext`]).
    TemporaryContext(String),
    /// Reject the mutation with a diagnostic ([`HookPhase::InterceptFileMutation`]).
    Block(String),
}

/// One atomic harness extension (ADR-0224).
///
/// Tools and facets are projections of this single primitive: a tool returns
/// `Some` from [`Self::tool`]; a facet declares non-empty [`Self::hooks`]. An
/// extension may be both. Selection is per extension `id`.
pub trait Extension: Send + Sync + std::fmt::Debug {
    /// Stable identifier (e.g. `"code_intelligence"`, or a tool name).
    fn id(&self) -> &str;

    /// The model-callable surface, when this extension is a tool.
    fn tool(&self) -> Option<Arc<dyn Tool>> {
        None
    }

    /// The hook phases this extension participates in.
    fn hooks(&self) -> &'static [HookPhase] {
        &[]
    }

    /// Run one declared hook phase. Called only for phases in [`Self::hooks`].
    fn run(&self, _phase: HookPhase, _ctx: &HookContext<'_>) -> HookOutcome {
        HookOutcome::None
    }
}

/// Adapts any [`Tool`] into an [`Extension`] with a model-callable surface and
/// no hooks.
#[derive(Clone)]
pub struct ToolExtension {
    tool: Arc<dyn Tool>,
}

impl std::fmt::Debug for ToolExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolExtension")
            .field("id", &self.tool.name())
            .finish()
    }
}

impl ToolExtension {
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self { tool }
    }
}

impl Extension for ToolExtension {
    fn id(&self) -> &str {
        self.tool.name()
    }

    fn tool(&self) -> Option<Arc<dyn Tool>> {
        Some(self.tool.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Dummy;

    impl Extension for Dummy {
        fn id(&self) -> &str {
            "dummy"
        }
        fn hooks(&self) -> &'static [HookPhase] {
            &[HookPhase::ProjectTemporaryContext]
        }
        fn run(&self, phase: HookPhase, _ctx: &HookContext<'_>) -> HookOutcome {
            match phase {
                HookPhase::ProjectTemporaryContext => HookOutcome::TemporaryContext("x".into()),
                HookPhase::InterceptFileMutation => HookOutcome::None,
            }
        }
    }

    #[test]
    fn default_extension_has_no_tool_and_no_hooks() {
        let e = Dummy;
        assert_eq!(e.id(), "dummy");
        assert!(e.tool().is_none());
        assert_eq!(e.hooks(), &[HookPhase::ProjectTemporaryContext]);
        assert_eq!(
            e.run(
                HookPhase::ProjectTemporaryContext,
                &HookContext::temporary_context(None)
            ),
            HookOutcome::TemporaryContext("x".to_string())
        );
    }
}
