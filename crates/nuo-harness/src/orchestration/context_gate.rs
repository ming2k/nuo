//! The mid-turn context projection gate.
//!
//! `project_context_if_needed` hands the live window to a
//! [`crate::ContextProjectionGate`] between ReAct turns, once request pressure crosses
//! the model-relative threshold. Nothing ever installed one: `bootstrap.rs`
//! carried a comment describing the install — "install a gate that …" — with no
//! code under it, so the hot path always took its `let Some(gate) = gate else`
//! early return. Long single rounds therefore had no relief valve at all, which is
//! exactly the shape a subagent takes: it runs one round to completion and never
//! passes through `execute_round`, so neither pre-round pruning nor pre-round
//! compaction can reach it.
//!
//! # What the gate is allowed to do
//!
//! Only what is safe mid-round, in this order:
//!
//! 1. **Prune** stale tool bodies in place. It keeps every message and every
//!    `tool_call_id`, so it cannot break the wire contract, and it is the cheapest
//!    relief available.
//! 2. **Compact** when pruning did not bring pressure under the prune threshold,
//!    cutting by `TargetTokens` so the result is bounded by the budget rather than
//!    by a round count.
//!
//! Both leave the caller's uncommitted tail intact: mid-round the live window
//! holds messages the session has not committed, and replacing it outright with
//! the IR's view would drop the turn in flight.
//!
//! # Why the settings are re-read rather than captured
//!
//! The thresholds are model-relative, and the active model can change at any point
//! (a `/model` switch). Capturing them at install time froze the gate on the
//! startup model's window for the rest of the session — relief would then fire at
//! the wrong pressure, which is worse than not firing at all because it spends a
//! summarization call to reach the wrong target. The gate therefore asks the
//! policy for its settings on every projection, which also means a re-install is
//! only needed when the *gate* changes, never when the model does.

use async_trait::async_trait;
use nuo_protocol::Message;
use std::sync::Arc;

use crate::cognition::compaction::CompactionCutMode;
use crate::cognition::context_projection::ContextProjectionGate;
use crate::orchestration::{
    ContextProjectionSettings, compact_round_history_with_mode, prune_and_commit,
};
use nuo_persistence::session::SessionStore;

/// Supplies the settings for the live model on demand.
///
/// A closure rather than a value so the gate never pins a model's window; the
/// composition root owns the lookup and can answer with the current provider.
pub type SettingsForWindow = Arc<dyn Fn() -> ContextProjectionSettings + Send + Sync>;

/// Applies the configured projection policy to a live window between turns.
pub struct PruningProjectionGate {
    session: Arc<SessionStore>,
    settings: SettingsForWindow,
    weights: Arc<nuo_protocol::MessageTokenWeights>,
}

impl PruningProjectionGate {
    pub fn new(
        session: Arc<SessionStore>,
        settings: SettingsForWindow,
        weights: Arc<nuo_protocol::MessageTokenWeights>,
    ) -> Self {
        Self {
            session,
            settings,
            weights,
        }
    }
}

#[async_trait]
impl ContextProjectionGate for PruningProjectionGate {
    async fn project_context(&self, mut messages: Vec<Message>) -> Option<Vec<Message>> {
        let settings = (self.settings)();
        let before = messages.len();
        let tokens_before = nuo_protocol::pressure::estimate_tokens(&messages);

        // Tier 1: clear stale tool bodies. In place, wire-compatible, no message
        // removed — so a failure here leaves the window exactly as it was.
        if let Err(error) = prune_and_commit(
            &mut messages,
            &self.session,
            &settings,
            Arc::clone(&self.weights),
        )
        .await
        {
            tracing::warn!(%error, "mid-turn prune failed; leaving the window untouched");
            return None;
        }

        // Tier 2: fold, but only if pruning left pressure above the prune
        // watermark. A prune that already relieved the session must not be
        // followed by a summarization call — that is the expensive tier, and
        // spending it on a healthy window is how compaction gets a reputation for
        // being slow.
        let tokens_after_prune = nuo_protocol::pressure::estimate_tokens(&messages);
        if tokens_after_prune > settings.budget.prune_threshold_tokens {
            let mode = CompactionCutMode::TargetTokens(settings.budget.target_tokens.max(1));
            match compact_round_history_with_mode(
                &mut messages,
                &self.session,
                &settings,
                None,
                mode,
                Vec::new(),
            )
            .await
            {
                Ok(Some(checkpoint)) => {
                    tracing::debug!(
                        before_messages = before,
                        after_messages = messages.len(),
                        tokens_before = checkpoint.window_tokens_before,
                        tokens_after = checkpoint.window_tokens_after,
                        "mid-turn compaction folded the window"
                    );
                }
                Ok(None) => {
                    tracing::debug!("mid-turn compaction found nothing to fold");
                }
                Err(error) => {
                    tracing::warn!(%error, "mid-turn compaction failed; keeping the pruned window");
                }
            }
        }

        let tokens_after = nuo_protocol::pressure::estimate_tokens(&messages);
        // Returning `Some` replaces the live list, so only claim a replacement
        // when something actually changed. `Some` with an identical window would
        // force a pointless request re-assembly every turn.
        if tokens_after < tokens_before || messages.len() != before {
            Some(messages)
        } else {
            None
        }
    }
}
