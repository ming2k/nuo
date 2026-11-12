//! The live token-source ledger.

use super::record::{Entry, key};
use super::report::{now_epoch_ms, request_display_order};
use super::*;

/// A thread-safe running ledger of token counts split by source (reported vs.
/// estimated), keyed by `(provider_id, model)`. Shared between the agent (the
/// writer — books each turn) and the TUI (the reader — renders the report).
#[derive(Default)]
pub struct TokenSourceLedger {
    /// `(provider, model)` → accumulator (totals + per-turn line items). A
    /// [`BTreeMap`] so the report iterates in a stable order.
    entries: Mutex<BTreeMap<(String, String), Entry>>,
    /// Lifecycle-aware request records. Legacy `record*` callers continue to
    /// use `entries`; production request accounting uses this keyed map so a
    /// terminal event updates exactly one attempt and duplicate events are
    /// idempotent.
    requests: Mutex<BTreeMap<RequestUsageKey, RequestUsageRecord>>,
    /// Session selected by the harness. `snapshot()` filters lifecycle records
    /// to this id, preventing usage from another opened session leaking into
    /// the current report.
    active_session: Mutex<Option<String>>,
    /// Optional durable mirror: every terminally settled request
    /// is forwarded to this sink. `None` in tests / when no store is bound.
    usage_sink: Mutex<Option<Arc<dyn UsageStatSink>>>,
    dirty_usage: Mutex<std::collections::HashSet<RequestUsageKey>>,
    /// Project bucket name stamped onto sink records (empty = unknown).
    usage_project: Mutex<String>,
}

impl std::fmt::Debug for TokenSourceLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSourceLedger")
            .field("active_session", &self.active_session())
            .field("usage_project", &self.usage_snapshot())
            .finish_non_exhaustive()
    }
}

/// Parameters for recording the start of a network request attempt in [`TokenSourceLedger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeginRequestParams<'a> {
    pub session_id: &'a str,
    pub actor_id: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub round: u64,
    pub turn: u32,
    pub projected_prompt_tokens: i64,
    /// Epoch milliseconds at the provider-call boundary.
    ///
    /// The caller owns this stamp so the durable record and the cost rating
    /// derived from it share one instant: they must never disagree about which
    /// rate window an attempt was billed under (ADR-0080).
    pub started_at_ms: u64,
}

