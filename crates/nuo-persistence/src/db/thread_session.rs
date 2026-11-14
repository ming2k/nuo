//! Session persistence, DAG tree tracking, and transcript views.
//!
//! Handles authoritative session state storage, incremental/full transcript commits,
//! CAS blob offloading, and retrieval projection views.
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, Result, Row, params};

use super::*;
use crate::blobs::BlobStore;

/// Create or update a thread record.
pub(crate) fn upsert_session(conn: &Connection, session: &ThreadRecord) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO threads (
            id, parent_thread_id, origin, title, msg_count, last_user_prompt,
            digest, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8 * 1000, ?9 * 1000)
        ON CONFLICT(id) DO UPDATE SET
            parent_thread_id = excluded.parent_thread_id,
            origin = excluded.origin,
            title = excluded.title,
            msg_count = excluded.msg_count,
            last_user_prompt = excluded.last_user_prompt,
            digest = excluded.digest,
            updated_at_ms = excluded.updated_at_ms;
        "#,
        params![
            session.id,
            session.parent_id,
            session.origin,
            session.title.as_deref().unwrap_or(""),
            session.msg_count,
            session.last_user_prompt,
            session.digest,
            session.created_at_s,
            session.updated_at_s,
        ],
    )?;
    conn.execute(
        r#"
        INSERT INTO thread_policies (thread_id, workspace_root, system_role, updated_at_ms)
        VALUES (?1, ?2, ?3, ?4 * 1000)
        ON CONFLICT(thread_id) DO UPDATE SET
            workspace_root = excluded.workspace_root,
            system_role = excluded.system_role,
            updated_at_ms = excluded.updated_at_ms;
        "#,
        params![
            session.id,
            session.workspace_root.as_deref().unwrap_or(""),
            session.persona.as_deref().unwrap_or("developer"),
            session.updated_at_s,
        ],
    )?;
    Ok(())
}

/// Every column a [`ThreadRecord`] maps positionally.
pub(crate) const THREAD_RECORD_COLUMNS: &str = "id, parent_id, origin, title, \
    created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, \
    digest, status, round_counter";

/// Build the WHERE fragment for a role-scoped thread query. `role_col` and
/// `workspace_col` let callers target either the canonical view columns or a
/// joined `thread_policies` row without duplicating the predicate matrix.
fn thread_filter_sql(
    filter: &nuo_protocol::ThreadFilter,
    include_subagents: bool,
    role_col: &str,
    workspace_col: &str,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    if !include_subagents {
        clauses.push("origin <> 'subagent'".to_string());
    }
    if let Some(role) = &filter.role {
        clauses.push(format!("{role_col} = ?"));
        values.push(Value::Text(role.clone()));
    }
    match &filter.workspace {
        nuo_protocol::WorkspaceFilter::Any => {}
        nuo_protocol::WorkspaceFilter::Path(path) => {
            clauses.push(format!("{workspace_col} = ?"));
            values.push(Value::Text(path.to_string_lossy().into_owned()));
        }
        nuo_protocol::WorkspaceFilter::Unbound => {
            clauses.push(format!("({workspace_col} IS NULL OR {workspace_col} = '')"));
        }
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, values)
}

/// Retrieve a single thread by id.
pub(crate) fn get_session(conn: &Connection, session_id: &str) -> Result<Option<ThreadRecord>> {
    let sql = format!("SELECT {THREAD_RECORD_COLUMNS} FROM thread_list_view WHERE id = ?1");
    conn.query_row(&sql, params![session_id], map_session_row)
        .optional()
}

