//! History compaction: summaries, pruning, and the commit path.

use super::projection::compaction_budgets;
use super::usage::estimate_session_weight_off_executor;
use super::*;

pub async fn compact_round_history(
    history: &mut Vec<Message>,
    session: &ThreadSession,
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
    session: &ThreadSession,
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
    let mut thread = session.thread().await;
    let branch = thread.state.active_branch.clone();
    let uncommitted = uncommitted_tail(history, &thread, &branch);
    if let Some(outcome) = crate::cognition::compaction::CausalCompactor::compact_thread(
        &mut thread,
        &branch,
        provider,
        mode,
        extra_context,
        summary_budget,
    )
    .await?
    {
        let window = nuo_protocol::ThreadWindow::from_thread(&thread, &branch, usize::MAX)
            .map_err(|error| error.to_string())?;
        let mut active_msgs = nuo_protocol::RequestView::from_window(&window).messages;
        active_msgs.extend(uncommitted);
        let tokens_after = nuo_protocol::pressure::estimate_tokens(&active_msgs);
        let checkpoint = ContextProjectionCheckpoint {
            operation: nuo_persistence::thread::ContextProjectionKind::Compact,
            archived_messages: outcome.nodes_folded,
            active_messages: active_msgs.len(),
            window_tokens_before,
            window_tokens_after: tokens_after,
            summary: Some(outcome.summary.clone()),
            tracked_files: outcome.tracked_files,
        };
        session.commit_thread(&thread).await?;
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
fn uncommitted_tail(
    history: &[Message],
    thread: &nuo_protocol::Thread,
    branch: &nuo_protocol::BranchId,
) -> Vec<Message> {
    let Ok(window) = nuo_protocol::ThreadWindow::from_thread(thread, branch, usize::MAX) else {
        return Vec::new();
    };
    let committed = nuo_protocol::RequestView::from_window(&window).messages;
    if committed.is_empty() {
        return history.to_vec();
    }
    let anchor = committed.last();
    let split = history
        .iter()
        .rposition(|message| Some(message) == anchor)
        .map(|index| index + 1)
        .unwrap_or(history.len());
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
    session: &ThreadSession,
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
        operation: nuo_persistence::thread::ContextProjectionKind::Prune,
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