impl TokenSourceLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cheap shared handle (the canonical way the agent and TUI share one
    /// ledger).
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn set_active_session(&self, session_id: impl Into<String>) {
        *self
            .active_session
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(session_id.into());
    }

    pub fn active_session(&self) -> Option<String> {
        self.active_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Install the durable usage-statistics sink. Every
    /// terminally settled request attempt is forwarded to it from
    /// [`Self::settle_request`]. Replaces any prior sink.
    pub fn install_usage_sink(&self, sink: Arc<dyn UsageStatSink>) {
        *self.usage_sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    /// Stamp the project bucket name forwarded with sink records. Called by
    /// the driver on session open/switch so records group by project.
    pub fn set_usage_project(&self, project: impl Into<String>) {
        *self.usage_project.lock().unwrap_or_else(|e| e.into_inner()) = project.into();
    }

    /// Current project bucket name (test/diagnostics).
    pub fn usage_snapshot(&self) -> String {
        self.usage_project
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Insert an in-flight request and allocate the next attempt number for
    /// its `(session, actor, round, turn)` tuple.
    pub fn begin_request(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        round: u64,
        turn: u32,
        projected_prompt_tokens: i64,
    ) -> RequestUsageKey {
        self.begin_request_for_actor(BeginRequestParams {
            session_id,
            actor_id: "master",
            provider,
            model,
            round,
            turn,
            projected_prompt_tokens,
            started_at_ms: now_epoch_ms(),
        })
    }

    pub fn begin_request_for_actor(&self, params: BeginRequestParams<'_>) -> RequestUsageKey {
        let BeginRequestParams {
            session_id,
            actor_id,
            provider,
            model,
            round,
            turn,
            projected_prompt_tokens,
            started_at_ms,
        } = params;
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let attempt = requests
            .keys()
            .filter(|key| {
                key.session_id == session_id
                    && key.actor_id == actor_id
                    && key.round == round
                    && key.turn == turn
            })
            .map(|key| key.attempt)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let key = RequestUsageKey {
            session_id: session_id.to_string(),
            actor_id: actor_id.to_string(),
            round,
            turn,
            attempt,
        };
        requests.insert(
            key.clone(),
            RequestUsageRecord {
                key: key.clone(),
                provider: provider.to_string(),
                model: model.to_string(),
                status: RequestUsageStatus::InFlight,
                source: RequestUsageSource::Unknown,
                projected_prompt_tokens: projected_prompt_tokens.max(0),
                started_at_ms,
                ..Default::default()
            },
        );
        self.dirty_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone());
        key
    }

    /// Terminally settle one attempt. Replaying the same event is harmless;
    /// authoritative reported usage can upgrade an estimate, but an estimate
    /// can never downgrade an already reported record. `generation_ms` is the
    /// attempt's provider-generation span (request dispatch → validated
    /// response; `0` when none was measured) and backs the per-attempt output
    /// rate shown by the Context Usage modal.
    pub fn settle_request(
        &self,
        key: &RequestUsageKey,
        status: RequestUsageStatus,
        usage: Option<crate::TokenUsage>,
        estimated_completion_tokens: i64,
        generation_ms: u64,
    ) {
        self.settle_request_with_performance_and_error(
            key,
            status,
            usage,
            estimated_completion_tokens,
            generation_ms,
            None,
            None,
        );
    }

    /// Terminally settle one attempt with optional failure error payload.
    pub fn settle_request_with_error(
        &self,
        key: &RequestUsageKey,
        status: RequestUsageStatus,
        usage: Option<crate::TokenUsage>,
        estimated_completion_tokens: i64,
        generation_ms: u64,
        error: Option<String>,
    ) {
        self.settle_request_with_performance_and_error(
            key,
            status,
            usage,
            estimated_completion_tokens,
            generation_ms,
            None,
            error,
        );
    }

    /// Terminally settle one attempt with structured performance telemetry.
    /// The legacy `generation_ms` remains populated for wire/session
    /// compatibility; new performance surfaces use `performance` exclusively.
    #[allow(clippy::too_many_arguments)]
    pub fn settle_request_with_performance_and_error(
        &self,
        key: &RequestUsageKey,
        status: RequestUsageStatus,
        usage: Option<crate::TokenUsage>,
        estimated_completion_tokens: i64,
        generation_ms: u64,
        performance: Option<RequestPerformance>,
        error: Option<String>,
    ) {
        if !status.is_terminal() {
            return;
        }
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = requests.get_mut(key) else {
            return;
        };
        if record.status.is_terminal() && record.source == RequestUsageSource::Reported {
            return;
        }
        record.status = status;
        record.generation_ms = generation_ms;
        if performance.is_some() {
            record.performance = performance;
        }
        if error.is_some() {
            record.error = error;
        }
        if let Some(usage) = usage {
            record.source = RequestUsageSource::Reported;
            record.prompt_tokens = usage.prompt_tokens.max(0);
            record.completion_tokens = usage.completion_tokens.max(0);
            record.total_tokens = usage.total_tokens.max(0);
            record.cache_write_tokens = usage.cache_creation_input_tokens.max(0);
            record.cache_read_tokens = usage.cache_read_input_tokens.max(0);
            record.cache_miss_tokens = usage.cache_miss_input_tokens.max(0);
            record.reasoning_tokens = usage.reasoning_tokens.max(0);
        } else {
            record.source = RequestUsageSource::Estimated;
            record.prompt_tokens = record.projected_prompt_tokens.max(0);
            record.completion_tokens = estimated_completion_tokens.max(0);
            record.total_tokens = record
                .prompt_tokens
                .saturating_add(record.completion_tokens);
            // Belt-and-braces: a caller bug cannot be allowed to persist a
            // physically impossible streamed count (this exact class of bug
            // once booked 14.7M completion tokens for one interrupted
            // attempt, which rendered as a 130 050 tok/s rate). Silently
            // clamped — this crate carries no tracing dependency, and the
            // repair is visible in the report itself.
            record.sanitize_poisoned_estimate();
        }
        if let Some(pricing) =
            crate::pricing::resolve_model_pricing(&record.provider, &record.model)
        {
            let tool_usage = nuo_tool::usage::TokenUsage {
                prompt_tokens: record.prompt_tokens,
                completion_tokens: record.completion_tokens,
                total_tokens: record.total_tokens,
                cache_creation_input_tokens: record.cache_write_tokens,
                cache_read_input_tokens: record.cache_read_tokens,
                cache_miss_input_tokens: record.cache_miss_tokens,
                reasoning_tokens: record.reasoning_tokens,
            };
            let cost = pricing.calculate_cost(
                &tool_usage,
                crate::pricing::RatingInstant::from_epoch_ms(record.started_at_ms),
            );
            record.cost_nano_usd = Some(cost.amount.0);
        }
        self.dirty_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone());
    }

    pub fn pending_records_for_session(&self, session_id: &str) -> Vec<RequestUsageRecord> {
        let requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let dirty = self.dirty_usage.lock().unwrap_or_else(|e| e.into_inner());
        // `dirty_usage` is a HashSet: iteration order is arbitrary. Persisting
        // in that order made the durable `request_usage_records` mirror
        // nondeterministic (and diverge from the sorted `records_for_session`
        // view), so sort into the stable display order before returning.
        let mut records: Vec<RequestUsageRecord> = dirty
            .iter()
            .filter(|key| key.session_id == session_id)
            .filter_map(|key| requests.get(key).cloned())
            .collect();
        records.sort_by(|a, b| request_display_order(a).cmp(&request_display_order(b)));
        records
    }

    pub fn acknowledge_records(&self, records: &[RequestUsageRecord]) {
        let requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let mut dirty = self.dirty_usage.lock().unwrap_or_else(|e| e.into_inner());
        for record in records {
            if requests.get(&record.key) == Some(record) {
                dirty.remove(&record.key);
            }
        }
    }

    pub async fn persist_request(&self, key: &RequestUsageKey) -> Result<(), String> {
        let sink = self
            .usage_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let record = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        if let (Some(sink), Some(record)) = (sink, record) {
            let project = self.usage_snapshot();
            sink.persist_usage(record.started_at_ms, &project, record.clone())
                .await?;
            self.acknowledge_records(&[record]);
        }
        Ok(())
    }

    pub async fn persist_pending(&self, session_id: &str) -> Result<(), String> {
        for record in self.pending_records_for_session(session_id) {
            self.persist_request(&record.key).await?;
        }
        Ok(())
    }

    /// Owned lifecycle records for one session, in stable request order.
    pub fn records_for_session(&self, session_id: &str) -> Vec<RequestUsageRecord> {
        let mut records = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|record| record.key.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|a, b| request_display_order(a).cmp(&request_display_order(b)));
        records
    }

    /// Replace one session's records from durable state. Any persisted
    /// in-flight request is crash residue and becomes `Abandoned` with an
    /// estimated prompt lower-bound before being exposed. Records persisted
    /// by the quadratic `observe_output` bug (see
    /// [`RequestUsageRecord::sanitize_poisoned_estimate`]) are repaired on
    /// load so a resumed session's report and rates stop showing the poison.
    pub fn restore_session(&self, session_id: &str, records: Vec<RequestUsageRecord>) {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        requests.retain(|key, _| key.session_id != session_id);
        for mut record in records {
            record.key.session_id = session_id.to_string();
            if record.status == RequestUsageStatus::InFlight {
                record.status = RequestUsageStatus::Abandoned;
                record.source = RequestUsageSource::Estimated;
                record.prompt_tokens = record.projected_prompt_tokens.max(0);
                record.total_tokens = record.prompt_tokens;
            }
            // Repair records persisted by the quadratic double-count bug
            // (silently — this crate carries no tracing dependency; the
            // repair is visible in the report itself, and the round/turn
            // identity stays intact).
            record.sanitize_poisoned_estimate();
            requests.insert(record.key.clone(), record);
        }
    }

    /// Book one turn as a line item — the single entry point all the public
    /// recorders funnel through. It appends the turn and folds it into the
    /// running totals. Non-positive totals are ignored; negative io/cache
    /// counts are clamped to zero.
    pub fn record_turn(&self, provider: &str, model: &str, turn: TokenTurn) {
        if turn.total_tokens <= 0 {
            return;
        }
        let turn = TokenTurn {
            prompt_tokens: turn.prompt_tokens.max(0),
            completion_tokens: turn.completion_tokens.max(0),
            cache_write_tokens: turn.cache_write_tokens.max(0),
            cache_read_tokens: turn.cache_read_tokens.max(0),
            cache_miss_tokens: turn.cache_miss_tokens.max(0),
            reasoning_tokens: turn.reasoning_tokens.max(0),
            ..turn
        };
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(key(provider, model)).or_default();
        if turn.reported {
            entry.totals.reported_tokens += turn.total_tokens;
            entry.totals.prompt_tokens += turn.prompt_tokens;
            entry.totals.completion_tokens += turn.completion_tokens;
            entry.totals.cache_write_tokens += turn.cache_write_tokens;
            entry.totals.cache_read_tokens += turn.cache_read_tokens;
            entry.totals.cache_miss_tokens += turn.cache_miss_tokens;
            entry.totals.reasoning_tokens += turn.reasoning_tokens;
        } else {
            entry.totals.estimated_tokens += turn.total_tokens;
        }
        entry.turns.push(turn);
    }

    /// Book one turn's token usage. When `reported` is `true`, the provider
    /// reported authoritative usage and `tokens` are real counts; when `false`,
    /// `tokens` are a local estimate.
    pub fn record(&self, provider: &str, model: &str, tokens: i64, reported: bool) {
        self.record_turn(
            provider,
            model,
            TokenTurn {
                reported,
                total_tokens: tokens,
                ..Default::default()
            },
        );
    }

    /// Book one turn's reported usage, including its prompt-cache split. The
    /// cache write/read counts are tracked as a breakout (they're already
    /// folded into `tokens` by the provider's usage parser); `cache_*` are
    /// clamped to non-negative. Callers with no caching pass `0, 0`.
    pub fn record_reported(
        &self,
        provider: &str,
        model: &str,
        tokens: i64,
        cache_write: i64,
        cache_read: i64,
    ) {
        self.record_turn(
            provider,
            model,
            TokenTurn {
                reported: true,
                total_tokens: tokens,
                cache_write_tokens: cache_write,
                cache_read_tokens: cache_read,
                ..Default::default()
            },
        );
    }

    /// The most recent *reported* turn for a `(provider, model)`, if any.
    ///
    /// Used by the TUI context meter as the authoritative anchor: the
    /// provider-reported `prompt_tokens` already measures the serialized
    /// request size (system prompt + every prior turn + tool schemas + per-
    /// message template overhead), which is more accurate than any local
    /// estimate of the transcript. `completion_tokens` is included because the
    /// assistant's last reply is now part of history.
    pub fn last_reported_turn(&self, provider: &str, model: &str) -> Option<TokenTurn> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(&key(provider, model))?;
        entry.turns.iter().rev().copied().find(|turn| turn.reported)
    }

    /// A snapshot of the ledger suitable for rendering (owned, no lock held).
    pub fn snapshot(&self) -> TokenSourceReport {
        let active_session = self.active_session();
        self.snapshot_filtered(active_session.as_deref())
    }

    pub fn snapshot_for_session(&self, session_id: &str) -> TokenSourceReport {
        self.snapshot_filtered(Some(session_id))
    }

    fn snapshot_filtered(&self, session_id: Option<&str>) -> TokenSourceReport {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<TokenSourceRow> = entries
            .iter()
            .map(|((provider, model), entry)| TokenSourceRow {
                provider: provider.to_string(),
                model: model.to_string(),
                totals: entry.totals,
                turns: entry.turns.clone(),
                requests: Vec::new(),
            })
            .collect();
        drop(entries);

        let requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        for record in requests.values().filter(|record| {
            session_id.is_none_or(|session_id| record.key.session_id == session_id)
        }) {
            let row = if let Some(row) = rows
                .iter_mut()
                .find(|row| row.provider == record.provider && row.model == record.model)
            {
                row
            } else {
                let index = rows.len();
                rows.push(TokenSourceRow {
                    provider: record.provider.clone(),
                    model: record.model.clone(),
                    totals: TokenSourceTotals::default(),
                    turns: Vec::new(),
                    requests: Vec::new(),
                });
                &mut rows[index]
            };
            row.requests.push(record.clone());
            if record.status.is_terminal() {
                row.totals.add(record.totals());
                row.turns.push(record.as_turn());
            }
        }
        rows.sort_by(|a, b| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)));
        for row in &mut rows {
            row.requests
                .sort_by(|a, b| request_display_order(a).cmp(&request_display_order(b)));
        }
        let grand_total =
            rows.iter()
                .map(|r| r.totals)
                .fold(TokenSourceTotals::default(), |mut acc, t| {
                    acc.add(t);
                    acc
                });
        TokenSourceReport { rows, grand_total }
    }
}
