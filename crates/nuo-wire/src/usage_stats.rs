//! Cross-session usage statistics: the durable, day-partitioned mirror of the
//! per-session token ledger (ADR-0122).
//!
//! The [`crate::TokenSourceLedger`] answers "what did *this* session use?" and
//! its records live inside the session file — deleted with the session. This
//! module is the **sibling** store: an append-only stream of terminal request
//! records, partitioned one file per local day, persisted under the
//! data directory (`usage/daily/<YYYY-MM-DD>.json`) next to — never inside —
//! the project session buckets. Clearing session history can never touch it,
//! so the `/usage` report reflects every day's real consumption forever.
//!
//! The store is append-mostly and idempotent: each record carries its
//! [`crate::RequestUsageKey`] and a `recorded_at_ms` timestamp; replaying the same
//! key is a no-op (crash-safe retries never double count), and a later replay
//! with a *stronger* source (reported usage arriving after an estimate was
//! already persisted) upgrades the existing row in place.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::token_ledger::{RequestUsageRecord, RequestUsageSource, RequestUsageStatus};
/// Local-timezone `YYYY-MM-DD` bucket key for a wall-clock instant.
///
/// Day boundaries follow the user's local timezone (`chrono::Local`) because
/// "daily usage" is a human concept — a day is when the user says it ended,
/// not 00:00 UTC.
pub fn day_key_from_epoch_ms(epoch_ms: u64) -> String {
    use chrono::TimeZone;
    let secs = (epoch_ms / 1_000) as i64;
    let nanos = ((epoch_ms % 1_000) * 1_000_000) as u32;
    chrono::Local
        .timestamp_opt(secs, nanos)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// One terminal request attempt as persisted into the usage store. A thin
/// wrapper over [`RequestUsageRecord`] (flattened onto the wire) that adds
/// the wall-clock arrival time (the record itself only carries relative
/// counters) and the project bucket the request ran in. The attempt's
/// identity is the flattened `record.key` — unique within a day file, so
/// replays are idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStatRecord {
    /// Local `YYYY-MM-DD` the record was first booked under.
    pub day: String,
    /// Wall-clock epoch milliseconds the attempt terminally settled
    /// (completed / interrupted / failed). Drives the event-log view.
    pub recorded_at_ms: u64,
    /// Project bucket name (produced by the persistence layer's
    /// `project_bucket_name`) the session belonged to, so the report can
    /// group by project without leaking absolute paths. Empty when the
    /// recorder could not resolve a project root.
    #[serde(default)]
    pub project: String,
    #[serde(flatten)]
    pub record: RequestUsageRecord,
}

impl UsageStatRecord {
    /// Whether this record carries authoritative provider-reported counts.
    pub fn is_reported(&self) -> bool {
        self.record.source == RequestUsageSource::Reported
    }

    /// Whether the attempt ended in a way that consumed generation (i.e. any
    /// terminal state; in-flight records are never persisted here).
    pub fn is_terminal(&self) -> bool {
        self.record.status.is_terminal()
    }
}

/// Per-`(provider, model)` totals over an aggregation window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageModelTotals {
    /// Terminal request attempts booked (completed + interrupted + failed).
    pub requests: u64,
    /// Attempts that reached a validated assistant response.
    pub completed: u64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    /// Tokens from attempts the provider never reported (local estimate).
    pub estimated_tokens: i64,
}

impl UsageModelTotals {
    fn add_record(&mut self, entry: &UsageStatRecord) {
        self.requests += 1;
        if entry.record.status == RequestUsageStatus::Completed {
            self.completed += 1;
        }
        match entry.record.source {
            RequestUsageSource::Reported => {
                self.prompt_tokens += entry.record.prompt_tokens.max(0);
                self.completion_tokens += entry.record.completion_tokens.max(0);
                self.total_tokens += entry.record.total_tokens.max(0);
                self.cache_write_tokens += entry.record.cache_write_tokens.max(0);
                self.cache_read_tokens += entry.record.cache_read_tokens.max(0);
            }
            RequestUsageSource::Estimated | RequestUsageSource::Unknown => {
                self.estimated_tokens += entry.record.total_tokens.max(0);
            }
        }
    }

    /// Grand total across reported + estimated tokens.
    pub fn grand_total(&self) -> i64 {
        self.total_tokens.saturating_add(self.estimated_tokens)
    }
}

/// One day's aggregate row (the per-day table in the `/usage` report).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageDayTotals {
    /// Local `YYYY-MM-DD`.
    pub day: String,
    #[serde(flatten)]
    pub totals: UsageModelTotals,
}

/// The user-facing usage report, aggregated over the whole store (all days).
///
/// Built by the persistence layer from the day files; rendered by the TUI's
/// `/usage` overlay and serialisable so the Web app can reuse it over the
/// control plane ([`crate::AgentResponse::UsageStatsReport`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStatsReport {
    /// Per-day totals, oldest first.
    pub days: Vec<UsageDayTotals>,
    /// Per-`(provider, model)` totals across all days, sorted by descending
    /// grand total.
    pub models: Vec<UsageModelRow>,
    /// Grand totals across every day.
    pub grand_total: UsageModelTotals,
    /// The most recent terminal records (newest last), for the event log
    /// view. Capped by the query.
    pub events: Vec<UsageStatRecord>,
    /// First and last day keys present in the store (empty store → `None`).
    pub first_day: Option<String>,
    pub last_day: Option<String>,
    #[serde(default)]
    pub source_revision: u64,
    #[serde(default)]
    pub projection_revision: u64,
}

