//! Session-scoped activation of deferred tools.
//!
//! Deferred tools exist so a large catalog — typically MCP servers — is not
//! declared in every request. `tool_search` activates the entries the model
//! asks for; the next request declares them alongside the direct set. The set
//! is deliberately in-memory and session-scoped: it is a view of what this
//! conversation has discovered, not durable configuration.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

/// The set of deferred tool names activated in one session.
#[derive(Clone, Default)]
pub(crate) struct ToolActivation {
    active: Arc<RwLock<HashSet<String>>>,
}

impl ToolActivation {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn activate<I, S>(&self, names: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut guard = self.active.write().unwrap_or_else(|e| e.into_inner());
        for name in names {
            guard.insert(name.into());
        }
    }

    pub(crate) fn is_active(&self, name: &str) -> bool {
        self.active
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(name)
    }

    /// How many deferred tools are currently activated. Diagnostic only: no
    /// context source may declare it as an input, so activation never moves the
    /// instruction prefix (`[INV-TOOLSURFACE-03]`, ADR-0216).
    pub(crate) fn len(&self) -> usize {
        self.active.read().unwrap_or_else(|e| e.into_inner()).len()
    }
}
