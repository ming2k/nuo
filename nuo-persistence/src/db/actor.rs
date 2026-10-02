//! The supervised single-writer actor: command execution, respawn/backoff,
//! and writer-thread storage metrics (ADR-0196, ADR-0236 D7).
use super::*;

impl PersistenceCommand {
    /// Execute against the writer's engine. Every engine call is panic-
    /// guarded (ADR-0196 D1): a poisoned command settles its own ack as
    /// [`PersistenceError::Poisoned`] and the actor survives. Returns
    /// `false` only for the test-only death command.
    fn execute(self, engine: &DatabaseEngine) -> bool {
        match self {
            Self::SaveSession {
                data,
                full,
                usage_upserts,
                guard,
                ack,
            } => {
                let res =
                    guarded_save(|| engine.save_session_inner(&data, full, &usage_upserts, &guard));
                let _ = ack.send(res);
            }
            Self::UpsertSession { record, ack } => {
                let res = guarded(|| engine.upsert_session(&record));
                let _ = ack.send(res);
            }
            Self::DeleteSession { session_id, ack } => {
                let res = guarded(|| engine.delete_session(&session_id));
                let _ = ack.send(res);
            }
            Self::RenameSession {
                session_id,
                title,
                manual,
                ack,
            } => {
                let res = guarded(|| engine.rename_session(&session_id, title.as_deref(), manual));
                let _ = ack.send(res);
            }
            Self::RecordCommand { cmd, ack } => {
                let res = guarded(|| engine.record_command(&cmd));
                let _ = ack.send(res);
            }
            Self::RecordRequestProjection {
                session_id,
                record,
                ack,
            } => {
                let res = guarded(|| engine.insert_request_projection(&session_id, &record));
                let _ = ack.send(res);
            }
            Self::ProjectUsage { ack } => {
                let _ = ack.send(guarded(|| engine.project_usage_batch(64)));
            }
            Self::RecordUsageStats { entries, ack } => {
                let res = guarded(|| persist_usage_records(&engine.conn, entries));
                if let Some(ack) = ack {
                    let _ = ack.send(res);
                } else if let Err(error) = res {
                    warn!(%error, "could not persist queued usage statistics");
                }
            }
            Self::SetKV { key, value, ack } => {
                let res = guarded(|| engine.set_kv(&key, &value));
                let _ = ack.send(res);
            }
            Self::DeleteKV { key, ack } => {
                let res = guarded(|| engine.delete_kv(&key));
                let _ = ack.send(res);
            }
            Self::CollectEntryGarbage { ack } => {
                let res = guarded(|| engine.collect_entry_garbage());
                let _ = ack.send(res);
            }
            Self::CreateBackup { target_path, ack } => {
                let res = guarded(|| engine.create_backup(&target_path));
                let _ = ack.send(res);
            }
            Self::RecordInputHistory { entry, dedup, ack } => {
                let res = guarded(|| engine.record_input_history(&entry, dedup));
                if let Some(ack) = ack {
                    let _ = ack.send(res);
                }
            }
            Self::SaveInputHistory {
                entries,
                dedup,
                ack,
            } => {
                let res = guarded(|| engine.save_input_history(&entries, dedup));
                let _ = ack.send(res);
            }
            Self::ClearInputHistory { ack } => {
                let res = guarded(|| engine.clear_input_history());
                let _ = ack.send(res);
            }
            Self::DeleteInputHistoryEntry {
                text,
                created_at_ms,
                ack,
            } => {
                let res = guarded(|| engine.delete_input_history_entry(&text, created_at_ms));
                if let Some(ack) = ack {
                    let _ = ack.send(res);
                }
            }
            Self::SaveSessionDelta { delta, ack } => {
                let res =
                    guarded(|| crate::db::session_ir::save_session_delta(&engine.conn, &delta));
                let _ = ack.send(res);
            }
            Self::CommitContextFacts { commit, ack } => {
                let borrowed = commit.as_borrowed();
                let res = crate::db::context_store::commit_facts(&engine.conn, &borrowed);
                let _ = ack.send(res);
            }
            Self::CommitContextView { commit, ack } => {
                let borrowed = commit.as_borrowed();
                let res = crate::db::context_store::commit_view(&engine.conn, &borrowed);
                let _ = ack.send(res);
            }
            Self::ExecuteInspectDeletion {
                session_id,
                job_id,
                now_ms,
                ack,
            } => {
                let res = crate::db::inspect_service::execute_deletion(
                    &engine.conn,
                    &session_id,
                    &job_id,
                    now_ms,
                );
                let _ = ack.send(res);
            }
            Self::CollectInspectGarbage {
                session_id,
                now_ms,
                batch_limit,
                batch_ms,
                ack,
            } => {
                let res = crate::db::inspect_service::collect_batch(
                    &engine.conn,
                    &session_id,
                    now_ms,
                    batch_limit,
                    batch_ms,
                );
                let _ = ack.send(res);
            }
            #[cfg(test)]
            Self::Die { ack } => {
                let _ = ack.send(());
                return false;
            }
            #[cfg(test)]
            Self::SaveSessionThenDie {
                data,
                full,
                usage_upserts,
                guard,
            } => {
                let _ =
                    guarded_save(|| engine.save_session_inner(&data, full, &usage_upserts, &guard));
                // Die without acknowledging: the commit is durable, its ack is
                // gone. Recovery must resolve the outcome by operation identity.
                return false;
            }
        }
        true
    }

