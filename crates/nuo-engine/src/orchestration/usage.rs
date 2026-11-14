//! Token accounting, usage persistence, and off-executor estimation.

use super::*;

pub(super) async fn send_context_projection(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Arc<Agent>,
    messages: &[Message],
) {
    let started = std::time::Instant::now();
    let estimate = estimate_off_executor(agent, messages).await;
    if started.elapsed() >= std::time::Duration::from_millis(250) {
        tracing::warn!(session = %session_id,
            duration_ms = started.elapsed().as_millis() as u64,
            "slow local final context projection");
    }
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::ContextTokens(nuo_protocol::ContextTokenSnapshot::from_estimate(
            estimate,
            nuo_protocol::ContextTokenSource::Projection,
        )),
    ));
}

pub(super) async fn persist_request_usage(
    agent: &Agent,
    session: &ThreadSession,
    session_id: &str,
) -> Result<(), String> {
    let Some(ledger) = agent.token_ledger() else {
        return Ok(());
    };
    session
        .set_request_usage_records(ledger.pending_records_for_session(session_id))
        .await
}

/// Run one full request estimate on the blocking pool. `Agent` is `Send +
/// Sync`; the model-request assembly and BPE tokenization are pure CPU work
/// over immutable inputs, so `spawn_blocking` is safe and keeps the async
/// executor free for stream forwarding and UI. Falls back to inline execution
/// only if the runtime is shutting down (the round is tearing down anyway).
pub(super) async fn estimate_off_executor(
    agent: &Arc<Agent>,
    messages: &[Message],
) -> RequestTokenEstimate {
    let agent = Arc::clone(agent);
    let snapshot = messages.to_vec();
    tokio::task::spawn_blocking(move || agent.estimate_next_request_tokens(&snapshot))
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "estimate task aborted; treating pressure as zero");
            RequestTokenEstimate::new(0, 0)
        })
}

/// Session-weight estimate (nested subagent children included — the
/// pressure/prune number, **not** the wire estimate) on the blocking pool,
/// through the shared content-addressed weights cache. Companion to
/// [`estimate_off_executor`]: BPE tokenization never runs on the async
/// executor, and repeated passes pay O(new bytes), not O(session). A
/// panicked/aborted task reads as `0` (no pressure), matching
/// `estimate_off_executor`'s fallback.
pub(super) async fn estimate_session_weight_off_executor(
    weights: Arc<nuo_protocol::MessageTokenWeights>,
    messages: &[Message],
) -> usize {
    let snapshot = messages.to_vec();
    tokio::task::spawn_blocking(move || nuo_protocol::estimate_tokens_weighted(&snapshot, &weights))
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "session-weight estimate task aborted; treating as zero");
            0
        })
}
