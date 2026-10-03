//! Authoritative embedded SQLite storage engine (ADR-0163, ADR-0187).
//!
//! Provides relational database initialization, schema migration tracking via
//! `PRAGMA user_version`, FTS5 full-text search, Content-Addressed Storage (CAS)
//! threshold isolation, and a single-writer persistence engine.

use crate::blobs::BlobStore;
use rusqlite::{Connection, OptionalExtension, Result, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{info, warn};

mod actor;
mod engine;
mod handle;
mod migrations;
mod reader;
pub mod context_store;
pub mod inspect_service;
pub mod migration_tool;
pub mod migration_policy;
pub mod session_ir;

pub use handle::get_persistence_handle;
pub(crate) use migrations::*;
pub use migrations::{
    CURRENT_DB_VERSION, SessionTranscriptView, open_in_memory_for_tests,
};

#[cfg(test)]
mod tests;

/// One projected message row inside a [`SessionTranscriptView`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessageView {
    pub seq: u64,
    /// Lowercase role string (`user` / `assistant` / `system` / `tool`).
    pub role: String,
    pub content: String,
}

fn role_str(role: nuo_wire::Role) -> &'static str {
    match role {
        nuo_wire::Role::User => "user",
        nuo_wire::Role::Assistant => "assistant",
        nuo_wire::Role::System => "system",
        nuo_wire::Role::Tool => "tool",
    }
}

fn role_from_str(role: &str) -> Option<nuo_wire::Role> {
    match role {
        "user" => Some(nuo_wire::Role::User),
        "assistant" => Some(nuo_wire::Role::Assistant),
        "system" => Some(nuo_wire::Role::System),
        "tool" => Some(nuo_wire::Role::Tool),
        _ => None,
    }
}

fn origin_str(origin: nuo_wire::EntryOrigin) -> &'static str {
    match origin {
        nuo_wire::EntryOrigin::Harness => "harness",
        nuo_wire::EntryOrigin::Checkpoint => "checkpoint",
    }
}

fn origin_from_str(origin: &str) -> Option<nuo_wire::EntryOrigin> {
    match origin {
        "harness" => Some(nuo_wire::EntryOrigin::Harness),
        "checkpoint" => Some(nuo_wire::EntryOrigin::Checkpoint),
        _ => None,
    }
}

fn serde_plain(kind: nuo_wire::DirectiveKind) -> rusqlite::Result<&'static str> {
    Ok(match kind {
        nuo_wire::DirectiveKind::Prune => "prune",
        nuo_wire::DirectiveKind::Compact => "compact",
        nuo_wire::DirectiveKind::Freeze => "freeze",
    })
}

/// Pull the `content_blob` hash out of a raw payload JSON document without a
/// typed decode (used for verbatim-preserved unknown entries).
fn extract_content_blob(payload_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    value
        .get("content_blob")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn map_session_row(row: &Row) -> Result<SessionRecord> {
    Ok(SessionRecord {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        fork_kind: row.get(2)?,
        title: row.get(3)?,
        created_at_s: row.get(4)?,
        updated_at_s: row.get(5)?,
        workspace_root: row.get(6)?,
        persona: row.get(7)?,
        msg_count: row.get(8)?,
        last_user_prompt: row.get(9)?,
        digest: row.get(10)?,
    })
}

type SummaryRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
);

fn map_summary_row(row: &Row) -> Result<SummaryRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn push_summary(
    summaries: &mut Vec<crate::session::SessionSummary>,
    item: SummaryRow,
    active_id: &str,
) {
    let (
        id,
        parent_id,
        fork_str,
        title,
        created_at_s,
        updated_at_s,
        msg_count,
        last_user_prompt,
        digest_json,
    ) = item;
    let fork_kind = match fork_str.as_str() {
        "fork" => nuo_wire::SessionForkKind::Fork,
        "aside" => nuo_wire::SessionForkKind::Aside,
        _ => nuo_wire::SessionForkKind::Trunk,
    };
    let final_msg_count = msg_count.max(0) as usize;
    if final_msg_count == 0 && id != active_id {
        return;
    }
    let overview = if let Some(t) = title.as_deref().filter(|t| !t.trim().is_empty()) {
        crate::session::truncate_preview(t, 64)
    } else if let Some(prompt) = last_user_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        crate::session::truncate_preview(prompt, 64)
    } else {
        "(empty session)".to_string()
    };
    let digest = digest_json.and_then(|digest_raw| serde_json::from_str(&digest_raw).ok());
    summaries.push(crate::session::SessionSummary {
        id: id.clone(),
        parent_id,
        fork_kind,
        message_count: final_msg_count,
        updated_at: updated_at_s.max(0) as u64,
        created_at: created_at_s.max(0) as u64,
        overview,
        active: id == active_id,
        digest,
    });
}

