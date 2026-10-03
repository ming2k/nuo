//! `DbReader` implementation: the read-only allow-list surface (ADR-0231).
//! The type itself lives in `db.rs`.
use super::*;

impl DbReader {
    /// The database file this reader is bound to.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The durable commit revision for a session (ADR-0236 D3); `0` when the
    /// session has never committed. A caller passes this as
    /// [`crate::session::CommitTurn::expected_revision`] to make its next write
    /// conditional on the revision it actually read.
    pub fn session_revision(&self, session_id: &str) -> Result<u64> {
        session_revision(&self.engine.conn, session_id)
    }

    /// The latest durable commit receipt for a session (ADR-0236 D3):
    /// `(operation_id, revision)` of its most recent commit. A caller that
    /// lost an acknowledgement resolves its outcome here by operation
    /// identity before replaying dependent work.
    pub fn commit_receipt(&self, session_id: &str) -> Result<Option<(String, u64)>> {
        latest_commit_receipt(&self.engine.conn, session_id)
            .map(|receipt| receipt.map(|(operation_id, _hash, revision)| (operation_id, revision)))
    }

    /// Load the canonical [`nuo_wire::SessionIR`] for a session if it exists (ADR-0241/ADR-0249).
    pub fn load_session_ir(&self, session_id: &str) -> Result<Option<nuo_wire::SessionIR>> {
        crate::db::session_ir::load_session_ir(&self.engine.conn, session_id)
    }

    #[allow(dead_code)]
    pub(crate) fn usage_days(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self
            .engine
            .conn
            .prepare("SELECT DISTINCT day FROM usage_records ORDER BY day DESC LIMIT ?1")?;
        stmt.query_map([limit as i64], |r| r.get(0))?.collect()
    }

    pub(crate) fn usage_records(
        &self,
        days: usize,
        limit: usize,
    ) -> Result<Vec<nuo_wire::usage_stats::UsageStatRecord>> {
        let mut stmt = self.engine.conn.prepare("SELECT payload,day,recorded_at_ms,project FROM usage_records
            WHERE day IN (SELECT DISTINCT day FROM usage_records ORDER BY day DESC LIMIT ?1)
            AND json_extract(payload,'$.status') != 'in_flight' ORDER BY recorded_at_ms DESC,session_id,actor_id,round,turn,attempt LIMIT ?2")?;
        let rows = stmt
            .query_map(
                params![days as i64, limit.min(i64::MAX as usize) as i64],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, u64>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(payload, day, recorded_at_ms, project)| {
                Ok(nuo_wire::usage_stats::UsageStatRecord {
                    record: decode_json(&payload)?,
                    day,
                    recorded_at_ms,
                    project,
                })
            })
            .collect()
    }

    pub(crate) fn usage_report(
        &self,
        days: usize,
        event_cap: usize,
    ) -> Result<nuo_wire::usage_stats::UsageStatsReport> {
        use nuo_wire::usage_stats::*;
        let mut report = UsageStatsReport::default();
        let mut day_map = std::collections::BTreeMap::<String, UsageModelTotals>::new();
        let mut model_map = std::collections::BTreeMap::<(String, String), UsageModelTotals>::new();
        let mut stmt = self.engine.conn.prepare("SELECT day,provider,model,SUM(requests),SUM(completed),SUM(prompt_tokens),SUM(completion_tokens),SUM(total_tokens),SUM(cache_write_tokens),SUM(cache_read_tokens),SUM(estimated_tokens)
            FROM usage_contributions WHERE requests>0 AND day IN (SELECT DISTINCT day FROM usage_records ORDER BY day DESC LIMIT ?1) GROUP BY day,provider,model")?;
        let rows = stmt.query_map([days as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                UsageModelTotals {
                    requests: r.get(3)?,
                    completed: r.get(4)?,
                    prompt_tokens: r.get(5)?,
                    completion_tokens: r.get(6)?,
                    total_tokens: r.get(7)?,
                    cache_write_tokens: r.get(8)?,
                    cache_read_tokens: r.get(9)?,
                    estimated_tokens: r.get(10)?,
                },
            ))
        })?;
        fn add(a: &mut UsageModelTotals, b: &UsageModelTotals) {
            a.requests += b.requests;
            a.completed += b.completed;
            a.prompt_tokens += b.prompt_tokens;
            a.completion_tokens += b.completion_tokens;
            a.total_tokens += b.total_tokens;
            a.cache_write_tokens += b.cache_write_tokens;
            a.cache_read_tokens += b.cache_read_tokens;
            a.estimated_tokens += b.estimated_tokens;
        }
        for row in rows {
            let (day, provider, model, totals) = row?;
            add(day_map.entry(day).or_default(), &totals);
            add(model_map.entry((provider, model)).or_default(), &totals);
            add(&mut report.grand_total, &totals);
        }
        report.days = day_map
            .into_iter()
            .map(|(day, totals)| UsageDayTotals { day, totals })
            .collect();
        report.models = model_map
            .into_iter()
            .map(|((provider, model), totals)| UsageModelRow {
                provider,
                model,
                totals,
            })
            .collect();
        report
            .models
            .sort_by_key(|r| std::cmp::Reverse(r.totals.grand_total()));
        report.first_day = report.days.first().map(|r| r.day.clone());
        report.last_day = report.days.last().map(|r| r.day.clone());
        report.events = self.usage_records(days, event_cap.min(1000))?;
        report.events.reverse();
        report.source_revision =
            self.engine
                .conn
                .query_row("SELECT revision FROM usage_clock WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        let oldest: Option<u64> =
            self.engine
                .conn
                .query_row("SELECT MIN(revision) FROM usage_dirty", [], |r| r.get(0))?;
        report.projection_revision = oldest
            .map(|r| r.saturating_sub(1))
            .unwrap_or(report.source_revision);
        Ok(report)
    }

    // -- sessions ----------------------------------------------------------

    /// One session's header row, if it exists.
    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        self.engine.get_session(session_id)
    }

