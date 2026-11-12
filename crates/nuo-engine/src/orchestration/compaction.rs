//! History compaction: summaries, pruning, and the commit path.

use super::projection::compaction_budgets;
use super::usage::estimate_session_weight_off_executor;
use super::*;

pub async fn compact_round_history(
    history: &mut Vec<Message>,
    session: &SessionStore,
    settings: &ContextProjectionSettings,
    provider: Option<Arc<dyn Provider>>,
    extra_context: Vec<String>,
) -> Result<Option<CompactionReport>, String> {
    // Automatic compaction is budget-driven: it must shrink the window *to* the
    // post-compaction target. `PreserveTailRounds` cannot do that — it keeps a
    // fixed number of rounds verbatim regardless of their size, so on a session
    // whose last six rounds exceed the target it folds a few nodes, adds a
    // summary, and hands back a window that is *larger* than the one it was
    // called to relieve. `TargetTokens` is the mode that bounds the tail by the
    // budget, which is what `CompactionCutMode`'s own documentation reserves for
    // this backstop role; `/compact` keeps the round-shaped variant.
    let (target_tokens, _) = compaction_budgets(settings);
    compact_round_history_with_mode(
        history,
        session,
        settings,
        provider,
        crate::cognition::compaction::CompactionCutMode::TargetTokens(target_tokens),
        extra_context,
    )
    .await
}

pub async fn compact_round_history_with_mode(
    history: &mut Vec<Message>,
    session: &SessionStore,
    settings: &ContextProjectionSettings,
    provider: Option<Arc<dyn Provider>>,
    mode: crate::cognition::compaction::CompactionCutMode,
    extra_context: Vec<String>,
) -> Result<Option<CompactionReport>, String> {
    let provider = if settings.summarize { provider } else { None };
    let (_, summary_budget) = compaction_budgets(settings);

    // Canonical path: Session IR Native Causal Compaction
    //
    // The window the caller is actually holding — that is the number an operator
    // reads off the compression card, and the only one that answers "did this
    // shrink my context?". `outcome.tokens_before` is the folded *prefix*, which
    // on any real session is a small fraction of the window and made every
    // successful fold look like it reclaimed nothing.
    let window_tokens_before = nuo_protocol::pressure::estimate_tokens(history);
    let mut ir = session.session_ir().await;
    // Entries the caller holds but the session has not committed yet, resolved
    // against the **pre-compaction** IR. The live round's history is
    // `model_window()` plus the prompt being admitted right now, so seeding the
    // window from storage and assigning it back wholesale would drop that prompt
    // — the model would answer the previous turn's question.
    //
    // Resolved before folding because the anchor is a property of the caller's
    // state, not of the fold's outcome. It happens to agree afterwards too (the
    // cut point always retains at least one node), but deriving it from the
    // window that has already been rewritten is the kind of coupling that breaks
    // silently the first time the retention floor changes.
    let uncommitted = uncommitted_tail(history, &ir);
    if let Some(outcome) = crate::cognition::compaction::CausalCompactor::compact_session_ir(
        &mut ir,
        provider,
        mode,
        extra_context,
        summary_budget,
    )
    .await?
    {
        let mut active_msgs = ir.resolve_active_messages();
        active_msgs.extend(uncommitted);
        let tokens_after = nuo_protocol::pressure::estimate_tokens(&active_msgs);
        let checkpoint = ContextProjectionCheckpoint {
            operation: nuo_persistence::session::ContextProjectionKind::Compact,
            archived_messages: outcome.nodes_folded,
            active_messages: active_msgs.len(),
            window_tokens_before,
            window_tokens_after: tokens_after,
            summary: Some(outcome.summary.clone()),
            tracked_files: outcome.tracked_files,
        };
        let meta = nuo_protocol::CompactionMeta {
            first_kept_node_id: outcome.first_kept_node_id.clone(),
            split_parent_node_id: outcome.split_parent_node_id.clone(),
            folded_through_seq: outcome.folded_through_seq,
            tokens_before: outcome.tokens_before,
            read_files: outcome.read_files.clone(),
            modified_files: outcome.modified_files.clone(),
        };
        // One write. The checkpoint entry is the fact; the causal node is
        // derived from it by the next drain. The in-memory IR already carries
        // the node under the same id (the compactor appended it), so the
        // durable and in-memory horizons agree by construction.
        let committed = session
            .commit_compaction(
                outcome.compaction_node_id.clone(),
                &outcome.summary,
                meta,
                checkpoint.clone(),
            )
            .await?;
        if committed.is_none() {
            // The fold has no transcript entry to hide (an IR-only boundary).
            // Handing the caller a folded window that cannot be persisted would
            // silently revert on the next round; keep the window it had.
            let session_id = session.id().await;
            tracing::warn!(
                session = %session_id,
                folded_through_seq = outcome.folded_through_seq,
                "compaction found no durable transcript boundary; keeping the unfolded window"
            );
            return Ok(None);
        }
        let gate = settings.budget.compaction_threshold_tokens;
        let satisfied =
            gate == 0 || tokens_after.saturating_add(settings.request_overhead_tokens) <= gate;
        if !satisfied {
            tracing::warn!(
                tokens_after,
                gate,
                overhead = settings.request_overhead_tokens,
                folded = outcome.nodes_folded,
                "compaction could not bring the window under the gate"
            );
        }
        *history = active_msgs;
        Ok(Some(CompactionReport {
            checkpoint,
            satisfied,
        }))
    } else {
        Ok(None)
    }
}

