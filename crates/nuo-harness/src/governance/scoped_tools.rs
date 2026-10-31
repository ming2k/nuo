//! Scoped tool-disabling state — the in-memory mask a hook applies via a
//! [`nuo_protocol::HookOutcome::ScopeTools`] outcome.
//!
//! Lives in the governance layer (`[INV-HARNESS-03]`, ADR-0100): the permission
//! policy and the dispatch tool manager both consult it to hide a tool from the
//! model and reject it at dispatch. Cognition-side code *uses* it but must not
//! own it, so this module never depends on a higher (cognition) layer.
use std::collections::HashMap;

/// In-memory only mask of tools a hook has temporarily disabled via a
/// [`nuo_protocol::HookOutcome::ScopeTools`] outcome, partitioned by the
/// [`nuo_protocol::RestorePoint`] at which each should come back.
///
/// Deliberately **separate** from the session-level, persisted
/// `Agent::disabled_tools`: scoped disables never reach the session store
/// (the snapshot path only clones the persisted mask), so they never survive a
/// restart and never collide with a user's manual `/tools` toggles. Each bucket
/// is a reference count (`HashMap<String, u32>`) rather than a flat set so two
/// hooks disabling the same tool at different restore points don't fight: the
/// earlier restore only decrements, the tool stays hidden until its last
/// refcount reaches zero.
#[derive(Default, Clone)]
pub(crate) struct ScopedToolDisable {
    round_end: HashMap<String, u32>,
    turn_end: HashMap<String, u32>,
}

impl ScopedToolDisable {
    /// Record a hook-fired disable for `tool` at `restore`. Increments the
    /// refcount so nested disables compose.
    pub(crate) fn disable(&mut self, tool: &str, restore: nuo_protocol::RestorePoint) {
        let bucket = match restore {
            nuo_protocol::RestorePoint::TurnEnd => &mut self.turn_end,
            nuo_protocol::RestorePoint::RoundEnd => &mut self.round_end,
        };
        *bucket.entry(tool.to_string()).or_insert(0) += 1;
    }

    /// Whether `tool` is currently scoped-disabled (hidden from the model and
    /// rejected at dispatch) under any restore point.
    pub(crate) fn contains(&self, tool: &str) -> bool {
        self.round_end.contains_key(tool) || self.turn_end.contains_key(tool)
    }

    /// Drop every `TurnEnd` disable at the ReAct-turn boundary. `RoundEnd`
    /// disables survive until the user round ends.
    pub(crate) fn restore_turn_end(&mut self) {
        self.turn_end.clear();
    }

    /// Drop every disable (both buckets). Called at user-round end so the
    /// toolset is fresh for the next user request.
    pub(crate) fn restore_round_end(&mut self) {
        self.round_end.clear();
        self.turn_end.clear();
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.round_end.is_empty() && self.turn_end.is_empty()
    }
}
