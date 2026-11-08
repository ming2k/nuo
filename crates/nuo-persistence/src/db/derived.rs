//! Derived indexes built off the hot path: the causal fact graph and the FTS
//! transcript index.
//!
//! A commit writes the *durable* transcript (entries, memberships, revision)
//! and enqueues the session in `derived_pending`. Everything derived from that
//! transcript — `causal_nodes` and `fts_entries` — is advanced later, on the
//! writer thread while it is idle, in bounded batches. Durable cursors
//! (`derived_cursors`) make the work resumable across a crash and make the
//! read-your-writes boundary explicit:
//!
//! - a save never mirrors a node or indexes a row;
//! - a reader that needs derived state (the Session IR, transcript search)
//!   calls [`flush_session`] / [`flush_all`] through the writer actor first;
//! - the idle drain is an optimization, never a correctness dependency.
//!
//! The cursors are the **only** authority on coverage. `causal_seq` means "the
//! graph represents the transcript through this membership sequence" — after a
//! fold it is advanced to the transcript's end because the folded graph covers
//! that range, not because every node still exists. `fts_seq` is the highest
//! membership sequence written into `fts_entries`.
use rusqlite::{Connection, OptionalExtension, Result, params};

use nuo_protocol::{EntryKind, EntryPayload, TranscriptEntry};

use super::{ir_mirror, origin_from_str, role_from_str, unix_ms};

/// Entries indexed per idle drain. Small on purpose: the drain runs between
/// commands on the single writer, so one pass must never become a multi-second
/// stall for the next turn.
pub(crate) const IDLE_BATCH: usize = 128;
/// Entries indexed per explicit flush batch. A read that needs the derived
/// state loops this until the session is caught up.
const FLUSH_BATCH: usize = 512;
/// Sessions probed per idle scrub pass. Bounded so the writer answers the next
/// command promptly; the cursor makes the scan resumable across passes.
pub(crate) const SCRUB_BATCH: usize = 32;
/// Resumable cursor for the online integrity scrub (`derived_meta`).
const SCRUB_CURSOR_KEY: &str = "scrub_cursor";

/// Record that `session_id` has durable transcript rows through `through_seq`
/// that the derived indexes have not consumed.
pub(crate) fn enqueue(
    conn: &Connection,
    session_id: &str,
    through_seq: u64,
    now_ms: u64,
) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO derived_pending (session_id, through_seq, enqueued_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(session_id) DO UPDATE SET
            through_seq = MAX(derived_pending.through_seq, excluded.through_seq),
            enqueued_at_ms = excluded.enqueued_at_ms
        "#,
        params![session_id, through_seq as i64, now_ms as i64],
    )?;
    Ok(())
}

/// Drop both derived views for a session and enqueue a full rebuild.
///
/// Called when the transcript itself was replaced (a full rewrite): entry
/// identities and sequences restart, so every mirror node and every FTS row is
/// stale by construction.
pub(crate) fn reset_for_rebuild(
    conn: &Connection,
    session_id: &str,
    through_seq: Option<u64>,
    now_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_entries WHERE session_id = ?1",
        params![session_id],
    )?;
    conn.execute(
        "DELETE FROM causal_nodes WHERE session_id = ?1",
        params![session_id],
    )?;
    conn.execute(
        r#"
        INSERT INTO derived_cursors (session_id, causal_seq, fts_seq)
        VALUES (?1, -1, -1)
        ON CONFLICT(session_id) DO UPDATE SET causal_seq = -1, fts_seq = -1
        "#,
        params![session_id],
    )?;
    match through_seq {
        Some(through) => enqueue(conn, session_id, through, now_ms)?,
        None => {
            // An empty transcript has nothing to rebuild; a stale pending row
            // would otherwise be drained against rows that no longer exist.
            conn.execute(
                "DELETE FROM derived_pending WHERE session_id = ?1",
                params![session_id],
            )?;
        }
    }
    Ok(())
}

/// The causal graph now represents the transcript through `seq` (a fold wrote
/// a checkpoint over the folded range). Never moves the cursor backwards.
pub(crate) fn advance_causal_cursor(conn: &Connection, session_id: &str, seq: u64) -> Result<()> {
    ensure_cursor_row(conn, session_id)?;
    conn.execute(
        "UPDATE derived_cursors SET causal_seq = MAX(causal_seq, ?2) WHERE session_id = ?1",
        params![session_id, seq as i64],
    )?;
    Ok(())
}

/// Remove every derived row and cursor for a session that is being deleted.
pub(crate) fn purge_session(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_entries WHERE session_id = ?1",
        params![session_id],
    )?;
    conn.execute(
        "DELETE FROM causal_nodes WHERE session_id = ?1",
        params![session_id],
    )?;
    conn.execute(
        "DELETE FROM derived_cursors WHERE session_id = ?1",
        params![session_id],
    )?;
    conn.execute(
        "DELETE FROM derived_pending WHERE session_id = ?1",
        params![session_id],
    )?;
    Ok(())
}