/// The messages a caller holds that the committed IR does not.
///
/// Matched on wire identity from the end of the caller's history, so a session
/// whose IR was persisted mid-round keeps exactly the tail it has not committed.
/// An empty result means the caller's view is fully durable.
fn uncommitted_tail(history: &[Message], ir: &nuo_protocol::SessionIR) -> Vec<Message> {
    let committed = ir.resolve_active_messages();
    if committed.is_empty() {
        return history.to_vec();
    }
    // Walk back from the end of `history` looking for the last committed
    // message; everything after it is uncommitted.
    let anchor = committed.last();
    let split = history
        .iter()
        .rposition(|message| Some(message) == anchor)
        .map(|index| index + 1)
        .unwrap_or_else(|| {
            // The committed tail is not present in the caller's window (a
            // projection removed it): fall back to appending nothing rather than
            // duplicating the whole window.
            history.len()
        });
    history[split..].to_vec()
}

/// Prune old tool results in place and durably commit the change. Pruning is an
/// implicit model-context projection step: it keeps the conversation and the
/// `tool_call_id` chain intact (only stale tool *bodies* are cleared), so unlike
/// a summarizing compaction it does **not** surface a transcript notice — it
/// only records a durable checkpoint and a `debug` trace for observability.
///
/// The before/after session-weight estimates (children included — the pressure
/// number, not the wire estimate) run through the shared weights cache on the
/// blocking pool, per the same executor discipline as `estimate_session_weight_off_executor`.
pub async fn prune_and_commit(
    history: &mut [Message],
    session: &SessionStore,
    settings: &ContextProjectionSettings,
    weights: Arc<nuo_protocol::MessageTokenWeights>,
) -> Result<(), String> {
    let window_tokens_before =
        estimate_session_weight_off_executor(Arc::clone(&weights), history).await;
    // Damper 1: Quantum Reclaim Gate ($Q_{min} >= 4,000 tokens)
    // Strictly forbids micro-pruning for negligible gains.
    let min_reclaim = settings.budget.quantum_floor_tokens;
    let Some(outcome) =
        nuo_protocol::prune_tool_results(history, settings.prune_protect_tokens, min_reclaim)
    else {
        return Ok(());
    };
    let window_tokens_after =
        estimate_session_weight_off_executor(Arc::clone(&weights), history).await;
    let checkpoint = ContextProjectionCheckpoint {
        operation: nuo_persistence::session::ContextProjectionKind::Prune,
        archived_messages: outcome.originals.len(),
        active_messages: history.len(),
        window_tokens_before,
        window_tokens_after,
        summary: None,
        tracked_files: Vec::new(),
    };
    tracing::debug!(
        pruned_tool_results = checkpoint.archived_messages,
        window_tokens_before,
        window_tokens_after,
        reclaimed_tokens = outcome.reclaimed_tokens,
        "pruned tool results under budget pressure"
    );
    session
        .commit_context_projection(ContextProjectionResult {
            model_window: history.to_owned(),
            archived_originals: outcome.originals,
            checkpoint,
        })
        .await
}

pub fn send_compaction(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    checkpoint: &ContextProjectionCheckpoint,
) {
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::Compacted {
            archived_messages: checkpoint.archived_messages,
            window_tokens_before: checkpoint.window_tokens_before,
            window_tokens_after: checkpoint.window_tokens_after,
            summary: checkpoint.summary.clone(),
            tracked_files: checkpoint.tracked_files.clone(),
        },
    ));
}
