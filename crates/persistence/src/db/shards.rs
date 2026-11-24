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

const CONTENT_SHARD_SCHEMA_VERSION: u32 = 1;

const CONTENT_SHARD_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS thread_branches (
    thread_id                  TEXT NOT NULL,
    branch_id                  TEXT NOT NULL,
    name                       TEXT NOT NULL,
    head_fact_id               TEXT,
    forked_from_fact_id        TEXT,
    compaction_horizon_fact_id TEXT,
    policy_json                TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(policy_json)),
    created_at_ms              INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms              INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (thread_id, branch_id)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_thread_branches_thread ON thread_branches(thread_id);

CREATE TABLE IF NOT EXISTS causal_nodes (
    id           TEXT NOT NULL,
    thread_id    TEXT NOT NULL,
    parent_id    TEXT,
    seq          INTEGER NOT NULL CHECK (seq > 0),
    kind         TEXT NOT NULL CHECK (kind IN ('dialogue','compaction','termination','system_notice','observation')),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    timestamp_ms INTEGER NOT NULL CHECK (timestamp_ms >= 0),
    context_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(context_json)),
    PRIMARY KEY (thread_id, id),
    UNIQUE (thread_id, seq)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_causal_nodes_thread_seq ON causal_nodes(thread_id, seq ASC);
CREATE INDEX IF NOT EXISTS idx_causal_nodes_parent ON causal_nodes(thread_id, parent_id);

CREATE TRIGGER IF NOT EXISTS trg_causal_nodes_immutable
BEFORE UPDATE ON causal_nodes
WHEN NEW.id <> OLD.id
  OR NEW.thread_id <> OLD.thread_id
  OR NEW.parent_id IS NOT OLD.parent_id
  OR NEW.seq <> OLD.seq
  OR NEW.kind <> OLD.kind
  OR NEW.payload_json <> OLD.payload_json
  OR NEW.timestamp_ms <> OLD.timestamp_ms
  OR NEW.context_json <> OLD.context_json
BEGIN
    SELECT RAISE(ABORT, 'thread facts are immutable');
END;