fn map_search_row(row: &Row) -> Result<HistorySearchResult> {
    Ok(HistorySearchResult {
        entry_id: row.get(0)?,
        session_id: row.get(1)?,
        workspace_root: row.get(2)?,
        session_title: row.get(3)?,
        role: row.get(4)?,
        snippet: row.get(5)?,
        score: row.get(6)?,
    })
}

/// Sanitize a free-text query into a safe FTS5 MATCH expression (ADR-0208).
///
/// Every whitespace-separated word becomes a quoted phrase token (`"retry"`
/// → `"retry"`, `retry loop` → `"retry" AND "loop"`), so user input can never
/// inject FTS5 column filters or boolean operators — the raw MATCH grammar
/// treats a bare word that names a column as a filter and errors out (the
/// live failure that motivated this: `no such column: needle`). Word
/// characters plus a small punctuation allowlist (`-` `_` `.`) survive;
/// everything else (including quotes) is stripped, so FTS5 string-literal
/// grammar is unreachable and the expression is injection-proof by
/// construction. A query with no word characters degenerates to an empty
/// string (no-op search).
///
/// `match_any` joins the tokens with OR instead of AND — the relaxed recall
/// mode (`search_history_relaxed`).
fn sanitize_fts_query_joined(query: &str, match_any: bool) -> String {
    let joiner = if match_any { " OR " } else { " AND " };
    sanitize_fts_words(query).join(joiner)
}

/// The word-level sanitizer: cleaned tokens (punctuation stripped, lowercased
/// implicitly by FTS's own tokenizer) that survive into any MATCH form.
fn sanitize_fts_words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter_map(|word| {
            let cleaned: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(format!("\"{cleaned}\""))
            }
        })
        .collect()
}

/// The strict (AND) form, kept as the test-facing wrapper around the joined
/// sanitizer.
#[cfg(test)]
fn sanitize_fts_query(query: &str) -> String {
    sanitize_fts_query_joined(query, false)
}

/// Envelope columns of one `entries` row, as written by a save (ADR-0186 §3).
struct EntryEnvelope<'a> {
    id: &'a str,
    kind: &'a str,
    role: Option<&'a str>,
    content: Option<&'a str>,
    origin: Option<&'a str>,
    hidden: bool,
    created_at_ms: u64,
    payload: &'a str,
}

