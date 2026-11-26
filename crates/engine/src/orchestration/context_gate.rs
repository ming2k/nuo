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
use std::sync::Arc;
use tool::Message;

use crate::cognition::compaction::CompactionCutMode;
use crate::cognition::context_projection::{ContextProjectionGate, ContextProjectionOutcome};
use crate::orchestration::{
    ContextProjectionSettings, compact_round_history_with_mode, prune_and_commit,
};
use persistence::thread::ThreadSession;

/// Supplies the settings for the live model on demand.
///
/// A closure rather than a value so the gate never pins a model's window; the
/// composition root owns the lookup and can answer with the current provider.
pub type SettingsForWindow = Arc<dyn Fn() -> ContextProjectionSettings + Send + Sync>;

/// Applies the configured projection policy to a live window between turns.
pub struct PruningProjectionGate {
    session: Arc<ThreadSession>,
    settings: SettingsForWindow,
    weights: Arc<protocol::MessageTokenWeights>,
    /// The live provider used for model-assisted mid-turn compaction. `None`
    /// keeps the deterministic excerpt fallback (headless embeddings and tests
    /// that intentionally have no model).
    provider: Option<Arc<dyn protocol::Provider>>,
}

impl PruningProjectionGate {
    pub fn new(
        session: Arc<ThreadSession>,
        settings: SettingsForWindow,
        weights: Arc<protocol::MessageTokenWeights>,
        provider: Option<Arc<dyn protocol::Provider>>,
    ) -> Self {
        Self {
            session,
            settings,
            weights,
            provider,
        }
    }
}

#[async_trait]
impl ContextProjectionGate for PruningProjectionGate {
    async fn project_context(
        &self,
        mut messages: Vec<Message>,
        committed_len: usize,
    ) -> Option<ContextProjectionOutcome> {
        let settings = (self.settings)();
        let before = messages.len();
        let mut committed_len = committed_len.min(before);
        let tokens_before = protocol::pressure::estimate_tokens_weighted(&messages, &self.weights);

        // Tier 1: clear stale tool bodies. In place, wire-compatible, no message
        // removed — so a failure here leaves the window and committed cursor untouched.
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
        let mut compacted = None;
        let tokens_after_prune =
            protocol::pressure::estimate_tokens_weighted(&messages, &self.weights);
        if tokens_after_prune > settings.budget.prune_threshold_tokens {
            let mode = CompactionCutMode::TargetTokens(settings.budget.target_tokens.max(1));
            match compact_round_history_with_mode(
                &mut messages,
                committed_len,
                &self.session,
                &settings,
                self.provider.clone(),
                mode,
                Vec::new(),
            )
            .await
            {
                Ok(Some(report)) => {
                    committed_len = report.committed_len;
                    tracing::debug!(
                        before_messages = before,
                        after_messages = messages.len(),
                        committed_len,
                        tokens_before = report.checkpoint.window_tokens_before,
                        tokens_after = report.checkpoint.window_tokens_after,
                        satisfied = report.satisfied,
                        "mid-turn compaction folded the window"
                    );
                    compacted = Some(report.checkpoint);
                }
                Ok(None) => {
                    tracing::debug!("mid-turn compaction found nothing to fold");
                }
                Err(error) => {
                    tracing::warn!(%error, "mid-turn compaction failed; keeping the pruned window");
                }
            }
        }

        let tokens_after = protocol::pressure::estimate_tokens_weighted(&messages, &self.weights);
        // Returning `Some` replaces the live list, so only claim a replacement
        // when something actually changed. `Some` with an identical window would
        // force a pointless request re-assembly every turn.
        if tokens_after < tokens_before || messages.len() != before {
            Some(ContextProjectionOutcome {
                messages,
                committed_len,
                compacted,
            })
        } else {
            None
        }
    }

    /// Relieve a request whose body is over the transport's byte ceiling.
    ///
    /// This is **not** a compaction tier. Compaction folds history to fit a
    /// token window and would happily drop the rounds the user is working in;
    /// a body-size refusal is caused by payload, and the payload's dominant
    /// term is attachments. So the ladder here is:
    ///
    /// 1. persist each attachment the plan selects to its content-addressed
    ///    path, because the invoice is about to name that path;
    /// 2. replace those attachments with the invoice (text preserved verbatim);
    /// 3. record the projection so the durable window and the next round agree,
    ///    instead of re-deriving the same eviction every turn.
    ///
    /// The newest attachments are never candidates
    /// ([`protocol::PAYLOAD_KEEP_NEWEST_IMAGES`]): the conversation is usually
    /// *about* them, and the model can still read an older one back through the
    /// invoice's artifact path.
    async fn project_payload(
        &self,
        mut messages: Vec<Message>,
        media_target_bytes: usize,
    ) -> Option<Vec<Message>> {
        let media_before = protocol::pressure::media_bytes(&messages);
        if media_before <= media_target_bytes {
            return None;
        }
        let plan = protocol::pressure::plan_media_offload(
            &messages,
            media_before,
            media_target_bytes,
            protocol::pressure::PAYLOAD_KEEP_NEWEST_IMAGES,
        );
        if plan.is_empty() {
            tracing::debug!(
                media_before,
                media_target_bytes,
                "payload relief found no attachment it may offload"
            );
            return None;
        }
        // Durable before dropped: the invoice is about to print these paths, so
        // an item whose bytes could not be persisted is not eligible.
        let plan = crate::orchestration::persist_media_artifacts(plan).await;
        if plan.is_empty() {
            tracing::warn!(
                media_before,
                media_target_bytes,
                "no attachment could be persisted; keeping the request intact"
            );
            return None;
        }
        let offloaded_messages = plan.len();
        let window_tokens_before =
            protocol::pressure::estimate_tokens_weighted(&messages, &self.weights);
        let outcome = protocol::pressure::apply_media_offload(&mut messages, plan);
        let media_after = protocol::pressure::media_bytes(&messages);
        let window_tokens_after =
            protocol::pressure::estimate_tokens_weighted(&messages, &self.weights);
        tracing::warn!(
            offloaded_images = outcome.offloaded_images,
            offloaded_messages,
            reclaimed_bytes = outcome.reclaimed_bytes,
            media_before,
            media_after,
            media_target_bytes,
            "offloaded attachments to fit the request-body ceiling"
        );
        if let Err(error) = self
            .session
            .commit_context_projection(protocol::ContextProjectionResult {
                model_window: messages.clone(),
                archived_originals: outcome.originals,
                checkpoint: protocol::ContextProjectionCheckpoint {
                    operation: protocol::ContextProjectionKind::Payload,
                    archived_messages: offloaded_messages,
                    active_messages: messages.len(),
                    window_tokens_before,
                    window_tokens_after,
                    summary: None,
                    tracked_files: Vec::new(),
                },
            })
            .await
        {
            tracing::warn!(%error, "could not commit the payload projection");
        }
        Some(messages)
    }
}
