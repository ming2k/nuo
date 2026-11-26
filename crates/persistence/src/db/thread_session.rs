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
    conn.prepare_cached(
        r#"
        INSERT INTO threads (
            id, parent_thread_id, origin, title, digest, created_at_ms, updated_at_ms
        )
        VALUES (
            ?1,
            CASE WHEN ?2 IS NOT NULL AND EXISTS(SELECT 1 FROM threads WHERE id = ?2) THEN ?2 ELSE NULL END,
            ?3, ?4, ?5, ?6 * 1000, ?7 * 1000
        )
        ON CONFLICT(id) DO UPDATE SET
            parent_thread_id = excluded.parent_thread_id,
            origin = excluded.origin,
            title = excluded.title,
            digest = excluded.digest,
            updated_at_ms = excluded.updated_at_ms;
        "#,
    )?
    .execute(params![
        session.id,
        session.parent_id,
        session.origin,
        session.title.as_deref().unwrap_or(""),
        session.digest,
        session.created_at_s,
        session.updated_at_s,
    ])?;
    conn.prepare_cached(
        r#"
        INSERT INTO thread_policies (thread_id, workspace_root, system_role, updated_at_ms)
        VALUES (?1, ?2, ?3, ?4 * 1000)
        ON CONFLICT(thread_id) DO UPDATE SET
            workspace_root = excluded.workspace_root,
            system_role = excluded.system_role,
            updated_at_ms = excluded.updated_at_ms;
        "#,
    )?
    .execute(params![
        session.id,
        session.workspace_root.as_deref().filter(|s| !s.is_empty()),
        session.persona.as_deref().unwrap_or("developer"),
        session.updated_at_s,
    ])?;
    Ok(())
}

/// Every column a [`ThreadRecord`] maps positionally.
pub(crate) const THREAD_RECORD_COLUMNS: &str = "id, parent_id, origin, title, \
    created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, \
    digest, round_counter";

