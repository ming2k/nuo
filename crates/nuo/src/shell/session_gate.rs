//! Session scoping for the response translator (ADR-0166 `[INV-TOOL-ID-06]`).
//!
//! The transcript buffer holds exactly one session. A switch replaces it
//! wholesale, but the round it left behind can still emit trailing frames:
//! `supersede_for_session_switch` calls `cancel_current`, which cancels a token
//! and returns — it does not join the round task. Those frames must not be
//! applied to the replacement buffer, where a result with no matching row would
//! become a fallback insert and a control-plane snapshot would tear down live
//! round state.
//!
//! The gate tracks the **superseded** set rather than comparing an event's
//! session against a "live" id. That distinction matters: a `ThreadSnapshot`
//! repoints the buffer without going through a switch frame, so an equality
//! check against a separately-maintained live id would starve a legitimately
//! hydrated session. A session this connection never switched away from is
//! always accepted.

use std::collections::HashSet;

#[derive(Debug, Default)]
pub(crate) struct SessionGate {
    superseded: HashSet<String>,
}

impl SessionGate {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The buffer moved from `previous` to `next`. Frames for `previous` are
    /// stale from now on; `next` is current again (it may itself have been
    /// superseded earlier in this connection's life).
    pub(crate) fn supersede(&mut self, previous: &str, next: &str) {
        // A no-op switch must not supersede the session that is still current.
        if previous != next {
            self.superseded.insert(previous.to_string());
        }
        self.superseded.remove(next);
    }

    /// A hydration frame for `session` replaced the buffer: that session is
    /// current, whatever an earlier switch recorded.
    pub(crate) fn hydrate(&mut self, session: &str) {
        self.superseded.remove(session);
    }

    /// Whether a `Round` frame for `session` may be applied to the buffer.
    pub(crate) fn accepts(&self, session: &str) -> bool {
        !self.superseded.contains(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_gate_accepts_every_session() {
        let gate = SessionGate::new();
        assert!(gate.accepts("a"));
        assert!(gate.accepts("b"));
    }

    #[test]
    fn a_switch_supersedes_the_session_it_leaves() {
        let mut gate = SessionGate::new();
        gate.supersede("a", "b");
        assert!(!gate.accepts("a"), "the left session is stale");
        assert!(gate.accepts("b"), "the entered session is current");
    }

    #[test]
    fn a_no_op_switch_does_not_supersede_the_current_session() {
        let mut gate = SessionGate::new();
        gate.supersede("a", "a");
        assert!(gate.accepts("a"));
    }

    #[test]
    fn hydration_un_supersedes_the_session_it_rebuilds() {
        let mut gate = SessionGate::new();
        gate.supersede("a", "b");
        assert!(!gate.accepts("a"));
        gate.hydrate("a");
        assert!(gate.accepts("a"), "a hydrated buffer is current again");
    }

    #[test]
    fn switching_back_and_forth_tracks_only_the_current_session() {
        let mut gate = SessionGate::new();
        gate.supersede("a", "b");
        gate.supersede("b", "a");
        assert!(gate.accepts("a"));
        assert!(!gate.accepts("b"));
        gate.supersede("a", "b");
        assert!(gate.accepts("b"));
        assert!(!gate.accepts("a"));
    }
}