/// Advance one pending session by at most `budget` membership entries on each
/// derived view. Returns the number of entries considered (0 = nothing left).
pub(crate) fn advance_batch(conn: &Connection, budget: usize) -> Result<usize> {
    let Some((session_id, through)) = pending_head(conn)? else {
        return Ok(0);
    };
    advance_session(conn, &session_id, through, budget)
}

/// Drain one session to its durable end. The read-your-writes primitive: call
/// before hydrating the Session IR or searching transcripts.
pub(crate) fn flush_session(conn: &Connection, session_id: &str) -> Result<usize> {
    let through = max_membership_seq(conn, session_id)?;
    if through < 0 {
        return Ok(0);
    }
    // Read-your-writes must also mean *read a walkable graph*. A session whose
    // cursors are fully caught up would otherwise never be re-examined, so a
    // corrupted lineage could survive indefinitely and trap the very caller
    // that is about to hydrate it. Coverage is only judged when the cursor
    // claims to be caught up: mid-drain, a partial graph is normal.
    let (causal, _fts) = cursors(conn, session_id)?;
    let needs_repair = ir_mirror::has_corrupt_lineage(conn, session_id)?
        || (causal >= through && ir_mirror::has_missing_coverage(conn, session_id)?);
    let mut total = 0usize;
    if needs_repair {
        tracing::warn!(
            %session_id,
            "derived lineage repaired from the durable transcript"
        );
        total += in_transaction(conn, |conn| {
            let rebuilt = rebuild_causal(conn, session_id)?;
            ensure_cursor_row(conn, session_id)?;
            set_cursor(conn, session_id, "causal_seq", through)?;
            Ok(rebuilt)
        })?;
    }
    // A legacy session may have no pending row even though its graph is
    // behind; enqueue it so the normal drain covers the gap.
    let (causal, fts) = cursors(conn, session_id)?;
    if (causal < through || fts < through) && pending_for(conn, session_id)?.is_none() {
        enqueue(conn, session_id, through as u64, unix_ms())?;
    }
    total += drain_session(conn, session_id)?;
    Ok(total)
}

