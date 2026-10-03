//! `DatabaseEngine` implementation: session/usage/kv/history access against one
//! owned connection (ADR-0231). The type itself lives in `db.rs`.
use super::*;

impl DatabaseEngine {
    /// Open or create a database engine on a file path.
    pub(crate) fn open(db_path: &Path, blob_store: Option<BlobStore>) -> Result<Self> {
        let conn = initialize_db(db_path)?;
        let engine = Self { conn, blob_store };
        if db_path == crate::paths::get().db_file() {
            let _ = engine.migrate_legacy_input_history();
        }
        Ok(engine)
    }

    /// Open an in-memory database engine. Test-only: the shipped surface has
    /// no way to name a connection — every real read and write goes through
    /// the handle's two doors (ADR-0231).
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let conn = initialize_in_memory_db()?;
        Ok(Self {
            conn,
            blob_store: None,
        })
    }

    pub(crate) fn project_usage_batch(&self, limit: usize) -> Result<usize> {
        use nuo_wire::{RequestUsageSource, RequestUsageStatus};
        let mut stmt = self.conn.prepare("SELECT u.payload,u.day,u.revision FROM usage_dirty d JOIN usage_records u USING(session_id,actor_id,round,turn,attempt) ORDER BY d.revision LIMIT ?1")?;
        let rows = stmt
            .query_map([limit.min(128) as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>>>()?;
        drop(stmt);
        if rows.is_empty() {
            return Ok(0);
        }
        // Pure preparation precedes the write transaction; each batch is bounded.
        let prepared = rows
            .into_iter()
            .map(|(json, day, rev)| {
                Ok((
                    decode_json::<nuo_wire::RequestUsageRecord>(&json)?,
                    day,
                    rev,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for (record, day, revision) in &prepared {
            let k = &record.key;
            let terminal = record.status.is_terminal();
            let reported = terminal && record.source == RequestUsageSource::Reported;
            let reported_count = |n: i64| if reported { n.max(0) } else { 0 };
            tx.execute("INSERT INTO usage_contributions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
                ON CONFLICT(session_id,actor_id,round,turn,attempt) DO UPDATE SET
                revision=excluded.revision, day=excluded.day,provider=excluded.provider,model=excluded.model,
                requests=excluded.requests,completed=excluded.completed,prompt_tokens=excluded.prompt_tokens,
                completion_tokens=excluded.completion_tokens,total_tokens=excluded.total_tokens,
                cache_write_tokens=excluded.cache_write_tokens,cache_read_tokens=excluded.cache_read_tokens,estimated_tokens=excluded.estimated_tokens
                WHERE usage_contributions.revision < excluded.revision",
                params![k.session_id,k.actor_id,k.round,k.turn,k.attempt,revision,day,record.provider,record.model,
                    terminal,record.status==RequestUsageStatus::Completed,reported_count(record.prompt_tokens),
                    reported_count(record.completion_tokens),reported_count(record.total_tokens),
                    reported_count(record.cache_write_tokens),reported_count(record.cache_read_tokens),
                    if terminal && !reported { record.total_tokens.max(0) } else {0}])?;
            tx.execute("DELETE FROM usage_dirty WHERE session_id=?1 AND actor_id=?2 AND round=?3 AND turn=?4 AND attempt=?5 AND revision=?6",
                params![k.session_id,k.actor_id,k.round,k.turn,k.attempt,revision])?;
        }
        tx.commit()?;
        Ok(prepared.len())
    }

    /// Create an online hot backup snapshot using `VACUUM INTO`.
    pub(crate) fn create_backup(&self, target_path: &Path) -> Result<()> {
        if let Some(parent) = target_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if target_path.exists() {
            let _ = std::fs::remove_file(target_path);
        }
        let target_str = target_path.to_str().ok_or_else(|| {
            rusqlite::Error::InvalidParameterName("backup path is not valid utf-8".into())
        })?;
        self.conn.execute("VACUUM INTO ?1", params![target_str])?;
        Ok(())
    }

    // Session Operations

    /// Create or update a session record.
    pub(crate) fn upsert_session(&self, session: &SessionRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                parent_id = excluded.parent_id,
                fork_kind = excluded.fork_kind,
                title = excluded.title,
                updated_at_s = excluded.updated_at_s,
                workspace_root = excluded.workspace_root,
                persona = excluded.persona,
                msg_count = excluded.msg_count,
                last_user_prompt = excluded.last_user_prompt,
                digest = excluded.digest;
            "#,
            params![
                session.id,
                session.parent_id,
                session.fork_kind,
                session.title,
                session.created_at_s,
                session.updated_at_s,
                session.workspace_root,
                session.persona,
                session.msg_count,
                session.last_user_prompt,
                session.digest,
            ],
        )?;
        Ok(())
    }

    /// Retrieve a single session by id.
    pub(crate) fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        self.conn
            .query_row(
                "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest FROM sessions WHERE id = ?1",
                params![session_id],
                map_session_row,
            )
            .optional()
    }

    /// List sessions, optionally filtered by a derived grouping (ADR-0226),
    /// sorted by `updated_at_s` descending.
    pub(crate) fn list_sessions(
        &self,
        filter: Option<&nuo_wire::WorkspaceFilter>,
    ) -> Result<Vec<SessionRecord>> {
        const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                            workspace_root, persona, msg_count, last_user_prompt, digest";
        let mut sessions = Vec::new();
        match filter {
            Some(nuo_wire::WorkspaceFilter::Path(path)) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE workspace_root = ?1 ORDER BY updated_at_s DESC"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![path.to_string_lossy()], map_session_row)?;
                for session in rows {
                    sessions.push(session?);
                }
            }
            Some(nuo_wire::WorkspaceFilter::Unbound) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE workspace_root IS NULL ORDER BY updated_at_s DESC"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], map_session_row)?;
                for session in rows {
                    sessions.push(session?);
                }
            }
            _ => {
                let sql = format!("SELECT {COLS} FROM sessions ORDER BY updated_at_s DESC");
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], map_session_row)?;
                for session in rows {
                    sessions.push(session?);
                }
            }
        }
        Ok(sessions)
    }

    /// List subagent sessions spawned by a given parent session (ADR-0262).
    pub(crate) fn list_subagent_sessions(&self, parent_id: &str) -> Result<Vec<SessionRecord>> {
        const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                            workspace_root, persona, msg_count, last_user_prompt, digest";
        let sql = format!("SELECT {COLS} FROM sessions WHERE parent_id = ?1 AND fork_kind = 'subagent' ORDER BY created_at_s ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![parent_id], map_session_row)?;
        let mut sessions = Vec::new();
        for session in rows {
            sessions.push(session?);
        }
        Ok(sessions)
    }

    /// Delete a session and cascade all its events, messages, and command records.
    pub(crate) fn delete_session(&self, session_id: &str) -> Result<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(affected > 0)
    }

    /// The live blob set: every hash the durable `blob_refs` ledger still
    /// references. Read-only, so a blob-store sweep runs on the caller's
    /// thread against this snapshot and never holds up the writer.
    pub(crate) fn live_blob_hashes(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT hash FROM blob_refs")?;
        let live = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(Result::ok)
            .collect();
        Ok(live)
    }

    /// Reclaim transcript entries no session membership references
    /// (ADR-0187). Entries are global facts shared by identity across forks,
    /// so only zero-reference rows are removable; the foreign key from
    /// `entry_memberships` makes the two tables' agreement structural, and
    /// entry insert + membership insert share one transaction, so a GC pass
    /// can never observe the half of a pair. Returns the rows reclaimed.
    pub(crate) fn collect_entry_garbage(&self) -> Result<usize> {
        let reclaimed = self.conn.execute(
            "DELETE FROM entries WHERE NOT EXISTS (
                SELECT 1 FROM entry_memberships m WHERE m.entry_id = entries.id
            )",
            [],
        )?;
        Ok(reclaimed)
    }

    /// Persist a complete [`crate::session::SessionData`] into SQLite in one
    /// transaction (ADR-0186): the session row (identity + working state),
    /// the session's memberships and directives, and the entries themselves
    /// (upsert — facts are shared by identity across forks).
    ///
    /// Write `data` authoritatively (full rewrite). Test-only: the one
    /// production writer is `PersistenceCommand::SaveSession`, which selects
    /// append vs. rewrite by generation (ADR-0187/0231).
    #[cfg(test)]
    pub(crate) fn save_session_full(
        &self,
        data: &crate::session::SessionData,
    ) -> std::result::Result<(), SaveError> {
        self.save_session_inner(data, true, &[], &CommitGuard::default())
            .map(|_| ())
    }

    pub(crate) fn save_session_inner(
        &self,
        data: &crate::session::SessionData,
        force_full: bool,
        usage_upserts: &[nuo_wire::RequestUsageRecord],
        guard: &CommitGuard,
    ) -> std::result::Result<u64, SaveError> {
        let fork_str = match data.fork_kind {
            nuo_wire::SessionForkKind::Trunk => "trunk",
            nuo_wire::SessionForkKind::Fork => "fork",
            nuo_wire::SessionForkKind::Aside => "aside",
            nuo_wire::SessionForkKind::Subagent => "subagent",
        };
        let digest_str = data
            .digest
            .as_ref()
            .and_then(|d| serde_json::to_string(d).ok());
        let last_prompt = crate::session::last_effective_prompt_from_data(data);
        let msg_count = (data.transcript.entries.len() + data.unknown_entries.len()) as i64;
        let tree_str = serde_json::to_string(&data.tree)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        // Expensive, pure payload preparation runs before the transaction
        // (ADR-0236 D3); only the receipt comparison runs inside it.
        let payload_hash = if guard.operation_id.is_some() {
            Some(commit_payload_digest(data, usage_upserts)?)
        } else {
            None
        };

        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let res: std::result::Result<u64, SaveError> = (|| {
            // ADR-0236 D3: resolve an idempotent replay before any write, then
            // enforce the revision precondition. A replay whose payload matches
            // returns the original receipt; reusing an identity for different
            // content is refused instead of silently applied or dropped.
            let current_revision = session_revision(&self.conn, &data.id)?;
            if let (Some(operation_id), Some(payload_hash)) =
                (guard.operation_id.as_deref(), payload_hash.as_deref())
                && let Some((stored_id, stored_hash, stored_revision)) =
                    latest_commit_receipt(&self.conn, &data.id)?
                && stored_id == operation_id
            {
                if stored_hash == payload_hash {
                    return Ok(stored_revision);
                }
                return Err(SaveError::OperationConflict {
                    operation_id: operation_id.to_string(),
                });
            }
            if let Some(expected) = guard.expected_revision
                && expected != current_revision
            {
                return Err(SaveError::StaleRevision {
                    expected,
                    actual: current_revision,
                });
            }

            // Read the durable generation BEFORE the row upsert stamps the
            // incoming one: the comparison decides full-rewrite vs append.
            let stored_generation: Option<String> = self
                .conn
                .query_row(
                    "SELECT transcript_generation FROM sessions WHERE id = ?1",
                    params![data.id],
                    |row| row.get(0),
                )
                .unwrap_or(None);
            // A `None` stored generation (never saved, or saved before the
            // generation column existed) also forces the full rewrite.
            let full = force_full || stored_generation.as_deref() != Some(data.generation.as_str());

            let persona = data.role.clone();
            let workspace_root = data
                .workspace
                .as_ref()
                .map(|w| w.root.to_string_lossy().into_owned());
            let additional_roots = match data.workspace.as_ref() {
                Some(w) => serde_json::to_string(&w.additional_roots),
                None => Ok("[]".to_string()),
            }
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

            self.conn.execute(
                r#"
                INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, additional_roots, persona, msg_count, last_user_prompt, digest, digest_anchor, tree, transcript_generation, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_resolutions, retry_pending, checksum, schema_version, role_manifest)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
                ON CONFLICT(id) DO UPDATE SET
                    parent_id = excluded.parent_id,
                    fork_kind = excluded.fork_kind,
                    title = excluded.title,
                    updated_at_s = excluded.updated_at_s,
                    workspace_root = excluded.workspace_root,
                    additional_roots = excluded.additional_roots,
                    persona = excluded.persona,
                    msg_count = excluded.msg_count,
                    last_user_prompt = excluded.last_user_prompt,
                    digest = excluded.digest,
                    digest_anchor = excluded.digest_anchor,
                    tree = excluded.tree,
                    transcript_generation = excluded.transcript_generation,
                    provider_connection = excluded.provider_connection,
                    round_counter = excluded.round_counter,
                    unattended = excluded.unattended,
                    disabled_tools = excluded.disabled_tools,
                    commands = excluded.commands,
                    round_interrupts = excluded.round_interrupts,
                    retry_resolutions = excluded.retry_resolutions,
                    retry_pending = excluded.retry_pending,
                    checksum = excluded.checksum,
                    schema_version = excluded.schema_version,
                    role_manifest = COALESCE(excluded.role_manifest, sessions.role_manifest);
                "#,
                params![
                    data.id,
                    data.parent_id,
                    fork_str,
                    data.title,
                    data.created_at as i64,
                    data.updated_at as i64,
                    workspace_root,
                    additional_roots,
                    persona,
                    msg_count,
                    last_prompt,
                    digest_str,
                    data.digest_anchor.map(|a| a as i64),
                    tree_str,
                    data.generation,
                    data.provider_selection.as_ref().and_then(|s| serde_json::to_string(s).ok()),
                    data.round_counter as i64,
                    data.unattended,
                    serde_json::to_string(&data.disabled_tools.iter().collect::<Vec<_>>()).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    serde_json::to_string(&data.commands).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    serde_json::to_string(&data.round_interrupts).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    serde_json::to_string(&data.retry_resolutions).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    data.retry_pending.as_ref().and_then(|p| serde_json::to_string(p).ok()),
                    data.checksum.map(|c| c as i64),
                    data.schema_version as i64,
                    data.role_manifest.as_ref().and_then(|m| serde_json::to_string(m).ok()),
                ],
            )?;

            if full {
                // Projection decisions: the session's own view history.
                self.conn.execute(
                    "DELETE FROM projections WHERE session_id = ?1",
                    params![data.id],
                )?;
                for directive in &data.transcript.directives {
                    self.insert_directive(data.id.as_str(), directive)?;
                }
                for unknown in &data.unknown_directives {
                    self.insert_unknown_directive(data.id.as_str(), unknown)?;
                }

                // Facts + memberships. Entries are global and shared by identity.
                self.conn.execute(
                    "DELETE FROM entry_memberships WHERE session_id = ?1",
                    params![data.id],
                )?;
                self.conn.execute(
                    "DELETE FROM blob_refs WHERE session_id = ?1",
                    params![data.id],
                )?;
                for entry in &data.transcript.entries {
                    self.upsert_entry(entry)?;
                    self.insert_membership(data.id.as_str(), entry.seq, entry.id.as_str())?;
                }
                for unknown in &data.unknown_entries {
                    self.upsert_entry_row(EntryEnvelope {
                        id: unknown.id.as_str(),
                        kind: unknown.kind.as_str(),
                        role: unknown.role.as_deref(),
                        content: unknown.content.as_deref(),
                        origin: unknown.origin.as_deref(),
                        hidden: unknown.hidden,
                        created_at_ms: unknown.created_at_ms,
                        payload: unknown.payload_json.as_str(),
                    })?;
                    self.insert_membership(data.id.as_str(), unknown.seq, unknown.id.as_str())?;
                }
                self.record_blob_refs(
                    data.id.as_str(),
                    &data.transcript.entries,
                    &data.unknown_entries,
                )?;
                for record in &data.request_usage_records {
                    insert_usage_record_tx(&self.conn, data.id.as_str(), record)?;
                }
                // A delta commit escalated to a full rewrite (generation
                // mismatch) carries no usage mirror; apply its changed upserts
                // here so the escalation cannot drop them (ADR-0236 invariant #3).
                for record in usage_upserts {
                    insert_usage_record_tx(&self.conn, data.id.as_str(), record)?;
                }
            } else {
                let watermark: i64 = self.conn.query_row(
                    "SELECT COALESCE(MAX(seq), -1) FROM entry_memberships WHERE session_id = ?1",
                    params![data.id],
                    |row| row.get(0),
                )?;
                let entry_start = data
                    .transcript
                    .entries
                    .partition_point(|entry| (entry.seq as i64) <= watermark);
                let mut new_entries = Vec::new();
                for entry in &data.transcript.entries[entry_start..] {
                    self.upsert_entry(entry)?;
                    self.insert_membership(data.id.as_str(), entry.seq, entry.id.as_str())?;
                    new_entries.push(entry);
                }
                let directive_watermark: i64 = self.conn.query_row(
                    "SELECT COALESCE(MAX(seq), -1) FROM projections WHERE session_id = ?1",
                    params![data.id],
                    |row| row.get(0),
                )?;
                let directive_start = data
                    .transcript
                    .directives
                    .partition_point(|directive| (directive.seq as i64) <= directive_watermark);
                for directive in &data.transcript.directives[directive_start..] {
                    self.insert_directive(data.id.as_str(), directive)?;
                }
                let new_entries: Vec<nuo_wire::TranscriptEntry> =
                    new_entries.into_iter().cloned().collect();
                self.record_blob_refs(data.id.as_str(), &new_entries, &[])?;
                for record in usage_upserts {
                    insert_usage_record_tx(&self.conn, data.id.as_str(), record)?;
                }
            }

            // ADR-0236 D3: the revision advance and the receipt land in the
            // same transaction as the facts they describe, so recovery can
            // always resolve an unknown outcome by operation identity.
            let committed_revision = current_revision + 1;
            self.conn.execute(
                "INSERT INTO session_revisions(session_id, revision) VALUES(?1, ?2)
                 ON CONFLICT(session_id) DO UPDATE SET revision=excluded.revision",
                params![data.id, committed_revision as i64],
            )?;
            if let (Some(operation_id), Some(payload_hash)) =
                (guard.operation_id.as_deref(), payload_hash.as_deref())
            {
                record_commit_receipt(
                    &self.conn,
                    &data.id,
                    operation_id,
                    committed_revision,
                    payload_hash,
                )?;
            }
            Ok(committed_revision)
        })();

        match res {
            Ok(revision) => {
                self.conn.execute("COMMIT", [])?;
                Ok(revision)
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    fn insert_membership(&self, session_id: &str, seq: u64, entry_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO entry_memberships (session_id, seq, entry_id) VALUES (?1, ?2, ?3)",
            params![session_id, seq as i64, entry_id],
        )?;
        Ok(())
    }

    fn insert_directive(
        &self,
        session_id: &str,
        directive: &nuo_wire::ProjectionDirective,
    ) -> Result<()> {
        let payload = serde_json::to_string(&directive.payload)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.conn.execute(
            "INSERT OR IGNORE INTO projections (session_id, seq, kind, up_to_seq, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                directive.seq as i64,
                serde_plain(directive.kind)?,
                directive.up_to_seq as i64,
                payload,
            ],
        )?;
        Ok(())
    }

    fn insert_unknown_directive(
        &self,
        session_id: &str,
        unknown: &crate::session::UnknownDirectiveRow,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO projections (session_id, seq, kind, up_to_seq, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                unknown.seq as i64,
                unknown.kind.as_str(),
                unknown.up_to_seq as i64,
                unknown.payload_json.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Write one entry row, offloading an oversized body to the CAS when this
    /// engine has a blob store (ADR-0187). The offload applies only to the
    /// row about to be inserted: already-durable rows keep their stored shape.
    fn upsert_entry(&self, entry: &nuo_wire::TranscriptEntry) -> Result<()> {
        let mut payload = entry.payload.clone();
        let mut content = entry.content.clone();
        if content
            .as_ref()
            .is_some_and(|c| c.len() > CAS_THRESHOLD_BYTES)
            && let nuo_wire::EntryPayload::Message(message_payload) = &mut payload
            && message_payload.content_blob.is_none()
            && let Some(blob_store) = &self.blob_store
        {
            let hash = blob_store
                .put(content.as_deref().unwrap_or_default().as_bytes())
                .map_err(rusqlite::Error::InvalidParameterName)?;
            message_payload.content_blob = Some(hash);
            if let Some(c) = &mut content {
                c.clear();
            }
        }
        // ADR-0285: Durable CAS publication for multimodal images
        if let Some(blob_store) = &self.blob_store
            && let nuo_wire::EntryPayload::Message(message_payload) = &payload
            && let Some(images) = &message_payload.images
        {
            use base64::{Engine, engine::general_purpose::STANDARD};
            for img in images {
                if let Ok(bytes) = STANDARD.decode(&img.data) {
                    let _ = blob_store.put(&bytes);
                }
            }
        }
        let payload = serde_json::to_string(&payload)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.upsert_entry_row(EntryEnvelope {
            id: entry.id.as_str(),
            kind: if entry.kind == nuo_wire::EntryKind::State {
                "state"
            } else {
                "message"
            },
            role: entry.role.map(role_str),
            content: content.as_deref(),
            origin: entry.origin.map(origin_str),
            hidden: entry.hidden,
            created_at_ms: entry.created_at_ms,
            payload: payload.as_str(),
        })
    }

    fn upsert_entry_row(&self, row: EntryEnvelope<'_>) -> Result<()> {
        let EntryEnvelope {
            id,
            kind,
            role,
            content,
            origin,
            hidden,
            created_at_ms,
            payload,
        } = row;
        self.conn.execute(
            r#"
            INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                role = excluded.role,
                content = excluded.content,
                origin = excluded.origin,
                hidden = excluded.hidden,
                created_at_ms = excluded.created_at_ms,
                payload = excluded.payload;
            "#,
            params![
                id,
                kind,
                role,
                content,
                origin,
                hidden,
                created_at_ms as i64,
                payload
            ],
        )?;
        Ok(())
    }

    /// Maintain the durable blob reference ledger for the entries this save
    /// touches. Callers pass exactly the entries whose memberships they wrote.
    fn record_blob_refs(
        &self,
        session_id: &str,
        entries: &[nuo_wire::TranscriptEntry],
        unknown: &[crate::session::UnknownEntryRow],
    ) -> Result<()> {
        let mut refs: Vec<String> = Vec::new();
        for entry in entries {
            if let nuo_wire::EntryPayload::Message(payload) = &entry.payload {
                if let Some(hash) = &payload.content_blob {
                    refs.push(hash.clone());
                }
                if let Some(images) = &payload.images {
                    use base64::{Engine, engine::general_purpose::STANDARD};
                    for img in images {
                        if let Ok(bytes) = STANDARD.decode(&img.data) {
                            refs.push(crate::blobs::BlobStore::hash(&bytes));
                        }
                    }
                }
            }
        }
        for row in unknown {
            if let Some(hash) = extract_content_blob(&row.payload_json) {
                refs.push(hash);
            }
        }
        for hash in refs {
            self.conn.execute(
                "INSERT OR IGNORE INTO blob_refs (session_id, hash) VALUES (?1, ?2)",
                params![session_id, hash],
            )?;
        }
        Ok(())
    }

    /// Read the durable usage ledger for one session, in deterministic key
    /// order (ADR-0187): rows live in their own table, not on the session row.
    fn load_usage_records(
        &self,
        session_id: &str,
    ) -> Result<Vec<nuo_wire::RequestUsageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT payload FROM usage_records WHERE session_id = ?1
             ORDER BY round ASC, turn ASC, attempt ASC, actor_id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for payload in rows {
            let payload = payload?;
            match serde_json::from_str(&payload) {
                Ok(record) => records.push(record),
                Err(error) => tracing::warn!(
                    session = %session_id,
                    error = %error,
                    "usage record payload undecodable; skipped"
                ),
            }
        }
        Ok(records)
    }

    /// Load a full [`crate::session::SessionData`] by session ID from SQLite.
    /// Entries and directives whose payloads the current binary cannot decode
    /// are preserved verbatim (ADR-0187): they ride in memory as raw rows and
    /// round-trip through every save untouched, so a database written by a
    /// newer binary survives an older binary without loss or orphaning.
    pub(crate) fn load_session_full(
        &self,
        session_id: &str,
    ) -> Result<Option<crate::session::SessionData>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, additional_roots, persona, digest, digest_anchor, tree, transcript_generation, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_resolutions, retry_pending, checksum, schema_version, role_manifest FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, i64>(14)?,
                        row.get::<_, i64>(15)?,
                        row.get::<_, String>(16)?,
                        row.get::<_, String>(17)?,
                        row.get::<_, String>(18)?,
                        row.get::<_, String>(19)?,
                        row.get::<_, Option<String>>(20)?,
                        row.get::<_, Option<i64>>(21)?,
                        row.get::<_, i64>(22)?,
                        row.get::<_, Option<String>>(23)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            parent_id,
            fork_kind,
            title,
            created_at_s,
            updated_at_s,
            workspace_root,
            additional_roots,
            persona,
            digest,
            digest_anchor,
            tree,
            generation,
            provider_connection,
            round_counter,
            unattended,
            disabled_tools,
            commands,
            round_interrupts,
            retry_resolutions,
            retry_pending,
            checksum,
            schema_version,
            role_manifest,
        )) = row
        else {
            return Ok(None);
        };

        let mut entries = Vec::new();
        let mut unknown_entries = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT e.id, m.seq, e.kind, e.role, e.content, e.origin, e.hidden, e.created_at_ms, e.payload \
                 FROM entry_memberships m JOIN entries e ON e.id = m.entry_id \
                 WHERE m.session_id = ?1 ORDER BY m.seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?;
            for row in rows {
                let (eid, seq, kind, role, content, origin, hidden, created_at_ms, payload) = row?;
                let decoded = (|| -> Option<nuo_wire::TranscriptEntry> {
                    let kind = if kind == "state" {
                        nuo_wire::EntryKind::State
                    } else {
                        nuo_wire::EntryKind::Message
                    };
                    let role = role.as_deref().and_then(role_from_str);
                    let origin = origin.as_deref().and_then(origin_from_str);
                    let payload: nuo_wire::EntryPayload =
                        serde_json::from_str(&payload).ok()?;
                    Some(nuo_wire::TranscriptEntry {
                        id: eid.clone(),
                        seq: seq.max(0) as u64,
                        kind,
                        role,
                        content: content.clone(),
                        origin,
                        hidden: hidden != 0,
                        created_at_ms: created_at_ms.max(0) as u64,
                        payload,
                    })
                })();
                if let Some(entry) = decoded {
                    entries.push(entry);
                } else {
                    tracing::warn!(
                        session = %session_id,
                        entry = %eid,
                        seq,
                        "entry payload not decodable by this binary; preserved verbatim"
                    );
                    unknown_entries.push(crate::session::UnknownEntryRow {
                        id: eid,
                        seq: seq.max(0) as u64,
                        kind,
                        role,
                        content,
                        origin,
                        hidden: hidden != 0,
                        created_at_ms: created_at_ms.max(0) as u64,
                        payload_json: payload,
                    });
                }
            }
        }

        let mut directives = Vec::new();
        let mut unknown_directives = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT seq, kind, up_to_seq, payload FROM projections WHERE session_id = ?1 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (seq, kind, up_to_seq, payload) = row?;
                let decoded = (|| -> Option<nuo_wire::ProjectionDirective> {
                    let kind = match kind.as_str() {
                        "prune" => nuo_wire::DirectiveKind::Prune,
                        "compact" => nuo_wire::DirectiveKind::Compact,
                        "freeze" => nuo_wire::DirectiveKind::Freeze,
                        _ => return None,
                    };
                    let payload: nuo_wire::DirectivePayload =
                        serde_json::from_str(&payload).ok()?;
                    Some(nuo_wire::ProjectionDirective {
                        seq: seq.max(0) as u64,
                        kind,
                        up_to_seq: up_to_seq.max(0) as u64,
                        payload,
                    })
                })();
                if let Some(directive) = decoded {
                    directives.push(directive);
                } else {
                    tracing::warn!(
                        session = %session_id,
                        seq,
                        "directive payload not decodable by this binary; preserved verbatim"
                    );
                    unknown_directives.push(crate::session::UnknownDirectiveRow {
                        seq: seq.max(0) as u64,
                        kind,
                        up_to_seq: up_to_seq.max(0) as u64,
                        payload_json: payload,
                    });
                }
            }
        }

        let digest: Option<nuo_wire::SessionDigest> = match digest.as_deref() {
            Some(raw) => match serde_json::from_str(raw) {
                Ok(parsed) => Some(parsed),
                Err(error) => {
                    tracing::warn!(session = %session_id, error = %error, "digest column undecodable; ignored");
                    None
                }
            },
            None => None,
        };
        let tree = match tree.as_deref() {
            Some(raw) => serde_json::from_str(raw).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "session tree column undecodable; reset");
                Default::default()
            }),
            None => Default::default(),
        };

        let data = crate::session::SessionData {
            transcript: nuo_wire::Transcript {
                min_next_seq: entries
                    .iter()
                    .map(|entry| entry.seq + 1)
                    .chain(unknown_entries.iter().map(|row| row.seq + 1))
                    .max()
                    .unwrap_or(0),
                min_next_directive_seq: directives
                    .iter()
                    .map(|directive| directive.seq + 1)
                    .chain(unknown_directives.iter().map(|row| row.seq + 1))
                    .max()
                    .unwrap_or(0),
                entries,
                directives,
            },
            last_projection: None,
            digest,
            // The anchor is a transcript char count (ADR-0187): persisted for
            // real; legacy rows without one refresh their digest once.
            digest_anchor: digest_anchor.map(|a| a.max(0) as u64),
            id,
            parent_id,
            fork_kind: match fork_kind.as_str() {
                "fork" => nuo_wire::SessionForkKind::Fork,
                "aside" => nuo_wire::SessionForkKind::Aside,
                "subagent" => nuo_wire::SessionForkKind::Subagent,
                _ => nuo_wire::SessionForkKind::Trunk,
            },
            title,
            created_at: created_at_s.max(0) as u64,
            updated_at: updated_at_s.max(0) as u64,
            role: persona,
            role_manifest: role_manifest
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            workspace: workspace_root.map(|root| nuo_wire::WorkspaceBinding {
                root: PathBuf::from(root),
                additional_roots: serde_json::from_str(&additional_roots).unwrap_or_else(|error| {
                    tracing::warn!(session = %session_id, error = %error, "additional_roots column undecodable; treated as empty");
                    Vec::new()
                }),
            }),
            schema_version: if schema_version > 0 { schema_version as u32 } else { crate::session::CURRENT_SCHEMA_VERSION },
            checksum: checksum.map(|c| c as u32),
            generation: generation.unwrap_or_default(),
            provider_selection: provider_connection
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            disabled_tools: serde_json::from_str(&disabled_tools).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "disabled_tools column undecodable; treated as empty");
                Default::default()
            }),
            round_counter: round_counter.max(0) as u64,
            request_usage_records: self.load_usage_records(session_id)?,
            commands: serde_json::from_str(&commands).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "commands column undecodable; treated as empty");
                Default::default()
            }),
            round_interrupts: serde_json::from_str(&round_interrupts).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "round_interrupts column undecodable; treated as empty");
                Default::default()
            }),
            retry_resolutions: serde_json::from_str(&retry_resolutions).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "retry_resolutions column undecodable; treated as empty");
                Default::default()
            }),
            retry_pending: retry_pending
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            unattended: unattended != 0,
            tree,
            unknown_entries,
            unknown_directives,
        };
        crate::session::verify_checksum(&data, session_id);
        Ok(Some(data))
    }

    /// Public read-only transcript accessor for out-of-crate readers (the
    /// runtime's Archivist tools, ADR-0208): the full persisted
    /// [`crate::session::SessionData`] for one session, checksum-verified,
    /// or `None` when the id is unknown.
    /// One persisted session's projected transcript tail plus metadata, as
    /// plain wire-serializable rows (ADR-0208): the read path the Archivist's
    /// `archivist_read_session` tool serves. [`SessionTranscriptView`] keeps
    /// `SessionData`'s fields private while exposing exactly the projection
    /// the retrieval plane needs.
    pub(crate) fn read_session_transcript(
        &self,
        session_id: &str,
        tail: usize,
    ) -> Result<Option<SessionTranscriptView>> {
        let Some(data) = self.load_session_full(session_id)? else {
            return Ok(None);
        };
        let projected = data.transcript.project();
        let total = projected.len();
        let start = total.saturating_sub(tail);
        Ok(Some(SessionTranscriptView {
            id: data.id.clone(),
            title: data.title.clone(),
            digest: data.digest.clone(),
            workspace_root: data
                .workspace
                .as_ref()
                .map(|w| w.root.to_string_lossy().into_owned()),
            message_count: total,
            messages: projected[start..]
                .iter()
                .map(|(seq, m)| SessionMessageView {
                    seq: *seq,
                    role: role_str(m.role).to_string(),
                    content: m.content.clone(),
                })
                .collect(),
        }))
    }

    /// Resolve a session ID prefix (4+ hex chars) to matching full session IDs.
    pub(crate) fn resolve_session_prefix(
        &self,
        prefix: &str,
        filter: Option<&nuo_wire::WorkspaceFilter>,
    ) -> Result<Vec<String>> {
        let pattern = format!("{prefix}%");
        let mut matches = Vec::new();
        match filter {
            Some(nuo_wire::WorkspaceFilter::Path(path)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM sessions WHERE id LIKE ?1 AND workspace_root = ?2 ORDER BY updated_at_s DESC",
                )?;
                let rows =
                    stmt.query_map(params![pattern, path.to_string_lossy()], |row| row.get(0))?;
                for id in rows {
                    matches.push(id?);
                }
            }
            Some(nuo_wire::WorkspaceFilter::Unbound) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM sessions WHERE id LIKE ?1 AND workspace_root IS NULL ORDER BY updated_at_s DESC",
                )?;
                let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
                for id in rows {
                    matches.push(id?);
                }
            }
            _ => {
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM sessions WHERE id LIKE ?1 ORDER BY updated_at_s DESC",
                )?;
                let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
                for id in rows {
                    matches.push(id?);
                }
            }
        }
        Ok(matches)
    }

    /// Resolve a session's durable workspace binding and staffing persona by
    /// exact id, regardless of the caller (ADR-0226). Used to lazily resume a
    /// session from any client and re-apply its persona.
    #[allow(clippy::type_complexity)]
    pub(crate) fn lookup_session_workspace(
        &self,
        session_id: &str,
    ) -> Result<Option<(Option<nuo_wire::WorkspaceBinding>, Option<String>)>> {
        let row = self
            .conn
            .query_row(
                "SELECT workspace_root, additional_roots, persona FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((workspace_root, additional_roots, persona)) = row else {
            return Ok(None);
        };
        let workspace = workspace_root.map(|root| nuo_wire::WorkspaceBinding {
            root: PathBuf::from(root),
            additional_roots: serde_json::from_str(&additional_roots).unwrap_or_default(),
        });
        Ok(Some((workspace, persona)))
    }

    /// Look up a session's immutable role manifest snapshot (ADR-0245).
    pub(crate) fn lookup_session_manifest(
        &self,
        session_id: &str,
    ) -> Result<Option<nuo_wire::SessionRoleManifest>> {
        let row: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT role_manifest FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(Some(raw_manifest)) = row else {
            return Ok(None);
        };
        Ok(serde_json::from_str(&raw_manifest).ok())
    }

    /// List session summaries for a derived grouping, sorted by `updated_at_s`
    /// descending.
    /// Retrieve switch candidate sessions for a given domain partition, strictly
    /// excluding the active session (ADR-0250).
    pub(crate) fn list_switch_candidates(
        &self,
        partition: &nuo_wire::SessionPartition,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                            msg_count, last_user_prompt, digest";
        let mut summaries = Vec::new();
        match partition {
            nuo_wire::SessionPartition::Workspace(path) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions \
                     WHERE workspace_root = ?1 AND id <> ?2 AND fork_kind <> 'subagent' \
                     ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows =
                    stmt.query_map(params![path.to_string_lossy(), active_id], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
            nuo_wire::SessionPartition::Role(role_id) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions \
                     WHERE workspace_root IS NULL AND persona = ?1 AND id <> ?2 AND fork_kind <> 'subagent' \
                     ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![role_id, active_id], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
        }
        summaries.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        Ok(summaries)
    }

    /// List session summaries for a domain partition, with active session tagged (ADR-0250).
    pub(crate) fn list_session_summaries_in_partition(
        &self,
        partition: &nuo_wire::SessionPartition,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                            msg_count, last_user_prompt, digest";
        let mut summaries = Vec::new();
        match partition {
            nuo_wire::SessionPartition::Workspace(path) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions \
                     WHERE workspace_root = ?1 AND fork_kind <> 'subagent' \
                     ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![path.to_string_lossy()], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
            nuo_wire::SessionPartition::Role(role_id) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions \
                     WHERE workspace_root IS NULL AND persona = ?1 AND fork_kind <> 'subagent' \
                     ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![role_id], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
        }
        summaries.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        Ok(summaries)
    }

    pub(crate) fn list_session_summaries(
        &self,
        filter: Option<&nuo_wire::WorkspaceFilter>,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                            msg_count, last_user_prompt, digest";
        let mut summaries = Vec::new();
        match filter {
            Some(nuo_wire::WorkspaceFilter::Path(path)) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE workspace_root = ?1 AND fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![path.to_string_lossy()], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
            Some(nuo_wire::WorkspaceFilter::Unbound) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE workspace_root IS NULL AND fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
            _ => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
        }
        summaries.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        Ok(summaries)
    }

    /// The most recent non-subagent session matching `filter`, optionally
    /// restricted to a staffing persona (ADR-0226). Backs `--resume`.
    pub(crate) fn latest_session(
        &self,
        filter: &nuo_wire::WorkspaceFilter,
        persona: Option<&str>,
    ) -> Result<Option<String>> {
        use nuo_wire::WorkspaceFilter;
        let base = "SELECT id FROM sessions WHERE fork_kind <> 'subagent'";
        let (sql, bind_path) = match (filter, persona.is_some()) {
            (WorkspaceFilter::Path(_), true) => (
                format!(
                    "{base} AND workspace_root = ?1 AND persona = ?2 ORDER BY updated_at_s DESC LIMIT 1"
                ),
                true,
            ),
            (WorkspaceFilter::Path(_), false) => (
                format!("{base} AND workspace_root = ?1 ORDER BY updated_at_s DESC LIMIT 1"),
                true,
            ),
            (WorkspaceFilter::Unbound, true) => (
                format!(
                    "{base} AND workspace_root IS NULL AND persona = ?1 ORDER BY updated_at_s DESC LIMIT 1"
                ),
                false,
            ),
            (WorkspaceFilter::Unbound, false) => (
                format!("{base} AND workspace_root IS NULL ORDER BY updated_at_s DESC LIMIT 1"),
                false,
            ),
            (WorkspaceFilter::Any, true) => (
                format!("{base} AND persona = ?1 ORDER BY updated_at_s DESC LIMIT 1"),
                false,
            ),
            (WorkspaceFilter::Any, false) => {
                (format!("{base} ORDER BY updated_at_s DESC LIMIT 1"), false)
            }
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let get = |row: &Row| row.get::<_, String>(0);
        let found = if let (true, Some(persona_val)) = (bind_path, persona) {
            let path = filter
                .as_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            stmt.query_row(params![path, persona_val], get).optional()?
        } else if bind_path {
            let path = filter
                .as_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            stmt.query_row(params![path], get).optional()?
        } else if let Some(persona) = persona {
            stmt.query_row(params![persona], get).optional()?
        } else {
            stmt.query_row([], get).optional()?
        };
        Ok(found)
    }

    /// The most recent non-subagent session in a partition (ADR-0250). Backs `--resume`.
    pub(crate) fn latest_session_in_partition(
        &self,
        partition: &nuo_wire::SessionPartition,
    ) -> Result<Option<String>> {
        let base = "SELECT id FROM sessions WHERE fork_kind <> 'subagent'";
        let get = |row: &Row| row.get::<_, String>(0);
        let found = match partition {
            nuo_wire::SessionPartition::Workspace(path) => {
                let sql =
                    format!("{base} AND workspace_root = ?1 ORDER BY updated_at_s DESC LIMIT 1");
                let mut stmt = self.conn.prepare(&sql)?;
                stmt.query_row(params![path.to_string_lossy()], get)
                    .optional()?
            }
            nuo_wire::SessionPartition::Role(role_id) => {
                let sql = format!(
                    "{base} AND workspace_root IS NULL AND persona = ?1 ORDER BY updated_at_s DESC LIMIT 1"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                stmt.query_row(params![role_id], get).optional()?
            }
        };
        Ok(found)
    }

    /// Retrieve full session detail for on-demand inspection.
    pub(crate) fn get_session_detail(
        &self,
        session_id: &str,
        active_id: &str,
    ) -> Result<Option<nuo_wire::SessionDetail>> {
        if let Some(data) = self.load_session_full(session_id)? {
            let last_prompt = crate::session::last_effective_prompt_from_data(&data);
            Ok(Some(nuo_wire::SessionDetail {
                id: data.id.clone(),
                title: data.title.clone(),
                digest: data.digest.clone(),
                created_at: data.created_at,
                updated_at: data.updated_at,
                message_count: data.transcript.entries.len(),
                active: data.id == active_id,
                last_prompt,
            }))
        } else {
            Ok(None)
        }
    }

    /// Rename a session in the database. ADR-0186: a non-`NULL` title is
    /// terminal; the manual flag is retained in the signature for the command
    /// surface but no longer stored. A single-row UPDATE: the transcript and
    /// working state are untouched, so no load/save round trip is needed.
    pub(crate) fn rename_session(
        &self,
        session_id: &str,
        title: Option<&str>,
        manual: bool,
    ) -> Result<bool> {
        let _ = manual;
        let now = crate::session::unix_timestamp() as i64;
        let affected = self.conn.execute(
            "UPDATE sessions SET title = ?1, updated_at_s = ?2 WHERE id = ?3",
            params![title, now, session_id],
        )?;
        Ok(affected > 0)
    }

    // Typed JSON KV Helpers (ADR-0168)

    /// Set (or overwrite) a key in the unified KV store.
    pub(crate) fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv_store (key, value, updated_at) VALUES (?1, ?2, strftime('%s','now')) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value],
        )?;
        Ok(())
    }

    /// Fetch a key from the unified KV store.
    pub(crate) fn get_kv(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM kv_store WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
    }

    /// Delete a key from the unified KV store.
    pub(crate) fn delete_kv(&self, key: &str) -> Result<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM kv_store WHERE key = ?1", params![key])?;
        Ok(affected > 0)
    }

    /// Record a slash-command invocation in the durable command ledger.
    pub(crate) fn record_command(&self, cmd: &nuo_wire::CommandRecord) -> Result<()> {
        let id = format!(
            "{}:{}:{}",
            cmd.name,
            cmd.timestamp,
            nuo_wire::todos::unix_now()
        );
        self.conn.execute(
            r#"
            INSERT INTO commands (id, session_id, name, arguments, result, status, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                id,
                "", // command ledger rows are session-agnostic audit records
                cmd.name,
                cmd.args,
                cmd.result
                    .as_ref()
                    .and_then(|r| serde_json::to_string(r).ok()),
                match cmd.status {
                    nuo_wire::CommandStatus::Success => "ok",
                    nuo_wire::CommandStatus::Error => "failed",
                    nuo_wire::CommandStatus::UserCancelled => "cancelled",
                },
                cmd.timestamp as i64,
            ],
        )?;
        Ok(())
    }

    /// Insert or replace one request-projection record (ADR-0218). Keyed by
    /// `(session, round, turn)`, so a re-recorded logical invocation replaces
    /// its row. After the insert, the per-session archive is trimmed to the
    /// newest [`MAX_RETAINED_REQUEST_PROJECTIONS`] rows.
    pub(crate) fn insert_request_projection(
        &self,
        session_id: &str,
        record: &nuo_wire::RequestProjection,
    ) -> Result<()> {
        let payload = serde_json::to_string(record)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.conn.execute(
            "INSERT OR REPLACE INTO request_projections
                (session_id, round, turn, created_at_ms, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                record.round as i64,
                record.turn as i64,
                record.created_at_ms as i64,
                payload,
            ],
        )?;
        self.conn.execute(
            "DELETE FROM request_projections
             WHERE session_id = ?1
               AND rowid NOT IN (
                   SELECT rowid FROM request_projections
                   WHERE session_id = ?1
                   ORDER BY created_at_ms DESC, rowid DESC
                   LIMIT ?2
               )",
            params![session_id, MAX_RETAINED_REQUEST_PROJECTIONS as i64],
        )?;
        Ok(())
    }

    /// Load a session's request-projection archive in capture order, oldest
    /// first, capped at `limit`. Undecodable payloads are skipped with a log,
    /// never surfaced as history.
    pub(crate) fn load_request_projections(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<nuo_wire::RequestProjection>> {
        let mut stmt = self.conn.prepare(
            "SELECT payload FROM request_projections WHERE session_id = ?1
             ORDER BY created_at_ms ASC, rowid ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut records = Vec::new();
        for payload in rows {
            let payload = payload?;
            match serde_json::from_str(&payload) {
                Ok(record) => records.push(record),
                Err(error) => tracing::warn!(
                    session = %session_id,
                    error = %error,
                    "request projection payload undecodable; skipped"
                ),
            }
        }
        Ok(records)
    }

    /// List keys with a given prefix, ordered descending.
    pub(crate) fn list_kv_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let pattern = format!("{prefix}%");
        let mut stmt = self
            .conn
            .prepare("SELECT key FROM kv_store WHERE key LIKE ?1 ORDER BY key DESC")?;
        let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
        let mut keys = Vec::new();
        for k in rows {
            keys.push(k?);
        }
        Ok(keys)
    }

    // FTS5 Full-Text History Search (proto.nuo.v1.MutaService/SearchHistory)

    /// Perform BM25 full-text search across transcript entries, optionally
    /// filtered by workspace root. Hits join the owning session's title and
    /// `updated_at` so a hit is presentable and rankable without a second
    /// round-trip (ADR-0208).
    ///
    /// The raw query is sanitized into a safe FTS5 MATCH expression: each
    /// whitespace-separated word becomes a quoted phrase, joined with AND.
    /// This is both injection-proof (a bare word that collides with a column
    /// name — `needle`, `OR`, `NOT` — is a syntax/column error in raw MATCH)
    /// and friendlier to recall: `retry loop` matches texts containing both
    /// words, in any position. Callers that need raw FTS5 operators can
    /// bypass by quoting inline (`"retry OR fail"` is preserved verbatim when
    /// the caller already supplies balanced quotes... no — every word is
    /// quoted; phrase operators are intentionally not reachable here).
    pub(crate) fn search_history(
        &self,
        query: &str,
        filter: Option<&nuo_wire::WorkspaceFilter>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.search_history_inner(query, filter, limit, false)
    }

    /// [`Self::search_history`] with the recall widened: the sanitized words
    /// are joined with OR instead of AND (ADR-0208 Layer 3's deterministic
    /// fallback). A gist query whose ANDed words never co-occur in one entry
    /// still recalls the entries carrying any of its words, BM25-ranked so
    /// multi-word hits float up. Callers fall back to this only after the
    /// strict search comes back empty, so the widened net never replaces a
    /// precise hit.
    pub(crate) fn search_history_relaxed(
        &self,
        query: &str,
        filter: Option<&nuo_wire::WorkspaceFilter>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.search_history_inner(query, filter, limit, true)
    }

    fn search_history_inner(
        &self,
        query: &str,
        filter: Option<&nuo_wire::WorkspaceFilter>,
        limit: usize,
        match_any: bool,
    ) -> Result<Vec<HistorySearchResult>> {
        let clean_query = sanitize_fts_query_joined(query, match_any);
        if clean_query.is_empty() {
            return Ok(Vec::new());
        }

        let cols = "f.entry_id, f.session_id, s.workspace_root, s.title, f.role, \
                    snippet(fts_entries, 3, '<b>', '</b>', '...', 16) AS snippet, \
                    bm25(fts_entries) AS score";
        let mut results = Vec::new();
        match filter {
            Some(nuo_wire::WorkspaceFilter::Path(path)) => {
                let sql = format!(
                    "SELECT {cols} FROM fts_entries f JOIN sessions s ON f.session_id = s.id \
                     WHERE fts_entries MATCH ?1 AND s.workspace_root = ?2 ORDER BY score ASC LIMIT ?3;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(
                    params![clean_query, path.to_string_lossy(), limit as i64],
                    map_search_row,
                )?;
                for item in rows {
                    results.push(item?);
                }
            }
            Some(nuo_wire::WorkspaceFilter::Unbound) => {
                let sql = format!(
                    "SELECT {cols} FROM fts_entries f JOIN sessions s ON f.session_id = s.id \
                     WHERE fts_entries MATCH ?1 AND s.workspace_root IS NULL ORDER BY score ASC LIMIT ?2;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![clean_query, limit as i64], map_search_row)?;
                for item in rows {
                    results.push(item?);
                }
            }
            _ => {
                let sql = format!(
                    "SELECT {cols} FROM fts_entries f JOIN sessions s ON f.session_id = s.id \
                     WHERE fts_entries MATCH ?1 ORDER BY score ASC LIMIT ?2;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![clean_query, limit as i64], map_search_row)?;
                for item in rows {
                    results.push(item?);
                }
            }
        }

        Ok(results)
    }

    // Typed JSON KV Helpers (ADR-0168)

    /// Retrieve and deserialize a JSON value from `kv_store`.
    pub(crate) fn get_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        if let Some(raw_json) = self.get_kv(key)? {
            match serde_json::from_str::<T>(&raw_json) {
                Ok(val) => Ok(Some(val)),
                Err(err) => {
                    tracing::warn!(key = %key, error = %err, "Failed to deserialize JSON from kv_store");
                    Ok(None)
                }
            }
        } else {
            Ok(None)
        }
    }

    // Authoritative Input History Operations (ADR-0168 / SSOT)

    /// Record a prompt into `input_history`, respecting `dedup` and the global `HISTORY_CAP`.
    pub(crate) fn record_input_history(
        &self,
        entry: &nuo_wire::HistoryEntry,
        dedup: bool,
    ) -> Result<()> {
        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let res = (|| -> Result<()> {
            if dedup {
                self.conn.execute(
                    "DELETE FROM input_history WHERE text = ?1",
                    params![entry.text],
                )?;
            } else if let Some(session_id) = &entry.session_id {
                let latest_same: bool = self
                    .conn
                    .query_row(
                        "SELECT text = ?1 FROM input_history WHERE session_id = ?2 ORDER BY created_at_ms DESC, id DESC LIMIT 1",
                        params![entry.text, session_id],
                        |row| row.get(0),
                    )
                    .unwrap_or(false);
                if latest_same {
                    return Ok(());
                }
            }

            self.conn.execute(
                r#"
                INSERT INTO input_history (text, session_id, workspace, created_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                "#,
                params![
                    entry.text,
                    entry.session_id,
                    entry.workspace,
                    entry.created_at_ms as i64,
                ],
            )?;

            self.conn.execute(
                r#"
                DELETE FROM input_history WHERE id NOT IN (
                    SELECT id FROM input_history ORDER BY created_at_ms DESC, id DESC LIMIT ?1
                )
                "#,
                params![nuo_wire::HISTORY_CAP as i64],
            )?;

            Ok(())
        })();

        match res {
            Ok(()) => {
                self.conn.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Load the newest prompt history entries up to `limit`.
    pub(crate) fn load_input_history(
        &self,
        limit: usize,
    ) -> Result<Vec<nuo_wire::HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT text, session_id, workspace, created_at_ms
            FROM input_history
            ORDER BY created_at_ms DESC, id DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let text: String = row.get(0)?;
            let session_id: Option<String> = row.get(1)?;
            let workspace: Option<String> = row.get(2)?;
            let created_at_ms: i64 = row.get(3)?;
            Ok(nuo_wire::HistoryEntry {
                text,
                session_id,
                workspace,
                created_at_ms: created_at_ms as u64,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// Persist or batch-merge a list of history entries into SQLite.
    pub(crate) fn save_input_history(
        &self,
        entries: &[nuo_wire::HistoryEntry],
        dedup: bool,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        if entries.len() == 1 {
            return self.record_input_history(&entries[0], dedup);
        }

        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let res = (|| -> Result<()> {
            let mut insert_stmt = self.conn.prepare(
                r#"
                INSERT INTO input_history (text, session_id, workspace, created_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                "#,
            )?;

            let mut delete_dedup_stmt = if dedup {
                Some(
                    self.conn
                        .prepare("DELETE FROM input_history WHERE text = ?1")?,
                )
            } else {
                None
            };

            for entry in entries {
                if let Some(del_stmt) = &mut delete_dedup_stmt {
                    del_stmt.execute(params![entry.text])?;
                }
                insert_stmt.execute(params![
                    entry.text,
                    entry.session_id,
                    entry.workspace,
                    entry.created_at_ms as i64,
                ])?;
            }

            self.conn.execute(
                r#"
                DELETE FROM input_history WHERE id NOT IN (
                    SELECT id FROM input_history ORDER BY created_at_ms DESC, id DESC LIMIT ?1
                )
                "#,
                params![nuo_wire::HISTORY_CAP as i64],
            )?;

            Ok(())
        })();

        match res {
            Ok(()) => {
                self.conn.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Delete all prompt history records.
    pub(crate) fn clear_input_history(&self) -> Result<()> {
        self.conn.execute("DELETE FROM input_history", [])?;
        Ok(())
    }

    /// Delete a specific prompt history record by text and timestamp.
    /// If `created_at_ms` is non-zero, matches both text and timestamp;
    /// otherwise falls back to text match. Returns the number of deleted rows.
    pub(crate) fn delete_input_history_entry(
        &self,
        text: &str,
        created_at_ms: u64,
    ) -> Result<usize> {
        let deleted = if created_at_ms > 0 {
            self.conn.execute(
                "DELETE FROM input_history WHERE text = ?1 AND created_at_ms = ?2",
                params![text, created_at_ms as i64],
            )?
        } else {
            self.conn
                .execute("DELETE FROM input_history WHERE text = ?1", params![text])?
        };
        Ok(deleted)
    }

    /// Migrate legacy history.json files into SQLite and purge them from disk.
    pub(crate) fn migrate_legacy_input_history(&self) -> usize {
        let mut candidates = Vec::new();
        let muta_state = crate::paths::get().state_dir;
        candidates.push(muta_state.join("history.json"));
        if let Some(parent) = muta_state.parent() {
            candidates.push(parent.join("mutx").join("history.json"));
            candidates.push(parent.join("neenee").join("history.json"));
        }

        if let Some(state_home) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
            candidates.push(state_home.join("mutx").join("history.json"));
            candidates.push(state_home.join("muta").join("history.json"));
            candidates.push(state_home.join("neenee").join("history.json"));
        } else if let Some(home) = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
        {
            let state_home = home.join(".local").join("state");
            candidates.push(state_home.join("mutx").join("history.json"));
            candidates.push(state_home.join("muta").join("history.json"));
            candidates.push(state_home.join("neenee").join("history.json"));
        }

        candidates.sort();
        candidates.dedup();

        let mut total = 0;
        for file in candidates {
            if !file.exists() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Ok(entries) = serde_json::from_str::<Vec<nuo_wire::HistoryEntry>>(&content)
            else {
                let _ = std::fs::remove_file(&file);
                continue;
            };

            if !entries.is_empty() {
                let count = entries.len();
                if self.save_input_history(&entries, true).is_ok() {
                    total += count;
                    let _ = std::fs::remove_file(&file);
                    info!(
                        path = %file.display(),
                        count,
                        "Migrated legacy input history JSON file into SQLite nuo.db and purged file"
                    );
                }
            } else {
                let _ = std::fs::remove_file(&file);
            }
        }

        total
    }

    // Legacy Flat-File Migration (ADR-0168)
}

// ---------------------------------------------------------------------------
// The read door (ADR-0231)
// ---------------------------------------------------------------------------
