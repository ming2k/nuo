//! Harness-state snapshots for running sessions.

use super::*;

pub fn send_harness_state_running(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
) {
    let round_counter = agent.round_count().saturating_add(1);
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::HarnessState(HarnessSnapshot {
            loop_status: LoopStatus::Running,
            round_counter,
            unattended: agent.unattended(),
            confined: agent.is_confined(),
            workspace_security: agent.workspace_security(),
            retry_pending: false,
            role: agent.active_role(),
            workspace: agent
                .workspace_root()
                .map(|p| p.to_string_lossy().to_string()),
        }),
    ));
}

/// Publish the authoritative [`HarnessSnapshot`] for a session.
///
/// Queries the durable store for the `/retry` affordance (`retry_pending`)
/// when idle, ensuring the projection stays strictly consistent with the
/// underlying session state.
pub async fn send_harness_state_for_session(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
    session: &ThreadSession,
    loop_status: LoopStatus,
) {
    // Running snapshots are emitted after lifecycle admission but
    // immediately before `execute_round` performs the counter bump. Project
    // that admitted round here so frontends receive the authoritative display
    // value without locally guessing from transcript length.
    let round_counter = agent
        .round_count()
        .saturating_add(u64::from(!loop_status.is_idle()));
    let retry_pending = loop_status.is_idle() && session.retry_pending().await.is_some();
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::HarnessState(HarnessSnapshot {
            loop_status,
            round_counter,
            unattended: agent.unattended(),
            confined: agent.is_confined(),
            workspace_security: agent.workspace_security(),
            retry_pending,
            role: agent.active_role().or_else(|| session.role()),
            workspace: session
                .workspace()
                .map(|w| w.root.to_string_lossy().to_string())
                .or_else(|| {
                    agent
                        .workspace_root()
                        .map(|p| p.to_string_lossy().to_string())
                }),
        }),
    ));
    if let Some(performance) =
        nuo_protocol::latest_turn_performance(&session.request_usage_records().await)
    {
        let _ = tx.send(round_response(
            session_id,
            RoundEvent::TurnPerformance(performance),
        ));
    }
}
