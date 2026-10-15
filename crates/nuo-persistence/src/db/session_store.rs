//! Session persistence, DAG tree tracking, and transcript views (ADR-0186, ADR-0187, ADR-0236).
//!
//! Handles authoritative session state storage, incremental/full transcript commits,
//! CAS blob offloading, and retrieval projection views.

use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, Result, Row, params};

use super::*;
use crate::blobs::BlobStore;

/// Create or update a session record.
pub(crate) fn upsert_session(conn: &Connection, session: &SessionRecord) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO threads (
            id, parent_thread_id, fork_kind, title, msg_count, last_user_prompt, digest, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8 * 1000, ?9 * 1000)
        ON CONFLICT(id) DO UPDATE SET
            parent_thread_id = excluded.parent_thread_id,
            fork_kind = excluded.fork_kind,
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
            session.persona.as_deref().unwrap_or("general"),
            session.updated_at_s,
        ],
    )?;
    Ok(())
}

/// Retrieve a single session by id.
pub(crate) fn get_session(conn: &Connection, session_id: &str) -> Result<Option<SessionRecord>> {
    conn.query_row(
        "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest FROM thread_list_view WHERE id = ?1",
        params![session_id],
        map_session_row,
    )
    .optional()
}

/// List sessions, optionally filtered by a derived grouping (ADR-0226),
/// sorted by `updated_at_s` descending.
pub(crate) fn list_sessions(
    conn: &Connection,
    filter: Option<&nuo_wire::WorkspaceFilter>,
) -> Result<Vec<SessionRecord>> {
    const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                        workspace_root, persona, msg_count, last_user_prompt, digest";
    let mut sessions = Vec::new();
    match filter {
        Some(nuo_wire::WorkspaceFilter::Path(path)) => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view WHERE workspace_root = ?1 ORDER BY updated_at_s DESC"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![path.to_string_lossy()], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        }
        Some(nuo_wire::WorkspaceFilter::Unbound) => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view WHERE workspace_root IS NULL OR workspace_root = '' ORDER BY updated_at_s DESC"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        }
        _ => {
            let sql = format!("SELECT {COLS} FROM thread_list_view ORDER BY updated_at_s DESC");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        }
    }
    Ok(sessions)
}

/// List subagent sessions spawned by a given parent session (ADR-0262).
pub(crate) fn list_subagent_sessions(
    conn: &Connection,
    parent_id: &str,
) -> Result<Vec<SessionRecord>> {
    const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                        workspace_root, persona, msg_count, last_user_prompt, digest";
    let sql = format!("SELECT {COLS} FROM thread_list_view WHERE parent_id = ?1 AND fork_kind = 'subagent' ORDER BY created_at_s ASC");
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
    let affected = conn.execute("DELETE FROM threads WHERE id = ?1", params![session_id])?;
    Ok(affected > 0)
}