/// Build the WHERE fragment for a role-scoped thread query. `role_col` and
/// `workspace_col` let callers target either the canonical view columns or a
/// joined `thread_policies` row without duplicating the predicate matrix.
fn thread_filter_sql(
    filter: &protocol::ThreadFilter,
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
    if let Some(path) = &filter.workspace {
        clauses.push(format!("{workspace_col} = ?"));
        values.push(Value::Text(path.to_string_lossy().into_owned()));
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
    conn.prepare_cached(&sql)?
        .query_row(params![session_id], map_session_row)
        .optional()
}

/// List threads matching a role-scoped filter, sorted by `updated_at_s`
/// descending. Subagent threads are internal runs and are excluded unless a
/// purge explicitly asks for them.
pub(crate) fn list_sessions(
    conn: &Connection,
    filter: &protocol::ThreadFilter,
    include_subagents: bool,
) -> Result<Vec<ThreadRecord>> {
    let (where_sql, values) =
        thread_filter_sql(filter, include_subagents, "persona", "workspace_root");
    let sql = format!(
        "SELECT {THREAD_RECORD_COLUMNS} FROM thread_list_view{where_sql} ORDER BY updated_at_s DESC"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
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
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![parent_id], map_session_row)?;
    let mut sessions = Vec::new();
    for session in rows {
        sessions.push(session?);
    }
    Ok(sessions)
}

/// Delete a session's catalog rows (`threads` cascades `thread_policies` and
/// `commit_receipts`; content-plane rows live on the owning shard and are
/// removed by `ContentShardPool::delete_thread`, ADR-0242, ADR-0255).
pub(crate) fn delete_session(conn: &Connection, session_id: &str) -> Result<bool> {
    let affected = conn
        .prepare_cached("DELETE FROM threads WHERE id = ?1")?
        .execute(params![session_id])?;
    let _ = conn.execute(
        "DELETE FROM projection_watermarks WHERE thread_id = ?1",
        params![session_id],
    );
    Ok(affected > 0)
}

/// Prepare everything a session commit needs **outside** the writer thread.
///
/// This is where the expensive, non-SQL half of a projection happens:
///
/// - projecting the transcript into the canonical fact graph;
/// - offloading oversized observation bodies into the CAS (a file write plus
///   `fdatasync`), which is why it must never run on the single writer;
/// - deriving the materialised catalog summary;
/// - encoding the JSON columns;
/// - fingerprinting the payload for idempotency replay (D3).
///
/// Doing it here has three consequences that the design depends on:
///
/// 1. the writer executes only `BEGIN`/`INSERT`/`UPDATE`/`COMMIT`, so a large
///    tool output cannot stall every other session's projection;
/// 2. a failure (an unreadable file, a full disk) leaves the database
///    untouched — there is no half-applied transaction to roll back;
/// 3. a blob written for a commit whose transaction later fails is simply
///    unreferenced, and the CAS collector reclaims it on its next pass.
pub(crate) fn prepare_save(
    data: crate::thread::ThreadData,
    mode: SaveMode,
    usage_upserts: &[protocol::RequestUsageRecord],
    guard: &CommitGuard,
    journal_seq: Option<u64>,
    blob_store: Option<&BlobStore>,
) -> std::result::Result<PreparedSave, SaveError> {
    let extra_blob_refs = if mode == SaveMode::MetadataOnly {
        Vec::new()
    } else {
        crate::thread::collect_session_blob_refs(&data)
    };
    let thread = if mode == SaveMode::MetadataOnly {
        None
    } else {
        let mut built = crate::thread::thread_from_session_data(&data);
        if let Some(blob_store) = blob_store {
            offload_observation_blobs(&mut built, blob_store)?;
        }
        Some(built)
    };

    let payload_hash = if guard.operation_id.is_some() {
        Some(commit_payload_digest(&data, usage_upserts)?)
    } else {
        None
    };

    let active_branch_id = thread
        .as_ref()
        .map(|t| t.state.active_branch.0.clone())
        .unwrap_or_else(|| data.active_branch.0.clone());
    let active_leaf = thread.as_ref().and_then(|t| {
        t.state
            .active_branch()
            .ok()
            .and_then(|b| b.head.as_ref())
            .map(|id| id.0.clone())
    });

    Ok(PreparedSave {
        digest_json: encode_json("digest", data.digest.as_ref())?,
        additional_roots_json: match data.workspace.as_ref() {
            Some(workspace) => encode_json("additional_roots", Some(&workspace.additional_roots))?
                .unwrap_or_else(|| "[]".to_string()),
            // A workspace-free session has no additional roots. `[]`, never
            // `null`: the column is a JSON array by contract, and readers
            // deserialize it as one.
            None => "[]".to_string(),
        },
        disabled_tools_json: encode_json(
            "disabled_tools",
            Some(&data.disabled_tools.iter().collect::<Vec<_>>()),
        )?
        .unwrap_or_else(|| "[]".to_string()),
        round_interrupts_json: encode_json("round_interrupts", Some(&data.round_interrupts))?
            .unwrap_or_else(|| "[]".to_string()),
        role_manifest_json: encode_json("role_manifest", data.role_manifest.as_ref())?,
        provider_pin_json: encode_json("provider_pin", data.provider_selection.as_ref())?,
        retry_pending_json: encode_json("retry_pending", data.retry_pending.as_ref())?,
        todos_json: todos_json_for(&data, mode)?,
        msg_count: if mode == SaveMode::FullRewrite {
            data.transcript.message_count() as i64
        } else {
            data.msg_count as i64
        },
        last_user_prompt: if mode == SaveMode::FullRewrite {
            crate::thread::last_effective_prompt_from_data(&data)
        } else {
            data.last_user_prompt.clone()
        },
        fork_str: match data.origin {
            protocol::ThreadOrigin::Trunk => "trunk",
            protocol::ThreadOrigin::Subagent => "subagent",
        },
        active_branch_id,
        active_leaf,
        data: Box::new(data),
        mode,
        usage_upserts: usage_upserts.to_vec(),
        guard: guard.clone(),
        journal_seq,
        thread,
        extra_blob_refs,
        payload_hash,
    })
}

/// Encode one JSON column during preparation.
///
/// A serialization failure is a *pre-write* failure: nothing has been
/// committed, so it surfaces as the same typed error an engine rejection
/// would, and the caller retries or reports it. It must never be swallowed
/// into `"[]"`, which would silently erase the column's content.
fn encode_json<T: Serialize>(
    column: &str,
    value: Option<&T>,
) -> std::result::Result<Option<String>, SaveError> {
    let Some(value) = value else {
        return Ok(None);
    };
    serde_json::to_string(value).map(Some).map_err(|error| {
        SaveError::Sql(rusqlite::Error::ToSqlConversionFailure(
            format!("could not encode {column}: {error}").into(),
        ))
    })
}

/// The todos column a save carries: `None` means "keep the stored value".
fn todos_json_for(
    data: &crate::thread::ThreadData,
    mode: SaveMode,
) -> std::result::Result<Option<String>, SaveError> {
    let todos = data.transcript.derive_todos();
    let value = if mode == SaveMode::FullRewrite {
        todos.unwrap_or_default()
    } else {
        match todos {
            Some(todos) => todos,
            None => return Ok(None),
        }
    };
    encode_json("todos", Some(&value))
}

/// Execute one prepared projection inside a single SQL transaction.
///
/// Every column this writes was already encoded by [`prepare_save`]; the only
/// work here is SQL. That restriction is what keeps the single writer cheap:
/// no filesystem call, no network, no schema decode, and no large in-memory
/// projection is performed while the write lock is held.
pub(crate) fn save_session_inner(
    conn: &Connection,
    prepared: &PreparedSave,
) -> std::result::Result<u64, SaveError> {
    let PreparedSave {
        data,
        mode,
        usage_upserts,
        guard,
        journal_seq,
        thread,
        extra_blob_refs: _,
        payload_hash,
        digest_json,
        additional_roots_json,
        disabled_tools_json,
        round_interrupts_json,
        role_manifest_json,
        provider_pin_json,
        retry_pending_json,
        todos_json,
        msg_count,
        last_user_prompt,
        fork_str,
        active_branch_id,
        active_leaf,
    } = prepared;
    let mode = *mode;

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

        let replace = match mode {
            SaveMode::MetadataOnly => false,
            SaveMode::FullRewrite => true,
            SaveMode::Delta => {
                // Read the durable generation BEFORE the row upsert stamps the
                // incoming one: the comparison decides full-rewrite vs append.
                let stored_generation: Option<String> = conn
                    .prepare_cached("SELECT transcript_generation FROM threads WHERE id = ?1")?
                    .query_row(params![data.id], |row| row.get(0))
                    .unwrap_or(None);
                // A `None` stored generation (never saved, or saved before the
                // generation column existed) also forces the full rewrite.
                stored_generation.as_deref() != Some(data.generation.as_str())
            }
        };

        let persona = data.role.as_deref();
        let workspace_root = data
            .workspace
            .as_ref()
            .map(|w| w.root.to_string_lossy().into_owned())
            .filter(|root| !root.is_empty());
        let (summary_msg_count, summary_last_prompt) = (*msg_count, last_user_prompt.clone());

        if mode == SaveMode::MetadataOnly {
            // Preserve `transcript_generation`, `active_branch_id`, and `active_leaf`
            // on existing rows: metadata setters must never alter transcript lineage.
            conn.prepare_cached(
                r#"
                INSERT INTO threads (
                    id, parent_thread_id, origin, title,
                    active_branch_id, round_counter,
                    transcript_generation, digest, digest_anchor, checksum,
                    created_at_ms, updated_at_ms, msg_count, last_user_prompt
                ) VALUES (
                    ?1,
                    CASE WHEN ?2 IS NOT NULL AND EXISTS(SELECT 1 FROM threads WHERE id = ?2) THEN ?2 ELSE NULL END,
                    ?3, ?4, 'main', ?5, ?6, ?7, ?8, ?9, ?10 * 1000, ?11 * 1000, ?12, ?13
                )
                ON CONFLICT(id) DO UPDATE SET
                    parent_thread_id = excluded.parent_thread_id,
                    origin = excluded.origin,
                    title = excluded.title,
                    round_counter = excluded.round_counter,
                    digest = excluded.digest,
                    digest_anchor = excluded.digest_anchor,
                    checksum = excluded.checksum,
                    updated_at_ms = excluded.updated_at_ms;
                "#,
            )?
            .execute(params![
                data.id,
                data.parent_id,
                fork_str,
                data.title.as_deref().unwrap_or(""),
                data.round_counter as i64,
                data.generation,
                digest_json,
                data.digest_anchor.map(|a| a as i64),
                data.checksum.map(|c| c as i64),
                data.created_at as i64,
                data.updated_at as i64,
                summary_msg_count,
                summary_last_prompt,
            ])?;
        } else {
            conn.prepare_cached(
                r#"
                INSERT INTO threads (
                    id, parent_thread_id, origin, title,
                    active_branch_id, round_counter,
                    transcript_generation, digest, digest_anchor, checksum,
                    created_at_ms, updated_at_ms, msg_count, last_user_prompt
                ) VALUES (
                    ?1,
                    CASE WHEN ?2 IS NOT NULL AND EXISTS(SELECT 1 FROM threads WHERE id = ?2) THEN ?2 ELSE NULL END,
                    ?3, ?4, 'main', ?5, ?6, ?7, ?8, ?9, ?10 * 1000, ?11 * 1000, ?12, ?13
                )
                ON CONFLICT(id) DO UPDATE SET
                    parent_thread_id = excluded.parent_thread_id,
                    origin = excluded.origin,
                    title = excluded.title,
                    round_counter = excluded.round_counter,
                    transcript_generation = excluded.transcript_generation,
                    digest = excluded.digest,
                    digest_anchor = excluded.digest_anchor,
                    checksum = excluded.checksum,
                    updated_at_ms = excluded.updated_at_ms,
                    msg_count = excluded.msg_count,
                    last_user_prompt = excluded.last_user_prompt;
                "#,
            )?
            .execute(params![
                data.id,
                data.parent_id,
                fork_str,
                data.title.as_deref().unwrap_or(""),
                data.round_counter as i64,
                data.generation,
                digest_json,
                data.digest_anchor.map(|a| a as i64),
                data.checksum.map(|c| c as i64),
                data.created_at as i64,
                data.updated_at as i64,
                summary_msg_count,
                summary_last_prompt,
            ])?;
        }

        conn.prepare_cached(
            r#"
            INSERT INTO thread_policies (
                thread_id, workspace_root, system_role, unattended, provider_pin,
                disabled_tools_json, additional_roots_json,
                round_interrupts_json, retry_pending,
                role_manifest, todos_json, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, COALESCE(?11, '[]'),
                      ?12 * 1000)
            ON CONFLICT(thread_id) DO UPDATE SET
                workspace_root = excluded.workspace_root,
                system_role = excluded.system_role,
                unattended = excluded.unattended,
                provider_pin = excluded.provider_pin,
                disabled_tools_json = excluded.disabled_tools_json,
                additional_roots_json = excluded.additional_roots_json,
                round_interrupts_json = excluded.round_interrupts_json,
                retry_pending = excluded.retry_pending,
                role_manifest = COALESCE(excluded.role_manifest, thread_policies.role_manifest),
                todos_json = COALESCE(?11, thread_policies.todos_json),
                updated_at_ms = excluded.updated_at_ms;
            "#,
        )?
        .execute(params![
            data.id,
            workspace_root.as_deref(),
            persona.unwrap_or("developer"),
            if data.unattended { 1 } else { 0 },
            provider_pin_json,
            disabled_tools_json,
            additional_roots_json,
            round_interrupts_json,
            retry_pending_json,
            role_manifest_json,
            todos_json,
            data.updated_at as i64,
        ])?;

        // ADR-0242 / ADR-0255: content-plane rows (facts, branches, blob refs,
        // FTS, command_acts) live exclusively in content shards. The catalog
        // plane records only the active branch cursor columns.
        let _ = thread;
        if mode != SaveMode::MetadataOnly {
            conn.prepare_cached(
                "UPDATE threads SET active_leaf = COALESCE(?2, active_leaf), active_branch_id = ?3 WHERE id = ?1",
            )?
            .execute(params![data.id, active_leaf, active_branch_id])?;
        }
        for record in usage_upserts {
            insert_usage_record_tx(conn, data.id.as_str(), record)?;
        }
        if replace {
            for record in &data.request_usage_records {
                insert_usage_record_tx(conn, data.id.as_str(), record)?;
            }
        }

        // The derived view's watermark advances only with the transaction
        // that materialised its facts (ADR-0242). A missing row replays from
        // the journal on the next catch-up.
        if let Some(seq) = journal_seq {
            conn.prepare_cached(
                "INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms)
                 VALUES(?1, ?2, ?3)
                 ON CONFLICT(thread_id) DO UPDATE SET
                   seq = MAX(projection_watermarks.seq, excluded.seq),
                   updated_at_ms = excluded.updated_at_ms",
            )?
            .execute(params![
                data.id,
                *seq as i64,
                data.updated_at.saturating_mul(1000) as i64,
            ])?;
        }

        // D3: the revision advance and the receipt land in the
        // same transaction as the facts they describe, so recovery can
        // always resolve an unknown outcome by operation identity.
        let committed_revision = current_revision + 1;
        conn.prepare_cached("UPDATE threads SET revision = ?2 WHERE id = ?1")?
            .execute(params![data.id, committed_revision as i64])?;
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
fn offload_observation_blobs(thread: &mut protocol::Thread, blob_store: &BlobStore) -> Result<()> {
    use protocol::{BlobRef, FactPayload, ObservationLifecycle};

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

/// Append one command act row (`O(1)`, ADR-0249).
pub(crate) fn append_command_act(
    conn: &Connection,
    thread_id: &str,
    seq: u64,
    payload_json: &str,
    journal_seq: Option<u64>,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO command_acts (thread_id, seq, payload_json) VALUES (?1, ?2, ?3)
         ON CONFLICT(thread_id, seq) DO UPDATE SET payload_json = excluded.payload_json",
    )?
    .execute(params![thread_id, seq as i64, payload_json])?;
    if let Some(jseq) = journal_seq {
        advance_watermark(conn, thread_id, jseq)?;
    }
    Ok(())
}

/// Replace all command acts for a thread during a full rewrite / journal rebuild.
pub(crate) fn sync_command_acts(
    conn: &Connection,
    thread_id: &str,
    commands: &[protocol::CommandRecord],
) -> Result<()> {
    conn.prepare_cached("DELETE FROM command_acts WHERE thread_id = ?1")?
        .execute(params![thread_id])?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO command_acts (thread_id, seq, payload_json) VALUES (?1, ?2, ?3)",
    )?;
    for (idx, record) in commands.iter().enumerate() {
        let payload = serde_json::to_string(record).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(
                format!("could not encode command act: {error}").into(),
            )
        })?;
        stmt.execute(params![thread_id, idx as i64, payload])?;
    }
    Ok(())
}

