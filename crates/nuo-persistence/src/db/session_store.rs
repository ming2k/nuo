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
            id, parent_thread_id, fork_kind, fork_round, fork_turn, title,
            msg_count, last_user_prompt, digest, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10 * 1000, ?11 * 1000)
        ON CONFLICT(id) DO UPDATE SET
            parent_thread_id = excluded.parent_thread_id,
            fork_kind = excluded.fork_kind,
            fork_round = COALESCE(excluded.fork_round, threads.fork_round),
            fork_turn = COALESCE(excluded.fork_turn, threads.fork_turn),
            title = excluded.title,
            msg_count = excluded.msg_count,
            last_user_prompt = excluded.last_user_prompt,
            digest = excluded.digest,
            updated_at_ms = excluded.updated_at_ms;
        "#,
        params![
            session.id,
            session.parent_id,
            session.fork_kind,
            session.fork_point.map(|point| point.round as i64),
            session
                .fork_point
                .and_then(|point| point.turn.map(|t| t as i64)),
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
pub(crate) const THREAD_RECORD_COLUMNS: &str = "id, parent_id, fork_kind, fork_round, \
    fork_turn, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, \
    last_user_prompt, digest, status, round_counter";

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
        clauses.push("fork_kind <> 'subagent'".to_string());
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
        "SELECT {THREAD_RECORD_COLUMNS} FROM thread_list_view WHERE parent_id = ?1 AND fork_kind = 'subagent' ORDER BY created_at_s ASC"
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
    crate::db::derived::purge_session(conn, session_id)?;
    let affected = conn.execute("DELETE FROM threads WHERE id = ?1", params![session_id])?;
    Ok(affected > 0)
}

/// Persist a complete [`crate::session::SessionData`] into SQLite in one
/// transaction: the session row (identity + working state),
/// the session's memberships and directives, and the entries themselves
/// (upsert — facts are shared by identity across forks).
#[cfg(test)]
pub(crate) fn save_session_full(
    conn: &Connection,
    blob_store: Option<&BlobStore>,
    data: &crate::session::SessionData,
) -> std::result::Result<(), SaveError> {
    save_session_inner(conn, blob_store, data, true, &[], &CommitGuard::default()).map(|_| ())
}

