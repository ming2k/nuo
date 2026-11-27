//! Fixed-K rebuildable content shards (ADR-0242).
//!
//! Every thread routes deterministically to one content shard via
//! `crc32c(thread_id) % shard_count` (default `K = 16`). Each shard has:
//!
//! - its own dedicated SQLite file (`<shards_dir>/content-XX.db`, WAL + `synchronous=NORMAL`);
//! - its own dedicated single-writer actor thread (spawned lazily on first write
//!   to that shard, strictly bounded by `K` = `O(1)` threads regardless of session
//!   or workspace count);
//! - its own clean `STRICT` schema with **zero** fake `threads` stub rows and no
//!   cross-plane foreign keys;
//! - its own `projection_watermarks(thread_id, seq, updated_at_ms)` table.
//!
//! A shard is 100% derived from the session journals: deleting `<shards_dir>`
//! with journals intact rebuilds every shard via `catch_up_projections_for`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Result, params};
use tokio::sync::{mpsc, oneshot};

use crate::db::{HistorySearchResult, PersistenceError, sanitize_fts_query_joined};
use crate::workspace_effects::{NewOwnedEffect, WorkspaceEffect};

/// Default fixed shard count (`K = 16`). Uniformly distributes concurrent
/// sessions (even inside one monorepo) while bounding open FDs and writer
/// threads to `O(1)`.
pub const DEFAULT_SHARD_COUNT: usize = 16;

/// Deterministic fixed-K shard assignment (`crc32c(thread_id) % K`).
pub(crate) fn shard_index(thread_id: &str, shard_count: usize) -> usize {
    (crc32c::crc32c(thread_id.as_bytes()) as usize) % shard_count.max(1)
}

/// Directory holding content shards beside a catalog database path.
pub(crate) fn shards_dir_for_catalog(catalog_db_path: &Path) -> PathBuf {
    catalog_db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("shards")
}

/// One shard's file path (`shards/content-00.db`).
pub(crate) fn shard_path(shards_dir: &Path, index: usize) -> PathBuf {
    shards_dir.join(format!("content-{index:02}.db"))
}

fn open_shard_writer_conn(path: &Path) -> Result<Connection> {
    crate::db::migrations::initialize_content_shard_db(path)
}

fn open_shard_reader_conn(path: &Path) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    if !crate::db::migrations::is_current_content_shard(&conn)? {
        return Ok(None);
    }
    Ok(Some(conn))
}

