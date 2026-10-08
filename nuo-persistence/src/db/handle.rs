//! `PersistenceHandle`: the public front door and every typed verb (ADR-0196/0231).
//! The struct and its observability types live in `db.rs`.
use super::actor::run_supervisor;
use super::*;

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
    let dirs = crate::paths::get();
    let db_path = dirs.db_file();
    let mut guard = GLOBAL_HANDLE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = guard.as_ref() {
        if handle.db_path == db_path && handle.startup_error.is_none() {
            return handle.clone();
        }
    }
    let blobs = Some(BlobStore::new(dirs.blobs_dir()));
    let handle = PersistenceHandle::spawn(db_path, blobs);
    if handle.startup_error.is_none() {
        *guard = Some(handle.clone());
    }
    handle
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
                    blob_store: owner.blob_store.clone(),
                    startup_error: None,
                    readers: owner.readers.clone(),
                    reader_ages: owner.reader_ages.clone(),
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
        let blobs = blob_store.clone();
        // Initialization owns its thread: no Tokio worker is needed to make
        // progress while a synchronous constructor waits for readiness.
        let opened = std::thread::spawn(move || {
            identity?;
            let lease = Arc::new(nuo_host::lock::ProcessLock::acquire_with_timeout(
                &path.with_extension("db.owner.lock"),
                Duration::from_millis(2000),
            )?);
            let engine = DatabaseEngine::open(&path, blobs).map_err(|e| e.to_string())?;
            Ok::<_, String>((engine, lease))
        })
        .join()
        .unwrap_or_else(|_| Err("database initialization panicked".into()));
        let readers = Arc::new(tokio::sync::Semaphore::new(READER_POOL_CAPACITY));
        let reader_ages = Arc::new(ReaderAges::default());
        let startup_error = match opened {
            Ok((engine, lease)) => {
                registry.insert(
                    db_path.clone(),
                    RegisteredOwner {
                        supervisor: supervisor.downgrade(),
                        health: health_rx.clone(),
                        blob_store: blob_store.clone(),
                        readers: readers.clone(),
                        reader_ages: reader_ages.clone(),
                        lease: Arc::downgrade(&lease),
                    },
                );
                let run = run_supervisor(
                    front_rx,
                    db_path.clone(),
                    blob_store.clone(),
                    health_tx,
                    lease,
                    Some(engine),
                    reader_ages.clone(),
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
            blob_store,
            startup_error,
            readers,
            reader_ages,
        }
    }

    /// Startup must succeed before a server admits work.
    pub fn ensure_ready(&self) -> std::result::Result<(), String> {
        match &self.startup_error {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        }
    }

    /// The current writer health snapshot (ADR-0196 D4).
    pub fn health(&self) -> WriterHealth {
        self.health.borrow().clone()
    }

    /// Subscribe to writer health transitions (ADR-0196 D4): the server
    /// folds these into the monitor stream; frontends render degradation.
    pub fn subscribe_health(&self) -> watch::Receiver<WriterHealth> {
        self.health.clone()
    }

    /// The database file path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Asynchronously save a session in SQLite, returning the committed
    /// session revision (ADR-0236 D3). The `guard` carries an optional
    /// idempotency identity and revision precondition; a replay returns the
    /// original revision without re-applying.
    pub(crate) async fn save_session(
        &self,
        data: crate::session::SessionData,
        full: bool,
        usage_upserts: Vec<nuo_wire::RequestUsageRecord>,
        guard: CommitGuard,
    ) -> Result<u64, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::SaveSession {
                data: Box::new(data),
                full,
                usage_upserts,
                guard,
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
        data: crate::session::SessionData,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::SaveSession {
                data: Box::new(data),
                full: true,
                usage_upserts: Vec::new(),
                guard: CommitGuard::default(),
                ack: ack_tx,
            },
            ack_rx,
        )
        .map(|_| ())
    }

    /// Asynchronously upsert a session record.
    pub async fn upsert_session(&self, record: SessionRecord) -> Result<(), PersistenceError> {
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
    pub fn try_upsert_session(&self, record: SessionRecord) {
        let (ack_tx, _) = oneshot::channel();
        if let Err(error) = self.supervisor.try_send(PersistenceCommand::UpsertSession {
            record,
            ack: ack_tx,
        }) {
            warn!(error = %error, "dropped fire-and-forget session upsert: writer unavailable");
        }
    }

    /// Asynchronously delete a session record.
    pub async fn delete_session(&self, session_id: String) -> Result<bool, PersistenceError> {
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

    /// Asynchronously record a command invocation.
    pub async fn record_command(
        &self,
        cmd: nuo_wire::CommandRecord,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::RecordCommand { cmd, ack: ack_tx })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Asynchronously insert a request-projection record (ADR-0218). Awaits the
    /// writer ack for callers that need the durable write confirmed (tests,
    /// tooling); the request hot path uses
    /// [`Self::try_record_request_projection`] instead.
    pub async fn record_request_projection(
        &self,
        session_id: String,
        record: nuo_wire::RequestProjection,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::RecordRequestProjection {
                session_id,
                record,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Non-blocking fire-and-forget request-projection insert (ADR-0218). Used
    /// on the model-request hot path: forensic persistence must never block
    /// dispatch. A dropped archive write is logged, not surfaced as a round
    /// failure.
    pub fn try_record_request_projection(
        &self,
        session_id: String,
        record: nuo_wire::RequestProjection,
    ) {
        let (ack_tx, _) = oneshot::channel();
        if let Err(error) = self
            .supervisor
            .try_send(PersistenceCommand::RecordRequestProjection {
                session_id,
                record,
                ack: ack_tx,
            })
        {
            warn!(error = %error, "dropped fire-and-forget request projection: writer unavailable");
        }
    }

    pub(crate) fn catch_up_usage(&self) -> Result<usize, PersistenceError> {
        let (ack, rx) = oneshot::channel();
        self.run_blocking(PersistenceCommand::ProjectUsage { ack }, rx)
    }

    pub async fn record_attempts(
        &self,
        entries: Vec<nuo_wire::usage_stats::UsageStatRecord>,
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
        entries: Vec<nuo_wire::usage_stats::UsageStatRecord>,
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
    pub fn try_record_input_history(&self, entry: nuo_wire::HistoryEntry, dedup: bool) {
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
        entry: nuo_wire::HistoryEntry,
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
        entries: Vec<nuo_wire::HistoryEntry>,
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

    /// Open a reader against this handle's database (ADR-0231).
    ///
    /// This is the **only** way to obtain a connection snapshot: the engine
    /// type is crate-private, so a caller cannot open one on a path of its own
    /// choosing. Reads never need the actor — WAL readers run concurrently
    /// with the writer — so this returns immediately instead of queueing
    /// behind a slow write.
    pub fn reader(&self) -> Result<DbReader> {
        if let Some(error) = &self.startup_error {
            // When another process already holds the single-writer advisory lock (e.g. background server),
            // this process cannot become the supervisor, but SQLite WAL mode safely allows concurrent read-only queries.
            let is_lock_contention = error.contains("could not acquire advisory lock");
            if !is_lock_contention {
                return Err(rusqlite::Error::InvalidParameterName(error.to_string()));
            }
        }
        let permit = self.readers.clone().try_acquire_owned().map_err(|_| {
            rusqlite::Error::InvalidParameterName("database reader capacity exhausted".into())
        })?;
        let conn = Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "query_only", true)?;
        conn.busy_timeout(Duration::from_millis(250))?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != CURRENT_DB_VERSION {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "reader schema mismatch: {version}"
            )));
        }
        conn.execute_batch("BEGIN DEFERRED")?;
        // Register only after every fallible step: an error must not leak a
        // registration whose guard was never constructed.
        let reader_ages = self.reader_ages.clone();
        let age = ReaderAgeGuard {
            id: reader_ages.register(),
            reader_ages,
        };
        Ok(DbReader {
            engine: DatabaseEngine {
                conn,
                blob_store: self.blob_store.clone(),
            },
            db_path: self.db_path.clone(),
            _permit: Some(permit),
            _age: Some(age),
        })
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

    /// Create an online hot backup snapshot of the database using SQLite's `VACUUM INTO`.
    ///
    /// The backup is written to `target_path` in a defragmented, standalone,
    /// consistent state while concurrent readers continue uninterrupted.
    pub async fn create_backup(&self, target_path: PathBuf) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::CreateBackup {
                target_path,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Synchronous/blocking variant of [`Self::create_backup`].
    pub fn create_backup_blocking(&self, target_path: PathBuf) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::CreateBackup {
                target_path,
                ack: ack_tx,
            },
            ack_rx,
        )
    }

    /// A storage-observability snapshot (ADR-0236 D7): WAL size, active reader
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

    /// Reclaim transcript entries no session references (ADR-0187): the
    /// periodic storage-maintenance sweep. Routed through the actor so the
    /// DELETE serializes with every other write.
    pub async fn collect_entry_garbage(&self) -> Result<usize, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::CollectEntryGarbage { ack: ack_tx })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Synchronous [`Self::collect_entry_garbage`] for a blocking maintenance
    /// pass.
    pub fn collect_entry_garbage_blocking(&self) -> Result<usize, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::CollectEntryGarbage { ack: ack_tx },
            ack_rx,
        )
    }

    /// Save an incremental [`nuo_wire::SessionDelta`] asynchronously (ADR-0241/ADR-0249, INV-SESSION-05).
    pub async fn save_session_delta(
        &self,
        delta: nuo_wire::SessionDelta,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::SaveSessionDelta {
                delta: Box::new(delta),
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Synchronous [`Self::save_session_delta`] for blocking callers.
    pub fn save_session_delta_blocking(
        &self,
        delta: nuo_wire::SessionDelta,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::SaveSessionDelta {
                delta: Box::new(delta),
                ack: ack_tx,
            },
            ack_rx,
        )
    }

    /// Commit immutable execution facts asynchronously through the single writer (ADR-0275 §7).
    pub async fn commit_context_facts(
        &self,
        commit: crate::db::context_store::OwnedFactCommit,
    ) -> Result<crate::db::context_store::CommitOutcome, crate::db::context_store::ContextCommitError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::CommitContextFacts {
                commit: Box::new(commit),
                ack: ack_tx,
            })
            .await
            .map_err(|_| crate::db::context_store::ContextCommitError::PersistenceFailure {
                detail: "writer supervisor is closed".into(),
            })?;
        ack_rx
            .await
            .map_err(|_| crate::db::context_store::ContextCommitError::PersistenceFailure {
                detail: "writer dropped response".into(),
            })?
    }

    /// Commit a branch context view asynchronously through the single writer (ADR-0275 §7, ADR-0278).
    pub async fn commit_context_view(
        &self,
        commit: crate::db::context_store::OwnedViewCommit,
    ) -> Result<crate::db::context_store::CommitOutcome, crate::db::context_store::ContextCommitError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::CommitContextView {
                commit: Box::new(commit),
                ack: ack_tx,
            })
            .await
            .map_err(|_| crate::db::context_store::ContextCommitError::PersistenceFailure {
                detail: "writer supervisor is closed".into(),
            })?;
        ack_rx
            .await
            .map_err(|_| crate::db::context_store::ContextCommitError::PersistenceFailure {
                detail: "writer dropped response".into(),
            })?
    }

    /// Execute an inspect deletion job asynchronously through the single writer (ADR-0279 §6).
    pub async fn execute_inspect_deletion(
        &self,
        session_id: impl Into<String>,
        job_id: impl Into<String>,
        now_ms: u64,
    ) -> Result<crate::db::inspect_service::DeletionReport, nuo_wire::context_lifecycle::InspectError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::ExecuteInspectDeletion {
                session_id: session_id.into(),
                job_id: job_id.into(),
                now_ms,
                ack: ack_tx,
            })
            .await
            .map_err(|_| nuo_wire::context_lifecycle::InspectError::Corrupt)?;
        ack_rx
            .await
            .map_err(|_| nuo_wire::context_lifecycle::InspectError::Corrupt)?
    }

    /// Collect a bounded garbage collection batch asynchronously through the single writer (ADR-0279 §5).
    pub async fn collect_inspect_garbage(
        &self,
        session_id: impl Into<String>,
        now_ms: u64,
        batch_limit: u32,
        batch_ms: u64,
    ) -> Result<crate::db::inspect_service::CollectionProgress, nuo_wire::context_lifecycle::InspectError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::CollectInspectGarbage {
                session_id: session_id.into(),
                now_ms,
                batch_limit,
                batch_ms,
                ack: ack_tx,
            })
            .await
            .map_err(|_| nuo_wire::context_lifecycle::InspectError::Corrupt)?;
        ack_rx
            .await
            .map_err(|_| nuo_wire::context_lifecycle::InspectError::Corrupt)?
    }

    /// The one blocking bridge for every synchronous verb (ADR-0196).
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
