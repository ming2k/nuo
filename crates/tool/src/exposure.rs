//! Model-facing exposure of a tool.
//!
//! Exposure is deliberately a *visibility* decision, not an authorization one:
//! a deferred tool is still subject to the same permission chain when it is
//! invoked. It exists so a large catalog — typically MCP servers — does not have
//! to be declared in every prompt just to remain reachable.

use serde::{Deserialize, Serialize};

/// How a tool participates in the per-request tool declaration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExposure {
    /// Declared in every request and directly callable.
    #[default]
    Direct,
    /// Registered but not declared until `tool_search` activates it. Always
    /// callable by a nested/batch executor that holds the snapshot.
    Deferred,
    /// Never declared and never reachable by the model. Harness-internal only.
    Hidden,
}

impl ToolExposure {
    /// Whether this exposure is declared to the model without discovery.
    pub const fn is_direct(self) -> bool {
        matches!(self, Self::Direct)
    }

    /// Whether this exposure can ever be reached by the model.
    pub const fn is_model_reachable(self) -> bool {
        !matches!(self, Self::Hidden)
    }

    /// Whether this exposure requires `tool_search` activation first.
    pub const fn needs_discovery(self) -> bool {
        matches!(self, Self::Deferred)
    }
}