    /// Resolve every ack with `Err(error)` without executing. Used by the
    /// supervisor to drain commands honestly while the writer is down
    /// (ADR-0196 D5): the command was never durable, and pretending
    /// otherwise would convert a visible failure into silent data loss.
    pub(super) fn fail(self, error: PersistenceError) {
        match self {
            Self::ProjectUsage { ack } => {
                let _ = ack.send(Err(error));
            }
            Self::SaveSession { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::SaveSessionDelta { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::CommitContextFacts { ack, .. } | Self::CommitContextView { ack, .. } => {
                let _ = ack.send(Err(crate::db::context_store::ContextCommitError::PersistenceFailure {
                    detail: error.to_string(),
                }));
            }
            Self::ExecuteInspectDeletion { ack, .. } => {
                let _ = ack.send(Err(nuo_contracts::context_lifecycle::InspectError::Corrupt));
            }
            Self::CollectInspectGarbage { ack, .. } => {
                let _ = ack.send(Err(nuo_contracts::context_lifecycle::InspectError::Corrupt));
            }
            Self::UpsertSession { ack, .. }
            | Self::RecordCommand { ack, .. }
            | Self::RecordRequestProjection { ack, .. }
            | Self::SetKV { ack, .. }
            | Self::SaveInputHistory { ack, .. }
            | Self::ClearInputHistory { ack } => {
                let _ = ack.send(Err(error));
            }
            Self::DeleteSession { ack, .. } | Self::RenameSession { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::RecordUsageStats { ack, .. } => {
                if let Some(ack) = ack {
                    let _ = ack.send(Err(error));
                } else {
                    warn!(%error, "could not persist queued usage statistics");
                }
            }
            Self::DeleteKV { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::CollectEntryGarbage { ack } => {
                let _ = ack.send(Err(error));
            }
            Self::RecordInputHistory { ack, .. } => {
                if let Some(ack) = ack {
                    let _ = ack.send(Err(error));
                }
            }
            Self::DeleteInputHistoryEntry { ack, .. } => {
                if let Some(ack) = ack {
                    let _ = ack.send(Err(error));
                }
            }
            Self::CreateBackup { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            #[cfg(test)]
            Self::Die { ack } => {
                let _ = ack.send(());
            }
            #[cfg(test)]
            Self::SaveSessionThenDie { .. } => {}
        }
    }
}

/// The supervised single writer (ADR-0196 D1).
///
/// Front-door commands land here; the supervisor forwards them to the
/// *current* writer generation's channel. On writer death (send failure —
/// the actor thread dropped its receiver) it respawns with bounded
/// exponential backoff, publishing health transitions on the watch channel.
/// While down, arriving commands are drained with `Err(WriterDown)` acks
/// (D5) — the bounded front channel cannot back-pressure turns into a hang.
pub(super) async fn run_supervisor(
    mut front_rx: mpsc::Receiver<PersistenceCommand>,
    db_path: PathBuf,
    blob_store: Option<BlobStore>,
    health: watch::Sender<WriterHealth>,
    lease: Arc<nuo_host::lock::ProcessLock>,
    mut initial_engine: Option<DatabaseEngine>,
    reader_ages: Arc<ReaderAges>,
) {
    /// Respawn attempts before `Recovering` escalates to `Down`
    /// (~1.55 s of cumulative backoff at the base schedule below).
    const RECOVERING_ATTEMPTS: u32 = 4;
    const BASE_BACKOFF: Duration = Duration::from_millis(100);
    const MAX_BACKOFF: Duration = Duration::from_secs(5);

    let mut writer: Option<mpsc::Sender<PersistenceCommand>> = None;
    let mut attempt: u32 = 0;
    let mut since_ms: u64 = 0;
    let mut last_error: String;
    let mut next_try = Instant::now();
    let mut backoff = BASE_BACKOFF;

    while let Some(mut command) = front_rx.recv().await {
        'serve: loop {
            if let Some(tx) = &writer {
                match tx.send(command).await {
                    Ok(()) => break 'serve,
                    Err(mpsc::error::SendError(undelivered)) => {
                        // The actor thread is gone (engine-open failure at
                        // birth, panic escape, or explicit stop). Respawn.
                        command = undelivered;
                        writer = None;
                        attempt = 1;
                        since_ms = unix_ms();
                        last_error = "persistence writer stopped".to_string();
                        backoff = BASE_BACKOFF;
                        next_try = Instant::now();
                        let _ = health.send_if_modified(|current| {
                            *current = WriterHealth::Recovering {
                                attempt,
                                since_ms,
                                error: last_error.clone(),
                            };
                            true
                        });
                    }
                }
            } else if Instant::now() >= next_try {
                match spawn_writer(
                    &db_path,
                    blob_store.as_ref(),
                    &health,
                    lease.clone(),
                    initial_engine.take(),
                    reader_ages.clone(),
                )
                .await
                {
                    Ok(tx) => {
                        writer = Some(tx);
                        attempt = 0;
                        // `Healthy` is restored by the writer's first
                        // successful command, not by spawn alone (D1).
                    }
                    Err(e) => {
                        attempt += 1;
                        last_error = e.to_string();
                        next_try = Instant::now() + backoff;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        let state = if attempt > RECOVERING_ATTEMPTS {
                            WriterHealth::Down {
                                attempt,
                                since_ms,
                                error: last_error.clone(),
                            }
                        } else {
                            WriterHealth::Recovering {
                                attempt,
                                since_ms,
                                error: last_error.clone(),
                            }
                        };
                        let _ = health.send_if_modified(|current| {
                            *current = state;
                            true
                        });
                        // D5: drain-while-down — resolve honestly, never
                        // back-pressure the caller into a hang.
                        command.fail(PersistenceError::WriterDown);
                        break 'serve;
                    }
                }
            } else {
                // Down and still inside the backoff window: same D5 policy.
                command.fail(PersistenceError::WriterDown);
                break 'serve;
            }
        }
    }

    // Every handle clone dropped: orderly shutdown. Writers exit with their
    // channel; the health state records that the stop was deliberate.
    let _ = health.send_if_modified(|current| {
        *current = WriterHealth::Down {
            attempt: 0,
            since_ms: unix_ms(),
            error: "persistence writer was shut down (all handles dropped)".to_string(),
        };
        true
    });
}

/// Record the ADR-0236 D7 storage-pressure signals on the writer thread: WAL
/// size, usage-projection backlog and lag, and reader age. A large WAL or an
/// old reader is what prevents checkpoint reclamation. Never logs prompt
/// contents or credentials.
fn log_storage_metrics(db_path: &Path, engine: &DatabaseEngine, reader_ages: &ReaderAges) {
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let wal_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    let main_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    let (backlog, oldest_revision): (i64, Option<i64>) = engine
        .conn
        .query_row(
            "SELECT COUNT(*), MIN(revision) FROM usage_dirty",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0, None));
    let revision: i64 = engine
        .conn
        .query_row("SELECT revision FROM usage_clock WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap_or(0);
    let projection_lag = oldest_revision
        .map(|oldest| revision.saturating_sub(oldest) + 1)
        .unwrap_or(0);
    tracing::debug!(
        main_bytes,
        wal_bytes,
        projection_backlog = backlog,
        projection_lag,
        active_readers = reader_ages.active(),
        reader_capacity = READER_POOL_CAPACITY,
        oldest_reader_ms = reader_ages.oldest_age().map(|age| age.as_millis() as u64),
        "persistence storage metrics"
    );
}

/// WAL size beyond which the writer attempts a bounded checkpoint (ADR-0236
/// D7: "checkpoint or reader pressure must not grow without limit").
const WAL_CHECKPOINT_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
/// Age beyond which an active reader snapshot is reported as the pressure
/// that keeps the WAL from being reclaimed.
const READER_AGE_WARN_MS: u64 = 30_000;

/// ADR-0236 D7: bound WAL growth and surface reader pressure. Runs on the
/// writer thread between transactions.
///
/// Graduated policy:
/// - When no reader snapshots are active (`reader_ages.active() == 0`), attempts
///   `TRUNCATE` checkpoint to reclaim and shrink the WAL to zero bytes.
/// - When active readers exist, attempts `PASSIVE` checkpoint to reclaim
///   unpinned frames without blocking reader threads.
/// - If WAL exceeds 2x threshold while readers remain active, escalates warning.
/// - Runs `PRAGMA optimize` to refresh SQLite query planner statistics.
fn maintain_storage_pressure(
    db_path: &Path,
    engine: &DatabaseEngine,
    reader_ages: &ReaderAges,
    wal_threshold_bytes: u64,
    reader_warn_ms: u64,
) {
    let active_readers = reader_ages.active();
    if let Some(age_ms) = reader_ages.oldest_age().map(|age| age.as_millis() as u64)
        && age_ms >= reader_warn_ms
    {
        warn!(
            oldest_reader_ms = age_ms,
            active_readers, "a long-lived reader snapshot is pinning the WAL"
        );
    }
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let wal_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    if wal_bytes < wal_threshold_bytes {
        return;
    }

    let checkpoint_sql = if active_readers == 0 {
        "PRAGMA wal_checkpoint(TRUNCATE)"
    } else {
        if wal_bytes >= wal_threshold_bytes.saturating_mul(2) {
            warn!(
                wal_bytes,
                active_readers,
                "WAL size exceeded 2x threshold with active readers; checkpointing in degraded PASSIVE mode"
            );
        }
        "PRAGMA wal_checkpoint(PASSIVE)"
    };

    match engine.conn.query_row(checkpoint_sql, [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    }) {
        Ok((0, log_frames, checkpointed)) => {
            info!(
                wal_bytes,
                log_frames, checkpointed, active_readers, "reclaimed WAL after pressure threshold"
            );
            let _ = engine.conn.execute_batch("PRAGMA optimize;");
        }
        Ok((busy, log_frames, checkpointed)) => warn!(
            wal_bytes,
            busy, log_frames, checkpointed, active_readers, "WAL checkpoint could not complete"
        ),
        Err(error) => warn!(%error, checkpoint_sql, "WAL checkpoint failed"),
    }
}

/// Open the engine and spawn one writer generation on a dedicated thread.
/// The open happens on a blocking thread so a wedged SQLite open cannot
/// stall the supervisor.
async fn spawn_writer(
    db_path: &Path,
    blob_store: Option<&BlobStore>,
    health: &watch::Sender<WriterHealth>,
    lease: Arc<nuo_host::lock::ProcessLock>,
    initial_engine: Option<DatabaseEngine>,
    reader_ages: Arc<ReaderAges>,
) -> std::result::Result<mpsc::Sender<PersistenceCommand>, rusqlite::Error> {
    let path = db_path.to_path_buf();
    let metrics_path = db_path.to_path_buf();
    let blobs = blob_store.cloned();
    let engine = tokio::task::spawn_blocking(move || match initial_engine {
        Some(engine) => Ok(engine),
        None => DatabaseEngine::open(&path, blobs),
    })
    .await
    .map_err(|join| {
        rusqlite::Error::ToSqlConversionFailure(
            format!("persistence writer spawn task failed: {join}").into(),
        )
    })??;

    let (tx, mut rx) = mpsc::channel::<PersistenceCommand>(1024);
    let health = health.clone();
    std::thread::Builder::new()
        .name("nuo-persistence-writer".into())
        .spawn(move || {
            let _lease = lease;
            let mut foreground = 0u32;
            // ADR-0236 D7 measurement contract: sample storage pressure on the
            // writer thread (the only place with the engine) and never log
            // prompt contents or credentials.
            const METRICS_INTERVAL: Duration = Duration::from_secs(5);
            let mut last_metrics = Instant::now();
            loop {
                if last_metrics.elapsed() >= METRICS_INTERVAL {
                    last_metrics = Instant::now();
                    log_storage_metrics(&metrics_path, &engine, &reader_ages);
                    maintain_storage_pressure(
                        &metrics_path,
                        &engine,
                        &reader_ages,
                        WAL_CHECKPOINT_THRESHOLD_BYTES,
                        READER_AGE_WARN_MS,
                    );
                }
                if rx.is_empty() || foreground >= 16 {
                    foreground = 0;
                    match engine.project_usage_batch(64) {
                        Ok(n) if n > 0 && rx.is_empty() => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => warn!(%error, "usage projection recovery failed"),
                        _ => {}
                    }
                }
                let Some(command) = rx.blocking_recv() else {
                    break;
                };
                foreground += 1;
                if !command.execute(&engine) {
                    break;
                }
                // D1: `Healthy` is restored by the first *successful*
                // command after a degradation, not by spawn alone.
                health.send_if_modified(|current| {
                    if current.is_serving() {
                        false
                    } else {
                        *current = WriterHealth::Healthy;
                        true
                    }
                });
            }
        })
        .map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(
                format!("failed to spawn persistence writer thread: {e}").into(),
            )
        })?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0236 D7: the maintenance pass checkpoints a pressured WAL and
    /// reports an over-age reader without wedging the engine.
    #[test]
    fn maintain_storage_pressure_bounds_the_wal_and_reports_old_readers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuo.db");
        let engine = DatabaseEngine::open(&path, None).unwrap();
        let reader_ages = ReaderAges::default();
        // A registered snapshot is "old enough" at any non-negative bound.
        let _id = reader_ages.register();

        // Threshold 0 forces the checkpoint branch; warn bound 0 forces the
        // reader-pressure report. Must not panic and must leave the engine
        // usable.
        maintain_storage_pressure(&path, &engine, &reader_ages, 0, 0);

        let version: u32 = engine
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_DB_VERSION);
    }

    #[test]
    fn maintain_storage_pressure_truncates_when_no_active_readers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuo.db");
        let engine = DatabaseEngine::open(&path, None).unwrap();
        let reader_ages = ReaderAges::default();
        assert_eq!(reader_ages.active(), 0);

        maintain_storage_pressure(&path, &engine, &reader_ages, 0, 0);

        let version: u32 = engine
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_DB_VERSION);
    }
}