/// Authoritative relational database access object for Muta persistence.
pub(crate) struct DatabaseEngine {
    conn: Connection,
    /// CAS store used to offload oversized entry bodies at insert time
    /// (ADR-0187): only rows this transaction inserts are offloaded, so the
    /// per-save cost stays proportional to the delta. `None` keeps content
    /// inline (readers, tests).
    blob_store: Option<BlobStore>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct FastSummaryProbe {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    digest: Option<nuo_wire::SessionDigest>,
    #[serde(default)]
    model_window: Vec<FastMessageProbe>,
    #[serde(default)]
    archived_transcript: Vec<FastMessageProbe>,
}

#[derive(Deserialize)]
struct FastMessageProbe {
    role: nuo_wire::Role,
    #[serde(default)]
    content: String,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    origin: Option<FastOriginProbe>,
}

#[derive(Deserialize)]
struct FastOriginProbe {
    kind: nuo_wire::InjectionKind,
}

/// The **only** way to read the unified store.
///
/// `DatabaseEngine` is crate-private, so no other module — in this crate or in
/// any other — can name a SQLite connection, let alone open one on a path of
/// its own choosing. Readers are handed out by the same handle that owns the
/// writer ([`PersistenceHandle::reader`]), which is what makes "every mutation
/// goes through the single-writer actor, every read through the handle" a
/// type-level invariant instead of a convention someone has to remember.
///
/// A reader is a short-lived connection snapshot: WAL readers neither block
/// the writer nor are blocked by it. Open one per read *session* (one per tool
/// call, one per report) rather than one per query inside a loop.
///
/// The method surface is deliberately an allow-list. Widening read reach is a
/// reviewable act in this file, not an incidental side effect of adding a
/// `pub fn` to the engine.
pub struct DbReader {
    engine: DatabaseEngine,
    db_path: PathBuf,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
    /// Registers this snapshot's age for the D7 measurement contract; releases
    /// the registration when the reader is dropped.
    _age: Option<ReaderAgeGuard>,
}

// Asynchronous Persistence Actor (Single-Writer Pattern, supervised — ADR-0196)

/// Typed failure of a persistence command (ADR-0196 D2).
///
/// The single-writer actor is a long-lived service, so its failures are
/// *lifecycle* facts, not SQL facts. Every `PersistenceHandle` method returns
/// this error type; callers classify instead of string-matching.
#[derive(Debug)]
pub enum PersistenceError {
    /// The single-writer actor is not serving: it died (engine-open failure
    /// or panic) and has not been respawned yet, the supervisor is still in
    /// its backoff window, or the handle was shut down. The command was **not
    /// executed**; nothing was written.
    WriterDown,
    /// The actor served the command and the SQLite engine rejected it.
    Engine(rusqlite::Error),
    /// The command handler panicked. The supervisor's per-command
    /// `catch_unwind` contained it: the actor survives, this command failed.
    Poisoned(String),
    /// A value failed to encode *before* reaching the writer (e.g. JSON
    /// serialization). Nothing was written.
    Encode(String),
    /// The handle was explicitly shut down (every clone dropped / supervisor
    /// stopped). Distinct from [`PersistenceError::WriterDown`] so an orderly
    /// teardown is distinguishable from a crash.
    Closed,
    /// The commit carried an `expected_revision` that no longer matches the
    /// durable session revision (ADR-0236 D3). The write was **not** applied;
    /// the caller must re-read and retry against the current revision.
    StaleRevision { expected: u64, actual: u64 },
    /// An idempotency key was reused with a different payload (ADR-0236 D3):
    /// the original operation already committed, so reusing its identity for
    /// other content is refused rather than silently applied or dropped.
    OperationConflict { operation_id: String },
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriterDown => write!(f, "persistence writer is down"),
            Self::Engine(e) => write!(f, "persistence engine rejected the command: {e}"),
            Self::Poisoned(msg) => write!(f, "persistence command handler panicked: {msg}"),
            Self::Encode(msg) => write!(f, "could not encode persistence payload: {msg}"),
            Self::Closed => write!(f, "persistence writer was shut down"),
            Self::StaleRevision { expected, actual } => write!(
                f,
                "stale session revision: expected {expected}, durable revision is {actual}"
            ),
            Self::OperationConflict { operation_id } => write!(
                f,
                "operation {operation_id} was reused with a different payload"
            ),
        }
    }
}

impl std::error::Error for PersistenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Engine(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for PersistenceError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Engine(e)
    }
}

/// Liveness of the single-writer actor, observable by any handle clone
/// (ADR-0196 D1/D4). Transitions are published on a `watch` channel; the
/// daemon folds them into the monitor stream, frontends render degradation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterHealth {
    /// Serving normally.
    Healthy,
    /// The writer died or failed to (re)start and the supervisor is retrying
    /// with backoff. `attempt` counts respawn attempts since the first
    /// failure; `error` is the latest cause.
    Recovering {
        attempt: u32,
        since_ms: u64,
        error: String,
    },
    /// Respawn attempts exceeded the recovering budget (`RECOVERING_ATTEMPTS`)
    /// and every further failure is at the capped backoff. Retries continue
    /// forever; this state says "durability is degraded, tell the user".
    Down {
        attempt: u32,
        since_ms: u64,
        error: String,
    },
}