    /// Sessions matching a derived grouping, newest first.
    pub fn list_sessions(
        &self,
        filter: Option<&nuo_wire::WorkspaceFilter>,
    ) -> Result<Vec<SessionRecord>> {
        self.engine.list_sessions(filter)
    }

    /// Subagent sessions spawned by `parent_id` (ADR-0262).
    pub fn list_subagent_sessions(&self, parent_id: &str) -> Result<Vec<SessionRecord>> {
        self.engine.list_subagent_sessions(parent_id)
    }

    /// Candidate sessions for switching, strictly excluding the active session (ADR-0250).
    pub fn list_switch_candidates(
        &self,
        partition: &nuo_wire::SessionPartition,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        self.engine.list_switch_candidates(partition, active_id)
    }

    /// List session summaries for a domain partition, with active session tagged (ADR-0250).
    pub fn list_session_summaries_in_partition(
        &self,
        partition: &nuo_wire::SessionPartition,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        self.engine
            .list_session_summaries_in_partition(partition, active_id)
    }

    /// The most recently updated session id in `partition` (ADR-0250 `--resume`).
    pub fn latest_session_in_partition(
        &self,
        partition: &nuo_wire::SessionPartition,
    ) -> Result<Option<String>> {
        self.engine.latest_session_in_partition(partition)
    }

    /// Picker-facing summaries for a derived grouping, newest first.
    pub fn list_session_summaries(
        &self,
        filter: Option<&nuo_wire::WorkspaceFilter>,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        self.engine.list_session_summaries(filter, active_id)
    }

    /// The most recently updated session id in `filter`, optionally narrowed
    /// to one persona (the `--resume` resolution leg).
    pub fn latest_session(
        &self,
        filter: &nuo_wire::WorkspaceFilter,
        persona: Option<&str>,
    ) -> Result<Option<String>> {
        self.engine.latest_session(filter, persona)
    }

    /// A session's inherited workspace binding plus its persona.
    pub fn lookup_session_workspace(
        &self,
        session_id: &str,
    ) -> Result<Option<(Option<nuo_wire::WorkspaceBinding>, Option<String>)>> {
        self.engine.lookup_session_workspace(session_id)
    }

    /// A session's immutable role manifest snapshot (ADR-0245).
    pub fn lookup_session_manifest(
        &self,
        session_id: &str,
    ) -> Result<Option<nuo_wire::SessionRoleManifest>> {
        self.engine.lookup_session_manifest(session_id)
    }