enum ShardCommand {
    SaveContent {
        thread_id: String,
        thread: Option<Box<nuo_protocol::Thread>>,
        extra_blob_refs: Vec<crate::db::content_projection::ExtraBlobRef>,
        replace: bool,
        commands: Option<Vec<nuo_protocol::CommandRecord>>,
        journal_seq: Option<u64>,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    DeleteThread {
        thread_id: String,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    AppendCommandAct {
        thread_id: String,
        seq: u64,
        payload_json: String,
        journal_seq: Option<u64>,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    AppendWorkspaceEffect {
        effect_id: i64,
        effect: Box<NewOwnedEffect>,
        journal_seq: Option<u64>,
        ack: oneshot::Sender<Result<i64, PersistenceError>>,
    },
    AppendCompletedWorkspaceEffect {
        effect_id: i64,
        effect: Box<NewOwnedEffect>,
        completed_at_ms: u64,
        journal_seq: Option<u64>,
        ack: oneshot::Sender<Result<i64, PersistenceError>>,
    },
    CompleteWorkspaceEffect {
        thread_id: String,
        effect_id: i64,
        after_hash: Option<String>,
        completed_at_ms: u64,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    MarkWorkspaceEffectReversed {
        thread_id: String,
        effect_id: i64,
        reversed_at_ms: u64,
        note: Option<String>,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    ReplayWorkspaceEffects {
        thread_id: String,
        effects: Vec<WorkspaceEffect>,
        journal_seq: Option<u64>,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    ExpirePreimages {
        older_than_ms: u64,
        now_ms: u64,
        ack: oneshot::Sender<Result<usize, PersistenceError>>,
    },
}

struct ShardWriter {
    tx: mpsc::Sender<ShardCommand>,
}

impl ShardWriter {
    fn spawn(index: usize, path: PathBuf) -> Result<Self, PersistenceError> {
        let mut conn = open_shard_writer_conn(&path)?;
        let (tx, mut rx) = mpsc::channel::<ShardCommand>(256);
        std::thread::Builder::new()
            .name(format!("nuo-content-shard-{index:02}"))
            .spawn(move || {
                while let Some(cmd) = rx.blocking_recv() {
                    // If the disposable shard file was removed on disk while the
                    // writer thread held an unlinked FD (Unix), reopen a fresh
                    // verified shard before applying the next command (ADR-0255).
                    if !path.exists()
                        && let Ok(reopened) = open_shard_writer_conn(&path)
                    {
                        conn = reopened;
                    }
                    execute_shard_command(&conn, cmd);
                }
            })
            .map_err(|error| PersistenceError::Encode(error.to_string()))?;
        Ok(Self { tx })
    }
}

fn execute_shard_command(conn: &Connection, cmd: ShardCommand) {
    match cmd {
        ShardCommand::SaveContent {
            thread_id,
            thread,
            extra_blob_refs,
            replace,
            commands,
            journal_seq,
            ack,
        } => {
            let res = (|| -> Result<(), PersistenceError> {
                conn.execute("BEGIN IMMEDIATE", [])?;
                let inner = (|| -> Result<(), PersistenceError> {
                    if let Some(thread) = thread.as_ref()
                        && (replace
                            || !thread.graph.is_empty()
                            || !thread.state.branches.is_empty()
                            || !extra_blob_refs.is_empty())
                    {
                        crate::db::content_projection::persist_content_facts(
                            conn,
                            thread,
                            replace,
                            &extra_blob_refs,
                        )
                        .map_err(PersistenceError::Encode)?;
                    }
                    if let Some(commands) = commands.as_ref() {
                        crate::db::thread_session::sync_command_acts(conn, &thread_id, commands)?;
                    }
                    if let Some(seq) = journal_seq {
                        crate::db::thread_session::advance_watermark(conn, &thread_id, seq)?;
                    }
                    Ok(())
                })();
                match inner {
                    Ok(()) => {
                        conn.execute("COMMIT", [])?;
                        Ok(())
                    }
                    Err(error) => {
                        let _ = conn.execute("ROLLBACK", []);
                        Err(error)
                    }
                }
            })();
            let _ = ack.send(res);
        }
        ShardCommand::DeleteThread { thread_id, ack } => {
            let res = (|| -> Result<(), PersistenceError> {
                conn.execute("BEGIN IMMEDIATE", [])?;
                let inner = (|| -> Result<(), PersistenceError> {
                    conn.prepare_cached("DELETE FROM causal_nodes WHERE thread_id = ?1")?
                        .execute(params![thread_id])?;
                    conn.prepare_cached("DELETE FROM thread_branches WHERE thread_id = ?1")?
                        .execute(params![thread_id])?;
                    conn.prepare_cached("DELETE FROM thread_blob_refs WHERE thread_id = ?1")?
                        .execute(params![thread_id])?;
                    conn.prepare_cached("DELETE FROM command_acts WHERE thread_id = ?1")?
                        .execute(params![thread_id])?;
                    conn.prepare_cached("DELETE FROM fts_thread_facts WHERE thread_id = ?1")?
                        .execute(params![thread_id])?;
                    conn.prepare_cached("DELETE FROM projection_watermarks WHERE thread_id = ?1")?
                        .execute(params![thread_id])?;
                    let _ = crate::workspace_effects::expire_thread_preimages(
                        conn,
                        &thread_id,
                        crate::db::unix_ms(),
                    );
                    Ok(())
                })();
                match inner {
                    Ok(()) => {
                        conn.execute("COMMIT", [])?;
                        Ok(())
                    }
                    Err(error) => {
                        let _ = conn.execute("ROLLBACK", []);
                        Err(error)
                    }
                }
            })();
            let _ = ack.send(res);
        }
        ShardCommand::AppendCommandAct {
            thread_id,
            seq,
            payload_json,
            journal_seq,
            ack,
        } => {
            let res = crate::db::thread_session::append_command_act(
                conn,
                &thread_id,
                seq,
                &payload_json,
                journal_seq,
            )
            .map_err(PersistenceError::Engine);
            let _ = ack.send(res);
        }
        ShardCommand::AppendWorkspaceEffect {
            effect_id,
            effect,
            journal_seq,
            ack,
        } => {
            let res = (|| -> Result<i64, PersistenceError> {
                let id = crate::workspace_effects::append_effect_with_id(
                    conn,
                    effect_id,
                    &effect.as_borrowed(),
                )?;
                if let Some(seq) = journal_seq {
                    crate::db::thread_session::advance_watermark(conn, &effect.thread_id, seq)?;
                }
                Ok(id)
            })();
            let _ = ack.send(res);
        }
        ShardCommand::AppendCompletedWorkspaceEffect {
            effect_id,
            effect,
            completed_at_ms,
            journal_seq,
            ack,
        } => {
            let res = (|| -> Result<i64, PersistenceError> {
                let id = crate::workspace_effects::append_completed_effect_with_id(
                    conn,
                    effect_id,
                    &effect.as_borrowed(),
                    completed_at_ms,
                )?;
                if let Some(seq) = journal_seq {
                    crate::db::thread_session::advance_watermark(conn, &effect.thread_id, seq)?;
                }
                Ok(id)
            })();
            let _ = ack.send(res);
        }
        ShardCommand::CompleteWorkspaceEffect {
            thread_id,
            effect_id,
            after_hash,
            completed_at_ms,
            ack,
        } => {
            let res = crate::workspace_effects::complete_effect_for_thread(
                conn,
                &thread_id,
                effect_id,
                after_hash.as_deref(),
                completed_at_ms,
            );
            let _ = ack.send(res);
        }
        ShardCommand::MarkWorkspaceEffectReversed {
            thread_id,
            effect_id,
            reversed_at_ms,
            note,
            ack,
        } => {
            let res = crate::workspace_effects::mark_reversed_for_thread(
                conn,
                &thread_id,
                effect_id,
                reversed_at_ms,
                note.as_deref(),
            );
            let _ = ack.send(res);
        }
        ShardCommand::ReplayWorkspaceEffects {
            thread_id,
            effects,
            journal_seq,
            ack,
        } => {
            let res = (|| -> Result<(), PersistenceError> {
                crate::workspace_effects::replay_effects(conn, &effects)?;
                if let Some(seq) = journal_seq {
                    crate::db::thread_session::advance_watermark(conn, &thread_id, seq)?;
                }
                Ok(())
            })();
            let _ = ack.send(res);
        }
        ShardCommand::ExpirePreimages {
            older_than_ms,
            now_ms,
            ack,
        } => {
            let res = crate::workspace_effects::expire_preimages(conn, older_than_ms, now_ms);
            let _ = ack.send(res);
        }
    }
}

/// Fixed-K pool of rebuildable content shards (ADR-0242).
pub(crate) struct ContentShardPool {
    shards_dir: PathBuf,
    shard_count: usize,
    /// Per-shard lazy writer slots. Using `Mutex<Option<ShardWriter>>` instead
    /// of `OnceLock<Result<...>>` ensures a transient spawn failure (e.g.
    /// `EMFILE`/`ENOSPC`) is never latched forever, and a dead writer channel
    /// can transparently respawn on the next write.
    writers: Vec<Mutex<Option<ShardWriter>>>,
}

impl std::fmt::Debug for ContentShardPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentShardPool")
            .field("shards_dir", &self.shards_dir)
            .field("shard_count", &self.shard_count)
            .finish()
    }
}

impl ContentShardPool {
    pub(crate) fn new(shards_dir: PathBuf, shard_count: usize) -> Arc<Self> {
        let count = shard_count.max(1);
        let mut writers = Vec::with_capacity(count);
        for _ in 0..count {
            writers.push(Mutex::new(None));
        }
        Arc::new(Self {
            shards_dir,
            shard_count: count,
            writers,
        })
    }

    pub(crate) fn shards_dir(&self) -> &Path {
        &self.shards_dir
    }

    pub(crate) fn shard_count(&self) -> usize {
        self.shard_count
    }

    fn writer_for_index(
        &self,
        index: usize,
    ) -> Result<mpsc::Sender<ShardCommand>, PersistenceError> {
        let mut slot = self.writers[index]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(writer) = slot.as_ref()
            && !writer.tx.is_closed()
        {
            return Ok(writer.tx.clone());
        }
        let path = shard_path(&self.shards_dir, index);
        let writer =
            ShardWriter::spawn(index, path).map_err(|e| PersistenceError::Encode(e.to_string()))?;
        let tx = writer.tx.clone();
        *slot = Some(writer);
        Ok(tx)
    }

    fn writer_for(&self, thread_id: &str) -> Result<mpsc::Sender<ShardCommand>, PersistenceError> {
        let idx = shard_index(thread_id, self.shard_count);
        self.writer_for_index(idx)
    }

    /// Open a read-only connection to the shard owning `thread_id`, if the
    /// shard file exists on disk.
    pub(crate) fn open_reader_for(&self, thread_id: &str) -> Result<Option<Connection>> {
        let idx = shard_index(thread_id, self.shard_count);
        open_shard_reader_conn(&shard_path(&self.shards_dir, idx))
    }

    pub(crate) fn projection_watermark(&self, thread_id: &str) -> Result<Option<u64>> {
        let Some(conn) = self.open_reader_for(thread_id)? else {
            return Ok(None);
        };
        conn.query_row(
            "SELECT seq FROM projection_watermarks WHERE thread_id = ?1",
            [thread_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.map(|seq| seq.max(0) as u64))
    }

    pub(crate) async fn save_content(
        &self,
        thread_id: String,
        thread: Option<nuo_protocol::Thread>,
        extra_blob_refs: Vec<crate::db::content_projection::ExtraBlobRef>,
        replace: bool,
        commands: Option<Vec<nuo_protocol::CommandRecord>>,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        let tx = self.writer_for(&thread_id)?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(ShardCommand::SaveContent {
            thread_id,
            thread: thread.map(Box::new),
            extra_blob_refs,
            replace,
            commands,
            journal_seq,
            ack: ack_tx,
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) fn save_content_blocking(
        &self,
        thread_id: String,
        thread: Option<nuo_protocol::Thread>,
        extra_blob_refs: Vec<crate::db::content_projection::ExtraBlobRef>,
        replace: bool,
        commands: Option<Vec<nuo_protocol::CommandRecord>>,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        futures::executor::block_on(self.save_content(
            thread_id,
            thread,
            extra_blob_refs,
            replace,
            commands,
            journal_seq,
        ))
    }

    pub(crate) async fn delete_thread(&self, thread_id: String) -> Result<(), PersistenceError> {
        let idx = shard_index(&thread_id, self.shard_count);
        if !shard_path(&self.shards_dir, idx).exists() {
            return Ok(());
        }
        let tx = self.writer_for_index(idx)?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(ShardCommand::DeleteThread {
            thread_id,
            ack: ack_tx,
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) async fn append_command_act(
        &self,
        thread_id: String,
        seq: u64,
        payload_json: String,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        let tx = self.writer_for(&thread_id)?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(ShardCommand::AppendCommandAct {
            thread_id,
            seq,
            payload_json,
            journal_seq,
            ack: ack_tx,
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) async fn append_workspace_effect(
        &self,
        effect_id: i64,
        effect: NewOwnedEffect,
        journal_seq: Option<u64>,
    ) -> Result<i64, PersistenceError> {
        let tx = self.writer_for(&effect.thread_id)?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(ShardCommand::AppendWorkspaceEffect {
            effect_id,
            effect: Box::new(effect),
            journal_seq,
            ack: ack_tx,
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) async fn append_completed_workspace_effect(
        &self,
        effect_id: i64,
        effect: NewOwnedEffect,
        completed_at_ms: u64,
        journal_seq: Option<u64>,
    ) -> Result<i64, PersistenceError> {
        let tx = self.writer_for(&effect.thread_id)?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(ShardCommand::AppendCompletedWorkspaceEffect {
            effect_id,
            effect: Box::new(effect),
            completed_at_ms,
            journal_seq,
            ack: ack_tx,
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) async fn complete_workspace_effect(
        &self,
        thread_id: &str,
        effect_id: i64,
        after_hash: Option<String>,
        completed_at_ms: u64,
    ) -> Result<bool, PersistenceError> {
        let tx = self.writer_for(thread_id)?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(ShardCommand::CompleteWorkspaceEffect {
            thread_id: thread_id.to_string(),
            effect_id,
            after_hash,
            completed_at_ms,
            ack: ack_tx,
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) async fn mark_workspace_effect_reversed(
        &self,
        thread_id: &str,
        effect_id: i64,
        reversed_at_ms: u64,
        note: Option<String>,
    ) -> Result<bool, PersistenceError> {
        let tx = self.writer_for(thread_id)?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(ShardCommand::MarkWorkspaceEffectReversed {
            thread_id: thread_id.to_string(),
            effect_id,
            reversed_at_ms,
            note,
            ack: ack_tx,
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) fn replay_workspace_effects_blocking(
        &self,
        thread_id: String,
        effects: Vec<WorkspaceEffect>,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        futures::executor::block_on(async {
            let tx = self.writer_for(&thread_id)?;
            let (ack_tx, ack_rx) = oneshot::channel();
            tx.send(ShardCommand::ReplayWorkspaceEffects {
                thread_id,
                effects,
                journal_seq,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
            ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
        })
    }

    pub(crate) fn list_workspace_effects(
        &self,
        thread_id: &str,
    ) -> Result<Vec<WorkspaceEffect>, PersistenceError> {
        let Some(conn) = self.open_reader_for(thread_id)? else {
            return Ok(Vec::new());
        };
        crate::workspace_effects::list_effects(&conn, thread_id)
    }

    pub(crate) fn undoable_workspace_effects(
        &self,
        thread_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkspaceEffect>, PersistenceError> {
        let Some(conn) = self.open_reader_for(thread_id)? else {
            return Ok(Vec::new());
        };
        crate::workspace_effects::undoable_effects(&conn, thread_id, limit)
    }

    pub(crate) async fn expire_preimages(
        &self,
        older_than_ms: u64,
        now_ms: u64,
    ) -> Result<usize, PersistenceError> {
        let mut total = 0usize;
        for idx in 0..self.shard_count {
            if !shard_path(&self.shards_dir, idx).exists() {
                continue;
            }
            let tx = self.writer_for_index(idx)?;
            let (ack_tx, ack_rx) = oneshot::channel();
            tx.send(ShardCommand::ExpirePreimages {
                older_than_ms,
                now_ms,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
            total = total.saturating_add(ack_rx.await.map_err(|_| PersistenceError::WriterDown)??);
        }
        Ok(total)
    }

    pub(crate) fn expire_preimages_blocking(
        &self,
        older_than_ms: u64,
        now_ms: u64,
    ) -> Result<usize, PersistenceError> {
        futures::executor::block_on(self.expire_preimages(older_than_ms, now_ms))
    }

    /// True if at least one content shard file currently exists on disk.
    pub(crate) fn has_any_shard(&self) -> bool {
        (0..self.shard_count).any(|idx| shard_path(&self.shards_dir, idx).exists())
    }

    /// Union of live blob hashes across every existing content shard.
    ///
    /// **Fail-closed (ADR-0255)**: if a shard file exists on disk but fails
    /// ledger/schema verification, returns `Err` rather than silently omitting
    /// its blob references (an incomplete live set would let CAS GC delete
    /// authoritative blobs still referenced by journals).
    pub(crate) fn live_blob_hashes(&self) -> Result<HashSet<String>> {
        let mut live = HashSet::new();
        for idx in 0..self.shard_count {
            let path = shard_path(&self.shards_dir, idx);
            if !path.exists() {
                continue;
            }
            let Some(conn) = open_shard_reader_conn(&path)? else {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "content shard {} failed schema verification; refusing incomplete blob live set",
                    path.display()
                )));
            };
            live.extend(crate::db::garbage_collection::live_blob_hashes(&conn)?);
        }
        Ok(live)
    }

    /// Global FTS5 fan-out across all existing content shards, joining catalog
    /// metadata (`workspace_root`, `title`) **only for matched thread ids**
    /// (O(hits), never loading the entire catalog table into memory) and
    /// merging by BM25 score (ADR-0242 `[INV-INDEX-04]`, ADR-0255).
    pub(crate) fn search_history_fanout(
        &self,
        catalog_conn: &Connection,
        query: &str,
        workspace: Option<&Path>,
        limit: usize,
        match_any: bool,
    ) -> Result<Vec<HistorySearchResult>> {
        let clean_query = sanitize_fts_query_joined(query, match_any);
        if clean_query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let per_shard_limit = (limit.saturating_mul(4)).max(32) as i64;
        let mut raw_hits: Vec<(String, String, String, String, f64)> = Vec::new();
        let mut matched_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for idx in 0..self.shard_count {
            let Some(conn) = open_shard_reader_conn(&shard_path(&self.shards_dir, idx))? else {
                continue;
            };
            let mut stmt = conn.prepare_cached(
                "SELECT fact_id, thread_id, role, \
                        snippet(fts_thread_facts, 3, '<b>', '</b>', '...', 16) AS snippet, \
                        bm25(fts_thread_facts) AS score \
                 FROM fts_thread_facts \
                 WHERE fts_thread_facts MATCH ?1 \
                 ORDER BY score ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![clean_query, per_shard_limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })?;
            for row in rows {
                let hit = row?;
                matched_ids.insert(hit.1.clone());
                raw_hits.push(hit);
            }
        }

        if raw_hits.is_empty() {
            return Ok(Vec::new());
        }

        // Targeted O(matched_ids) catalog lookup rather than scanning the
        // entire `thread_list_view` table on every search query.
        let mut meta: std::collections::HashMap<String, (Option<String>, Option<String>)> =
            std::collections::HashMap::with_capacity(matched_ids.len());
        let workspace_filter = workspace.map(|p| p.to_string_lossy().into_owned());
        let mut stmt = catalog_conn
            .prepare_cached("SELECT workspace_root, title FROM thread_list_view WHERE id = ?1")?;
        for id in matched_ids {
            let row: Option<(Option<String>, Option<String>)> = stmt
                .query_row(params![id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            let Some((ws, title)) = row else {
                continue;
            };
            if let Some(expected_ws) = workspace_filter.as_deref()
                && ws.as_deref() != Some(expected_ws)
            {
                continue;
            }
            meta.insert(id, (ws, title));
        }

        let mut results = Vec::new();
        for (entry_id, session_id, role, snippet, score) in raw_hits {
            if let Some((workspace_root, session_title)) = meta.get(&session_id) {
                results.push(HistorySearchResult {
                    entry_id,
                    session_id,
                    workspace_root: workspace_root.clone(),
                    session_title: session_title.clone(),
                    role,
                    snippet,
                    score,
                });
            }
        }

        results.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.session_id.cmp(&b.session_id))
                .then_with(|| a.entry_id.cmp(&b.entry_id))
        });
        results.truncate(limit);
        Ok(results)
    }
}

#[cfg(test)]
mod tests;
