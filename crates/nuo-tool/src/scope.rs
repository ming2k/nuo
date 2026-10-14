//! Tool scoping and dynamic assembly primitives.

use serde::{Deserialize, Serialize};

/// Functional capability domain of a tool for stage-gated assembly and security scoping.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolScope {
    /// Safe, read-only inspection or pure calculation (safe for planning & discovery).
    ReadOnly,
    /// Local workspace manipulation (file reading, writing, editing, patching).
    Workspace,
    /// System command execution or process invocation.
    Execution,
    /// Network I/O or external API calls.
    Network,
    /// Collaboration and peer-to-peer delegation.
    Collaboration,
    /// Custom domain or stage tag.
    Custom(String),
}

impl ToolScope {
    pub fn custom(name: impl Into<String>) -> Self {
        Self::Custom(name.into())
    }
}

/// What a tool call acts on, so the operation-scope gate can match it against
/// the agent's granted scope. A tool reports this via its [`ToolDescriptor`] or
/// per-call refinement; each variant names a locatable target a tool may report.
/// [`ScopeTarget::Unspecified`] is the default for tools with no locatable target.
///
/// Lives in the tool leaf (ADR-0008 `[INV-TOOL-11]`): it is pure tool metadata
/// with no dependency on any higher layer.
///
/// [`ToolDescriptor`]: crate::descriptor::ToolDescriptor
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeTarget {
    /// A filesystem path the tool writes or reads (e.g. `write_file`, `edit_text`).
    /// Checked against the scope's granted directory prefixes.
    Path(std::path::PathBuf),
    /// A shell command string (e.g. `bash`). Checked against the scope's command
    /// allowlist, when one is set.
    Command(String),
    /// The tool declares no locatable target (e.g. `search_text`, `list_dir`).
    /// Admitted by the scope gate without a dimension check.
    Unspecified,
}

impl Default for ScopeTarget {
    fn default() -> Self {
        Self::Unspecified
    }
}