    /// Resolve an id prefix to every matching session id.
    pub fn resolve_session_prefix(
        &self,
        prefix: &str,
        filter: Option<&nuo_wire::WorkspaceFilter>,
    ) -> Result<Vec<String>> {
        self.engine.resolve_session_prefix(prefix, filter)
    }

    /// Full detail for one session (the session-info sub-view).
    pub fn get_session_detail(
        &self,
        session_id: &str,
        active_id: &str,
    ) -> Result<Option<nuo_wire::SessionDetail>> {
        self.engine.get_session_detail(session_id, active_id)
    }

    /// One session's raw durable data (the load path of `SessionStore`).
    pub fn load_session_full(
        &self,
        session_id: &str,
    ) -> Result<Option<crate::session::SessionData>> {
        self.engine.load_session_full(session_id)
    }

    /// A session's projected transcript tail as wire rows (the Archivist's
    /// read tool).
    pub fn read_session_transcript(
        &self,
        session_id: &str,
        tail: usize,
    ) -> Result<Option<SessionTranscriptView>> {
        self.engine.read_session_transcript(session_id, tail)
    }

    /// Every blob hash the durable reference ledger still mentions. The sweep
    /// that consumes this is a *filesystem* pass, so it runs on the caller's
    /// thread rather than blocking the writer.
    pub fn live_blob_hashes(&self) -> Result<std::collections::HashSet<String>> {
        self.engine.live_blob_hashes()
    }

    // -- recall / history --------------------------------------------------

    /// BM25 search over every persisted transcript entry (strict AND form).
    pub fn search_history(
        &self,
        query: &str,
        filter: Option<&nuo_wire::WorkspaceFilter>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.engine.search_history(query, filter, limit)
    }

    /// [`Self::search_history`] with the words OR-joined (recall fallback).
    pub fn search_history_relaxed(
        &self,
        query: &str,
        filter: Option<&nuo_wire::WorkspaceFilter>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.engine.search_history_relaxed(query, filter, limit)
    }

    /// The prompt history, oldest first, capped at `limit`.
    pub fn load_input_history(&self, limit: usize) -> Result<Vec<nuo_wire::HistoryEntry>> {
        self.engine.load_input_history(limit)
    }

    /// Archived request projections for one session, oldest first.
    pub fn load_request_projections(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<nuo_wire::RequestProjection>> {
        self.engine.load_request_projections(session_id, limit)
    }

    // -- typed key/value ---------------------------------------------------

    /// A raw key-value entry.
    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        self.engine.get_kv(key)
    }

    /// A JSON-encoded key-value entry.
    pub fn get_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        self.engine.get_json(key)
    }

    /// Every key with the given prefix.
    pub fn list_kv_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        self.engine.list_kv_keys_with_prefix(prefix)
    }

    // -- context lifecycle (ADR-0275 / ADR-0279) ---------------------------

    /// Query the current revision of a context branch.
    pub fn context_branch_revision(
        &self,
        session_id: &str,
        branch_id: &str,
    ) -> std::result::Result<Option<u64>, crate::db::context_store::ContextCommitError> {
        crate::db::context_store::branch_revision(&self.engine.conn, session_id, branch_id)
    }

    /// Load ordered immutable execution facts for a session branch.
    pub fn load_context_facts(
        &self,
        session_id: &str,
    ) -> std::result::Result<Vec<nuo_wire::context_lifecycle::FactNode>, crate::db::context_store::ContextCommitError> {
        crate::db::context_store::load_facts(&self.engine.conn, session_id)
    }

    /// Compute the SHA-256 digest of all facts for a session.
    pub fn context_facts_digest(
        &self,
        session_id: &str,
    ) -> std::result::Result<String, crate::db::context_store::ContextCommitError> {
        crate::db::context_store::facts_digest(&self.engine.conn, session_id)
    }

    /// Obtain an inspect retrieval service bound to this reader connection (ADR-0279).
    pub fn inspect_service(&self) -> crate::db::inspect_service::InspectService<'_> {
        crate::db::inspect_service::InspectService::new(&self.engine.conn)
    }
}
