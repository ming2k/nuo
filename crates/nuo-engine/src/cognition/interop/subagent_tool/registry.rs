//! The subagent registry: live handles and their cancellation.

use super::*;
use crate::cognition::agent::SubagentHandle;

#[derive(Default)]
pub struct SubagentRegistry {
    map: std::sync::Mutex<std::collections::HashMap<String, SubagentHandle>>,
    /// Live child cancellation tokens keyed by the parent tool-call id
    /// (scene scope). Mirror of the tool's own `active_cancels`, but
    /// reachable from the harness so a **single** subagent can be interrupted
    /// promptly (the token races the child's in-flight model request) without
    /// stopping the enclosing primary round — which the primary round's own
    /// `Tool::request_cancel` sweep would otherwise do for *every* child.
    cancels: std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
}

impl SubagentRegistry {
    /// Register a steering handle for the subagent spawned by the
    /// `parent_call_id` tool call. Replaces any prior entry for that id.
    pub fn register(&self, parent_call_id: &str, handle: SubagentHandle) {
        // Poison-recovery idiom (codebase convention): a panic in another
        // holder poisoned the lock; recover the inner data rather than
        // panicking on a second, downstream error.
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(parent_call_id.to_string(), handle);
    }

    /// Register the `parent_call_id` child's live cancellation token so a
    /// scene-scoped interrupt ([`Self::interrupt`]) can stop it promptly. The
    /// token is the same one the child's round loop races against its model
    /// request (see `SubagentTool::run_subagent_outcome`).
    pub fn register_cancel(&self, parent_call_id: &str, token: CancellationToken) {
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(parent_call_id.to_string(), token);
    }

    /// Look up the handle for a live subagent by its parent tool-call id.
    /// Returns a cloned handle (cheap) so the caller can reply without holding
    /// the lock.
    pub fn get(&self, parent_call_id: &str) -> Option<SubagentHandle> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(parent_call_id)
            .cloned()
    }

    /// Remove the entry for a finished subagent. Called when the `task` tool
    /// returns, so the registry never accumulates dead handles for completed
    /// calls (a handle whose `Weak` already expired is harmless but useless).
    /// The cancellation token is dropped alongside so a late interrupt for a
    /// finished call finds nothing to cancel.
    pub fn remove(&self, parent_call_id: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(parent_call_id);
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(parent_call_id);
    }

    /// Interrupt the single subagent spawned by `parent_call_id` (
    /// scene scope): cancel its live token so an in-flight model request or
    /// tool call unwinds promptly, unblock any parked human decision, and
    /// submit a boundary `AgentOp::Interrupt` as the idle/queued fallback. The
    /// parent round and every sibling child are left running.
    ///
    /// Returns `false` for an unknown or already-finished call — the child has
    /// settled, so there is nothing to stop (a graceful no-op, mirroring a late
    /// `Tool::request_cancel`). This is the harness-facing entry the driver's
    /// [`AgentRequest::InterruptSubagent`] arm calls.
    ///
    /// [`AgentRequest::InterruptSubagent`]: nuo_protocol::AgentRequest::InterruptSubagent
    pub fn interrupt(&self, parent_call_id: &str) -> bool {
        let token = self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(parent_call_id)
            .cloned();
        let handle = self.get(parent_call_id);
        if token.is_none() && handle.is_none() {
            return false;
        }
        if let Some(token) = token {
            token.cancel();
        }
        // Reject parked human decisions and queue the boundary op through the
        // handle (a no-op if the child already dropped).
        if let Some(handle) = handle {
            handle.interrupt();
        }
        true
    }
}