impl WriterHealth {
    /// Whether writes can currently be expected to succeed.
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// The latest failure cause, when degraded.
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Healthy => None,
            Self::Recovering { error, .. } | Self::Down { error, .. } => Some(error),
        }
    }

    /// The wire representation for the monitor stream (ADR-0196 D4). The
    /// conversion lives here, next to the state machine, so no consumer can
    /// mis-translate a transition.
    pub fn to_wire(&self) -> nuo_wire::monitor::PersistenceHealth {
        match self {
            Self::Healthy => nuo_wire::monitor::PersistenceHealth::Healthy,
            Self::Recovering {
                attempt,
                since_ms,
                error,
            } => nuo_wire::monitor::PersistenceHealth::Recovering {
                attempt: *attempt,
                since_ms: *since_ms,
                error: error.clone(),
            },
            Self::Down {
                attempt,
                since_ms,
                error,
            } => nuo_wire::monitor::PersistenceHealth::Down {
                attempt: *attempt,
                since_ms: *since_ms,
                error: error.clone(),
            },
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Optional idempotency identity and revision precondition for one session
/// commit (ADR-0236 D3).
///
/// `operation_id` makes a replay return the original receipt instead of
/// re-applying the operation; `expected_revision` fails the commit closed
/// when the durable session revision has moved past what the caller saw.
/// Both are opt-in: a default guard preserves the pre-ADR behaviour.
#[derive(Debug, Clone, Default)]
pub(crate) struct CommitGuard {
    pub(crate) operation_id: Option<String>,
    pub(crate) expected_revision: Option<u64>,
}

/// Typed outcome of [`DatabaseEngine::save_session_inner`]: either the committed
/// revision, a SQL failure, or one of the ADR-0236 D3 refusals.
#[derive(Debug)]
pub(crate) enum SaveError {
    Sql(rusqlite::Error),
    StaleRevision { expected: u64, actual: u64 },
    OperationConflict { operation_id: String },
}

impl From<rusqlite::Error> for SaveError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl SaveError {
    fn into_persistence(self) -> PersistenceError {
        match self {
            Self::Sql(error) => PersistenceError::Engine(error),
            Self::StaleRevision { expected, actual } => {
                PersistenceError::StaleRevision { expected, actual }
            }
            Self::OperationConflict { operation_id } => {
                PersistenceError::OperationConflict { operation_id }
            }
        }
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(f, "{error}"),
            Self::StaleRevision { expected, actual } => write!(
                f,
                "stale session revision: expected {expected}, durable revision is {actual}"
            ),
            Self::OperationConflict { operation_id } => {
                write!(
                    f,
                    "operation {operation_id} was reused with a different payload"
                )
            }
        }
    }
}

/// SHA-256 fingerprint of the authoritative commit payload (ADR-0236 D3): it
/// detects an idempotency key reused for different content.
///
/// The fingerprint is deliberately O(1) in the session's size. It covers the
/// row checksum (working state), the transcript's tail identity, and the usage
/// upserts — enough to distinguish a replay from a different operation without
/// re-serializing the whole transcript, which `SessionData` setter paths clone
/// in full. Preparation runs **before** the writer transaction.
fn commit_payload_digest(
    data: &crate::session::SessionData,
    usage_upserts: &[nuo_wire::RequestUsageRecord],
) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.id.as_bytes());
    hasher.update([0u8]);
    hasher.update(data.generation.as_bytes());
    hasher.update([0u8]);
    hasher.update(data.updated_at.to_le_bytes());
    hasher.update(data.round_counter.to_le_bytes());
    hasher.update(data.checksum.unwrap_or(0).to_le_bytes());
    hasher.update((data.transcript.entries.len() as u64).to_le_bytes());
    if let Some(last) = data.transcript.entries.last() {
        hasher.update(last.id.as_bytes());
        hasher.update(last.seq.to_le_bytes());
    }
    let usage = serde_json::to_vec(usage_upserts)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    hasher.update(&usage);
    Ok(format!("{:x}", hasher.finalize()))
}

/// The durable revision for a session (ADR-0236 D3). Absent means revision 0.
fn session_revision(conn: &Connection, session_id: &str) -> Result<u64> {
    conn.query_row(
        "SELECT revision FROM session_revisions WHERE session_id=?1",
        params![session_id],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|revision| revision.unwrap_or(0).max(0) as u64)
}

/// The latest retained commit receipt for a session, if any. Receipts are
/// retained one-per-session: a later commit subsumes an earlier one, and a
/// replay only ever needs the most recent operation identity.
fn latest_commit_receipt(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<(String, String, u64)>> {
    conn.query_row(
        "SELECT operation_id, payload_hash, revision FROM commit_receipts
         WHERE session_id=?1 ORDER BY revision DESC LIMIT 1",
        params![session_id],
        |row| {
            let revision: i64 = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, revision.max(0) as u64))
        },
    )
    .optional()
}

fn record_commit_receipt(
    conn: &Connection,
    session_id: &str,
    operation_id: &str,
    revision: u64,
    payload_hash: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM commit_receipts WHERE session_id=?1 AND operation_id<>?2",
        params![session_id, operation_id],
    )?;
    conn.execute(
        "INSERT INTO commit_receipts(operation_id, session_id, revision, payload_hash)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(operation_id) DO UPDATE SET
             session_id=excluded.session_id, revision=excluded.revision, payload_hash=excluded.payload_hash",
        params![operation_id, session_id, revision as i64, payload_hash],
    )?;
    Ok(())
}

