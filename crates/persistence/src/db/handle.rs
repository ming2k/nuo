//! `PersistenceHandle`: the public front door and every typed verb (0231).
//! The struct and its observability types live in `db.rs`.
use super::actor::{SupervisorContext, run_supervisor};
use super::*;

/// Summary of unified storage maintenance operations (ADR-0090, ADR-0255).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageMaintenanceReport {
    pub reclaimed_blobs: usize,
    pub reclaimed_blob_bytes: u64,
    /// Pre-images whose retention window elapsed this pass. Their journal rows
    /// survive as evidence; only their bytes became reclaimable.
    pub expired_preimages: usize,
    pub created_backup: Option<PathBuf>,
    pub created_role_memory_backup: Option<PathBuf>,
    pub created_journal_backup: Option<PathBuf>,
    pub pruned_backups: usize,
}

impl fmt::Debug for PersistenceHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistenceHandle")
            .field("db_path", &self.db_path)
            .field("health", &self.health.borrow())
            .finish_non_exhaustive()
    }
}

/// Get or initialize the global shared [`PersistenceHandle`].
pub fn get_persistence_handle() -> PersistenceHandle {
    get_or_init_global_persistence(None)
}

/// Initialize the global shared [`PersistenceHandle`] with an optional
/// migration progress observer and enforce [`PersistenceHandle::ensure_ready`]
/// (ADR-0191, ADR-0266 `[INV-SRVPHASE-06]`).
///
/// Server startup MUST use this entry point so database open or migration
/// failures fail fast into `ServerPhase::Failed` rather than advancing to
/// `ServerPhase::Ready`.
pub fn init_global_persistence(
    observer: Option<MigrationObserver>,
) -> std::result::Result<PersistenceHandle, String> {
    let handle = get_or_init_global_persistence(observer);
    handle.ensure_ready()?;
    Ok(handle)
}

fn get_or_init_global_persistence(observer: Option<MigrationObserver>) -> PersistenceHandle {
    let dirs = host::paths::get();
    let db_path = dirs.db_file();
    let mut guard = GLOBAL_HANDLE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = guard.as_ref()
        && handle.db_path == db_path
        && handle.startup_error.is_none()
    {
        return handle.clone();
    }
    let blobs = Some(BlobStore::new(dirs.blobs_dir()));
    let handle = PersistenceHandle::spawn_with_observer(db_path, blobs, observer);
    if handle.startup_error.is_none() {
        *guard = Some(handle.clone());
    }
    handle
}

fn prepare_checked_out_reader(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN DEFERRED")?;
    // Reading `user_version` inside `BEGIN DEFERRED` both acquires the WAL
    // read snapshot (surfacing any unlinked `-wal`/`-shm` wedge immediately)
    // and enforces schema compatibility.
    let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != CURRENT_DB_VERSION {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "reader schema mismatch: {version}"
        )));
    }
    Ok(())
}

impl PersistenceHandle {
    /// Start the supervised persistence actor.
    ///
    /// The supervisor runs as a task on the current Tokio runtime when one
    /// is active, otherwise on a dedicated single-thread runtime of its own,
    /// so construction stays valid from sync contexts (library callers,
    /// tests).
    #[allow(clippy::expect_used)]
    pub fn spawn(db_path: PathBuf, blob_store: Option<BlobStore>) -> Self {
        Self::spawn_with_observer(db_path, blob_store, None)
    }