/// List threads matching a role-scoped filter, sorted by `updated_at_s`
/// descending. Subagent threads are internal runs and are excluded unless a
/// purge explicitly asks for them.
pub(crate) fn list_sessions(
    conn: &Connection,
    filter: &nuo_protocol::ThreadFilter,
    include_subagents: bool,
) -> Result<Vec<ThreadRecord>> {
    let (where_sql, values) =
        thread_filter_sql(filter, include_subagents, "persona", "workspace_root");
    let sql = format!(
        "SELECT {THREAD_RECORD_COLUMNS} FROM thread_list_view{where_sql} ORDER BY updated_at_s DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), map_session_row)?;
    let mut sessions = Vec::new();
    for session in rows {
        sessions.push(session?);
    }
    Ok(sessions)
}

/// List subagent sessions spawned by a given parent session.
pub(crate) fn list_subagent_sessions(
    conn: &Connection,
    parent_id: &str,
) -> Result<Vec<ThreadRecord>> {
    let sql = format!(
        "SELECT {THREAD_RECORD_COLUMNS} FROM thread_list_view WHERE parent_id = ?1 AND origin = 'subagent' ORDER BY created_at_s ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![parent_id], map_session_row)?;
    let mut sessions = Vec::new();
    for session in rows {
        sessions.push(session?);
    }
    Ok(sessions)
}

/// Delete a session and cascade all its events, messages, and command records.
pub(crate) fn delete_session(conn: &Connection, session_id: &str) -> Result<bool> {
    // Derived rows carry no foreign key to `threads` (FTS is a virtual table
    // and `causal_nodes` predates the thread schema), so the membership trigger
    // was what used to reclaim FTS rows. With derived indexing off the hot path
    // the deletion is explicit and ordered before the owning row disappears.
    conn.execute(
        "DELETE FROM fts_thread_facts WHERE thread_id = ?1",
        params![session_id],
    )?;
    // The workspace-effect journal is append-only evidence and deliberately has
    // no foreign key to `threads`: deleting the conversation must not rewrite
    // or erase what it did to the filesystem. Expiring the pre-image bytes lets
    // the CAS collector reclaim them without deleting the observation.
    let _ =
        crate::workspace_effects::expire_thread_preimages(conn, session_id, crate::db::unix_ms());
    let affected = conn.execute("DELETE FROM threads WHERE id = ?1", params![session_id])?;
    Ok(affected > 0)
}

/// Persist a complete [`crate::thread::ThreadData`] into SQLite in one
/// transaction: the session row (identity + working state),
/// the session's memberships and directives, and the entries themselves
/// (upsert — facts are shared by identity across forks).
pub(crate) fn save_session_inner(
    conn: &Connection,
    blob_store: Option<&BlobStore>,
    data: &crate::thread::ThreadData,
    force_full: bool,
    usage_upserts: &[nuo_protocol::RequestUsageRecord],
    guard: &CommitGuard,
) -> std::result::Result<u64, SaveError> {
    let fork_str = match data.origin {
        nuo_protocol::ThreadOrigin::Trunk => "trunk",
        nuo_protocol::ThreadOrigin::Subagent => "subagent",
    };
    let digest_str = data
        .digest
        .as_ref()
        .and_then(|d| serde_json::to_string(d).ok());
    let last_prompt = crate::thread::last_effective_prompt_from_data(data);
    let msg_count = data.transcript.entries.len() as i64;
    let mut thread = crate::thread::thread_from_session_data(data);
    if let Some(blob_store) = blob_store {
        offload_observation_blobs(&mut thread, blob_store)?;
    }

    // Expensive, pure payload preparation runs before the transaction
    // (D3); only the receipt comparison runs inside it.
    let payload_hash = if guard.operation_id.is_some() {
        Some(commit_payload_digest(data, usage_upserts)?)
    } else {
        None
    };

    conn.execute("BEGIN IMMEDIATE", [])?;
    let res: std::result::Result<u64, SaveError> = (|| {
        // D3: resolve an idempotent replay before any write, then
        // enforce the revision precondition. A replay whose payload matches
        // returns the original receipt; reusing an identity for different
        // content is refused instead of silently applied or dropped.
        let current_revision = session_revision(conn, &data.id)?;
        if let (Some(operation_id), Some(payload_hash)) =
            (guard.operation_id.as_deref(), payload_hash.as_deref())
            && let Some((stored_id, stored_hash, stored_revision)) =
                latest_commit_receipt(conn, &data.id)?
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
        let stored_generation: Option<String> = conn
            .query_row(
                "SELECT transcript_generation FROM threads WHERE id = ?1",
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

        conn.execute(
            r#"
            INSERT INTO threads (
                id, parent_thread_id, origin, title, title_manual, status,
                active_branch_id, round_counter, msg_count, last_user_prompt,
                transcript_generation, digest, digest_anchor, checksum,
                created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 0, 'idle', 'main', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12 * 1000, ?13 * 1000)
            ON CONFLICT(id) DO UPDATE SET
                parent_thread_id = excluded.parent_thread_id,
                origin = excluded.origin,
                title = excluded.title,
                round_counter = excluded.round_counter,
                msg_count = excluded.msg_count,
                last_user_prompt = excluded.last_user_prompt,
                transcript_generation = excluded.transcript_generation,
                digest = excluded.digest,
                digest_anchor = excluded.digest_anchor,
                checksum = excluded.checksum,
                updated_at_ms = excluded.updated_at_ms;
            "#,
            params![
                data.id,
                data.parent_id,
                fork_str,
                data.title.as_deref().unwrap_or(""),
                data.round_counter as i64,
                msg_count,
                last_prompt,
                data.generation,
                digest_str,
                data.digest_anchor.map(|a| a as i64),
                data.checksum.map(|c| c as i64),
                data.created_at as i64,
                data.updated_at as i64,
            ],
        )?;

        conn.execute(
            r#"
            INSERT INTO thread_policies (
                thread_id, workspace_root, system_role, unattended, provider_pin,
                disabled_tools_json, additional_roots_json, commands_json,
                round_interrupts_json, retry_resolutions_json, retry_pending,
                role_manifest, todos_json, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                      ?14 * 1000)
            ON CONFLICT(thread_id) DO UPDATE SET
                workspace_root = excluded.workspace_root,
                system_role = excluded.system_role,
                unattended = excluded.unattended,
                provider_pin = excluded.provider_pin,
                disabled_tools_json = excluded.disabled_tools_json,
                additional_roots_json = excluded.additional_roots_json,
                commands_json = excluded.commands_json,
                round_interrupts_json = excluded.round_interrupts_json,
                retry_resolutions_json = excluded.retry_resolutions_json,
                retry_pending = excluded.retry_pending,
                role_manifest = COALESCE(excluded.role_manifest, thread_policies.role_manifest),
                todos_json = excluded.todos_json,
                updated_at_ms = excluded.updated_at_ms;
            "#,
            params![
                data.id,
                workspace_root.as_deref().unwrap_or(""),
                persona.as_deref().unwrap_or("developer"),
                if data.unattended { 1 } else { 0 },
                data.provider_selection
                    .as_ref()
                    .and_then(|s| serde_json::to_string(s).ok()),
                serde_json::to_string(&data.disabled_tools.iter().collect::<Vec<_>>())
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                additional_roots,
                serde_json::to_string(&data.commands)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                serde_json::to_string(&data.round_interrupts)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                "[]",
                data.retry_pending
                    .as_ref()
                    .and_then(|s| serde_json::to_string(s).ok()),
                data.role_manifest
                    .as_ref()
                    .and_then(|m| serde_json::to_string(m).ok()),
                serde_json::to_string(&data.transcript.derive_todos().unwrap_or_default())
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                data.updated_at as i64,
            ],
        )?;

        // The durable conversation is the fact graph. The transcript is a
        // projection rebuilt on read; nothing transcript-shaped is written.
        crate::db::thread_store::persist_thread_facts(conn, &thread, full)
            .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
        for record in usage_upserts {
            insert_usage_record_tx(conn, data.id.as_str(), record)?;
        }
        if full {
            for record in &data.request_usage_records {
                insert_usage_record_tx(conn, data.id.as_str(), record)?;
            }
        }

        // D3: the revision advance and the receipt land in the
        // same transaction as the facts they describe, so recovery can
        // always resolve an unknown outcome by operation identity.
        let committed_revision = current_revision + 1;
        conn.execute(
            "INSERT INTO thread_revisions(thread_id, revision) VALUES(?1, ?2)
             ON CONFLICT(thread_id) DO UPDATE SET revision=excluded.revision",
            params![data.id, committed_revision as i64],
        )?;
        conn.execute(
            "UPDATE threads SET revision = ?2 WHERE id = ?1",
            params![data.id, committed_revision as i64],
        )?;
        if let (Some(operation_id), Some(payload_hash)) =
            (guard.operation_id.as_deref(), payload_hash.as_deref())
        {
            record_commit_receipt(
                conn,
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
            conn.execute("COMMIT", [])?;
            Ok(revision)
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

/// Move oversized tool output bodies into the CAS and leave a bounded
/// observation lifecycle behind.
///
/// The fact keeps the full `BlobRef`; the model-facing projection sees a
/// bounded head/tail preview. A missing or evicted blob therefore degrades to
/// an explicit stub, never to an empty result.
fn offload_observation_blobs(
    thread: &mut nuo_protocol::Thread,
    blob_store: &BlobStore,
) -> Result<()> {
    use nuo_protocol::{BlobRef, FactPayload, ObservationLifecycle};

    let mut replacements = Vec::new();
    for fact in thread.graph.iter() {
        let FactPayload::Observation {
            output_ref,
            lifecycle,
            metrics,
            ..
        } = &fact.payload
        else {
            continue;
        };
        let ObservationLifecycle::Raw { content } = lifecycle else {
            continue;
        };
        if content.len() <= CAS_THRESHOLD_BYTES {
            continue;
        }
        let hash = blob_store
            .put(content.as_bytes())
            .map_err(rusqlite::Error::InvalidParameterName)?;
        let head: String = content.chars().take(CAS_THRESHOLD_BYTES / 4).collect();
        let tail: String = content
            .chars()
            .rev()
            .take(CAS_THRESHOLD_BYTES / 8)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let reference = BlobRef {
            hash,
            size: content.len() as u64,
            mime: output_ref.mime.clone(),
            preview: Some(head.clone()),
            trust: output_ref.trust,
        };
        replacements.push((
            fact.id.clone(),
            reference,
            ObservationLifecycle::ActiveTruncated {
                head_preview: head,
                tail_preview: tail,
                retained_tokens: metrics.original_tokens.min(CAS_THRESHOLD_BYTES / 4),
                omitted_bytes: content.len().saturating_sub(CAS_THRESHOLD_BYTES),
            },
        ));
    }
    for (id, reference, lifecycle) in replacements {
        if let Some(fact) = thread.graph.fact_mut(&id)
            && let FactPayload::Observation {
                output_ref,
                lifecycle: stored,
                ..
            } = &mut fact.payload
        {
            *output_ref = reference;
            *stored = lifecycle;
        }
    }
    Ok(())
}

/// Read the durable usage ledger for one session, in deterministic key
/// order: rows live in their own table, not on the session row.
fn load_usage_records(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<nuo_protocol::RequestUsageRecord>> {
    let mut stmt = conn.prepare(
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

/// Load a full [`crate::thread::ThreadData`] by session ID from SQLite.
/// Entries and directives whose payloads the current binary cannot decode
/// are preserved verbatim: they ride in memory as raw rows and
/// round-trip through every save untouched, so a database written by a
/// newer binary survives an older binary without loss or orphaning.
pub(crate) fn load_session_full(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<crate::thread::ThreadData>> {
    let row = conn
        .query_row(
            r#"
            SELECT t.id, t.parent_thread_id, t.origin, t.title,
                   t.created_at_ms / 1000, t.updated_at_ms / 1000,
                   p.workspace_root, p.additional_roots_json, p.system_role,
                   t.digest, t.digest_anchor, t.transcript_generation,
                   p.provider_pin, t.round_counter, p.unattended,
                   p.disabled_tools_json, p.commands_json, p.round_interrupts_json,
                   p.retry_resolutions_json, p.retry_pending, t.checksum,
                   13, p.role_manifest
            FROM threads t
            LEFT JOIN thread_policies p ON p.thread_id = t.id
            WHERE t.id = ?1
            "#,
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
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, Option<String>>(19)?,
                    row.get::<_, Option<i64>>(20)?,
                    row.get::<_, i64>(21)?,
                    row.get::<_, Option<String>>(22)?,
                ))
            },
        )
        .optional()?;
    let Some((
        id,
        parent_id,
        origin,
        title,
        created_at_s,
        updated_at_s,
        workspace_root,
        additional_roots,
        persona,
        digest,
        digest_anchor,
        generation,
        provider_connection,
        round_counter,
        unattended,
        disabled_tools,
        commands,
        round_interrupts,
        _retry_resolutions,
        retry_pending,
        checksum,
        schema_version,
        role_manifest,
    )) = row
    else {
        return Ok(None);
    };

    let mut entries = Vec::new();
    let mut directives = Vec::new();
    let mut loaded_branch = nuo_protocol::BranchId::main();

    // Facts are the only durable conversation rows; the transcript is rebuilt
    // from the active branch.
    let thread =
        crate::db::thread_store::load_thread(conn, &nuo_protocol::ThreadId::new(session_id))
            .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
    if let Some(thread) = thread {
        loaded_branch = thread.state.active_branch.clone();
        let projected = crate::thread::transcript_from_thread(&thread);
        entries = projected.entries;
        directives = projected.directives;
    }

    let digest: Option<nuo_protocol::ThreadDigest> = match digest.as_deref() {
        Some(raw) => match serde_json::from_str(raw) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                tracing::warn!(session = %session_id, error = %error, "digest column undecodable; ignored");
                None
            }
        },
        None => None,
    };
    let mut data = crate::thread::ThreadData {
        transcript: nuo_protocol::Transcript {
            min_next_seq: entries
                .iter()
                .map(|entry| entry.seq + 1)
                .chain(entries.iter().map(|row| row.seq + 1))
                .max()
                .unwrap_or(0),
            min_next_directive_seq: directives
                .iter()
                .map(|directive| directive.seq + 1)
                .chain(directives.iter().map(|row| row.seq + 1))
                .max()
                .unwrap_or(0),
            entries,
            directives,
        },
        last_projection: None,
        digest,
        // The anchor is a transcript char count: persisted for
        // real; legacy rows without one refresh their digest once.
        digest_anchor: digest_anchor.map(|a| a.max(0) as u64),
        id,
        active_branch: loaded_branch,
        parent_id,
        origin: match origin.as_str() {
            "fork" => nuo_protocol::ThreadOrigin::Trunk,
            "subagent" => nuo_protocol::ThreadOrigin::Subagent,
            _ => nuo_protocol::ThreadOrigin::Trunk,
        },
        // The column is `TEXT NOT NULL DEFAULT ''` because a NULL title is not
        // a useful distinction for the index; `Option` is. Reading `''` back as
        // `Some("")` made "this thread has no title" indistinguishable from
        // "this thread has an empty one", so a brand-new branch looked titled to
        // every display site (ADR-0120).
        title: title.filter(|t| !t.trim().is_empty()),
        created_at: created_at_s.max(0) as u64,
        updated_at: updated_at_s.max(0) as u64,
        role: persona,
        role_manifest: role_manifest
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
        workspace: workspace_root.map(|root| nuo_protocol::WorkspaceBinding {
            root: PathBuf::from(root),
            additional_roots: serde_json::from_str(&additional_roots).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "additional_roots column undecodable; treated as empty");
                Vec::new()
            }),
        }),
        schema_version: if schema_version > 0 { schema_version as u32 } else { crate::thread::CURRENT_SCHEMA_VERSION },
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
        request_usage_records: load_usage_records(conn, session_id)?,
        commands: serde_json::from_str(&commands).unwrap_or_else(|error| {
            tracing::warn!(session = %session_id, error = %error, "commands column undecodable; treated as empty");
            Default::default()
        }),
        round_interrupts: serde_json::from_str(&round_interrupts).unwrap_or_else(|error| {
            tracing::warn!(session = %session_id, error = %error, "round_interrupts column undecodable; treated as empty");
            Default::default()
        }),
        retry_pending: retry_pending
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
        unattended: unattended != 0,
    };
    let todos: nuo_tool::TodoList = conn
        .query_row(
            "SELECT todos_json FROM thread_policies WHERE thread_id = ?1",
            params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if !todos.items.is_empty() {
        data.transcript
            .push(nuo_protocol::TranscriptEntry::from_state(0, Some(todos)));
    }
    crate::thread::verify_checksum(&data, session_id);
    Ok(Some(data))
}

/// Public read-only transcript accessor for out-of-crate readers (the
/// runtime's Archivist tools): the full persisted
/// [`crate::thread::ThreadData`] for one session, checksum-verified,
/// or `None` when the id is unknown.
pub(crate) fn read_session_transcript(
    conn: &Connection,
    session_id: &str,
    tail: usize,
) -> Result<Option<ThreadTranscriptView>> {
    let Some(data) = load_session_full(conn, session_id)? else {
        return Ok(None);
    };
    let projected = data.transcript.project();
    let total = projected.len();
    let start = total.saturating_sub(tail);
    Ok(Some(ThreadTranscriptView {
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
            .map(|(seq, m)| ThreadMessageView {
                seq: *seq,
                role: role_str(m.role).to_string(),
                content: m.content.clone(),
            })
            .collect(),
    }))
}

/// Resolve a session ID prefix (4+ hex chars) to matching full session IDs
/// inside a role-scoped filter.
pub(crate) fn resolve_session_prefix(
    conn: &Connection,
    prefix: &str,
    filter: &nuo_protocol::ThreadFilter,
) -> Result<Vec<String>> {
    let pattern = format!("{prefix}%");
    let (where_sql, mut values) =
        thread_filter_sql(filter, true, "p.system_role", "p.workspace_root");
    let separator = if where_sql.is_empty() {
        " WHERE "
    } else {
        " AND "
    };
    let sql = format!(
        "SELECT t.id FROM threads t LEFT JOIN thread_policies p ON p.thread_id = t.id{where_sql}{separator}t.id LIKE ? ORDER BY t.updated_at_ms DESC"
    );
    values.push(rusqlite::types::Value::Text(pattern));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| row.get(0))?;
    let mut matches = Vec::new();
    for id in rows {
        matches.push(id?);
    }
    Ok(matches)
}

/// Resolve a session's durable workspace binding and staffing persona by
/// exact id, regardless of the caller. Used to lazily resume a
/// session from any client and re-apply its persona.
#[allow(clippy::type_complexity)]
pub(crate) fn lookup_session_workspace(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<(Option<nuo_protocol::WorkspaceBinding>, Option<String>)>> {
    let row = conn
        .query_row(
            "SELECT workspace_root, additional_roots_json, system_role FROM thread_policies WHERE thread_id = ?1",
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
    let workspace = workspace_root.map(|root| nuo_protocol::WorkspaceBinding {
        root: PathBuf::from(root),
        additional_roots: serde_json::from_str(&additional_roots).unwrap_or_default(),
    });
    Ok(Some((workspace, persona)))
}

/// Look up a session's immutable role manifest snapshot.
pub(crate) fn lookup_session_manifest(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<nuo_protocol::ThreadRoleManifest>> {
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT role_manifest FROM thread_policies WHERE thread_id = ?1",
            params![session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    let Some(Some(raw_manifest)) = row else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw_manifest).ok())
}

/// Retrieve switch candidate sessions for a scope, strictly excluding the
/// active session.
pub(crate) fn list_switch_candidates(
    conn: &Connection,
    filter: &nuo_protocol::ThreadFilter,
    active_id: &str,
) -> Result<Vec<crate::thread::ThreadSummary>> {
    const COLS: &str = "id, parent_id, origin, title, created_at_s, updated_at_s, \
                        msg_count, last_user_prompt, digest";
    let (where_sql, values) = thread_filter_sql(filter, false, "persona", "workspace_root");
    let separator = if where_sql.is_empty() {
        " WHERE "
    } else {
        " AND "
    };
    let sql = format!(
        "SELECT {COLS} FROM thread_list_view{where_sql}{separator}id <> ? ORDER BY updated_at_s DESC;"
    );
    let mut params = values;
    params.push(rusqlite::types::Value::Text(active_id.to_string()));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), map_summary_row)?;
    let mut summaries = Vec::new();
    for item in rows {
        push_summary(&mut summaries, item?, active_id);
    }
    summaries.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    Ok(summaries)
}

/// List session summaries for a role-scoped filter, with the active session
/// tagged. Subagent runs never enter the user-facing picker.
pub(crate) fn list_session_summaries(
    conn: &Connection,
    filter: &nuo_protocol::ThreadFilter,
    active_id: &str,
) -> Result<Vec<crate::thread::ThreadSummary>> {
    const COLS: &str = "id, parent_id, origin, title, created_at_s, updated_at_s, \
                        msg_count, last_user_prompt, digest";
    let (where_sql, values) = thread_filter_sql(filter, false, "persona", "workspace_root");
    let sql = format!("SELECT {COLS} FROM thread_list_view{where_sql} ORDER BY updated_at_s DESC;");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), map_summary_row)?;
    let mut summaries = Vec::new();
    for item in rows {
        push_summary(&mut summaries, item?, active_id);
    }
    summaries.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    Ok(summaries)
}

/// The most recent non-subagent session matching `filter`. Backs `--resume`.
pub(crate) fn latest_session(
    conn: &Connection,
    filter: &nuo_protocol::ThreadFilter,
) -> Result<Option<String>> {
    let (where_sql, values) = thread_filter_sql(filter, false, "persona", "workspace_root");
    let sql =
        format!("SELECT id FROM thread_list_view{where_sql} ORDER BY updated_at_s DESC LIMIT 1");
    let mut stmt = conn.prepare(&sql)?;
    let get = |row: &Row| row.get::<_, String>(0);
    stmt.query_row(rusqlite::params_from_iter(values), get)
        .optional()
}

/// The most recent non-subagent session in an exact scope. Backs `--resume`.
pub(crate) fn latest_session_in_scope(
    conn: &Connection,
    scope: &nuo_protocol::ThreadScope,
) -> Result<Option<String>> {
    latest_session(conn, &scope.filter())
}

/// Retrieve full session detail for on-demand inspection.
pub(crate) fn get_session_detail(
    conn: &Connection,
    session_id: &str,
    active_id: &str,
) -> Result<Option<nuo_protocol::ThreadDetail>> {
    if let Some(data) = load_session_full(conn, session_id)? {
        let last_prompt = crate::thread::last_effective_prompt_from_data(&data);
        Ok(Some(nuo_protocol::ThreadDetail {
            id: data.id.clone(),
            title: data.title.clone(),
            parent_id: data.parent_id.clone(),
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

/// Rename a session in the database.: a non-`NULL` title is
/// terminal; the manual flag is retained in the signature for the command
/// surface but no longer stored. A single-row UPDATE: the transcript and
/// working state are untouched, so no load/save round trip is needed.
pub(crate) fn rename_session(
    conn: &Connection,
    session_id: &str,
    title: Option<&str>,
    manual: bool,
) -> Result<bool> {
    let _ = manual;
    let now_ms = (crate::thread::unix_timestamp() * 1000) as i64;
    let affected = conn.execute(
        "UPDATE threads SET title = ?1, updated_at_ms = ?2 WHERE id = ?3",
        params![title, now_ms, session_id],
    )?;
    Ok(affected > 0)
}
