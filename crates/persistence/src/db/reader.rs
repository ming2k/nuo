//! `DbReader` implementation: the read-only allow-list surface.
//! The type itself lives in `db.rs`.
use super::*;

impl DbReader {
    fn conn(&self) -> &Connection {
        match &self.conn {
            // `conn` is only taken by `Drop`; no `DbReader` method can run
            // after its destructor starts.
            Some(conn) => conn,
            None => unreachable!("DbReader connection is present until Drop"),
        }
    }

    /// The database file this reader is bound to.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The durable commit revision for a session (D3); `0` when the
    /// session has never committed. A caller passes this as
    /// [`crate::thread::CommitTurn::expected_revision`] to make its next write
    /// conditional on the revision it actually read.
    pub fn session_revision(&self, session_id: &str) -> Result<u64> {
        session_revision(self.conn(), session_id)
    }

    /// The durable journal seq absorbed by the catalog plane (`nuo.db`) alone.
    pub fn catalog_projection_watermark(&self, session_id: &str) -> Result<Option<u64>> {
        self.conn()
            .query_row(
                "SELECT seq FROM projection_watermarks WHERE thread_id = ?1",
                [session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|value| value.map(|seq| seq.max(0) as u64))
    }

    /// The minimum durable journal seq absorbed by **both** the catalog plane
    /// and the owning content shard (ADR-0242). If either plane is missing or
    /// behind, catch-up rebuilds it from the journal.
    pub fn projection_watermark(&self, session_id: &str) -> Result<Option<u64>> {
        let catalog_wm = self.catalog_projection_watermark(session_id)?;
        let shard_wm = self.shards.projection_watermark(session_id)?;
        match (catalog_wm, shard_wm) {
            (Some(c), Some(s)) => Ok(Some(c.min(s))),
            _ => Ok(None),
        }
    }

    /// Pure catalog primary-key check for draft reconciliation (ADR-0250):
    /// returns the subset of `thread_ids` that do **not** exist in the catalog.
    pub fn missing_thread_ids(&self, thread_ids: &[String]) -> Result<Vec<String>> {
        let mut missing = Vec::new();
        let mut stmt = self
            .conn()
            .prepare_cached("SELECT 1 FROM threads WHERE id = ?1")?;
        for id in thread_ids {
            let exists: Option<i64> = stmt.query_row([id], |row| row.get(0)).optional()?;
            if exists.is_none() {
                missing.push(id.clone());
            }
        }
        Ok(missing)
    }

    /// The latest durable commit receipt for a session (D3):
    /// `(operation_id, revision)` of its most recent commit. A caller that
    /// lost an acknowledgement resolves its outcome here by operation
    /// identity before replaying dependent work.
    pub fn commit_receipt(&self, session_id: &str) -> Result<Option<(String, u64)>> {
        latest_commit_receipt(self.conn(), session_id)
            .map(|receipt| receipt.map(|(operation_id, _hash, revision)| (operation_id, revision)))
    }

    /// Open a read-only connection to `thread_id`'s owning content shard.
    ///
    /// Runtime self-healing (ADR-0255): if the disposable shard file is absent
    /// or fails schema validation while a journal exists for `thread_id`,
    /// replays that single session into its owning shard on demand and retries
    /// once rather than silently reporting missing data until the next restart.
    fn open_content_reader(&self, thread_id: &str) -> Result<Option<Connection>> {
        if let Some(conn) = self.shards.open_reader_for(thread_id)? {
            return Ok(Some(conn));
        }
        if let Some(repair) = self.shard_repair.as_ref()
            && repair(thread_id)
        {
            return self.shards.open_reader_for(thread_id);
        }
        Ok(None)
    }

    /// Load the canonical durable thread aggregate from catalog + content shard (ADR-0242).
    pub fn load_thread(&self, thread_id: &str) -> Result<Option<protocol::Thread>> {
        let Some(content_conn) = self.open_content_reader(thread_id)? else {
            return Ok(None);
        };
        crate::db::content_projection::load_thread_from_planes(
            self.conn(),
            &content_conn,
            &protocol::ThreadId::new(thread_id),
            self.blob_store.as_ref(),
        )
        .map_err(rusqlite::Error::InvalidParameterName)
    }

    #[allow(dead_code)]
    pub(crate) fn usage_days(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self
            .conn()
            .prepare_cached("SELECT DISTINCT day FROM usage_records ORDER BY day DESC LIMIT ?1")?;
        stmt.query_map([limit as i64], |r| r.get(0))?.collect()
    }

    pub(crate) fn usage_records(
        &self,
        days: usize,
        limit: usize,
    ) -> Result<Vec<protocol::usage_stats::UsageStatRecord>> {
        let mut stmt = self.conn().prepare_cached("SELECT payload,day,recorded_at_ms,project FROM usage_records
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
                Ok(protocol::usage_stats::UsageStatRecord {
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
    ) -> Result<protocol::usage_stats::UsageStatsReport> {
        use protocol::usage_stats::*;
        let mut report = UsageStatsReport::default();
        let mut day_map = std::collections::BTreeMap::<String, UsageModelTotals>::new();
        let mut model_map = std::collections::BTreeMap::<(String, String), UsageModelTotals>::new();
        let mut stmt = self.conn().prepare_cached("SELECT day,provider,model,SUM(requests),SUM(completed),SUM(prompt_tokens),SUM(completion_tokens),SUM(total_tokens),SUM(cache_write_tokens),SUM(cache_read_tokens),SUM(estimated_tokens)
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
                    cost_nano_usd: 0,
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
            a.cost_nano_usd = a.cost_nano_usd.saturating_add(b.cost_nano_usd);
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
        report.source_revision = self
            .conn()
            .prepare_cached("SELECT revision FROM usage_clock WHERE id=1")?
            .query_row([], |r| r.get(0))?;
        let oldest: Option<u64> = self
            .conn()
            .prepare_cached("SELECT MIN(revision) FROM usage_dirty")?
            .query_row([], |r| r.get(0))?;
        report.projection_revision = oldest
            .map(|r| r.saturating_sub(1))
            .unwrap_or(report.source_revision);
        Ok(report)
    }

    // -- sessions ----------------------------------------------------------

    /// One thread's header row, if it exists.
    pub fn get_session(&self, session_id: &str) -> Result<Option<ThreadRecord>> {
        crate::db::thread_session::get_session(self.conn(), session_id)
    }

    /// Threads matching a role-scoped filter, newest first.
    pub fn list_sessions(
        &self,
        filter: &protocol::ThreadFilter,
        include_subagents: bool,
    ) -> Result<Vec<ThreadRecord>> {
        crate::db::thread_session::list_sessions(self.conn(), filter, include_subagents)
    }

    /// Subagent threads spawned by `parent_id`.
    pub fn list_subagent_sessions(&self, parent_id: &str) -> Result<Vec<ThreadRecord>> {
        crate::db::thread_session::list_subagent_sessions(self.conn(), parent_id)
    }

    /// Candidate sessions for switching, strictly excluding the active session.
    pub fn list_switch_candidates(
        &self,
        filter: &protocol::ThreadFilter,
        active_id: &str,
    ) -> Result<Vec<crate::thread::ThreadSummary>> {
        crate::db::thread_session::list_switch_candidates(self.conn(), filter, active_id)
    }

    /// Picker-facing summaries for a role-scoped filter, newest first.
    pub fn list_session_summaries(
        &self,
        filter: &protocol::ThreadFilter,
        active_id: &str,
    ) -> Result<Vec<crate::thread::ThreadSummary>> {
        crate::db::thread_session::list_session_summaries(self.conn(), filter, active_id)
    }

    /// The most recently updated session id matching `filter` (`--resume`).
    pub fn latest_session(&self, filter: &protocol::ThreadFilter) -> Result<Option<String>> {
        crate::db::thread_session::latest_session(self.conn(), filter)
    }

    /// A session's inherited workspace binding plus its persona.
    pub fn lookup_session_workspace(
        &self,
        session_id: &str,
    ) -> Result<Option<(Option<protocol::WorkspaceBinding>, Option<String>)>> {
        crate::db::thread_session::lookup_session_workspace(self.conn(), session_id)
    }

    /// A session's immutable role manifest snapshot.
    pub fn lookup_session_manifest(
        &self,
        session_id: &str,
    ) -> Result<Option<protocol::ThreadRoleManifest>> {
        crate::db::thread_session::lookup_session_manifest(self.conn(), session_id)
    }

    /// Resolve an id prefix to every matching session id.
    pub fn resolve_session_prefix(
        &self,
        prefix: &str,
        filter: &protocol::ThreadFilter,
    ) -> Result<Vec<String>> {
        crate::db::thread_session::resolve_session_prefix(self.conn(), prefix, filter)
    }

    /// Full detail for one session (the session-info sub-view).
    pub fn get_session_detail(
        &self,
        session_id: &str,
        active_id: &str,
    ) -> Result<Option<protocol::ThreadDetail>> {
        let content_conn = self.open_content_reader(session_id)?;
        crate::db::thread_session::get_session_detail_from_planes(
            self.conn(),
            content_conn.as_ref(),
            session_id,
            active_id,
            self.blob_store.as_ref(),
        )
    }

    /// One session's raw durable data (the load path of `ThreadSession`),
    /// assembled from catalog metadata + owning content shard (ADR-0242).
    pub fn load_session_full(&self, session_id: &str) -> Result<Option<crate::thread::ThreadData>> {
        let content_conn = self.open_content_reader(session_id)?;
        crate::db::thread_session::load_session_full_from_planes(
            self.conn(),
            content_conn.as_ref(),
            session_id,
            self.blob_store.as_ref(),
        )
    }

    /// A session's projected transcript tail as wire rows (the Archivist's
    /// read tool), assembled from catalog + owning content shard (ADR-0242).
    pub fn read_session_transcript(
        &self,
        session_id: &str,
        tail: usize,
    ) -> Result<Option<ThreadTranscriptView>> {
        let content_conn = self.open_content_reader(session_id)?;
        crate::db::thread_session::read_session_transcript_from_planes(
            self.conn(),
            content_conn.as_ref(),
            session_id,
            tail,
            self.blob_store.as_ref(),
        )
    }

    /// Every blob hash the durable reference ledger still mentions across all
    /// content shards (ADR-0242). Fail-closed if any existing shard fails
    /// schema verification (ADR-0255).
    pub fn live_blob_hashes(&self) -> Result<std::collections::HashSet<String>> {
        self.shards.live_blob_hashes()
    }

    // -- recall / history --------------------------------------------------

    /// Ensure at least one shard exists when searching globally after a
    /// runtime `rm -rf shards/` (ADR-0255).
    fn ensure_shards_for_global_search(&self) {
        if !self.shards.has_any_shard()
            && let Some(repair) = self.full_shard_repair.as_ref()
        {
            let _ = repair();
        }
    }

    /// BM25 search over every persisted transcript entry (strict AND form),
    /// fanning out across all content shards and merging by score (ADR-0242).
    pub fn search_history(
        &self,
        query: &str,
        workspace: Option<&Path>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.ensure_shards_for_global_search();
        self.shards
            .search_history_fanout(self.conn(), query, workspace, limit, false)
    }

    /// [`Self::search_history`] with the words OR-joined (recall fallback).
    pub fn search_history_relaxed(
        &self,
        query: &str,
        workspace: Option<&Path>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.ensure_shards_for_global_search();
        self.shards
            .search_history_fanout(self.conn(), query, workspace, limit, true)
    }

    /// The prompt history, oldest first, capped at `limit`.
    pub fn load_input_history(&self, limit: usize) -> Result<Vec<protocol::HistoryEntry>> {
        crate::db::input_history::load_input_history(self.conn(), limit)
    }

    // -- typed key/value ---------------------------------------------------

    /// A raw key-value entry.
    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        crate::db::kv_store::get_kv(self.conn(), key)
    }

    /// A JSON-encoded key-value entry.
    pub fn get_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        crate::db::kv_store::get_json(self.conn(), key)
    }

    /// Every key with the given prefix.
    pub fn list_kv_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        crate::db::kv_store::list_kv_keys_with_prefix(self.conn(), prefix)
    }

    /// Load all persisted `allow` and `revoked` permission rules for `(role_id, workspace)`.
    pub fn load_permission_rules(
        &self,
        target: &crate::db::permissions::PermissionScopeTarget,
    ) -> Result<Vec<crate::db::permissions::StoredPermissionRuleRow>> {
        crate::db::permissions::load_permission_rules(self.conn(), target)
    }
}