/// Persist a complete [`crate::session::SessionData`] into SQLite in one
/// transaction (ADR-0186): the session row (identity + working state),
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
    usage_upserts: &[nuo_wire::RequestUsageRecord],
    guard: &CommitGuard,
) -> std::result::Result<u64, SaveError> {
    let fork_str = match data.fork_kind {
        nuo_wire::SessionForkKind::Trunk => "trunk",
        nuo_wire::SessionForkKind::Fork => "fork",
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

    conn.execute("BEGIN IMMEDIATE", [])?;
    let res: std::result::Result<u64, SaveError> = (|| {
        // ADR-0236 D3: resolve an idempotent replay before any write, then
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
                id, parent_thread_id, fork_kind, title, title_manual, status,
                active_branch_id, round_counter, msg_count, last_user_prompt, tree,
                transcript_generation, digest, digest_anchor, checksum, created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 0, 'idle', 'main', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13 * 1000, ?14 * 1000)
            ON CONFLICT(id) DO UPDATE SET
                parent_thread_id = excluded.parent_thread_id,
                fork_kind = excluded.fork_kind,
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
                role_manifest, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13 * 1000)
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
                updated_at_ms = excluded.updated_at_ms;
            "#,
            params![
                data.id,
                workspace_root.as_deref().unwrap_or(""),
                persona.as_deref().unwrap_or("general"),
                if data.unattended { 1 } else { 0 },
                data.provider_selection.as_ref().and_then(|s| serde_json::to_string(s).ok()),
                serde_json::to_string(&data.disabled_tools.iter().collect::<Vec<_>>()).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                additional_roots,
                serde_json::to_string(&data.commands).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                serde_json::to_string(&data.round_interrupts).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                serde_json::to_string(&data.retry_resolutions).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                data.retry_pending.as_ref().and_then(|s| serde_json::to_string(s).ok()),
                data.role_manifest.as_ref().and_then(|m| serde_json::to_string(m).ok()),
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
            for entry in &data.transcript.entries {
                upsert_entry(conn, blob_store, entry)?;
                insert_membership(conn, data.id.as_str(), entry.seq, entry.id.as_str())?;
            }
            for unknown in &data.unknown_entries {
                upsert_entry_row(conn, EntryEnvelope {
                    id: unknown.id.as_str(),
                    kind: unknown.kind.as_str(),
                    role: unknown.role.as_deref(),
                    content: unknown.content.as_deref(),
                    origin: unknown.origin.as_deref(),
                    hidden: unknown.hidden,
                    created_at_ms: unknown.created_at_ms,
                    payload: unknown.payload_json.as_str(),
                })?;
                insert_membership(conn, data.id.as_str(), unknown.seq, unknown.id.as_str())?;
            }
            record_blob_refs(
                conn,
                data.id.as_str(),
                &data.transcript.entries,
                &data.unknown_entries,
            )?;
            for record in &data.request_usage_records {
                insert_usage_record_tx(conn, data.id.as_str(), record)?;
            }
            // A delta commit escalated to a full rewrite (generation
            // mismatch) carries no usage mirror; apply its changed upserts
            // here so the escalation cannot drop them (ADR-0236 invariant #3).
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
            let mut new_entries = Vec::new();
            for entry in &data.transcript.entries[entry_start..] {
                upsert_entry(conn, blob_store, entry)?;
                insert_membership(conn, data.id.as_str(), entry.seq, entry.id.as_str())?;
                new_entries.push(entry);
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
            let new_entries: Vec<nuo_wire::TranscriptEntry> =
                new_entries.into_iter().cloned().collect();
            record_blob_refs(conn, data.id.as_str(), &new_entries, &[])?;
            for record in usage_upserts {
                insert_usage_record_tx(conn, data.id.as_str(), record)?;
            }
        }

        // ADR-0236 D3: the revision advance and the receipt land in the
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
    directive: &nuo_wire::ProjectionDirective,
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
/// engine has a blob store (ADR-0187). The offload applies only to the
/// row about to be inserted: already-durable rows keep their stored shape.
fn upsert_entry(
    conn: &Connection,
    blob_store: Option<&BlobStore>,
    entry: &nuo_wire::TranscriptEntry,
) -> Result<()> {
    let mut payload = entry.payload.clone();
    let mut content = entry.content.clone();
    if content
        .as_ref()
        .is_some_and(|c| c.len() > CAS_THRESHOLD_BYTES)
        && let nuo_wire::EntryPayload::Message(message_payload) = &mut payload
        && message_payload.content_blob.is_none()
        && let Some(blob_store) = blob_store
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
    if let Some(blob_store) = blob_store
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
    upsert_entry_row(conn, EntryEnvelope {
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

/// Maintain the durable blob reference ledger for the entries this save
/// touches. Callers pass exactly the entries whose memberships they wrote.
fn record_blob_refs(
    conn: &Connection,
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
        conn.execute(
            "INSERT OR IGNORE INTO blob_refs (session_id, hash) VALUES (?1, ?2)",
            params![session_id, hash],
        )?;
    }
    Ok(())
}

/// Read the durable usage ledger for one session, in deterministic key
/// order (ADR-0187): rows live in their own table, not on the session row.
fn load_usage_records(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<nuo_wire::RequestUsageRecord>> {
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
/// are preserved verbatim (ADR-0187): they ride in memory as raw rows and
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
            "fork" | "aside" => nuo_wire::SessionForkKind::Fork,
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
        request_usage_records: load_usage_records(conn, session_id)?,
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
pub(crate) fn read_session_transcript(
    conn: &Connection,
    session_id: &str,
    tail: usize,
) -> Result<Option<SessionTranscriptView>> {
    let Some(data) = load_session_full(conn, session_id)? else {
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
    conn: &Connection,
    prefix: &str,
    filter: Option<&nuo_wire::WorkspaceFilter>,
) -> Result<Vec<String>> {
    let pattern = format!("{prefix}%");
    let mut matches = Vec::new();
    match filter {
        Some(nuo_wire::WorkspaceFilter::Path(path)) => {
            let mut stmt = conn.prepare(
                "SELECT t.id FROM threads t LEFT JOIN thread_policies p ON p.thread_id = t.id WHERE t.id LIKE ?1 AND p.workspace_root = ?2 ORDER BY t.updated_at_ms DESC",
            )?;
            let rows =
                stmt.query_map(params![pattern, path.to_string_lossy()], |row| row.get(0))?;
            for id in rows {
                matches.push(id?);
            }
        }
        Some(nuo_wire::WorkspaceFilter::Unbound) => {
            let mut stmt = conn.prepare(
                "SELECT t.id FROM threads t LEFT JOIN thread_policies p ON p.thread_id = t.id WHERE t.id LIKE ?1 AND (p.workspace_root IS NULL OR p.workspace_root = '') ORDER BY t.updated_at_ms DESC",
            )?;
            let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
            for id in rows {
                matches.push(id?);
            }
        }
        _ => {
            let mut stmt = conn.prepare(
                "SELECT id FROM threads WHERE id LIKE ?1 ORDER BY updated_at_ms DESC",
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
    conn: &Connection,
    session_id: &str,
) -> Result<Option<(Option<nuo_wire::WorkspaceBinding>, Option<String>)>> {
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
    let workspace = workspace_root.map(|root| nuo_wire::WorkspaceBinding {
        root: PathBuf::from(root),
        additional_roots: serde_json::from_str(&additional_roots).unwrap_or_default(),
    });
    Ok(Some((workspace, persona)))
}

/// Look up a session's immutable role manifest snapshot (ADR-0245).
pub(crate) fn lookup_session_manifest(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<nuo_wire::SessionRoleManifest>> {
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

/// Retrieve switch candidate sessions for a given domain partition, strictly
/// excluding the active session (ADR-0250).
pub(crate) fn list_switch_candidates(
    conn: &Connection,
    partition: &nuo_wire::SessionPartition,
    active_id: &str,
) -> Result<Vec<crate::session::SessionSummary>> {
    const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                        msg_count, last_user_prompt, digest";
    let mut summaries = Vec::new();
    match partition {
        nuo_wire::SessionPartition::Workspace(path) => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view \
                 WHERE workspace_root = ?1 AND id <> ?2 AND fork_kind <> 'subagent' \
                 ORDER BY updated_at_s DESC;"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows =
                stmt.query_map(params![path.to_string_lossy(), active_id], map_summary_row)?;
            for item in rows {
                push_summary(&mut summaries, item?, active_id);
            }
        }
        nuo_wire::SessionPartition::Role(role_id) => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view \
                 WHERE (workspace_root IS NULL OR workspace_root = '') AND persona = ?1 AND id <> ?2 AND fork_kind <> 'subagent' \
                 ORDER BY updated_at_s DESC;"
            );
            let mut stmt = conn.prepare(&sql)?;
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
    conn: &Connection,
    partition: &nuo_wire::SessionPartition,
    active_id: &str,
) -> Result<Vec<crate::session::SessionSummary>> {
    const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                        msg_count, last_user_prompt, digest";
    let mut summaries = Vec::new();
    match partition {
        nuo_wire::SessionPartition::Workspace(path) => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view \
                 WHERE workspace_root = ?1 AND fork_kind <> 'subagent' \
                 ORDER BY updated_at_s DESC;"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![path.to_string_lossy()], map_summary_row)?;
            for item in rows {
                push_summary(&mut summaries, item?, active_id);
            }
        }
        nuo_wire::SessionPartition::Role(role_id) => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view \
                 WHERE (workspace_root IS NULL OR workspace_root = '') AND persona = ?1 AND fork_kind <> 'subagent' \
                 ORDER BY updated_at_s DESC;"
            );
            let mut stmt = conn.prepare(&sql)?;
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
    conn: &Connection,
    filter: Option<&nuo_wire::WorkspaceFilter>,
    active_id: &str,
) -> Result<Vec<crate::session::SessionSummary>> {
    const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                        msg_count, last_user_prompt, digest";
    let mut summaries = Vec::new();
    match filter {
        Some(nuo_wire::WorkspaceFilter::Path(path)) => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view WHERE workspace_root = ?1 AND fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![path.to_string_lossy()], map_summary_row)?;
            for item in rows {
                push_summary(&mut summaries, item?, active_id);
            }
        }
        Some(nuo_wire::WorkspaceFilter::Unbound) => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view WHERE (workspace_root IS NULL OR workspace_root = '') AND fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], map_summary_row)?;
            for item in rows {
                push_summary(&mut summaries, item?, active_id);
            }
        }
        _ => {
            let sql = format!(
                "SELECT {COLS} FROM thread_list_view WHERE fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
            );
            let mut stmt = conn.prepare(&sql)?;
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
    conn: &Connection,
    filter: &nuo_wire::WorkspaceFilter,
    persona: Option<&str>,
) -> Result<Option<String>> {
    use nuo_wire::WorkspaceFilter;
    let base = "SELECT id FROM thread_list_view WHERE fork_kind <> 'subagent'";
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
                "{base} AND (workspace_root IS NULL OR workspace_root = '') AND persona = ?1 ORDER BY updated_at_s DESC LIMIT 1"
            ),
            false,
        ),
        (WorkspaceFilter::Unbound, false) => (
            format!("{base} AND (workspace_root IS NULL OR workspace_root = '') ORDER BY updated_at_s DESC LIMIT 1"),
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
    let mut stmt = conn.prepare(&sql)?;
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
    conn: &Connection,
    partition: &nuo_wire::SessionPartition,
) -> Result<Option<String>> {
    let base = "SELECT id FROM thread_list_view WHERE fork_kind <> 'subagent'";
    let get = |row: &Row| row.get::<_, String>(0);
    let found = match partition {
        nuo_wire::SessionPartition::Workspace(path) => {
            let sql =
                format!("{base} AND workspace_root = ?1 ORDER BY updated_at_s DESC LIMIT 1");
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_row(params![path.to_string_lossy()], get)
                .optional()?
        }
        nuo_wire::SessionPartition::Role(role_id) => {
            let sql = format!(
                "{base} AND (workspace_root IS NULL OR workspace_root = '') AND persona = ?1 ORDER BY updated_at_s DESC LIMIT 1"
            );
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_row(params![role_id], get).optional()?
        }
    };
    Ok(found)
}

/// Retrieve full session detail for on-demand inspection.
pub(crate) fn get_session_detail(
    conn: &Connection,
    session_id: &str,
    active_id: &str,
) -> Result<Option<nuo_wire::SessionDetail>> {
    if let Some(data) = load_session_full(conn, session_id)? {
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