    /// Start the supervised persistence actor, emitting [`MigrationProgress`]
    /// transitions to `observer` whenever pending migrations execute (ADR-0266).
    #[allow(clippy::expect_used)]
    pub fn spawn_with_observer(
        db_path: PathBuf,
        blob_store: Option<BlobStore>,
        observer: Option<MigrationObserver>,
    ) -> Self {
        let identity = database_identity(&db_path);
        let db_path = identity.as_ref().cloned().unwrap_or(db_path);
        let mut registry = OWNERS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        registry.retain(|_, entry| entry.lease.strong_count() > 0);
        if let Some(owner) = registry.get(&db_path) {
            if let Some(supervisor) = owner.supervisor.upgrade() {
                return Self {
                    supervisor,
                    health: owner.health.clone(),
                    db_path,
                    shards: owner.shards.clone(),
                    blob_store: owner.blob_store.clone(),
                    startup_error: None,
                    readers: owner.readers.clone(),
                    reader_pool: owner.reader_pool.clone(),
                    reader_ages: owner.reader_ages.clone(),
                    reader_open_failures: owner.reader_open_failures.clone(),
                    rebuild_requested: owner.rebuild_requested.clone(),
                };
            }
            // A previous local owner is draining accepted commands. Do not
            // race its last transaction or release its lease prematurely.
            let started = Instant::now();
            while owner.lease.strong_count() > 0 && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let (supervisor, front_rx) = mpsc::channel::<PersistenceCommand>(256);
        let (health_tx, health_rx) = watch::channel(WriterHealth::Healthy);
        let path = db_path.clone();
        // Initialization owns its thread: no Tokio worker is needed to make
        // progress while a synchronous constructor waits for readiness.
        let opened = std::thread::spawn(move || {
            identity?;
            let lease = Arc::new(host::lock::ProcessLock::acquire_with_timeout(
                &path.with_extension("db.owner.lock"),
                Duration::from_millis(2000),
            )?);
            let engine = DatabaseEngine::open_with_observer(&path, observer.as_ref())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((engine, lease))
        })
        .join()
        .unwrap_or_else(|_| Err("database initialization panicked".into()));
        let shards = crate::db::shards::ContentShardPool::new(
            crate::db::shards::shards_dir_for_catalog(&db_path),
            crate::db::shards::DEFAULT_SHARD_COUNT,
        );
        let readers = Arc::new(tokio::sync::Semaphore::new(READER_POOL_CAPACITY));
        let reader_pool = Arc::new(ReaderPool::default());
        let reader_ages = Arc::new(ReaderAges::default());
        let reader_open_failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rebuild_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let startup_error = match opened {
            Ok((engine, lease)) => {
                registry.insert(
                    db_path.clone(),
                    RegisteredOwner {
                        supervisor: supervisor.downgrade(),
                        health: health_rx.clone(),
                        shards: shards.clone(),
                        blob_store: blob_store.clone(),
                        readers: readers.clone(),
                        reader_pool: reader_pool.clone(),
                        reader_ages: reader_ages.clone(),
                        reader_open_failures: reader_open_failures.clone(),
                        rebuild_requested: rebuild_requested.clone(),
                        lease: Arc::downgrade(&lease),
                    },
                );
                let run = run_supervisor(
                    front_rx,
                    SupervisorContext {
                        db_path: db_path.clone(),
                        health: health_tx,
                        lease,
                        reader_ages: reader_ages.clone(),
                        reader_pool: reader_pool.clone(),
                    },
                    Some(engine),
                );
                std::thread::Builder::new()
                    .name("nuo-persistence-supervisor".into())
                    .spawn(move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("failed to build persistence supervisor runtime");
                        runtime.block_on(run);
                    })
                    .expect("failed to spawn persistence supervisor thread");
                None
            }
            Err(error) => {
                let _ = health_tx.send(WriterHealth::Down {
                    attempt: 0,
                    since_ms: unix_ms(),
                    error: error.clone(),
                });
                drop(front_rx);
                Some(Arc::from(error))
            }
        };
        Self {
            supervisor,
            health: health_rx,
            db_path,
            shards,
            blob_store,
            startup_error,
            readers,
            reader_pool,
            reader_ages,
            reader_open_failures,
            rebuild_requested,
        }
    }

    /// Startup must succeed before a server admits work.
    pub fn ensure_ready(&self) -> std::result::Result<(), String> {
        match &self.startup_error {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        }
    }

    /// The current writer health snapshot (D4).
    pub fn health(&self) -> WriterHealth {
        self.health.borrow().clone()
    }

    /// Subscribe to writer health transitions (D4): the server
    /// folds these into the monitor stream; frontends render degradation.
    pub fn subscribe_health(&self) -> watch::Receiver<WriterHealth> {
        self.health.clone()
    }