/// One provider+model row of the model breakdown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageModelRow {
    pub provider: String,
    pub model: String,
    #[serde(flatten)]
    pub totals: UsageModelTotals,
}

/// Aggregate a slice of records into the full report shape. Exposed so the
/// store tests and any future importer (e.g. backfilling from old session
/// files) share one definition of the aggregates.
pub fn aggregate_usage_records(records: &[UsageStatRecord], event_cap: usize) -> UsageStatsReport {
    let mut day_map: BTreeMap<String, UsageModelTotals> = BTreeMap::new();
    let mut model_map: BTreeMap<(String, String), UsageModelTotals> = BTreeMap::new();
    let mut grand = UsageModelTotals::default();
    for entry in records {
        let day_entry = day_map.entry(entry.day.clone()).or_default();
        day_entry.add_record(entry);
        let model_entry = model_map
            .entry((entry.record.provider.clone(), entry.record.model.clone()))
            .or_default();
        model_entry.add_record(entry);
        grand.add_record(entry);
    }
    let mut models: Vec<UsageModelRow> = model_map
        .into_iter()
        .map(|((provider, model), totals)| UsageModelRow {
            provider,
            model,
            totals,
        })
        .collect();
    models.sort_by_key(|row| std::cmp::Reverse(row.totals.grand_total()));
    let days: Vec<UsageDayTotals> = day_map
        .into_iter()
        .map(|(day, totals)| UsageDayTotals { day, totals })
        .collect();
    let mut events: Vec<UsageStatRecord> = records.to_vec();
    events.sort_by_key(|event| event.recorded_at_ms);
    if events.len() > event_cap {
        let start = events.len() - event_cap;
        events.drain(..start);
    }
    UsageStatsReport {
        first_day: days.first().map(|d| d.day.clone()),
        last_day: days.last().map(|d| d.day.clone()),
        source_revision: 0,
        projection_revision: 0,
        days,
        models,
        grand_total: grand,
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_ledger::RequestUsageKey;

    fn entry(
        day: &str,
        provider: &str,
        model: &str,
        total: i64,
        reported: bool,
    ) -> UsageStatRecord {
        UsageStatRecord {
            day: day.to_string(),
            recorded_at_ms: 1_700_000_000_000,
            project: "abc123".to_string(),
            record: RequestUsageRecord {
                key: RequestUsageKey::default(),
                provider: provider.to_string(),
                model: model.to_string(),
                status: RequestUsageStatus::Completed,
                source: if reported {
                    RequestUsageSource::Reported
                } else {
                    RequestUsageSource::Estimated
                },
                prompt_tokens: if reported { total - 100 } else { 0 },
                completion_tokens: if reported { 100 } else { 0 },
                total_tokens: total,
                ..Default::default()
            },
        }
    }

    #[test]
    fn aggregates_days_models_and_grand_total() {
        let records = vec![
            entry("2026-08-19", "anthropic", "claude-sonnet", 1_000, true),
            entry("2026-08-19", "openai", "gpt-5", 500, true),
            entry("2026-08-20", "anthropic", "claude-sonnet", 2_000, true),
            entry("2026-08-20", "openai", "gpt-5", 300, false),
        ];
        let report = aggregate_usage_records(&records, 10);
        assert_eq!(report.days.len(), 2);
        assert_eq!(report.days[0].day, "2026-08-19");
        assert_eq!(report.days[0].totals.total_tokens, 1_500);
        assert_eq!(report.days[1].totals.total_tokens, 2_000);
        assert_eq!(report.days[1].totals.estimated_tokens, 300);
        // Model rows sorted by descending grand total.
        assert_eq!(report.models.len(), 2);
        assert_eq!(report.models[0].provider, "anthropic");
        assert_eq!(report.models[0].totals.grand_total(), 3_000);
        assert_eq!(report.models[1].totals.grand_total(), 800);
        assert_eq!(report.grand_total.requests, 4);
        assert_eq!(report.grand_total.completed, 4);
        assert_eq!(report.grand_total.total_tokens, 3_500);
        assert_eq!(report.grand_total.estimated_tokens, 300);
        assert_eq!(report.first_day.as_deref(), Some("2026-08-19"));
        assert_eq!(report.last_day.as_deref(), Some("2026-08-20"));
    }

    #[test]
    fn event_cap_keeps_newest() {
        let mut records: Vec<UsageStatRecord> = (0..10)
            .map(|i| {
                let mut e = entry("2026-08-20", "openai", "gpt-5", 10, true);
                e.recorded_at_ms = 1_000 + i;
                e
            })
            .collect();
        records.reverse();
        let report = aggregate_usage_records(&records, 3);
        assert_eq!(report.events.len(), 3);
        assert_eq!(report.events[0].recorded_at_ms, 1_007);
        assert_eq!(report.events[2].recorded_at_ms, 1_009);
    }

    #[test]
    fn day_key_uses_local_calendar_day() {
        let key = day_key_from_epoch_ms(1_700_000_000_000);
        assert_eq!(key.len(), 10);
        assert_eq!(key.as_bytes()[4], b'-');
        assert_eq!(key.as_bytes()[7], b'-');
    }

    #[test]
    fn interrupted_attempt_counts_as_request_not_completed() {
        let mut e = entry("2026-08-20", "openai", "gpt-5", 700, true);
        e.record.status = RequestUsageStatus::Interrupted;
        e.record.total_tokens = 700;
        let report = aggregate_usage_records(std::slice::from_ref(&e), 10);
        assert_eq!(report.grand_total.requests, 1);
        assert_eq!(report.grand_total.completed, 0);
        assert_eq!(report.grand_total.total_tokens, 700);
    }
}