/// Probe and repair a bounded slice of sessions.
///
/// Runs on the writer while it is idle. The cursor lives in `derived_meta`, so
/// the scan resumes across passes and a crash only repeats one slice. Every hit
/// goes through [`flush_session`] — the same probe/repair a reader would run —
/// which keeps one repair implementation instead of freezing a one-off
/// migration for this incident.
pub(crate) fn scrub_batch(conn: &Connection, budget: usize) -> Result<usize> {
    let cursor = meta_get(conn, SCRUB_CURSOR_KEY)?.unwrap_or_default();
    let budget = budget.max(1) as i64;
    let ids: Vec<String> = {
        let mut stmt =
            conn.prepare_cached("SELECT id FROM threads WHERE id > ?1 ORDER BY id ASC LIMIT ?2")?;
        let rows = stmt.query_map(params![cursor, budget], |row| row.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    if ids.is_empty() {
        // End of the catalogue: restart at the beginning on the next pass.
        if !cursor.is_empty() {
            meta_set(conn, SCRUB_CURSOR_KEY, "")?;
        }
        return Ok(0);
    }
    let mut scanned = 0usize;
    for session_id in &ids {
        if let Err(error) = flush_session(conn, session_id) {
            // One unreadable session must not stop the scan.
            tracing::warn!(%error, session = %session_id, "derived scrub could not inspect a session");
            continue;
        }
        scanned += 1;
    }
    if let Some(last) = ids.last() {
        meta_set(conn, SCRUB_CURSOR_KEY, last)?;
    }
    Ok(scanned)
}

fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM derived_meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO derived_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Drain every pending session. Used by transcript search, which spans
/// sessions and therefore cannot scope a flush to one id.
pub(crate) fn flush_all(conn: &Connection) -> Result<usize> {
    let mut total = 0usize;
    loop {
        let Some((session_id, through)) = pending_head(conn)? else {
            break;
        };
        let progressed = advance_session(conn, &session_id, through, FLUSH_BATCH)?;
        total += progressed;
        if progressed == 0 {
            // No progress is impossible while `through > cursors`; break rather
            // than spin. The row stays queued so the idle drain can retry.
            break;
        }
    }
    Ok(total)
}

fn drain_session(conn: &Connection, session_id: &str) -> Result<usize> {
    let mut total = 0usize;
    loop {
        let Some(through) = pending_for(conn, session_id)? else {
            break;
        };
        let progressed = advance_session(conn, session_id, through, FLUSH_BATCH)?;
        total += progressed;
        if progressed == 0 {
            break;
        }
    }
    Ok(total)
}

/// Run `work` in a write transaction unless the caller already opened one.
///
/// A derived batch updates up to four tables (graph rows, thread cursor, FTS
/// rows, pending row) that describe **one** position. Committing them one
/// statement at a time has two costs: an fsync per row on a
/// `synchronous=FULL` connection (a 926-node rebuild became ~900 commits), and
/// a crash between the rows leaves a graph whose cursor disagrees with what is
/// actually mirrored. One transaction makes the batch both cheap and atomic;
/// the nested case runs inline so `flush_session` can own the boundary.
fn in_transaction<T>(conn: &Connection, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    if !conn.is_autocommit() {
        return work(conn);
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    // Disarm on the explicit paths; if `work` unwinds, dropping the guard
    // releases the write lock instead of leaving the writer's only connection
    // inside an aborted transaction.
    let mut guard = RollbackOnUnwind { conn, armed: true };
    let result = work(conn);
    guard.armed = false;
    match result {
        Ok(value) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

struct RollbackOnUnwind<'a> {
    conn: &'a Connection,
    armed: bool,
}

impl Drop for RollbackOnUnwind<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

fn advance_session(
    conn: &Connection,
    session_id: &str,
    through: i64,
    budget: usize,
) -> Result<usize> {
    in_transaction(conn, |conn| {
        advance_session_inner(conn, session_id, through, budget)
    })
}

fn advance_session_inner(
    conn: &Connection,
    session_id: &str,
    through: i64,
    budget: usize,
) -> Result<usize> {
    ensure_cursor_row(conn, session_id)?;
    let (causal, fts) = cursors(conn, session_id)?;
    let budget = budget.max(1) as i64;
    let mut progressed = 0usize;

    let mut causal_now = causal;
    // Corruption is checked even when the cursor claims full coverage: the
    // cursor records *how far* the graph was built, not whether its edges are
    // walkable. A graph damaged by a previous binary must heal on the next read
    // rather than keep a `linear_path` caller trapped in a cycle.
    if ir_mirror::has_corrupt_lineage(conn, session_id)? {
        progressed += rebuild_causal(conn, session_id)?;
        set_cursor(conn, session_id, "causal_seq", through)?;
        causal_now = through;
    }
    if causal_now < through {
        let end = through.min(causal_now + budget);
        let advance = advance_causal(conn, session_id, causal_now, end)?;
        // A repair does not mirror one budget's worth of entries: it rebuilds
        // the **whole** transcript and links every node from the root. The
        // cursor must therefore jump to the rebuilt end. Advancing it by the
        // batch budget instead leaves the drain thinking the tail is still
        // unmigrated; the next batch then re-mirrors rows that already exist,
        // and before lineage became write-once that re-link is exactly how the
        // chain's head was pointed into its own tail.
        let next = if advance.rebuilt { through } else { end };
        progressed += advance.covered;
        set_cursor(conn, session_id, "causal_seq", next)?;
        causal_now = next;
    }
    let mut fts_now = fts;
    if fts_now < through {
        let end = through.min(fts_now + budget);
        progressed += advance_fts(conn, session_id, fts_now, end)?;
        set_cursor(conn, session_id, "fts_seq", end)?;
        fts_now = end;
    }
    if causal_now >= through && fts_now >= through {
        conn.execute(
            "DELETE FROM derived_pending WHERE session_id = ?1",
            params![session_id],
        )?;
    }
    Ok(progressed.max(1))
}

/// Mirror the dialogue entries in `(cursor, end]`. A graph that does not cover
/// the range (empty, lost, never mirrored, or visibly corrupted) is rebuilt
/// from the durable transcript instead of being linked to a missing parent.
struct CausalAdvance {
    /// Entries the call accounted for.
    covered: usize,
    /// The whole transcript was re-mirrored; the caller must move the cursor to
    /// the rebuilt end rather than to this batch's `end`.
    rebuilt: bool,
}

fn advance_causal(
    conn: &Connection,
    session_id: &str,
    cursor: i64,
    end: i64,
) -> Result<CausalAdvance> {
    let entries = load_dialogue_range(conn, session_id, cursor, end)?;
    let newest = ir_mirror::newest_position(conn, session_id)?;
    let repair = match entries.first().map(|entry| entry.seq) {
        None => false,
        Some(first_seq) => {
            if cursor < 0 {
                // Nothing has been considered yet: any dialogue before the
                // range is unmirrored, and a graph that already has facts while
                // the cursor says "none" is inconsistent.
                newest.is_some()
                    || ir_mirror::has_unmirrored_dialogue(conn, session_id, None, first_seq)?
            } else {
                // The cursor promises coverage; an empty graph breaks it.
                newest.is_none()
            }
        }
    };

    if repair {
        rebuild_causal(conn, session_id)?;
    } else if !entries.is_empty() {
        ir_mirror::mirror_transcript(
            conn,
            session_id,
            session_updated_at(conn, session_id)?,
            &entries,
            false,
        )?;
    }
    Ok(CausalAdvance {
        covered: (end - cursor).max(0) as usize,
        rebuilt: repair,
    })
}

/// Rebuild the session's dialogue graph from the durable transcript.
///
/// A whole-transcript rebuild: the caller must move the causal cursor to the
/// rebuilt end in the same operation, never just one batch further.
fn rebuild_causal(conn: &Connection, session_id: &str) -> Result<usize> {
    let all = load_dialogue_range(conn, session_id, -1, i64::MAX)?;
    ir_mirror::mirror_transcript(
        conn,
        session_id,
        session_updated_at(conn, session_id)?,
        &all,
        true,
    )?;
    Ok(all.len())
}

fn advance_fts(conn: &Connection, session_id: &str, cursor: i64, end: i64) -> Result<usize> {
    conn.execute(
        r#"
        INSERT OR IGNORE INTO fts_entries (rowid, entry_id, session_id, role, content)
        SELECT m.rowid, m.entry_id, m.session_id,
               COALESCE(e.role, ''), COALESCE(e.content, '')
          FROM entry_memberships m
          JOIN entries e ON e.id = m.entry_id
         WHERE m.session_id = ?1
           AND m.seq > ?2
           AND m.seq <= ?3
        "#,
        params![session_id, cursor, end],
    )?;
    Ok((end - cursor) as usize)
}

fn load_dialogue_range(
    conn: &Connection,
    session_id: &str,
    after_seq: i64,
    through_seq: i64,
) -> Result<Vec<TranscriptEntry>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.id, m.seq, e.kind, e.role, e.content, e.origin, e.hidden, e.created_at_ms, e.payload \
         FROM entry_memberships m JOIN entries e ON e.id = m.entry_id \
         WHERE m.session_id = ?1 AND m.seq > ?2 AND m.seq <= ?3 \
           AND e.kind = 'message' AND e.role IS NOT NULL \
         ORDER BY m.seq ASC",
    )?;
    let rows = stmt.query_map(params![session_id, after_seq, through_seq], |row| {
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
    let mut entries = Vec::new();
    for row in rows {
        let (id, seq, kind, role, content, origin, hidden, created_at_ms, payload) = row?;
        let Ok(payload) = serde_json::from_str::<EntryPayload>(&payload) else {
            // An undecodable payload is a fact the current binary cannot
            // represent; skipping it keeps the index monotone instead of
            // refusing the whole drain. The cursor still advances past it.
            continue;
        };
        entries.push(TranscriptEntry {
            id,
            seq: seq.max(0) as u64,
            kind: if kind == "state" {
                EntryKind::State
            } else {
                EntryKind::Message
            },
            role: role.as_deref().and_then(role_from_str),
            content,
            origin: origin.as_deref().and_then(origin_from_str),
            hidden: hidden != 0,
            created_at_ms: created_at_ms.max(0) as u64,
            payload,
        });
    }
    Ok(entries)
}

fn pending_head(conn: &Connection) -> Result<Option<(String, i64)>> {
    conn.query_row(
        "SELECT session_id, through_seq FROM derived_pending ORDER BY enqueued_at_ms ASC, session_id ASC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

fn pending_for(conn: &Connection, session_id: &str) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT through_seq FROM derived_pending WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )
    .optional()
}

fn max_membership_seq(conn: &Connection, session_id: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(seq), -1) FROM entry_memberships WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )
}

fn cursors(conn: &Connection, session_id: &str) -> Result<(i64, i64)> {
    conn.query_row(
        "SELECT causal_seq, fts_seq FROM derived_cursors WHERE session_id = ?1",
        params![session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map(|found| found.unwrap_or((-1, -1)))
}

fn ensure_cursor_row(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO derived_cursors (session_id, causal_seq, fts_seq) VALUES (?1, -1, -1)",
        params![session_id],
    )?;
    Ok(())
}

fn set_cursor(conn: &Connection, session_id: &str, column: &str, value: i64) -> Result<()> {
    // `column` is a literal chosen by this module, never caller input.
    debug_assert!(matches!(column, "causal_seq" | "fts_seq"));
    let sql = format!("UPDATE derived_cursors SET {column} = ?2 WHERE session_id = ?1");
    conn.execute(&sql, params![session_id, value])?;
    Ok(())
}

fn session_updated_at(conn: &Connection, session_id: &str) -> Result<u64> {
    conn.query_row(
        "SELECT updated_at_ms / 1000 FROM threads WHERE id = ?1",
        params![session_id],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|found| found.unwrap_or_else(|| unix_ms() as i64).max(0) as u64)
}