/// Load a thread's normalized command act ledger in sequence order (ADR-0249).
pub(crate) fn load_command_acts(
    conn: &Connection,
    thread_id: &str,
) -> Result<Vec<protocol::CommandRecord>> {
    let mut stmt = conn.prepare_cached(
        "SELECT payload_json FROM command_acts WHERE thread_id = ?1 ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map(params![thread_id], |row| row.get::<_, String>(0))?;
    let mut commands = Vec::new();
    for payload in rows {
        let payload = payload?;
        match serde_json::from_str(&payload) {
            Ok(record) => commands.push(record),
            Err(error) => tracing::warn!(
                session = %thread_id,
                error = %error,
                "command act payload undecodable; skipped"
            ),
        }
    }
    Ok(commands)
}

pub(crate) fn advance_watermark(conn: &Connection, thread_id: &str, seq: u64) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO projection_watermarks(thread_id, seq, updated_at_ms)
         VALUES(?1, ?2, ?3)
         ON CONFLICT(thread_id) DO UPDATE SET
           seq = MAX(projection_watermarks.seq, excluded.seq),
           updated_at_ms = excluded.updated_at_ms",
    )?
    .execute(params![thread_id, seq as i64, crate::db::unix_ms() as i64])?;
    Ok(())
}

/// Read the durable usage ledger for one session, in deterministic key
/// order: rows live in their own table, not on the session row.
fn load_usage_records(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<protocol::RequestUsageRecord>> {
    let mut stmt = conn.prepare_cached(
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

/// Load a full [`crate::thread::ThreadData`] by joining lifecycle metadata from
/// `catalog_conn` with facts, branches, and command acts from `content_conn`
/// (ADR-0242, ADR-0255).
pub(crate) fn load_session_full_from_planes(
    catalog_conn: &Connection,
    content_conn: Option<&Connection>,
    session_id: &str,
    blob_store: Option<&BlobStore>,
) -> Result<Option<crate::thread::ThreadData>> {
    let row = catalog_conn
        .prepare_cached(
            r#"
            SELECT t.id, t.parent_thread_id, t.origin, t.title,
                   t.created_at_ms / 1000, t.updated_at_ms / 1000,
                   p.workspace_root, p.additional_roots_json, p.system_role,
                   t.digest, t.digest_anchor, t.transcript_generation,
                   p.provider_pin, t.round_counter, p.unattended,
                   p.disabled_tools_json, p.round_interrupts_json,
                   p.retry_pending, t.checksum, p.role_manifest
            FROM threads t
            LEFT JOIN thread_policies p ON p.thread_id = t.id
            WHERE t.id = ?1
            "#,
        )?
        .query_row(params![session_id], |row| {
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
                row.get::<_, Option<String>>(17)?,
                row.get::<_, Option<i64>>(18)?,
                row.get::<_, Option<String>>(19)?,
            ))
        })
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
        round_interrupts,
        retry_pending,
        checksum,
        role_manifest,
    )) = row
    else {
        return Ok(None);
    };

    let mut entries = Vec::new();
    let mut directives = Vec::new();
    let mut loaded_branch = protocol::BranchId::main();
    let mut loaded_branches = std::collections::BTreeMap::new();
    let mut loaded_graph = None;

    // Facts are the only durable conversation rows; the transcript is rebuilt
    // from the active branch on the content shard (ADR-0242).
    if let Some(content) = content_conn {
        let thread = crate::db::content_projection::load_thread_from_planes(
            catalog_conn,
            content,
            &protocol::ThreadId::new(session_id),
            blob_store,
        )
        .map_err(rusqlite::Error::InvalidParameterName)?;
        if let Some(thread) = thread {
            loaded_branch = thread.state.active_branch.clone();
            loaded_branches = thread.state.branches.clone();
            loaded_graph = Some(thread.clone());
            let projected = crate::thread::transcript_from_thread(&thread);
            entries = projected.entries;
            directives = projected.directives;
        }
    }

    let digest: Option<protocol::ThreadDigest> = match digest.as_deref() {
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
        transcript: protocol::Transcript {
            min_next_seq: entries
                .iter()
                .map(|entry| entry.seq + 1)
                .max()
                .unwrap_or(0),
            min_next_directive_seq: directives
                .iter()
                .map(|directive| directive.seq + 1)
                .max()
                .unwrap_or(0),
            entries,
            directives,
        },
        branches: loaded_branches,
        graph: loaded_graph,
        graph_image_blobs: Vec::new(),
        msg_count: 0,
        last_user_prompt: None,
        last_projection: None,
        digest,
        // The anchor is a transcript char count: persisted for
        // real; legacy rows without one refresh their digest once.
        digest_anchor: digest_anchor.map(|a| a.max(0) as u64),
        id,
        active_branch: loaded_branch,
        parent_id,
        origin: match origin.as_str() {
            "fork" => protocol::ThreadOrigin::Trunk,
            "subagent" => protocol::ThreadOrigin::Subagent,
            _ => protocol::ThreadOrigin::Trunk,
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
        workspace: workspace_root
            .filter(|root| !root.is_empty())
            .map(|root| protocol::WorkspaceBinding {
                root: PathBuf::from(root),
                additional_roots: serde_json::from_str(&additional_roots).unwrap_or_else(|error| {
                    tracing::warn!(session = %session_id, error = %error, "additional_roots column undecodable; treated as empty");
                    Vec::new()
                }),
            }),
        // ADR-0255 `[INV-SHARD-05]`: rows hydrated from the v7+ normalized
        // `command_acts` table already carry post-ADR-0140 acts only; stamping
        // `CURRENT_SCHEMA_VERSION` prevents `migrate_session_data` from wiping
        // them on SQLite fallback.
        schema_version: crate::thread::CURRENT_SCHEMA_VERSION,
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
        request_usage_records: load_usage_records(catalog_conn, session_id)?,
        commands: match content_conn {
            Some(content) => load_command_acts(content, session_id)?,
            None => Vec::new(),
        },
        round_interrupts: serde_json::from_str(&round_interrupts).unwrap_or_else(|error| {
            tracing::warn!(session = %session_id, error = %error, "round_interrupts column undecodable; treated as empty");
            Default::default()
        }),
        round_summaries: Vec::new(),
        retry_pending: retry_pending
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
        unattended: unattended != 0,
    };
    let todos: tool::TodoList = catalog_conn
        .prepare_cached("SELECT todos_json FROM thread_policies WHERE thread_id = ?1")?
        .query_row(params![session_id], |row| row.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if !todos.items.is_empty() {
        data.transcript
            .push(protocol::TranscriptEntry::from_state(0, Some(todos)));
    }
    crate::thread::verify_checksum(&data, session_id);
    data.msg_count = data.transcript.message_count() as u64;
    data.last_user_prompt = crate::thread::last_effective_prompt_from_data(&data);
    Ok(Some(data))
}

/// Public read-only transcript accessor for out-of-crate readers (the
/// runtime's Archivist tools): the full persisted
/// [`crate::thread::ThreadData`] for one session, checksum-verified,
/// or `None` when the id is unknown.
pub(crate) fn read_session_transcript_from_planes(
    catalog_conn: &Connection,
    content_conn: Option<&Connection>,
    session_id: &str,
    tail: usize,
    blob_store: Option<&BlobStore>,
) -> Result<Option<ThreadTranscriptView>> {
    let Some(data) =
        load_session_full_from_planes(catalog_conn, content_conn, session_id, blob_store)?
    else {
        return Ok(None);
    };
    // The Archivist reads the **conversation**, not the request window: a fold
    // is context relief, so an investigation that cannot see the folded rounds
    // would be reading a truncated thread (ADR-0246 `[INV-CMP-08]`).
    let conversation = data.transcript.conversation_rows();
    let total = conversation.len();
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
        messages: conversation[start..]
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
    filter: &protocol::ThreadFilter,
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
    let mut stmt = conn.prepare_cached(&sql)?;
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
) -> Result<Option<(Option<protocol::WorkspaceBinding>, Option<String>)>> {
    let row = conn
        .prepare_cached(
            "SELECT workspace_root, additional_roots_json, system_role FROM thread_policies WHERE thread_id = ?1",
        )?
        .query_row(params![session_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .optional()?;
    let Some((workspace_root, additional_roots, persona)) = row else {
        return Ok(None);
    };
    let workspace =
        workspace_root
            .filter(|root| !root.is_empty())
            .map(|root| protocol::WorkspaceBinding {
                root: PathBuf::from(root),
                additional_roots: serde_json::from_str(&additional_roots).unwrap_or_default(),
            });
    Ok(Some((workspace, persona)))
}

/// Look up a session's immutable role manifest snapshot.
pub(crate) fn lookup_session_manifest(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<protocol::ThreadRoleManifest>> {
    let row: Option<Option<String>> = conn
        .prepare_cached("SELECT role_manifest FROM thread_policies WHERE thread_id = ?1")?
        .query_row(params![session_id], |row| row.get::<_, Option<String>>(0))
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
    filter: &protocol::ThreadFilter,
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
    let mut stmt = conn.prepare_cached(&sql)?;
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
    filter: &protocol::ThreadFilter,
    active_id: &str,
) -> Result<Vec<crate::thread::ThreadSummary>> {
    const COLS: &str = "id, parent_id, origin, title, created_at_s, updated_at_s, \
                        msg_count, last_user_prompt, digest";
    let (where_sql, values) = thread_filter_sql(filter, false, "persona", "workspace_root");
    let sql = format!("SELECT {COLS} FROM thread_list_view{where_sql} ORDER BY updated_at_s DESC;");
    let mut stmt = conn.prepare_cached(&sql)?;
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
    filter: &protocol::ThreadFilter,
) -> Result<Option<String>> {
    let (where_sql, values) = thread_filter_sql(filter, false, "persona", "workspace_root");
    let sql =
        format!("SELECT id FROM thread_list_view{where_sql} ORDER BY updated_at_s DESC LIMIT 1");
    let mut stmt = conn.prepare_cached(&sql)?;
    let get = |row: &Row| row.get::<_, String>(0);
    stmt.query_row(rusqlite::params_from_iter(values), get)
        .optional()
}

/// Retrieve full session detail for on-demand inspection.
pub(crate) fn get_session_detail_from_planes(
    catalog_conn: &Connection,
    content_conn: Option<&Connection>,
    session_id: &str,
    active_id: &str,
    blob_store: Option<&BlobStore>,
) -> Result<Option<protocol::ThreadDetail>> {
    if let Some(data) =
        load_session_full_from_planes(catalog_conn, content_conn, session_id, blob_store)?
    {
        let last_prompt = crate::thread::last_effective_prompt_from_data(&data);
        Ok(Some(protocol::ThreadDetail {
            id: data.id.clone(),
            title: data.title.clone(),
            parent_id: data.parent_id.clone(),
            digest: data.digest.clone(),
            created_at: data.created_at,
            updated_at: data.updated_at,
            message_count: data.transcript.message_count(),
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
    let affected = conn
        .prepare_cached("UPDATE threads SET title = ?1, updated_at_ms = ?2 WHERE id = ?3")?
        .execute(params![title, now_ms, session_id])?;
    Ok(affected > 0)
}