// The read/merge/write must share an IMMEDIATE transaction: separate reader
// and writer calls lose updates even when each individual write is serialized.
fn persist_usage_records(
    conn: &Connection,
    entries: Vec<nuo_wire::usage_stats::UsageStatRecord>,
) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for entry in entries {
        upsert_attempt(&tx, &entry)?;
    }
    tx.commit()
}

fn decode_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// Migration-only import of one historical attempt. Conflicting historical
/// facts for the same attempt identity are *reported* and the first record
/// wins, instead of aborting the migration — aborting leaves `user_version`
/// below v15, so every later open retries and fails the same import forever
/// (ADR-0236 "report conflicting records rather than silently selecting one").
/// The live settlement path keeps [`upsert_attempt`]'s hard refusal.
fn import_legacy_attempt(
    conn: &Connection,
    entry: &nuo_wire::usage_stats::UsageStatRecord,
) -> Result<()> {
    match upsert_attempt(conn, entry) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::InvalidParameterName(message))
            if message.contains("conflicting reported usage") =>
        {
            warn!(
                %message,
                "usage migration: conflicting historical attempt; keeping the first record"
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn apply_durable_attempt_schema(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    // Idempotent: an already-migrated database carries `usage_clock`. A re-run
    // (e.g. a test that re-migrates a current schema from a stale user_version)
    // must no-op instead of renaming `usage_records` again and colliding with
    // the existing `usage_records_day` index.
    if table_exists(tx, "usage_clock")? {
        return Ok(());
    }
    tx.execute_batch("ALTER TABLE usage_records RENAME TO legacy_usage_records;
        CREATE TABLE usage_records (
            session_id TEXT NOT NULL, actor_id TEXT NOT NULL, round INTEGER NOT NULL,
            turn INTEGER NOT NULL, attempt INTEGER NOT NULL, payload TEXT NOT NULL,
            day TEXT NOT NULL, recorded_at_ms INTEGER NOT NULL, project TEXT NOT NULL,
            revision INTEGER NOT NULL,
            PRIMARY KEY(session_id, actor_id, round, turn, attempt)
        );
        CREATE INDEX usage_records_day ON usage_records(day, recorded_at_ms);
        CREATE TABLE usage_clock (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO usage_clock VALUES(1,0);
        CREATE TABLE usage_dirty (
            session_id TEXT NOT NULL, actor_id TEXT NOT NULL, round INTEGER NOT NULL,
            turn INTEGER NOT NULL, attempt INTEGER NOT NULL, revision INTEGER NOT NULL,
            PRIMARY KEY(session_id, actor_id, round, turn, attempt),
            FOREIGN KEY(session_id, actor_id, round, turn, attempt)
                REFERENCES usage_records ON DELETE CASCADE
        );
        CREATE INDEX usage_dirty_revision ON usage_dirty(revision);
        CREATE TABLE usage_contributions (
            session_id TEXT NOT NULL, actor_id TEXT NOT NULL, round INTEGER NOT NULL,
            turn INTEGER NOT NULL, attempt INTEGER NOT NULL, revision INTEGER NOT NULL,
            day TEXT NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL,
            requests INTEGER NOT NULL, completed INTEGER NOT NULL,
            prompt_tokens INTEGER NOT NULL, completion_tokens INTEGER NOT NULL, total_tokens INTEGER NOT NULL,
            cache_write_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL, estimated_tokens INTEGER NOT NULL,
            PRIMARY KEY(session_id, actor_id, round, turn, attempt),
            FOREIGN KEY(session_id, actor_id, round, turn, attempt)
                REFERENCES usage_records ON DELETE CASCADE
        );
        CREATE INDEX usage_contributions_day ON usage_contributions(day, provider, model);
        CREATE TABLE commit_receipts (
            operation_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, revision INTEGER NOT NULL,
            payload_hash TEXT NOT NULL
        );
        CREATE TABLE session_revisions (session_id TEXT PRIMARY KEY, revision INTEGER NOT NULL);
    ")?;
    // Prefer historical cross-session attribution; per-session facts fill gaps.
    // `kv_store` predates this migration on every real database, but a
    // hand-built test schema may omit it — there is then nothing historical to
    // import, so the import and its delete are both skipped.
    let has_kv_store = table_exists(tx, "kv_store")?;
    if has_kv_store {
        let mut stmt =
            tx.prepare("SELECT value FROM kv_store WHERE key LIKE 'usage:day:%' ORDER BY key")?;
        let blobs = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>>>()?;
        drop(stmt);
        #[derive(Deserialize)]
        struct Day {
            records: Vec<nuo_wire::usage_stats::UsageStatRecord>,
        }
        for blob in blobs {
            let day: Day = decode_json(&blob)?;
            for entry in day.records {
                import_legacy_attempt(tx, &entry)?;
            }
        }
    }
    let mut stmt = tx.prepare("SELECT u.payload, COALESCE(s.workspace_root,''), COALESCE(s.updated_at_s,0) FROM legacy_usage_records u LEFT JOIN sessions s ON s.id=u.session_id")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    drop(stmt);
    for (payload, root, at) in rows {
        let record: nuo_wire::RequestUsageRecord = decode_json(&payload)?;
        let at = if record.started_at_ms > 0 {
            record.started_at_ms
        } else {
            at.saturating_mul(1000)
        };
        import_legacy_attempt(
            tx,
            &nuo_wire::usage_stats::UsageStatRecord {
                day: nuo_wire::usage_stats::day_key_from_epoch_ms(at),
                recorded_at_ms: at,
                project: if root.is_empty() {
                    String::new()
                } else {
                    crate::paths::project_bucket_name(Path::new(&root))
                },
                record,
            },
        )?;
    }
    tx.execute_batch("DROP TABLE legacy_usage_records;")?;
    if has_kv_store {
        tx.execute_batch("DELETE FROM kv_store WHERE key LIKE 'usage:day:%';")?;
    }
    Ok(())
}

fn upsert_attempt(
    conn: &Connection,
    entry: &nuo_wire::usage_stats::UsageStatRecord,
) -> Result<bool> {
    use nuo_wire::{RequestUsageRecord, RequestUsageSource};
    let key = &entry.record.key;
    let previous: Option<String> = conn.query_row(
        "SELECT payload FROM usage_records WHERE session_id=?1 AND actor_id=?2 AND round=?3 AND turn=?4 AND attempt=?5",
        params![key.session_id,key.actor_id,key.round,key.turn,key.attempt], |r| r.get(0)).optional()?;
    if let Some(previous) = previous {
        let old: RequestUsageRecord = decode_json(&previous)?;
        if old == entry.record {
            return Ok(false);
        }
        if old.status.is_terminal() {
            if !entry.record.status.is_terminal() {
                return Ok(false);
            }
            if old.source == RequestUsageSource::Reported
                && entry.record.source != RequestUsageSource::Reported
            {
                return Ok(false);
            }
            if old.source == RequestUsageSource::Reported
                && entry.record.source == RequestUsageSource::Reported
            {
                // Allow repaired metadata, but never conflicting authoritative counts.
                if old.prompt_tokens != entry.record.prompt_tokens
                    || old.completion_tokens != entry.record.completion_tokens
                    || old.total_tokens != entry.record.total_tokens
                {
                    return Err(rusqlite::Error::InvalidParameterName(format!(
                        "conflicting reported usage for {:?}",
                        key
                    )));
                }
            }
        }
    }
    let payload = serde_json::to_string(&entry.record)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    conn.execute("UPDATE usage_clock SET revision=revision+1 WHERE id=1", [])?;
    let revision: i64 = conn.query_row("SELECT revision FROM usage_clock WHERE id=1", [], |r| {
        r.get(0)
    })?;
    conn.execute("INSERT INTO usage_records(session_id,actor_id,round,turn,attempt,payload,day,recorded_at_ms,project,revision)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
        ON CONFLICT(session_id,actor_id,round,turn,attempt) DO UPDATE SET payload=excluded.payload, revision=excluded.revision,
        project=CASE WHEN usage_records.project='' THEN excluded.project ELSE usage_records.project END",
        params![key.session_id,key.actor_id,key.round,key.turn,key.attempt,payload,entry.day,entry.recorded_at_ms,entry.project,revision])?;
    conn.execute("INSERT INTO usage_dirty VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(session_id,actor_id,round,turn,attempt) DO UPDATE SET revision=excluded.revision",
        params![key.session_id,key.actor_id,key.round,key.turn,key.attempt,revision])?;
    Ok(true)
}

/// Command variants dispatched to the single-writer persistence actor.
pub(crate) enum PersistenceCommand {
    SaveSession {
        data: Box<crate::session::SessionData>,
        /// `true`: rewrite every membership/projection/blob reference from
        /// `data`. `false`: append only rows above the durable watermark,
        /// escalating to a full rewrite on generation mismatch (ADR-0187).
        full: bool,
        /// Usage-record upserts to apply on a delta save (the durable usage
        /// ledger lives in its own table; a full save rewrites it from
        /// `data`).
        usage_upserts: Vec<nuo_wire::RequestUsageRecord>,
        /// ADR-0236 D3 idempotency identity and revision precondition.
        guard: CommitGuard,
        /// The committed session revision (ADR-0236 D3); a replay returns the
        /// original receipt rather than a second revision.
        ack: oneshot::Sender<Result<u64, PersistenceError>>,
    },
    UpsertSession {
        record: SessionRecord,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    DeleteSession {
        session_id: String,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    RenameSession {
        session_id: String,
        title: Option<String>,
        manual: bool,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    RecordCommand {
        cmd: nuo_wire::CommandRecord,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    RecordRequestProjection {
        session_id: String,
        record: nuo_wire::RequestProjection,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    ProjectUsage {
        ack: oneshot::Sender<Result<usize, PersistenceError>>,
    },
    RecordUsageStats {
        entries: Vec<nuo_wire::usage_stats::UsageStatRecord>,
        ack: Option<oneshot::Sender<Result<(), PersistenceError>>>,
    },
    SetKV {
        key: String,
        value: String,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    DeleteKV {
        key: String,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    RecordInputHistory {
        entry: nuo_wire::HistoryEntry,
        dedup: bool,
        ack: Option<oneshot::Sender<Result<(), PersistenceError>>>,
    },
    SaveInputHistory {
        entries: Vec<nuo_wire::HistoryEntry>,
        dedup: bool,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    ClearInputHistory {
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    DeleteInputHistoryEntry {
        text: String,
        created_at_ms: u64,
        ack: Option<oneshot::Sender<Result<usize, PersistenceError>>>,
    },
    /// Reclaim transcript entries no session membership references
    /// (ADR-0187).
    CollectEntryGarbage {
        ack: oneshot::Sender<Result<usize, PersistenceError>>,
    },
    /// Create an online hot backup snapshot using `VACUUM INTO`.
    CreateBackup {
        target_path: PathBuf,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    /// Persist an incremental Session IR delta directly into SQLite (ADR-0241/ADR-0249, INV-SESSION-05).
    SaveSessionDelta {
        delta: Box<nuo_wire::SessionDelta>,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    /// Commit immutable execution facts to the canonical context store (ADR-0275 §7).
    CommitContextFacts {
        commit: Box<crate::db::context_store::OwnedFactCommit>,
        ack: oneshot::Sender<Result<crate::db::context_store::CommitOutcome, crate::db::context_store::ContextCommitError>>,
    },
    /// Commit a branch context view to the canonical context store (ADR-0275 §7, ADR-0278).
    CommitContextView {
        commit: Box<crate::db::context_store::OwnedViewCommit>,
        ack: oneshot::Sender<Result<crate::db::context_store::CommitOutcome, crate::db::context_store::ContextCommitError>>,
    },
    /// Execute an inspect deletion job (ADR-0279 §6).
    ExecuteInspectDeletion {
        session_id: String,
        job_id: String,
        now_ms: u64,
        ack: oneshot::Sender<Result<crate::db::inspect_service::DeletionReport, nuo_wire::context_lifecycle::InspectError>>,
    },
    /// Collect a bounded garbage collection batch for artifacts/leases (ADR-0279 §5).
    CollectInspectGarbage {
        session_id: String,
        now_ms: u64,
        batch_limit: u32,
        batch_ms: u64,
        ack: oneshot::Sender<Result<crate::db::inspect_service::CollectionProgress, nuo_wire::context_lifecycle::InspectError>>,
    },
    /// Test-only: the writer acks and then exits its loop, simulating actor
    /// death so the supervisor's respawn path is exercisable (ADR-0196 D6).
    #[cfg(test)]
    Die { ack: oneshot::Sender<()> },
    /// Test-only: apply a session commit and then exit the loop **without
    /// acknowledging** — simulating death after the commit but before its ack
    /// (ADR-0236 D3 verification gate: an unknown outcome is resolved by
    /// operation identity, never assumed uncommitted).
    #[cfg(test)]
    SaveSessionThenDie {
        data: Box<crate::session::SessionData>,
        full: bool,
        usage_upserts: Vec<nuo_wire::RequestUsageRecord>,
        guard: CommitGuard,
    },
}

/// Run one engine call, catching handler panics (ADR-0196 D1).
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T, PersistenceError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(res) => res.map_err(PersistenceError::Engine),
        Err(panic) => Err(PersistenceError::Poisoned(panic_message(&panic))),
    }
}

/// Run one session commit, catching handler panics and preserving the typed
/// ADR-0236 D3 refusals (`StaleRevision` / `OperationConflict`) instead of
/// flattening them into `Engine`.
fn guarded_save<T>(
    f: impl FnOnce() -> std::result::Result<T, SaveError>,
) -> Result<T, PersistenceError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(res) => res.map_err(SaveError::into_persistence),
        Err(panic) => Err(PersistenceError::Poisoned(panic_message(&panic))),
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Asynchronous handle for interacting with the supervised single-writer
/// persistence actor without blocking the Tokio runtime (ADR-0196).
///
/// The front door and the health channel survive writer death: a respawned
/// writer is reachable through the same handle, so no call site changes on
/// recovery. Clone freely; dropping the last clone shuts the supervisor down.
#[derive(Clone)]
pub struct PersistenceHandle {
    supervisor: mpsc::Sender<PersistenceCommand>,
    health: watch::Receiver<WriterHealth>,
    db_path: PathBuf,
    blob_store: Option<BlobStore>,
    startup_error: Option<Arc<str>>,
    readers: Arc<tokio::sync::Semaphore>,
    reader_ages: Arc<ReaderAges>,
}

/// Reader-pool capacity (ADR-0231: one lease per read session).
const READER_POOL_CAPACITY: usize = 16;

/// Live reader registrations for the ADR-0236 D7 measurement contract: a
/// long-lived reader snapshot is what pins the WAL, so its age is the useful
/// pressure signal. Keyed by a monotonic id so each `DbReader`'s drop guard
/// removes exactly its own entry.
#[derive(Debug, Default)]
struct ReaderAges {
    ages: std::sync::Mutex<std::collections::BTreeMap<u64, Instant>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl ReaderAges {
    fn register(&self) -> u64 {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.ages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Instant::now());
        id
    }

    fn release(&self, id: u64) {
        self.ages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }

    fn active(&self) -> usize {
        self.ages.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn oldest_age(&self) -> Option<Duration> {
        self.ages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .min()
            .map(Instant::elapsed)
    }
}

/// Releases a reader's registration when its snapshot is dropped.
struct ReaderAgeGuard {
    reader_ages: Arc<ReaderAges>,
    id: u64,
}

impl Drop for ReaderAgeGuard {
    fn drop(&mut self) {
        self.reader_ages.release(self.id);
    }
}

/// A storage-observability snapshot (ADR-0236 D7 measurement contract):
/// WAL growth and reader age are what decide whether checkpointing can make
/// progress. Recorded without prompt contents or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageMetrics {
    pub main_bytes: u64,
    pub wal_bytes: u64,
    pub active_readers: usize,
    pub reader_capacity: usize,
    /// Age of the oldest active reader snapshot. A large age means the WAL
    /// cannot be reclaimed.
    pub oldest_reader_ms: Option<u64>,
}

static GLOBAL_HANDLE: OnceLock<PersistenceHandle> = OnceLock::new();

struct RegisteredOwner {
    supervisor: mpsc::WeakSender<PersistenceCommand>,
    health: watch::Receiver<WriterHealth>,
    blob_store: Option<BlobStore>,
    readers: Arc<tokio::sync::Semaphore>,
    reader_ages: Arc<ReaderAges>,
    lease: std::sync::Weak<nuo_host::lock::ProcessLock>,
}

static OWNERS: OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, RegisteredOwner>>> =
    OnceLock::new();

fn database_identity(path: &Path) -> std::result::Result<PathBuf, String> {
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if std::fs::metadata(path).map_err(|e| e.to_string())?.nlink() != 1 {
                return Err("hard-linked databases are not supported".into());
            }
        }
        return path.canonicalize().map_err(|e| e.to_string());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    Ok(parent
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(path.file_name().ok_or("database path has no filename")?))
}