pub(crate) fn save_session_inner(
    conn: &Connection,
    blob_store: Option<&BlobStore>,
    data: &crate::session::SessionData,
    force_full: bool,
    usage_upserts: &[nuo_protocol::RequestUsageRecord],
    guard: &CommitGuard,
) -> std::result::Result<u64, SaveError> {
    let fork_str = match data.fork_kind {
        nuo_protocol::ThreadForkKind::Trunk => "trunk",
        nuo_protocol::ThreadForkKind::Fork => "fork",
        nuo_protocol::ThreadForkKind::Subagent => "subagent",
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
        // The Session IR's declarative policy, derived from this thread's own
        // state so `load_session_ir` always has a decodable object.
        let ir_policy = crate::db::ir_mirror::project_policy(data);
        let additional_roots = match data.workspace.as_ref() {
            Some(w) => serde_json::to_string(&w.additional_roots),
            None => Ok("[]".to_string()),
        }
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        conn.execute(
            r#"
            INSERT INTO threads (
                id, parent_thread_id, fork_kind, fork_round, fork_turn, title,
                title_manual, status, active_branch_id, round_counter, msg_count,
                last_user_prompt, tree, transcript_generation, digest,
                digest_anchor, checksum, created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 'idle', 'main', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15 * 1000, ?16 * 1000)
            ON CONFLICT(id) DO UPDATE SET
                parent_thread_id = excluded.parent_thread_id,
                fork_kind = excluded.fork_kind,
                fork_round = excluded.fork_round,
                fork_turn = excluded.fork_turn,
                title = excluded.title,
                round_counter = excluded.round_counter,
                msg_count = excluded.msg_count,
                last_user_prompt = excluded.last_user_prompt,
                tree = excluded.tree,
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
                data.fork_point.map(|point| point.round as i64),
                data.fork_point
                    .and_then(|point| point.turn.map(|t| t as i64)),
                data.title.as_deref().unwrap_or(""),
                data.round_counter as i64,
                msg_count,
                last_prompt,
                tree_str,
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
                role_manifest, rules_json, capabilities_json, guardrails_json,
                budget_json, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                      ?14, ?15, ?16, ?17 * 1000)
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
                -- The IR policy columns are a *derivation* of the thread row this
                -- same statement writes. Nothing populated them before, so they held
                -- the schema's `{}` placeholder and `load_session_ir` could not
                -- deserialize them — which failed the whole hydrate, taking every
                -- causal node with it.
                rules_json = excluded.rules_json,
                capabilities_json = excluded.capabilities_json,
                guardrails_json = excluded.guardrails_json,
                budget_json = excluded.budget_json,
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
                // Derived from the same thread row, so the two views cannot drift.
                ir_policy.rules_json,
                ir_policy.capabilities_json,
                ir_policy.guardrails_json,
                ir_policy.budget_json,
                data.updated_at as i64,
            ],
        )?;

        if full {
            // Projection decisions: the session's own view history.
            conn.execute(
                "DELETE FROM projections WHERE session_id = ?1",
                params![data.id],
            )?;
            for directive in &data.transcript.directives {
                insert_directive(conn, data.id.as_str(), directive)?;
            }
            for unknown in &data.unknown_directives {
                insert_unknown_directive(conn, data.id.as_str(), unknown)?;
            }

            // Facts + memberships. Entries are global and shared by identity.
            conn.execute(
                "DELETE FROM entry_memberships WHERE session_id = ?1",
                params![data.id],
            )?;
            conn.execute(
                "DELETE FROM blob_refs WHERE session_id = ?1",
                params![data.id],
            )?;
            let mut refs: Vec<String> = Vec::new();
            for entry in &data.transcript.entries {
                refs.extend(upsert_entry(conn, blob_store, entry)?);
                insert_membership(conn, data.id.as_str(), entry.seq, entry.id.as_str())?;
            }
            for unknown in &data.unknown_entries {
                refs.extend(extract_content_blob(&unknown.payload_json));
            }
            for unknown in &data.unknown_entries {
                upsert_entry_row(
                    conn,
                    EntryEnvelope {
                        id: unknown.id.as_str(),
                        kind: unknown.kind.as_str(),
                        role: unknown.role.as_deref(),
                        content: unknown.content.as_deref(),
                        origin: unknown.origin.as_deref(),
                        hidden: unknown.hidden,
                        created_at_ms: unknown.created_at_ms,
                        payload: unknown.payload_json.as_str(),
                    },
                )?;
                insert_membership(conn, data.id.as_str(), unknown.seq, unknown.id.as_str())?;
            }
            record_blob_refs(conn, data.id.as_str(), &refs)?;
            for record in &data.request_usage_records {
                insert_usage_record_tx(conn, data.id.as_str(), record)?;
            }
            // A delta commit escalated to a full rewrite (generation
            // mismatch) carries no usage mirror; apply its changed upserts
            // here so the escalation cannot drop them (invariant #3).
            for record in usage_upserts {
                insert_usage_record_tx(conn, data.id.as_str(), record)?;
            }
        } else {
            let watermark: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq), -1) FROM entry_memberships WHERE session_id = ?1",
                params![data.id],
                |row| row.get(0),
            )?;
            let entry_start = data
                .transcript
                .entries
                .partition_point(|entry| (entry.seq as i64) <= watermark);
            let mut refs: Vec<String> = Vec::new();
            for entry in &data.transcript.entries[entry_start..] {
                refs.extend(upsert_entry(conn, blob_store, entry)?);
                insert_membership(conn, data.id.as_str(), entry.seq, entry.id.as_str())?;
            }
            let directive_watermark: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq), -1) FROM projections WHERE session_id = ?1",
                params![data.id],
                |row| row.get(0),
            )?;
            let directive_start = data
                .transcript
                .directives
                .partition_point(|directive| (directive.seq as i64) <= directive_watermark);
            for directive in &data.transcript.directives[directive_start..] {
                insert_directive(conn, data.id.as_str(), directive)?;
            }
            record_blob_refs(conn, data.id.as_str(), &refs)?;
            for record in usage_upserts {
                insert_usage_record_tx(conn, data.id.as_str(), record)?;
            }
        }

        // Derived indexes are **not** written here. The commit only records the
        // durable transcript and enqueues the session; the causal mirror and
        // FTS index are advanced off the hot path by
        // `derived::advance_batch` (idle) or `derived::flush_session` (a reader
        // that needs them). A full rewrite resets both views first: entry
        // identities and sequences restarted, so every derived row is stale by
        // construction.
        let through = data.transcript.entries.last().map(|entry| entry.seq);
        let now_ms = crate::db::unix_ms();
        if full {
            crate::db::derived::reset_for_rebuild(conn, &data.id, through, now_ms)?;
        } else if let Some(through) = through {
            crate::db::derived::enqueue(conn, &data.id, through, now_ms)?;
        }

        // D3: the revision advance and the receipt land in the
        // same transaction as the facts they describe, so recovery can
        // always resolve an unknown outcome by operation identity.
        let committed_revision = current_revision + 1;
        conn.execute(
            "INSERT INTO session_revisions(session_id, revision) VALUES(?1, ?2)
             ON CONFLICT(session_id) DO UPDATE SET revision=excluded.revision",
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

fn insert_membership(conn: &Connection, session_id: &str, seq: u64, entry_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO entry_memberships (session_id, seq, entry_id) VALUES (?1, ?2, ?3)",
        params![session_id, seq as i64, entry_id],
    )?;
    Ok(())
}

