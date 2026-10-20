//! Usage projection batching, request projection archiving, and command ledger auditing.
//!
//! Handles token usage materialization into `usage_contributions` and `request_projections` retention.
use rusqlite::{Connection, Result, TransactionBehavior, params};

use super::{MAX_RETAINED_REQUEST_PROJECTIONS, decode_json};

/// Batch project dirty usage records into aggregated contributions.
pub(crate) fn project_usage_batch(conn: &Connection, limit: usize) -> Result<usize> {
    use nuo_wire::{RequestUsageSource, RequestUsageStatus};
    let mut stmt = conn.prepare("SELECT u.payload,u.day,u.revision FROM usage_dirty d JOIN usage_records u USING(session_id,actor_id,round,turn,attempt) ORDER BY d.revision LIMIT ?1")?;
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
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
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

/// Record a slash-command invocation in the durable command ledger.
pub(crate) fn record_command(conn: &Connection, cmd: &nuo_wire::CommandRecord) -> Result<()> {
    let id = format!(
        "{}:{}:{}",
        cmd.name,
        cmd.timestamp,
        nuo_wire::todos::unix_now()
    );
    conn.execute(
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

/// Insert or replace one request-projection record. Keyed by
/// `(session, round, turn)`, so a re-recorded logical invocation replaces
/// its row. After the insert, the per-session archive is trimmed to the
/// newest [`MAX_RETAINED_REQUEST_PROJECTIONS`] rows.
pub(crate) fn insert_request_projection(
    conn: &Connection,
    session_id: &str,
    record: &nuo_wire::RequestProjection,
) -> Result<()> {
    let payload = serde_json::to_string(record)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    conn.execute(
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
    conn.execute(
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
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> Result<Vec<nuo_wire::RequestProjection>> {
    let mut stmt = conn.prepare(
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