CREATE TABLE IF NOT EXISTS thread_blob_refs (
    thread_id TEXT NOT NULL,
    fact_id   TEXT NOT NULL,
    hash      TEXT NOT NULL,
    size      INTEGER NOT NULL CHECK (size >= 0),
    mime      TEXT NOT NULL,
    preview   TEXT,
    trust     TEXT NOT NULL DEFAULT 'trusted'
              CHECK (trust IN ('trusted','untrusted')),
    PRIMARY KEY (thread_id, fact_id)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_thread_blob_refs_hash ON thread_blob_refs(hash);

CREATE TABLE IF NOT EXISTS command_acts (
    thread_id    TEXT NOT NULL,
    seq          INTEGER NOT NULL CHECK (seq >= 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    PRIMARY KEY (thread_id, seq)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_command_acts_thread ON command_acts(thread_id, seq ASC);

CREATE TABLE IF NOT EXISTS workspace_effects (
    id                  INTEGER PRIMARY KEY,
    thread_id           TEXT NOT NULL,
    round_id            TEXT NOT NULL,
    turn_id             TEXT NOT NULL,
    call_id             TEXT NOT NULL,
    seq                 INTEGER NOT NULL,
    tool_name           TEXT NOT NULL,
    path                TEXT,
    op                  TEXT NOT NULL CHECK (op IN ('create','edit','delete')),
    effect_class        TEXT NOT NULL
                        CHECK (effect_class IN ('none','file_bytes','external_effect','process_effect','opaque')),
    before_hash         TEXT,
    after_hash          TEXT,
    irreversible_reason TEXT,
    completed_at_ms     INTEGER,
    reversed_at_ms      INTEGER,
    reversal_note       TEXT,
    expired_at_ms       INTEGER,
    created_at_ms       INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS idx_workspace_effects_thread_seq ON workspace_effects(thread_id, seq);
CREATE INDEX IF NOT EXISTS idx_workspace_effects_path ON workspace_effects(thread_id, path);
CREATE INDEX IF NOT EXISTS idx_workspace_effects_undoable
    ON workspace_effects(thread_id, id DESC)
    WHERE effect_class = 'file_bytes' AND reversed_at_ms IS NULL;

CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_no_delete
BEFORE DELETE ON workspace_effects
BEGIN
    SELECT RAISE(ABORT, 'workspace journal rows are append-only; prune by retention policy, not delete (INV-EFFECT-01)');
END;

CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_immutable
BEFORE UPDATE ON workspace_effects
WHEN NEW.id <> OLD.id
  OR NEW.thread_id <> OLD.thread_id
  OR NEW.round_id <> OLD.round_id
  OR NEW.turn_id <> OLD.turn_id
  OR NEW.call_id <> OLD.call_id
  OR NEW.seq <> OLD.seq
  OR NEW.tool_name <> OLD.tool_name
  OR NEW.path IS NOT OLD.path
  OR NEW.op <> OLD.op
  OR NEW.effect_class <> OLD.effect_class
  OR NEW.before_hash IS NOT OLD.before_hash
  OR NEW.irreversible_reason IS NOT OLD.irreversible_reason
  OR NEW.created_at_ms <> OLD.created_at_ms
BEGIN
    SELECT RAISE(ABORT, 'workspace journal observations are write-once (INV-EFFECT-01)');
END;

CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_after_hash_monotonic
BEFORE UPDATE OF after_hash ON workspace_effects
WHEN OLD.after_hash IS NOT NULL
  AND NEW.after_hash IS NOT OLD.after_hash
BEGIN
    SELECT RAISE(ABORT, 'workspace journal post-image is write-once (INV-EFFECT-03)');
END;

CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_completion_monotonic
BEFORE UPDATE OF completed_at_ms ON workspace_effects
WHEN OLD.completed_at_ms IS NOT NULL
  AND NEW.completed_at_ms IS NOT OLD.completed_at_ms
BEGIN
    SELECT RAISE(ABORT, 'workspace journal completion is write-once (INV-EFFECT-03)');
END;

CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_reversal_monotonic
BEFORE UPDATE OF reversed_at_ms ON workspace_effects
WHEN OLD.reversed_at_ms IS NOT NULL
  AND NEW.reversed_at_ms IS NOT OLD.reversed_at_ms
BEGIN
    SELECT RAISE(ABORT, 'workspace journal reversal is write-once (INV-EFFECT-02)');
END;

CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_expiry_monotonic
BEFORE UPDATE OF expired_at_ms ON workspace_effects
WHEN OLD.expired_at_ms IS NOT NULL
  AND NEW.expired_at_ms IS NOT OLD.expired_at_ms
BEGIN
    SELECT RAISE(ABORT, 'workspace journal expiry is write-once (INV-EFFECT-05)');
END;

CREATE VIRTUAL TABLE IF NOT EXISTS fts_thread_facts USING fts5(
    fact_id UNINDEXED,
    thread_id UNINDEXED,
    role UNINDEXED,
    content
);

CREATE TABLE IF NOT EXISTS projection_watermarks (
    thread_id     TEXT PRIMARY KEY,
    seq           INTEGER NOT NULL CHECK (seq >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
) STRICT;
"#;

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
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.execute_batch(CONTENT_SHARD_SCHEMA)?;
    conn.pragma_update(None, "user_version", CONTENT_SHARD_SCHEMA_VERSION)?;
    Ok(conn)
}

fn open_shard_reader_conn(path: &Path) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(Some(conn))
}

enum ShardCommand {
    SaveContent {
        thread_id: String,
        thread: Option<Box<protocol::Thread>>,
        replace: bool,
        commands: Option<Vec<protocol::CommandRecord>>,
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
        effect_id: i64,
        after_hash: Option<String>,
        completed_at_ms: u64,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    MarkWorkspaceEffectReversed {
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
        let conn = open_shard_writer_conn(&path)?;
        let (tx, mut rx) = mpsc::channel::<ShardCommand>(256);
        std::thread::Builder::new()
            .name(format!("nuo-content-shard-{index:02}"))
            .spawn(move || {
                while let Some(cmd) = rx.blocking_recv() {
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
                            || !thread.state.branches.is_empty())
                    {
                        crate::db::thread_store::persist_content_facts(conn, thread, replace)
                            .map_err(|e| PersistenceError::Encode(e.to_string()))?;
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
            effect_id,
            after_hash,
            completed_at_ms,
            ack,
        } => {
            let res = crate::workspace_effects::complete_effect(
                conn,
                effect_id,
                after_hash.as_deref(),
                completed_at_ms,
            );
            let _ = ack.send(res);
        }
        ShardCommand::MarkWorkspaceEffectReversed {
            effect_id,
            reversed_at_ms,
            note,
            ack,
        } => {
            let res = crate::workspace_effects::mark_reversed(
                conn,
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
        thread: Option<protocol::Thread>,
        replace: bool,
        commands: Option<Vec<protocol::CommandRecord>>,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        let tx = self.writer_for(&thread_id)?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(ShardCommand::SaveContent {
            thread_id,
            thread: thread.map(Box::new),
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
        thread: Option<protocol::Thread>,
        replace: bool,
        commands: Option<Vec<protocol::CommandRecord>>,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        futures::executor::block_on(self.save_content(
            thread_id,
            thread,
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

    /// Union of live blob hashes across every existing content shard.
    pub(crate) fn live_blob_hashes(&self) -> Result<HashSet<String>> {
        let mut live = HashSet::new();
        for idx in 0..self.shard_count {
            let Some(conn) = open_shard_reader_conn(&shard_path(&self.shards_dir, idx))? else {
                continue;
            };
            live.extend(crate::db::garbage_collection::live_blob_hashes(&conn)?);
        }
        Ok(live)
    }

    /// Global FTS5 fan-out across all existing content shards, joined with
    /// catalog metadata (`workspace_root`, `title`) and merged by BM25 score
    /// (ADR-0242 `[INV-INDEX-04]`).
    pub(crate) fn search_history_fanout(
        &self,
        catalog_conn: &Connection,
        query: &str,
        filter: Option<&protocol::WorkspaceFilter>,
        limit: usize,
        match_any: bool,
    ) -> Result<Vec<HistorySearchResult>> {
        let clean_query = sanitize_fts_query_joined(query, match_any);
        if clean_query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        // Preload eligible catalog threads (id -> (workspace_root, title)).
        let mut meta: std::collections::HashMap<String, (Option<String>, Option<String>)> =
            std::collections::HashMap::new();
        match filter {
            Some(protocol::WorkspaceFilter::Path(path)) => {
                let mut stmt = catalog_conn.prepare_cached(
                    "SELECT id, workspace_root, title FROM thread_list_view WHERE workspace_root = ?1",
                )?;
                let rows = stmt.query_map(params![path.to_string_lossy()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
                for row in rows {
                    let (id, ws, title) = row?;
                    meta.insert(id, (ws, title));
                }
            }
            Some(protocol::WorkspaceFilter::Unbound) => {
                let mut stmt = catalog_conn.prepare_cached(
                    "SELECT id, workspace_root, title FROM thread_list_view \
                     WHERE workspace_root IS NULL OR workspace_root = ''",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
                for row in rows {
                    let (id, ws, title) = row?;
                    meta.insert(id, (ws, title));
                }
            }
            _ => {
                let mut stmt = catalog_conn
                    .prepare_cached("SELECT id, workspace_root, title FROM thread_list_view")?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
                for row in rows {
                    let (id, ws, title) = row?;
                    meta.insert(id, (ws, title));
                }
            }
        }
        if meta.is_empty() {
            return Ok(Vec::new());
        }

        let per_shard_limit = (limit.saturating_mul(4)).max(32) as i64;
        let mut results = Vec::new();
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
                let (entry_id, session_id, role, snippet, score) = row?;
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
