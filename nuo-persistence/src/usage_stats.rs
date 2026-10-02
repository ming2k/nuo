//! Authoritative request-attempt accounting and recoverable usage projections (ADR-0236).
//! No production read imports legacy files, and no settlement rewrites a day bucket.
use nuo_contracts::usage_stats::{UsageStatRecord, UsageStatsReport, day_key_from_epoch_ms};
use nuo_contracts::{RequestUsageKey, RequestUsageRecord};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct UsageStatsStore {
    #[allow(dead_code)]
    root: Option<PathBuf>,
    handle: Option<crate::db::PersistenceHandle>,
}
impl UsageStatsStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_root(root: PathBuf) -> Self {
        let root = root.join("usage");
        let handle = crate::db::PersistenceHandle::spawn(root.join("usage.db"), None);
        Self {
            root: Some(root),
            handle: Some(handle),
        }
    }
    fn handle(&self) -> crate::db::PersistenceHandle {
        self.handle
            .clone()
            .unwrap_or_else(crate::db::get_persistence_handle)
    }
    pub fn record(
        &self,
        at: u64,
        project: &str,
        record: &RequestUsageRecord,
    ) -> Result<(), String> {
        self.record_batch(&[(at, project, record.clone())])
    }
    pub fn record_batch(&self, entries: &[(u64, &str, RequestUsageRecord)]) -> Result<(), String> {
        let records = entries
            .iter()
            .filter(|(_, _, r)| r.status.is_terminal())
            .map(|(at, p, r)| UsageStatRecord {
                day: day_key_from_epoch_ms(*at),
                recorded_at_ms: *at,
                project: p.to_string(),
                record: r.clone(),
            })
            .collect();
        self.handle()
            .record_usage_stats_blocking(records)
            .map_err(|e| e.to_string())
    }
    pub async fn persist_attempt(
        &self,
        at: u64,
        project: &str,
        record: RequestUsageRecord,
    ) -> Result<(), String> {
        self.handle()
            .record_attempts(vec![UsageStatRecord {
                day: day_key_from_epoch_ms(at),
                recorded_at_ms: at,
                project: project.into(),
                record,
            }])
            .await
            .map_err(|e| e.to_string())
    }
    pub fn all_records(&self) -> Vec<UsageStatRecord> {
        self.handle()
            .reader()
            .and_then(|r| r.usage_records(400, usize::MAX))
            .unwrap_or_else(|e| {
                tracing::warn!(%e,"usage read failed");
                Vec::new()
            })
    }
    pub fn report(&self, event_cap: usize) -> UsageStatsReport {
        let handle = self.handle();
        // On-demand bounded catch-up; never called from turn completion.
        if let Err(error) = handle.catch_up_usage() {
            tracing::warn!(%error,"usage projection catch-up failed");
        }
        handle
            .reader()
            .and_then(|r| r.usage_report(400, event_cap))
            .unwrap_or_else(|e| {
                tracing::warn!(%e,"usage report failed");
                UsageStatsReport::default()
            })
    }
    pub fn prune_old_days(&self) -> usize {
        0
    }
}
pub fn day_key(at: u64) -> String {
    day_key_from_epoch_ms(at)
}
pub fn same_attempt(a: &RequestUsageKey, b: &RequestUsageKey) -> bool {
    a == b
}
impl nuo_contracts::UsageStatSink for UsageStatsStore {
    fn persist_usage<'a>(
        &'a self,
        at: u64,
        project: &'a str,
        record: RequestUsageRecord,
    ) -> futures::future::BoxFuture<'a, Result<(), String>> {
        Box::pin(self.persist_attempt(at, project, record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_contracts::{RequestUsageKey, RequestUsageSource, RequestUsageStatus};

    fn sample_record(session: &str, attempt: u32, total: i64) -> RequestUsageRecord {
        RequestUsageRecord {
            key: RequestUsageKey {
                session_id: session.to_string(),
                actor_id: "root".to_string(),
                round: 1,
                turn: 1,
                attempt,
            },
            provider: "openai".to_string(),
            model: "gpt-5".to_string(),
            status: RequestUsageStatus::Completed,
            source: RequestUsageSource::Reported,
            prompt_tokens: total - 50,
            completion_tokens: 50,
            total_tokens: total,
            ..Default::default()
        }
    }

    fn temp_root() -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("create temp root");
        let path = dir.path().to_path_buf();
        (path, dir)
    }

    #[test]
    fn concurrent_usage_writers_preserve_all_records() {
        let (root, _tmp) = temp_root();
        // Independent handles model standalone processes sharing one DB.
        let stores: Vec<_> = (0..4)
            .map(|_| UsageStatsStore::with_root(root.clone()))
            .collect();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(stores.len()));
        let threads: Vec<_> = stores
            .into_iter()
            .enumerate()
            .map(|(i, store)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for attempt in 0..8 {
                        store
                            .record(
                                1_700_000_000_000,
                                "p",
                                &sample_record(&format!("s{i}"), attempt, 100),
                            )
                            .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(UsageStatsStore::with_root(root).all_records().len(), 32);
    }

    #[test]
    fn record_round_trips_through_disk() {
        let (root, _tmp) = temp_root();
        let store = UsageStatsStore::with_root(root);
        let record = sample_record("s1", 1, 1_000);
        store
            .record(1_700_000_000_000, "bucket-a", &record)
            .expect("record");
        let reloaded =
            UsageStatsStore::with_root(store.root.clone().unwrap().parent().unwrap().to_path_buf());
        let records = reloaded.all_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record.total_tokens, 1_000);
        assert_eq!(records[0].project, "bucket-a");
        assert_eq!(records[0].record.model, "gpt-5");
    }

    #[test]
    fn replay_is_idempotent_but_reported_upgrades_estimate() {
        let (root, _tmp) = temp_root();
        let store = UsageStatsStore::with_root(root);
        let mut estimated = sample_record("s1", 1, 900);
        estimated.source = RequestUsageSource::Estimated;
        estimated.total_tokens = 900;
        estimated.prompt_tokens = 850;
        store.record(1_700_000_000_000, "p", &estimated).unwrap();

        // Replay the same key → no duplicate.
        store.record(1_700_000_000_500, "p", &estimated).unwrap();
        assert_eq!(store.all_records().len(), 1);

        // A reported replay for the same key upgrades the estimate in place.
        let reported = sample_record("s1", 1, 1_200);
        store.record(1_700_000_001_000, "p", &reported).unwrap();
        let records = store.all_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record.source, RequestUsageSource::Reported);
        assert_eq!(records[0].record.total_tokens, 1_200);

        // A weaker replay after a reported record changes nothing.
        store.record(1_700_000_002_000, "p", &estimated).unwrap();
        assert_eq!(store.all_records()[0].record.total_tokens, 1_200);
    }

    #[test]
    fn in_flight_records_are_not_persisted() {
        let (root, _tmp) = temp_root();
        let store = UsageStatsStore::with_root(root);
        let mut in_flight = sample_record("s1", 1, 500);
        in_flight.status = RequestUsageStatus::InFlight;
        store.record(1_700_000_000_000, "p", &in_flight).unwrap();
        assert!(store.all_records().is_empty());
    }

    #[test]
    fn days_partition_and_aggregate() {
        let (root, _tmp) = temp_root();
        let store = UsageStatsStore::with_root(root);
        // Two different local days: 2023-11-14 22:13:20Z and (likely) the
        // next local day is far away, so pick two instants 48h apart.
        let t0 = 1_700_000_000_000u64;
        let t1 = t0 + 48 * 3600 * 1_000;
        // Distinct attempts: the same `(session, actor, round, turn, attempt)`
        // identity may not be re-reported with conflicting authoritative
        // counts (ADR-0236; `upsert_attempt` refuses it), so two days means two
        // attempts, not one attempt recorded twice.
        store
            .record(t0, "p", &sample_record("s1", 1, 1_000))
            .unwrap();
        store
            .record(t1, "p", &sample_record("s1", 2, 2_000))
            .unwrap();
        let report = store.report(10);
        assert_eq!(report.days.len(), 2);
        assert_eq!(report.grand_total.total_tokens, 3_000);
        assert_ne!(report.days[0].day, report.days[1].day);
    }

    #[test]
    fn record_batch_groups_by_day() {
        let (root, _tmp) = temp_root();
        let store = UsageStatsStore::with_root(root);
        let t0 = 1_700_000_000_000u64;
        let t1 = t0 + 48 * 3600 * 1_000;
        let a = sample_record("s1", 1, 100);
        let b = sample_record("s1", 2, 200);
        let c = sample_record("s2", 1, 300);
        store
            .record_batch(&[(t0, "p", a), (t0, "p", b), (t1, "q", c)])
            .unwrap();
        let report = store.report(10);
        assert_eq!(report.days.len(), 2);
        assert_eq!(report.grand_total.requests, 3);
    }

    /// End-to-end: a `TokenSourceLedger` with this store installed as its
    /// `UsageStatSink` mirrors terminal settles into the day files, and the
    /// aggregate matches what the ledger itself would report — the same
    /// wiring the daemon bootstrap performs.
    #[test]
    fn ledger_sink_end_to_end_persists_and_aggregates() {
        use nuo_contracts::TokenUsage;
        use std::sync::Arc;

        let (root, _tmp) = temp_root();
        let store = Arc::new(UsageStatsStore::with_root(root));
        let ledger = nuo_contracts::TokenSourceLedger::new();
        nuo_contracts::TokenSourceLedger::install_usage_sink(
            &ledger,
            store.clone() as Arc<dyn nuo_contracts::UsageStatSink>,
        );
        ledger.set_usage_project("proj-bucket");

        // One completed reported attempt.
        let first = ledger.begin_request("s1", "anthropic", "claude", 1, 1, 1_000);
        ledger.settle_request(
            &first,
            RequestUsageStatus::Completed,
            Some(TokenUsage {
                prompt_tokens: 1_200,
                completion_tokens: 300,
                total_tokens: 1_500,
                cache_creation_input_tokens: 200,
                cache_read_input_tokens: 500,
                cache_miss_input_tokens: 0,
                ..Default::default()
            }),
            0,
            4_000,
        );
        // One failed attempt (still consumes a request slot upstream).
        let retry = ledger.begin_request("s1", "anthropic", "claude", 1, 2, 900);
        ledger.settle_request(&retry, RequestUsageStatus::Failed, None, 20, 0);

        futures::executor::block_on(ledger.persist_pending("s1")).unwrap();
        // A FIFO writer barrier makes the asynchronous sink visible to readers.
        store
            .handle()
            .set_kv_blocking("test:barrier".into(), "1".into())
            .unwrap();
        let report = store.report(10);
        assert_eq!(report.grand_total.requests, 2);
        assert_eq!(report.grand_total.completed, 1);
        assert_eq!(report.grand_total.total_tokens, 1_500);
        assert_eq!(report.grand_total.estimated_tokens, 920);
        assert_eq!(report.models.len(), 1);
        assert_eq!(report.models[0].provider, "anthropic");
        // Both records landed in today's day file under the stamped project.
        let records = store.all_records();
        assert!(records.iter().all(|r| r.project == "proj-bucket"));
        assert_eq!(records.len(), 2);
        // The store survives a fresh instance reading the same root (i.e.
        // session cleanup / restart cannot remove it). `root()` already
        // carries the `usage` segment, so re-wrap from its parent.
        let reread = UsageStatsStore::with_root(
            store
                .root
                .clone()
                .expect("root is set")
                .parent()
                .expect("root has a parent")
                .to_path_buf(),
        );
        assert_eq!(reread.report(10).grand_total.requests, 2);
    }
}