    /// The catalog database file path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub(crate) fn blob_store(&self) -> Option<&BlobStore> {
        self.blob_store.as_ref()
    }

    /// Directory containing the fixed-K rebuildable content shards (ADR-0242).
    pub fn shards_dir(&self) -> &Path {
        self.shards.shards_dir()
    }

    /// Number of content shards in this store's pool.
    pub fn shard_count(&self) -> usize {
        self.shards.shard_count()
    }

    /// Asynchronously save a session across the catalog and its owning content
    /// shard (ADR-0242 `[INV-INDEX-03]`): no cross-shard transaction, and
    /// content writes for different sessions execute in parallel across shards.
    pub(crate) async fn save_session(
        &self,
        data: crate::thread::ThreadData,
        mode: SaveMode,
        usage_upserts: Vec<protocol::RequestUsageRecord>,
        guard: CommitGuard,
        journal_seq: Option<u64>,
    ) -> Result<u64, PersistenceError> {
        let mut prepared = crate::db::thread_session::prepare_save(
            data,
            mode,
            &usage_upserts,
            &guard,
            journal_seq,
            self.blob_store.as_ref(),
        )
        .map_err(SaveError::into_persistence)?;

        // Route content-plane data (facts, branches, blob_refs, FTS, command_acts)
        // to the owning content shard before the catalog commit.
        let thread = prepared.thread.take();
        let extra_blob_refs = std::mem::take(&mut prepared.extra_blob_refs);
        let commands = if mode == SaveMode::FullRewrite {
            Some(prepared.data.commands.clone())
        } else {
            None
        };
        if thread.is_some() || commands.is_some() || !extra_blob_refs.is_empty() {
            let replace = mode == SaveMode::FullRewrite;
            self.shards
                .save_content(
                    prepared.data.id.clone(),
                    thread,
                    extra_blob_refs,
                    replace,
                    commands,
                    journal_seq,
                )
                .await?;
        }

        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::SaveSession {
                prepared: Box::new(prepared),
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Synchronously save a session on a blocking thread (always a full
    /// rewrite: the blocking callers write fresh or rebuilt state).
    pub(crate) fn save_session_blocking(
        &self,
        data: crate::thread::ThreadData,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        let mut prepared = crate::db::thread_session::prepare_save(
            data,
            SaveMode::FullRewrite,
            &[],
            &CommitGuard::default(),
            journal_seq,
            self.blob_store.as_ref(),
        )
        .map_err(SaveError::into_persistence)?;

        let thread = prepared.thread.take();
        let extra_blob_refs = std::mem::take(&mut prepared.extra_blob_refs);
        let commands = Some(prepared.data.commands.clone());
        self.shards.save_content_blocking(
            prepared.data.id.clone(),
            thread,
            extra_blob_refs,
            true,
            commands,
            journal_seq,
        )?;

        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::SaveSession {
                prepared: Box::new(prepared),
                ack: ack_tx,
            },
            ack_rx,
        )
        .map(|_| ())
    }

    /// Asynchronously upsert a session record.
    pub async fn upsert_session(&self, record: ThreadRecord) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::UpsertSession {
                record,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Non-blocking fire-and-forget session upsert to avoid blocking synchronous writers.
    pub fn try_upsert_session(&self, record: ThreadRecord) {
        let (ack_tx, _) = oneshot::channel();
        if let Err(error) = self.supervisor.try_send(PersistenceCommand::UpsertSession {
            record,
            ack: ack_tx,
        }) {
            warn!(error = %error, "dropped fire-and-forget session upsert: writer unavailable");
        }
    }

    /// Asynchronously delete a session record from both catalog and content shard.
    pub async fn delete_session(&self, session_id: String) -> Result<bool, PersistenceError> {
        let _ = self.shards.delete_thread(session_id.clone()).await;
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::DeleteSession {
                session_id,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Asynchronously rename a session.
    pub async fn rename_session(
        &self,
        session_id: String,
        title: Option<String>,
        manual: bool,
    ) -> Result<bool, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::RenameSession {
                session_id,
                title,
                manual,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Append one command act row (`O(1)`, ADR-0249, routed to content shard).
    pub(crate) async fn append_command_act(
        &self,
        thread_id: String,
        seq: u64,
        record: &protocol::CommandRecord,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        let payload_json = serde_json::to_string(record)
            .map_err(|error| PersistenceError::Encode(error.to_string()))?;
        self.shards
            .append_command_act(thread_id, seq, payload_json, journal_seq)
            .await
    }

    /// Replay journal-recovered workspace effects synchronously into the owning
    /// content shard during startup catch-up / shard rebuild (ADR-0249, ADR-0242).
    pub(crate) fn replay_workspace_effects_blocking(
        &self,
        thread_id: String,
        effects: Vec<crate::workspace_effects::WorkspaceEffect>,
        journal_seq: Option<u64>,
    ) -> Result<(), PersistenceError> {
        self.shards
            .replay_workspace_effects_blocking(thread_id, effects, journal_seq)
    }

    /// Append a write-ahead workspace effect, returning its journal id
    /// (ADR-0125, ADR-0249, ADR-0242).
    ///
    /// Journal-first: the pre-image intent is appended and `fdatasync`-ed to
    /// the thread's session journal **before** projecting to the owning content
    /// shard, guaranteeing power-loss durability and full rebuildability.
    pub async fn append_workspace_effect(
        &self,
        effect: crate::workspace_effects::NewOwnedEffect,
    ) -> Result<i64, PersistenceError> {
        let journal = crate::thread::open_journal(&self.db_path, &effect.thread_id);
        let (journal_seq, effect_id) = journal
            .append_workspace_effect_started(&effect)
            .await
            .map_err(PersistenceError::Encode)?;
        self.shards
            .append_workspace_effect(effect_id, effect, Some(journal_seq))
            .await
    }

    /// Append a workspace effect that needs no recovery point, returning its
    /// journal id (ADR-0249: journal-first, routed to content shard).
    pub async fn append_completed_workspace_effect(
        &self,
        effect: crate::workspace_effects::NewOwnedEffect,
        completed_at_ms: u64,
    ) -> Result<i64, PersistenceError> {
        let journal = crate::thread::open_journal(&self.db_path, &effect.thread_id);
        let (journal_seq, effect_id) = journal
            .append_workspace_effect_completed_receipt(&effect, completed_at_ms)
            .await
            .map_err(PersistenceError::Encode)?;
        self.shards
            .append_completed_workspace_effect(
                effect_id,
                effect,
                completed_at_ms,
                Some(journal_seq),
            )
            .await
    }

    /// Complete a write-ahead effect with its post-image (ADR-0249: journal-first).
    /// `Ok(false)` means the row was already completed or does not exist.
    pub async fn complete_workspace_effect(
        &self,
        thread_id: &str,
        effect_id: i64,
        after_hash: Option<String>,
        completed_at_ms: u64,
    ) -> Result<bool, PersistenceError> {
        let journal = crate::thread::open_journal(&self.db_path, thread_id);
        let _ = journal
            .append_workspace_effect_completed(effect_id, after_hash.as_deref(), completed_at_ms)
            .await
            .map_err(PersistenceError::Encode)?;
        self.shards
            .complete_workspace_effect(thread_id, effect_id, after_hash, completed_at_ms)
            .await
    }

    /// Stamp a journal row as reversed (ADR-0249: journal-first).
    /// `Ok(false)` means another caller already reversed it.
    pub async fn mark_workspace_effect_reversed(
        &self,
        thread_id: &str,
        effect_id: i64,
        reversed_at_ms: u64,
        note: Option<String>,
    ) -> Result<bool, PersistenceError> {
        let journal = crate::thread::open_journal(&self.db_path, thread_id);
        let _ = journal
            .append_workspace_effect_reversed(effect_id, reversed_at_ms, note.as_deref())
            .await
            .map_err(PersistenceError::Encode)?;
        self.shards
            .mark_workspace_effect_reversed(thread_id, effect_id, reversed_at_ms, note)
            .await
    }

    /// Read a thread's whole journal in observation order from its content shard.
    ///
    /// Self-heals on demand if the disposable shard is missing/outdated at
    /// runtime while the session's journal exists (ADR-0255).
    pub async fn list_workspace_effects(
        &self,
        thread_id: String,
    ) -> Result<Vec<crate::workspace_effects::WorkspaceEffect>, PersistenceError> {
        let handle = self.clone();
        tokio::task::spawn_blocking(move || {
            if handle
                .shards
                .open_reader_for(&thread_id)
                .ok()
                .flatten()
                .is_none()
            {
                let _ = crate::thread::catch_up_single_session(&handle, &thread_id);
            }
            handle.shards.list_workspace_effects(&thread_id)
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?
    }

    /// Expire pre-images older than `older_than_ms` across all content shards.
    pub async fn expire_preimages(
        &self,
        older_than_ms: u64,
        now_ms: u64,
    ) -> Result<usize, PersistenceError> {
        self.shards.expire_preimages(older_than_ms, now_ms).await
    }

    /// Blocking variant of [`Self::expire_preimages`], for the maintenance pass.
    pub fn expire_preimages_blocking(
        &self,
        older_than_ms: u64,
        now_ms: u64,
    ) -> Result<usize, PersistenceError> {
        self.shards.expire_preimages_blocking(older_than_ms, now_ms)
    }

    /// Read the newest confirmed, not-yet-reversed reversible effects from the
    /// owning content shard, newest first. Self-heals missing shards on demand.
    pub async fn undoable_workspace_effects(
        &self,
        thread_id: String,
        limit: u32,
    ) -> Result<Vec<crate::workspace_effects::WorkspaceEffect>, PersistenceError> {
        let handle = self.clone();
        tokio::task::spawn_blocking(move || {
            if handle
                .shards
                .open_reader_for(&thread_id)
                .ok()
                .flatten()
                .is_none()
            {
                let _ = crate::thread::catch_up_single_session(&handle, &thread_id);
            }
            handle
                .shards
                .undoable_workspace_effects(&thread_id, limit as usize)
        })
        .await
        .map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) fn catch_up_usage(&self) -> Result<usize, PersistenceError> {
        let (ack, rx) = oneshot::channel();
        self.run_blocking(PersistenceCommand::ProjectUsage { ack }, rx)
    }

    pub async fn record_attempts(
        &self,
        entries: Vec<protocol::usage_stats::UsageStatRecord>,
    ) -> Result<(), PersistenceError> {
        let (ack, rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::RecordUsageStats {
                entries,
                ack: Some(ack),
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    pub(crate) fn record_usage_stats_blocking(
        &self,
        entries: Vec<protocol::usage_stats::UsageStatRecord>,
    ) -> Result<(), PersistenceError> {
        let (ack, rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::RecordUsageStats {
                entries,
                ack: Some(ack),
            },
            rx,
        )
    }

    /// Asynchronously set a key-value entry.
    pub async fn set_kv(&self, key: String, value: String) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::SetKV {
                key,
                value,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Synchronously set a key-value entry.
    pub fn set_kv_blocking(&self, key: String, value: String) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::SetKV {
                key,
                value,
                ack: ack_tx,
            },
            ack_rx,
        )
    }

    /// Asynchronously set a JSON-serializable value in the key-value store.
    pub async fn set_json<T: serde::Serialize>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), PersistenceError> {
        let serialized =
            serde_json::to_string(value).map_err(|e| PersistenceError::Encode(e.to_string()))?;
        self.set_kv(key.to_string(), serialized).await
    }

    /// Synchronously set a JSON-serializable value in the key-value store.
    pub fn set_json_blocking<T: serde::Serialize>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), PersistenceError> {
        let serialized =
            serde_json::to_string(value).map_err(|e| PersistenceError::Encode(e.to_string()))?;
        self.set_kv_blocking(key.to_string(), serialized)
    }

    /// Asynchronously delete a key-value entry.
    pub async fn delete_kv(&self, key: String) -> Result<bool, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::DeleteKV { key, ack: ack_tx })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Asynchronously record an input history entry (fire-and-forget).
    pub fn try_record_input_history(&self, entry: protocol::HistoryEntry, dedup: bool) {
        if let Err(error) = self
            .supervisor
            .try_send(PersistenceCommand::RecordInputHistory {
                entry,
                dedup,
                ack: None,
            })
        {
            warn!(error = %error, "dropped fire-and-forget input history entry: writer unavailable");
        }
    }

    /// Record an input history entry, waiting for single-writer SQLite actor confirmation.
    pub fn record_input_history_blocking(
        &self,
        entry: protocol::HistoryEntry,
        dedup: bool,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::RecordInputHistory {
                entry,
                dedup,
                ack: Some(ack_tx),
            },
            ack_rx,
        )
    }

    /// Save multiple input history entries synchronously, waiting for SQLite actor confirmation.
    pub fn save_input_history_blocking(
        &self,
        entries: Vec<protocol::HistoryEntry>,
        dedup: bool,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::SaveInputHistory {
                entries,
                dedup,
                ack: ack_tx,
            },
            ack_rx,
        )
    }

    /// Clear all input history entries from SQLite.
    pub fn clear_input_history_blocking(&self) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::ClearInputHistory { ack: ack_tx },
            ack_rx,
        )
    }

    /// Best-effort asynchronous delete of an input history record by text and timestamp.
    pub fn try_delete_input_history_entry(&self, text: String, created_at_ms: u64) {
        if let Err(error) = self
            .supervisor
            .try_send(PersistenceCommand::DeleteInputHistoryEntry {
                text,
                created_at_ms,
                ack: None,
            })
        {
            warn!(error = %error, "dropped fire-and-forget input history delete: writer unavailable");
        }
    }

    /// Synchronously delete an input history entry by text and timestamp.
    pub fn delete_input_history_entry_blocking(
        &self,
        text: &str,
        created_at_ms: u64,
    ) -> Result<usize, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::DeleteInputHistoryEntry {
                text: text.to_string(),
                created_at_ms,
                ack: Some(ack_tx),
            },
            ack_rx,
        )
    }

    /// Open a reader against this handle's database.
    ///
    /// Check whether this handle encountered single-writer lock contention on startup (INV-PERSIST-LOCK-01).
    pub fn is_lock_contention(&self) -> bool {
        self.startup_error
            .as_deref()
            .map(|e| {
                e.contains("could not acquire advisory lock")
                    || e.contains("timed out waiting for process lock")
                    || e.contains("Resource temporarily unavailable")
            })
            .unwrap_or(false)
    }

    /// This is the **only** way to obtain a connection snapshot: the engine
    /// type is crate-private, so a caller cannot open one on a path of its own
    /// choosing. Reads never need the actor — WAL readers run concurrently
    /// with the writer — so this returns without queueing behind a slow write.
    ///
    /// Capacity is a **bounded wait**, not an instant refusal. With a fixed
    /// pool and a server hosting many sessions, a burst of concurrent reads
    /// (history listing, recall search, offstream enumeration) used to turn
    /// into a hard error at the call site — which most callers mapped to
    /// `.ok()` and therefore to an *empty result*, silently. A short wait
    /// converts that into latency, which is honest, and only a genuinely
    /// saturated pool reports exhaustion.
    pub fn reader(&self) -> Result<DbReader> {
        if let Some(error) = &self.startup_error {
            // When another process already holds the single-writer advisory lock (e.g. background server),
            // this process cannot become the supervisor, but SQLite WAL mode safely allows concurrent read-only queries.
            if !self.is_lock_contention() {
                return Err(rusqlite::Error::InvalidParameterName(error.to_string()));
            }
        }
        let permit = self.acquire_reader_lease(READER_LEASE_WAIT)?;
        // The whole checkout/open sequence is wedge-sensitive: against a
        // missing `-wal`/`-shm` pair, an open succeeds and the first
        // access that attaches the WAL index fails. Every fallible step is
        // therefore inside one scope so any failure counts toward the
        // rebuild threshold (and drops the permit with it).
        let generation = self.reader_pool.current_generation();
        let outcome = (|| {
            let conn = loop {
                if let Some(pooled) = self.reader_pool.try_pop(generation) {
                    if prepare_checked_out_reader(&pooled).is_ok() {
                        break pooled;
                    }
                    // Stale or wedged pooled handle; discard and try another or cold-open.
                    continue;
                }
                let fresh = Connection::open_with_flags(
                    &self.db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                fresh.pragma_update(None, "query_only", true)?;
                fresh.busy_timeout(Duration::from_millis(250))?;
                fresh.pragma_update(None, "cache_size", -8192)?; // 8 MiB page cache per reader
                let _ = fresh.pragma_update(None, "mmap_size", 268_435_456_i64);
                fresh.set_prepared_statement_cache_capacity(64);
                prepare_checked_out_reader(&fresh)?;
                break fresh;
            };
            // Register only after every fallible step: an error must not leak a
            // registration whose guard was never constructed.
            let reader_ages = self.reader_ages.clone();
            let age = ReaderAgeGuard {
                id: reader_ages.register(),
                reader_ages,
            };
            let repair_handle = self.clone();
            let shard_repair: ShardRepairFn = Arc::new(move |thread_id: &str| {
                crate::thread::catch_up_single_session(&repair_handle, thread_id).unwrap_or(false)
            });
            let full_repair_handle = self.clone();
            let full_shard_repair: FullShardRepairFn = Arc::new(move || {
                crate::thread::catch_up_projections_for(&full_repair_handle).unwrap_or(0)
            });
            Ok(DbReader {
                conn: Some(conn),
                db_path: self.db_path.clone(),
                shards: self.shards.clone(),
                blob_store: self.blob_store.clone(),
                shard_repair: Some(shard_repair),
                full_shard_repair: Some(full_shard_repair),
                pool: Some(self.reader_pool.clone()),
                generation,
                _permit: Some(permit),
                _age: Some(age),
            })
        })();
        match outcome {
            Ok(reader) => {
                // A successful open ends any wedge episode the failures were
                // counting.
                self.reader_open_failures
                    .store(0, std::sync::atomic::Ordering::SeqCst);
                self.rebuild_requested
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                Ok(reader)
            }
            Err(error) => {
                self.note_reader_open_failure(&error);
                Err(error)
            }
        }
    }

    /// Take one reader lease, waiting at most `budget`.
    ///
    /// Deliberately a bounded poll rather than an async wait: this API is
    /// synchronous and is called from async workers, blocking-pool threads, and
    /// plain threads alike. Polling a semaphore is correct in all three without
    /// depending on a runtime being present or on `block_on` being legal in the
    /// current context. The sleep is short enough that a lease handed back by a
    /// finishing read is picked up promptly, and long enough not to spin a core.
    fn acquire_reader_lease(&self, budget: Duration) -> Result<tokio::sync::OwnedSemaphorePermit> {
        let deadline = std::time::Instant::now() + budget;
        loop {
            match self.readers.clone().try_acquire_owned() {
                Ok(permit) => return Ok(permit),
                Err(tokio::sync::TryAcquireError::Closed) => {
                    return Err(rusqlite::Error::InvalidParameterName(
                        "database reader pool closed".into(),
                    ));
                }
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Err(rusqlite::Error::InvalidParameterName(format!(
                            "database reader capacity exhausted after {}ms",
                            budget.as_millis()
                        )));
                    }
                    std::thread::sleep((deadline - now).min(Duration::from_millis(2)));
                }
            }
        }
    }

    /// Execute a scoped read closure against a fresh [`DbReader`].
    ///
    /// Guarantees that the reader permit and snapshot transaction are dropped
    /// immediately upon closure return, preventing long-running read transactions
    /// from accidentally pinning the WAL log across async suspensions.
    pub fn with_reader<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&DbReader) -> Result<T>,
    {
        let reader = self.reader()?;
        f(&reader)
    }

    /// Count one failed fresh-reader open and, past the bounded threshold,
    /// ask the supervisor to rebuild the engine.
    ///
    /// The wedge this heals: the on-disk `-wal`/`-shm` pair removed under a
    /// live writer. The writer keeps serving into the unlinked inode, but
    /// every *new* read-only open fails, and nothing else in the system can
    /// detect that state — the writer health stays `Healthy`. A rebuild is
    /// the only cure, and its clean engine close is safe: SQLite checkpoints
    /// the still-valid deleted WAL back into the main database before the
    /// fresh generation opens a new `-wal`/`-shm`.
    ///
    /// Bounded by construction: the counter only advances on open failures,
    /// a CAS latch admits exactly one rebuild request per wedge episode, and
    /// the next successful open resets both. Non-owner handles (another
    /// process sharing the WAL) never request a rebuild — there is no engine
    /// here to rebuild, and the owner process runs its own detector.
    fn note_reader_open_failure(&self, error: &rusqlite::Error) {
        if self.startup_error.is_some() {
            return;
        }
        let failures = self
            .reader_open_failures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if failures < READER_REBUILD_FAILURE_THRESHOLD {
            return;
        }
        if self
            .rebuild_requested
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return; // a rebuild is already in flight for this episode.
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        match self
            .supervisor
            .try_send(PersistenceCommand::RebuildEngine { ack: ack_tx })
        {
            Ok(()) => {
                warn!(
                    db_path = %self.db_path.display(),
                    failures,
                    error = %error,
                    "fresh reader opens keep failing; requesting persistence engine rebuild"
                );
                // Fire-and-forget: `reader()` is synchronous and must not
                // block on the rebuild. The ack is dropped — the supervisor
                // performs the rebuild regardless, and the next reader open
                // (this caller's retry, or the next security snapshot) is
                // the success probe that clears the latch.
                drop(ack_rx);
            }
            Err(send_error) => {
                // Supervisor gone or front channel saturated: clear the
                // latch so a later failure can retry the request instead
                // of latching a wedge forever.
                self.rebuild_requested
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                warn!(
                    db_path = %self.db_path.display(),
                    error = %send_error,
                    "could not deliver the engine rebuild request"
                );
            }
        }
    }

    /// Create a rolling online hot backup with automatic retention pruning
    /// (ADR-0090).
    ///
    /// The snapshot runs on its own read-only connection, never on the writer:
    /// `VACUUM INTO` is a whole-database read, and serialising it behind the
    /// single writer would stop every session's projection for its duration.
    pub fn create_rolling_backup_blocking(
        &self,
        backups_dir: PathBuf,
        retention_limit: usize,
    ) -> Result<super::backup::BackupMetadata, String> {
        let manager = super::backup::BackupManager::new(backups_dir);
        manager.create_rolling_backup(&self.db_path, retention_limit)
    }

    /// A storage-observability snapshot (D7): WAL size, active reader
    /// count, and the oldest reader snapshot's age. A large WAL or an old
    /// reader is the pressure that blocks checkpoint reclamation.
    pub fn storage_metrics(&self) -> StorageMetrics {
        let size = |path: &Path| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let wal_path = PathBuf::from(format!("{}-wal", self.db_path.display()));
        StorageMetrics {
            main_bytes: size(&self.db_path),
            wal_bytes: size(&wal_path),
            active_readers: self.reader_ages.active(),
            reader_capacity: READER_POOL_CAPACITY,
            oldest_reader_ms: self
                .reader_ages
                .oldest_age()
                .map(|age| age.as_millis() as u64),
        }
    }

    /// Synchronously delete a key-value entry on a blocking thread.
    pub fn delete_kv_blocking(&self, key: String) -> Result<bool, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(PersistenceCommand::DeleteKV { key, ack: ack_tx }, ack_rx)
    }

    /// Refresh SQLite's query-planner statistics (`PRAGMA optimize`).
    ///
    /// Runs on its own write connection, not through the writer queue:
    /// `optimize` may run `ANALYZE` over the whole schema, which is cold,
    /// periodic work and must not queue ahead of a session projection.
    pub fn optimize_blocking(&self) -> Result<(), PersistenceError> {
        let conn = rusqlite::Connection::open(&self.db_path).map_err(PersistenceError::Engine)?;
        conn.busy_timeout(Duration::from_millis(250))
            .map_err(PersistenceError::Engine)?;
        conn.execute_batch("PRAGMA optimize;")
            .map_err(PersistenceError::Engine)
    }

    /// Synchronously load all persisted permission rules for `target`.
    pub fn load_permission_rules_blocking(
        &self,
        target: &crate::db::permissions::PermissionScopeTarget,
    ) -> Result<Vec<crate::db::permissions::StoredPermissionRuleRow>, PersistenceError> {
        self.with_reader(|reader| reader.load_permission_rules(target))
            .map_err(PersistenceError::Engine)
    }

    /// Synchronously upsert one `allow` or `revoked` permission rule.
    pub fn upsert_permission_rule_blocking(
        &self,
        target: crate::db::permissions::PermissionScopeTarget,
        tool: String,
        scope: String,
        effect: crate::db::permissions::PermissionRuleEffect,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let now_ms = unix_ms() as i64;
        self.run_blocking(
            PersistenceCommand::UpsertPermissionRule {
                target,
                tool,
                scope,
                effect,
                now_ms,
                ack: ack_tx,
            },
            ack_rx,
        )
    }

    /// Fire-and-forget update of `last_used_ms` on an allowed permission rule.
    pub fn try_touch_permission_rule_last_used(
        &self,
        target: crate::db::permissions::PermissionScopeTarget,
        tool: String,
        scope: String,
    ) {
        let now_ms = unix_ms() as i64;
        if let Err(error) =
            self.supervisor
                .try_send(PersistenceCommand::TouchPermissionRuleLastUsed {
                    target,
                    tool,
                    scope,
                    now_ms,
                })
        {
            warn!(error = %error, "dropped fire-and-forget permission last_used_ms update");
        }
    }

    /// Synchronously clear all `allow` rules in `target` and persist `revoked` markers.
    pub fn clear_permission_rules_blocking(
        &self,
        target: crate::db::permissions::PermissionScopeTarget,
        active_rules: Vec<(String, String)>,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let now_ms = unix_ms() as i64;
        self.run_blocking(
            PersistenceCommand::ClearPermissionRules {
                target,
                active_rules,
                now_ms,
                ack: ack_tx,
            },
            ack_rx,
        )
    }

    /// Unified storage maintenance (ADR-0090, ADR-0255):
    /// 1. Catches up all derived projections from authoritative journals
    ///    (fail-closed prerequisite for CAS GC).
    /// 2. Expires workspace pre-images past the retention window.
    /// 3. Sweeps unreferenced CAS blobs older than `DEFAULT_BLOB_GC_GRACE`
    ///    ONLY when all journals are caught up and all shards verify cleanly.
    /// 4. Generates rolling backups for authoritative stores (`nuo.db`,
    ///    `role_memory.db`, and `journal/`) and records exact pruned counts.
    /// 5. Optimizes SQLite internal query-planner statistics.
    pub fn perform_storage_maintenance_blocking(
        &self,
        retention_limit: usize,
    ) -> Result<StorageMaintenanceReport, PersistenceError> {
        let mut report = StorageMaintenanceReport::default();

        // 1. Fail-closed catch-up before GC: ensure derived content shards
        //    reflect every authoritative journal before computing live blobs
        //    (ADR-0255). A lagging or missing shard must never cause GC to
        //    delete blobs still referenced by journals.
        let projections_caught_up = match crate::thread::catch_up_projections_for(self) {
            Ok(_) => match crate::thread::all_journals_caught_up(self) {
                Ok(true) => true,
                Ok(false) => {
                    tracing::warn!(
                        "skipping CAS blob GC during maintenance: one or more session journals are still ahead of derived projections"
                    );
                    false
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "skipping CAS blob GC during maintenance: could not verify journal catch-up state"
                    );
                    false
                }
            },
            Err(error) => {
                tracing::warn!(
                    %error,
                    "skipping CAS blob GC during maintenance: projection catch-up failed"
                );
                false
            }
        };

        // 2. Expire pre-images past the retention window, *then* sweep the CAS.
        //
        // Order is load-bearing: the sweep's live set is derived from the journal,
        // so expiring first is what makes an aged-out pre-image actually
        // reclaimable. Sweeping first would keep every pre-image alive forever,
        // because nothing would ever remove it from the live set.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let window_ms = crate::workspace_effects::DEFAULT_PREIMAGE_RETENTION_DAYS
            .saturating_mul(24 * 60 * 60 * 1000);
        match self.expire_preimages_blocking(now_ms.saturating_sub(window_ms), now_ms) {
            Ok(expired) => report.expired_preimages = expired,
            Err(error) => {
                tracing::warn!(%error, "pre-image retention sweep failed; keeping all pre-images");
            }
        }

        // 3. Fail-closed CAS blob sweep against reader snapshot with an mtime
        //    grace window so freshly written blobs in `prepare_save` cannot
        //    race with GC (ADR-0255).
        if projections_caught_up && let Some(ref blob_store) = self.blob_store {
            let (blob_count, blob_bytes) = match self.reader().and_then(|r| r.live_blob_hashes()) {
                Ok(live) => {
                    blob_store.retain_only_with_grace(&live, crate::blobs::DEFAULT_BLOB_GC_GRACE)
                }
                Err(error) => {
                    tracing::warn!(%error, "blob garbage collection failed during maintenance; skipping");
                    (0, 0)
                }
            };
            report.reclaimed_blobs = blob_count;
            report.reclaimed_blob_bytes = blob_bytes;
        }

        // 4. Online hot rolling backups for all authoritative local stores
        //    (ADR-0255 `[INV-SHARD-03]`): `nuo.db`, `role_memory.db`, and a
        //    point-in-time rolling snapshot of `journal/`.
        let backup_dir = self.db_path.with_extension("backups");
        let backup_mgr = super::backup::BackupManager::new(backup_dir);
        match backup_mgr.create_rolling_backup_with_pruned(&self.db_path, retention_limit) {
            Ok((meta, pruned)) => {
                report.created_backup = Some(meta.path);
                report.pruned_backups = report.pruned_backups.saturating_add(pruned);
            }
            Err(error) => {
                tracing::warn!(%error, "rolling hot backup of nuo.db failed during maintenance");
            }
        }

        let role_memory_path = self
            .db_path
            .parent()
            .map(|parent| parent.join("role_memory.db"))
            .filter(|path| path.exists())
            .or_else(|| {
                let default_db = host::paths::get().db_file();
                if self.db_path == default_db {
                    let rm = host::paths::get().role_memory_db();
                    rm.exists().then_some(rm)
                } else {
                    None
                }
            });
        if let Some(rm_path) = role_memory_path {
            let rm_backup_mgr =
                super::backup::BackupManager::new(rm_path.with_extension("backups"));
            match rm_backup_mgr.create_rolling_backup_with_pruned(&rm_path, retention_limit) {
                Ok((meta, pruned)) => {
                    report.created_role_memory_backup = Some(meta.path);
                    report.pruned_backups = report.pruned_backups.saturating_add(pruned);
                }
                Err(error) => {
                    tracing::warn!(%error, "rolling hot backup of role_memory.db failed during maintenance");
                }
            }
        }

        let journal_root = crate::thread::journal_root(&self.db_path);
        match backup_mgr.create_journal_snapshot_with_pruned(&journal_root, retention_limit) {
            Ok((path, pruned)) => {
                report.created_journal_backup = path;
                report.pruned_backups = report.pruned_backups.saturating_add(pruned);
            }
            Err(error) => {
                tracing::warn!(%error, "rolling journal snapshot failed during maintenance");
            }
        }

        // 5. Run PRAGMA optimize
        let _ = self.optimize_blocking();

        Ok(report)
    }

    /// The one blocking bridge for every synchronous verb.
    ///
    /// On a **multi-thread runtime** `block_in_place` is preferred: it parks
    /// this worker and hands its core back, so the runtime keeps making
    /// progress and no thread is spawned. On a **current-thread runtime** there
    /// is no other worker that could drive the supervisor, so the wait must
    /// move off this thread entirely: `spawn` + `join`.
    ///
    /// That second branch is only deadlock-free *because*
    /// [`PersistenceHandle::spawn`] guarantees the supervisor owns a thread of
    /// its own whenever the caller's runtime is current-thread — the two
    /// decisions are coupled and must move together. (The previous version of
    /// this bridge took the thread path only for `!MultiThread` but left the
    /// supervisor as a task on that same current-thread runtime, so the `join`
    /// blocked the only thread that could ever serve the ack: every sync verb
    /// deadlocked under `#[tokio::test]`.)
    fn run_blocking<T: Send + 'static>(
        &self,
        command: PersistenceCommand,
        ack_rx: oneshot::Receiver<Result<T, PersistenceError>>,
    ) -> Result<T, PersistenceError> {
        let supervisor = self.supervisor.clone();
        let run = move || {
            supervisor
                .blocking_send(command)
                .map_err(|_| PersistenceError::WriterDown)?;
            ack_rx
                .blocking_recv()
                .map_err(|_| PersistenceError::WriterDown)?
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(run)
            }
            _ => std::thread::spawn(run).join().map_err(|_| {
                PersistenceError::Poisoned("persistence blocking bridge panicked".into())
            })?,
        }
    }
}
