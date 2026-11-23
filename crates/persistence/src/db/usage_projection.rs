//! Usage projection batching.
//!
//! Handles token usage materialization into `usage_contributions`. Request
//! projections are a file ring beside the journal, never a table (ADR-0240).
use rusqlite::{Connection, Result, TransactionBehavior, params};

use super::decode_json;

/// Batch project dirty usage records into aggregated contributions.
pub(crate) fn project_usage_batch(conn: &Connection, limit: usize) -> Result<usize> {
    use protocol::{RequestUsageSource, RequestUsageStatus};
    let mut stmt = conn.prepare_cached("SELECT u.payload,u.day,u.revision FROM usage_dirty d JOIN usage_records u USING(session_id,actor_id,round,turn,attempt) ORDER BY d.revision LIMIT ?1")?;
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
                decode_json::<protocol::RequestUsageRecord>(&json)?,
                day,
                rev,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    {
        let mut upsert_stmt = tx.prepare_cached("INSERT INTO usage_contributions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
            ON CONFLICT(session_id,actor_id,round,turn,attempt) DO UPDATE SET
            revision=excluded.revision, day=excluded.day,provider=excluded.provider,model=excluded.model,
            requests=excluded.requests,completed=excluded.completed,prompt_tokens=excluded.prompt_tokens,
            completion_tokens=excluded.completion_tokens,total_tokens=excluded.total_tokens,
            cache_write_tokens=excluded.cache_write_tokens,cache_read_tokens=excluded.cache_read_tokens,estimated_tokens=excluded.estimated_tokens
            WHERE usage_contributions.revision < excluded.revision")?;
        let mut delete_dirty_stmt = tx.prepare_cached("DELETE FROM usage_dirty WHERE session_id=?1 AND actor_id=?2 AND round=?3 AND turn=?4 AND attempt=?5 AND revision=?6")?;
        for (record, day, revision) in &prepared {
            let k = &record.key;
            let terminal = record.status.is_terminal();
            let reported = terminal && record.source == RequestUsageSource::Reported;
            let reported_count = |n: i64| if reported { n.max(0) } else { 0 };
            upsert_stmt.execute(params![
                k.session_id,
                k.actor_id,
                k.round,
                k.turn,
                k.attempt,
                revision,
                day,
                record.provider,
                record.model,
                terminal,
                record.status == RequestUsageStatus::Completed,
                reported_count(record.prompt_tokens),
                reported_count(record.completion_tokens),
                reported_count(record.total_tokens),
                reported_count(record.cache_write_tokens),
                reported_count(record.cache_read_tokens),
                if terminal && !reported {
                    record.total_tokens.max(0)
                } else {
                    0
                }
            ])?;
            delete_dirty_stmt.execute(params![
                k.session_id,
                k.actor_id,
                k.round,
                k.turn,
                k.attempt,
                revision
            ])?;
        }
    }
    tx.commit()?;
    Ok(prepared.len())
}
