//! Ledger reports and latest-turn projections.

use super::*;

pub(super) fn request_display_order(record: &RequestUsageRecord) -> (u64, u8, u32, u32, &str) {
    (
        record.key.round,
        u8::from(!is_root_actor(&record.key.actor_id)),
        record.key.turn,
        record.key.attempt,
        record.key.actor_id.as_str(),
    )
}

/// Wall-clock epoch milliseconds. Kept here (rather than at each call site)
/// so the usage-stat day bucket derives from one definition of "now".
pub(super) fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One row of the report: a single provider+model and its source split.
///
/// Serialisable so an attached frontend can receive the server-side report
/// over the wire ([`crate::AgentResponse::TokenUsageReport`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceRow {
    pub provider: String,
    pub model: String,
    pub totals: TokenSourceTotals,
    /// The ordered per-turn line items behind `totals`.
    pub turns: Vec<TokenTurn>,
    /// Lifecycle-aware attempts behind this provider/model row.
    pub requests: Vec<RequestUsageRecord>,
}

/// A full snapshot of the ledger: per-row breakdown + a grand total.
///
/// Serialisable so an attached frontend can receive the server-side report
/// over the wire ([`crate::AgentResponse::TokenUsageReport`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceReport {
    pub rows: Vec<TokenSourceRow>,
    pub grand_total: TokenSourceTotals,
}

impl TokenSourceReport {
    /// Most recent completed primary-model attempt carrying structured
    /// performance telemetry. Subagent actors are deliberately excluded from
    /// the session hint: it describes the principal conversation's last
    /// model turn.
    pub fn latest_turn_performance(&self) -> Option<TurnPerformanceSnapshot> {
        self.rows
            .iter()
            .flat_map(|row| row.requests.iter())
            .filter(|record| {
                is_root_actor(&record.key.actor_id)
                    && record.status == RequestUsageStatus::Completed
                    && record.performance.is_some()
            })
            .max_by_key(|record| (record.key.round, record.key.turn, record.key.attempt))
            .and_then(RequestUsageRecord::performance_snapshot)
    }
}

/// Slice counterpart of [`TokenSourceReport::latest_turn_performance`] for
/// attach/resume paths that already hold the durable request records.
pub fn latest_turn_performance(records: &[RequestUsageRecord]) -> Option<TurnPerformanceSnapshot> {
    records
        .iter()
        .filter(|record| {
            is_root_actor(&record.key.actor_id)
                && record.status == RequestUsageStatus::Completed
                && record.performance.is_some()
        })
        .max_by_key(|record| (record.key.round, record.key.turn, record.key.attempt))
        .and_then(RequestUsageRecord::performance_snapshot)
}