fn insert_directive(
    conn: &Connection,
    session_id: &str,
    directive: &nuo_protocol::ProjectionDirective,
) -> Result<()> {
    let payload = serde_json::to_string(&directive.payload)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    conn.execute(
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
    conn: &Connection,
    session_id: &str,
    unknown: &crate::session::UnknownDirectiveRow,
) -> Result<()> {
    conn.execute(
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
/// engine has a blob store. The offload applies only to the
/// row about to be inserted: already-durable rows keep their stored shape.
///
/// Returns every blob hash this row references, so the caller can maintain
/// `blob_refs` from the hashes the write already computed. The earlier shape
/// re-decoded each image *again* in the reference ledger — a second base64
/// decode and hash per image per save.
fn upsert_entry(
    conn: &Connection,
    blob_store: Option<&BlobStore>,
    entry: &nuo_protocol::TranscriptEntry,
) -> Result<Vec<String>> {
    let mut hashes: Vec<String> = Vec::new();
    // The payload is cheap to clone (the body lives in the row's `content`
    // column, not in the payload). The body itself is cloned only when it is
    // about to be cleared after offloading — never for an ordinary message.
    let mut payload = entry.payload.clone();
    let mut body_offloaded = false;
    if let nuo_protocol::EntryPayload::Message(message_payload) = &mut payload {
        if let Some(blob_store) = blob_store
            && message_payload.content_blob.is_none()
            && entry
                .content
                .as_ref()
                .is_some_and(|body| body.len() > CAS_THRESHOLD_BYTES)
        {
            let hash = blob_store
                .put(entry.content.as_deref().unwrap_or_default().as_bytes())
                .map_err(rusqlite::Error::InvalidParameterName)?;
            message_payload.content_blob = Some(hash);
            body_offloaded = true;
        }
        // Record the reference whether this save created it or found it: a
        // store opened without a blob store still owns the refs its rows
        // already carry.
        if let Some(hash) = &message_payload.content_blob {
            hashes.push(hash.clone());
        }
        // Durable CAS publication for multimodal images. A failed publish does
        // not produce a reference: recording a hash for bytes that never
        // landed would keep a missing blob marked live.
        if let Some(blob_store) = blob_store
            && let Some(images) = &message_payload.images
        {
            use base64::{Engine, engine::general_purpose::STANDARD};
            for img in images {
                if let Ok(bytes) = STANDARD.decode(&img.data)
                    && let Ok(hash) = blob_store.put(&bytes)
                {
                    hashes.push(hash);
                }
            }
        }
    }
    let row_content: Option<&str> = if body_offloaded {
        Some("")
    } else {
        entry.content.as_deref()
    };
    let payload = serde_json::to_string(&payload)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    upsert_entry_row(
        conn,
        EntryEnvelope {
            id: entry.id.as_str(),
            kind: if entry.kind == nuo_protocol::EntryKind::State {
                "state"
            } else {
                "message"
            },
            role: entry.role.map(role_str),
            content: row_content,
            origin: entry.origin.map(origin_str),
            hidden: entry.hidden,
            created_at_ms: entry.created_at_ms,
            payload: payload.as_str(),
        },
    )?;
    Ok(hashes)
}

fn upsert_entry_row(conn: &Connection, row: EntryEnvelope<'_>) -> Result<()> {
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
    conn.execute(
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

/// Maintain the durable blob reference ledger for the blobs this save
/// touches. Callers pass exactly the hashes the row writes already produced —
/// the ledger never re-decodes or re-hashes content.
fn record_blob_refs(conn: &Connection, session_id: &str, hashes: &[String]) -> Result<()> {
    for hash in hashes {
        conn.execute(
            "INSERT OR IGNORE INTO blob_refs (session_id, hash) VALUES (?1, ?2)",
            params![session_id, hash],
        )?;
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

/// Load a full [`crate::session::SessionData`] by session ID from SQLite.
/// Entries and directives whose payloads the current binary cannot decode
/// are preserved verbatim: they ride in memory as raw rows and
/// round-trip through every save untouched, so a database written by a
/// newer binary survives an older binary without loss or orphaning.
pub(crate) fn load_session_full(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<crate::session::SessionData>> {
    let row = conn
        .query_row(
            r#"
            SELECT t.id, t.parent_thread_id, t.fork_kind, t.title,
                   t.fork_round, t.fork_turn,
                   t.created_at_ms / 1000, t.updated_at_ms / 1000,
                   p.workspace_root, p.additional_roots_json, p.system_role,
                   t.digest, t.digest_anchor, t.tree, t.transcript_generation,
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
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, String>(20)?,
                    row.get::<_, String>(21)?,
                    row.get::<_, Option<String>>(22)?,
                    row.get::<_, Option<i64>>(23)?,
                    row.get::<_, i64>(24)?,
                    row.get::<_, Option<String>>(25)?,
                ))
            },
        )
        .optional()?;
    let Some((
        id,
        parent_id,
        fork_kind,
        title,
        fork_round,
        fork_turn,
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
    let mut unknown_entries = Vec::new();
    {
        let mut stmt = conn.prepare(
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
            let decoded = (|| -> Option<nuo_protocol::TranscriptEntry> {
                let kind = if kind == "state" {
                    nuo_protocol::EntryKind::State
                } else {
                    nuo_protocol::EntryKind::Message
                };
                let role = role.as_deref().and_then(role_from_str);
                let origin = origin.as_deref().and_then(origin_from_str);
                let payload: nuo_protocol::EntryPayload = serde_json::from_str(&payload).ok()?;
                Some(nuo_protocol::TranscriptEntry {
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
        let mut stmt = conn.prepare(
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
            let decoded = (|| -> Option<nuo_protocol::ProjectionDirective> {
                let kind = match kind.as_str() {
                    "prune" => nuo_protocol::DirectiveKind::Prune,
                    "compact" => nuo_protocol::DirectiveKind::Compact,
                    _ => return None,
                };
                let payload: nuo_protocol::DirectivePayload =
                    serde_json::from_str(&payload).ok()?;
                Some(nuo_protocol::ProjectionDirective {
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
    let tree = match tree.as_deref() {
        Some(raw) => serde_json::from_str(raw).unwrap_or_else(|error| {
            tracing::warn!(session = %session_id, error = %error, "session tree column undecodable; reset");
            Default::default()
        }),
        None => Default::default(),
    };

    let data = crate::session::SessionData {
        transcript: nuo_protocol::Transcript {
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
        // The anchor is a transcript char count: persisted for
        // real; legacy rows without one refresh their digest once.
        digest_anchor: digest_anchor.map(|a| a.max(0) as u64),
        id,
        parent_id,
        fork_kind: match fork_kind.as_str() {
            "fork" => nuo_protocol::ThreadForkKind::Fork,
            "subagent" => nuo_protocol::ThreadForkKind::Subagent,
            _ => nuo_protocol::ThreadForkKind::Trunk,
        },
        // Provenance (ADR-0120). A partial record (a turn with no round) is
        // not a representable `ForkPoint`, so it degrades to "unrecorded"
        // rather than inventing a round the writer never stamped.
        fork_point: fork_round.map(|round| nuo_protocol::ForkPoint {
            round: round.max(0) as u64,
            turn: fork_turn.map(|t| t.max(0) as u32),
        }),
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
        tree,
        unknown_entries,
        unknown_directives,
    };
    crate::session::verify_checksum(&data, session_id);
    Ok(Some(data))
}

/// Public read-only transcript accessor for out-of-crate readers (the
/// runtime's Archivist tools): the full persisted
/// [`crate::session::SessionData`] for one session, checksum-verified,
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
            .map(|(seq, m)| SessionMessageView {
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
) -> Result<Option<nuo_protocol::SessionRoleManifest>> {
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
) -> Result<Vec<crate::session::SessionSummary>> {
    const COLS: &str = "id, parent_id, fork_kind, fork_round, fork_turn, title, created_at_s, \
                        updated_at_s, msg_count, last_user_prompt, digest";
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
) -> Result<Vec<crate::session::SessionSummary>> {
    const COLS: &str = "id, parent_id, fork_kind, fork_round, fork_turn, title, created_at_s, \
                        updated_at_s, msg_count, last_user_prompt, digest";
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
        let last_prompt = crate::session::last_effective_prompt_from_data(&data);
        Ok(Some(nuo_protocol::ThreadDetail {
            id: data.id.clone(),
            title: data.title.clone(),
            parent_id: data.parent_id.clone(),
            fork_point: data.fork_point,
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
    let now_ms = (crate::session::unix_timestamp() * 1000) as i64;
    let affected = conn.execute(
        "UPDATE threads SET title = ?1, updated_at_ms = ?2 WHERE id = ?3",
        params![title, now_ms, session_id],
    )?;
    Ok(affected > 0)
}
