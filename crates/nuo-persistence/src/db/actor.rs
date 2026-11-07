//! The supervised single-writer actor: command execution, respawn/backoff,
//! and writer-thread storage metrics (D7).
use super::*;

impl PersistenceCommand {
    /// Execute against the writer's engine. Every engine call is panic-
    /// guarded (D1): a poisoned command settles its own ack as
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
                let res = guarded_save(|| {
                    crate::db::session_store::save_session_inner(
                        &engine.conn,
                        engine.blob_store.as_ref(),
                        &data,
                        full,
                        &usage_upserts,
                        &guard,
                    )
                });
                let _ = ack.send(res);
            }
            Self::UpsertSession { record, ack } => {
                let res =
                    guarded(|| crate::db::session_store::upsert_session(&engine.conn, &record));
                let _ = ack.send(res);
            }
            Self::DeleteSession { session_id, ack } => {
                let res =
                    guarded(|| crate::db::session_store::delete_session(&engine.conn, &session_id));
                let _ = ack.send(res);
            }
            Self::RenameSession {
                session_id,
                title,
                manual,
                ack,
            } => {
                let res = guarded(|| {
                    crate::db::session_store::rename_session(
                        &engine.conn,
                        &session_id,
                        title.as_deref(),
                        manual,
                    )
                });
                let _ = ack.send(res);
            }
            Self::AppendWorkspaceEffect { effect, ack } => {
                let res = guarded(|| {
                    crate::workspace_effects::append_effect(&engine.conn, &effect.as_borrowed())
                        .map_err(engine_error)
                });
                let _ = ack.send(res);
            }
            Self::AppendCompletedWorkspaceEffect {
                effect,
                completed_at_ms,
                ack,
            } => {
                let res = guarded(|| {
                    crate::workspace_effects::append_completed_effect(
                        &engine.conn,
                        &effect.as_borrowed(),
                        completed_at_ms,
                    )
                    .map_err(engine_error)
                });
                let _ = ack.send(res);
            }
            Self::CompleteWorkspaceEffect {
                effect_id,
                after_hash,
                completed_at_ms,
                ack,
            } => {
                let res = guarded(|| {
                    crate::workspace_effects::complete_effect(
                        &engine.conn,
                        effect_id,
                        after_hash.as_deref(),
                        completed_at_ms,
                    )
                    .map_err(engine_error)
                });
                let _ = ack.send(res);
            }
            Self::MarkWorkspaceEffectReversed {
                effect_id,
                reversed_at_ms,
                note,
                ack,
            } => {
                let res = guarded(|| {
                    crate::workspace_effects::mark_reversed(
                        &engine.conn,
                        effect_id,
                        reversed_at_ms,
                        note.as_deref(),
                    )
                    .map_err(engine_error)
                });
                let _ = ack.send(res);
            }
            Self::ListWorkspaceEffects { thread_id, ack } => {
                let res = guarded(|| {
                    crate::workspace_effects::list_effects(&engine.conn, &thread_id)
                        .map_err(engine_error)
                });
                let _ = ack.send(res);
            }
            Self::ExpirePreimages {
                older_than_ms,
                now_ms,
                ack,
            } => {
                let res = guarded(|| {
                    crate::workspace_effects::expire_preimages(&engine.conn, older_than_ms, now_ms)
                        .map_err(engine_error)
                });
                let _ = ack.send(res);
            }
            Self::UndoableWorkspaceEffects {
                thread_id,
                limit,
                ack,
            } => {
                let res = guarded(|| {
                    crate::workspace_effects::undoable_effects(
                        &engine.conn,
                        &thread_id,
                        limit as usize,
                    )
                    .map_err(engine_error)
                });
                let _ = ack.send(res);
            }
            Self::RecordRequestProjection {
                session_id,
                record,
                ack,
            } => {
                let res = guarded(|| {
                    crate::db::usage_projection::insert_request_projection(
                        &engine.conn,
                        &session_id,
                        &record,
                    )
                });
                let _ = ack.send(res);
            }
            Self::ProjectUsage { ack } => {
                let _ = ack.send(guarded(|| {
                    crate::db::usage_projection::project_usage_batch(&engine.conn, 64)
                }));
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
                let res = guarded(|| crate::db::kv_store::set_kv(&engine.conn, &key, &value));
                let _ = ack.send(res);
            }
            Self::DeleteKV { key, ack } => {
                let res = guarded(|| crate::db::kv_store::delete_kv(&engine.conn, &key));
                let _ = ack.send(res);
            }
            Self::CollectEntryGarbage { ack } => {
                let res =
                    guarded(|| crate::db::garbage_collection::collect_entry_garbage(&engine.conn));
                let _ = ack.send(res);
            }
            // Supervisor-only: `run_supervisor` intercepts `RebuildEngine`
            // before any writer forwarding, so `execute` never observes it.
            // A deflected ack is the honest outcome — the rebuild was not
            // performed — never a fabricated success.
            Self::RebuildEngine { ack } => {
                let _ = ack.send(Err(PersistenceError::WriterDown));
            }
            // Internal handshake: the writer loop intercepts `RetireEngine`
            // before `execute` (the engine is closed there, not here), and
            // the supervisor is its only sender, so this arm is unreachable
            // by construction.
            Self::RetireEngine { .. } => {}
            Self::CreateBackup { target_path, ack } => {
                let res = guarded(|| engine.create_backup(&target_path));
                let _ = ack.send(res);
            }
            Self::Optimize { ack } => {
                let res = guarded(|| {
                    engine.conn.execute_batch("PRAGMA optimize;")?;
                    Ok(())
                });
                let _ = ack.send(res);
            }
            Self::RecordInputHistory { entry, dedup, ack } => {
                let res = guarded(|| {
                    crate::db::input_history::record_input_history(&engine.conn, &entry, dedup)
                });
                if let Some(ack) = ack {
                    let _ = ack.send(res);
                }
            }
            Self::SaveInputHistory {
                entries,
                dedup,
                ack,
            } => {
                let res = guarded(|| {
                    crate::db::input_history::save_input_history(&engine.conn, &entries, dedup)
                });
                let _ = ack.send(res);
            }
            Self::ClearInputHistory { ack } => {
                let res = guarded(|| crate::db::input_history::clear_input_history(&engine.conn));
                let _ = ack.send(res);
            }
            Self::DeleteInputHistoryEntry {
                text,
                created_at_ms,
                ack,
            } => {
                let res = guarded(|| {
                    crate::db::input_history::delete_input_history_entry(
                        &engine.conn,
                        &text,
                        created_at_ms,
                    )
                });
                if let Some(ack) = ack {
                    let _ = ack.send(res);
                }
            }
            Self::SaveSessionDelta { delta, ack } => {
                let res =
                    guarded(|| crate::db::session_ir::save_session_delta(&engine.conn, &delta));
                let _ = ack.send(res);
            }
            Self::FlushDerivedSession { session_id, ack } => {
                let res = guarded(|| crate::db::derived::flush_session(&engine.conn, &session_id));
                let _ = ack.send(res);
            }
            Self::FlushDerivedAll { ack } => {
                let res = guarded(|| crate::db::derived::flush_all(&engine.conn));
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
                let _ = guarded_save(|| {
                    crate::db::session_store::save_session_inner(
                        &engine.conn,
                        engine.blob_store.as_ref(),
                        &data,
                        full,
                        &usage_upserts,
                        &guard,
                    )
                });
                // Die without acknowledging: the commit is durable, its ack is
                // gone. Recovery must resolve the outcome by operation identity.
                return false;
            }
        }
        true
    }

    /// Resolve every ack with `Err(error)` without executing. Used by the
    /// supervisor to drain commands honestly while the writer is down
    /// (D5): the command was never durable, and pretending
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
            Self::FlushDerivedSession { ack, .. } | Self::FlushDerivedAll { ack } => {
                let _ = ack.send(Err(error));
            }
            Self::CommitContextFacts { ack, .. } | Self::CommitContextView { ack, .. } => {
                let _ = ack.send(Err(
                    crate::db::context_store::ContextCommitError::PersistenceFailure {
                        detail: error.to_string(),
                    },
                ));
            }
            Self::ExecuteInspectDeletion { ack, .. } => {
                let _ = ack.send(Err(nuo_protocol::context_lifecycle::InspectError::Corrupt));
            }
            Self::CollectInspectGarbage { ack, .. } => {
                let _ = ack.send(Err(nuo_protocol::context_lifecycle::InspectError::Corrupt));
            }
            Self::UpsertSession { ack, .. }
            | Self::RecordRequestProjection { ack, .. }
            | Self::SetKV { ack, .. }
            | Self::SaveInputHistory { ack, .. }
            | Self::ClearInputHistory { ack } => {
                let _ = ack.send(Err(error));
            }
            // The journal's acks carry a value (`i64` id, `bool` race result, or
            // the effect list), so they cannot share the unit-ack arm above.
            Self::AppendWorkspaceEffect { ack, .. }
            | Self::AppendCompletedWorkspaceEffect { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::CompleteWorkspaceEffect { ack, .. }
            | Self::MarkWorkspaceEffectReversed { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::ListWorkspaceEffects { ack, .. } | Self::UndoableWorkspaceEffects { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::ExpirePreimages { ack, .. } => {
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
            Self::RebuildEngine { ack } => {
                let _ = ack.send(Err(error));
            }
            // Internal handshake (supervisor → live writer only); the
            // drain-while-down path can never observe it — no ack, which
            // the awaiting supervisor reads as "retire never happened".
            Self::RetireEngine { .. } => {}
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
            Self::CreateBackup { ack, .. } | Self::Optimize { ack, .. } => {
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

/// The supervised single writer (D1).
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
        // A deliberate engine rebuild never forwards to a writer generation.
        // Dropping the current sender lets the writer thread finish; its
        // engine close checkpoints any live WAL back into the main database,
        // and the fresh generation recreates the on-disk `-wal`/`-shm` pair
        // that heals subsequent read-only reader opens.
        if let PersistenceCommand::RebuildEngine { ack } = command {
            attempt = 1;
            since_ms = unix_ms();
            last_error = "engine rebuild requested".to_string();
            let _ = health.send_if_modified(|current| {
                *current = WriterHealth::Recovering {
                    attempt,
                    since_ms,
                    error: last_error.clone(),
                };
                true
            });
            // Retire the live generation with the deterministic handshake:
            // the ack resolves only after the engine is closed — a clean
            // close checkpoints any live WAL (including an unlinked one)
            // into the main database, so nothing committed is stranded in
            // the deleted inode. Opening the fresh generation only after
            // this avoids the concurrent-open SQLite disk-I/O failure.
            if let Some(tx) = writer.take() {
                let (retire_tx, retire_rx) = oneshot::channel();
                if tx
                    .send(PersistenceCommand::RetireEngine { ack: retire_tx })
                    .await
                    .is_ok()
                {
                    let _ = retire_rx.await;
                }
            }
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
                    let _ = ack.send(Ok(()));
                }
                Err(e) => {
                    attempt += 1;
                    last_error = e.to_string();
                    next_try = Instant::now();
                    backoff = BASE_BACKOFF;
                    let _ = health.send_if_modified(|current| {
                        *current = WriterHealth::Down {
                            attempt,
                            since_ms,
                            error: last_error.clone(),
                        };
                        true
                    });
                    let _ = ack.send(Err(PersistenceError::WriterDown));
                }
            }
            continue;
        }
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

/// Record the D7 storage-pressure signals: WAL size, usage-projection backlog
/// and lag, and reader age. A large WAL or an old reader is what prevents
/// checkpoint reclamation. Never logs prompt contents or credentials.
///
/// Runs on the maintenance thread's own connection, never on the writer: the
/// queries below are cheap, but *any* work on the writer thread is work the
/// next turn waits for.
fn log_storage_metrics(db_path: &Path, conn: &Connection, reader_ages: &ReaderAges) {
    let wal_bytes = wal_bytes(db_path);
    let main_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    let (backlog, oldest_revision): (i64, Option<i64>) = conn
        .query_row(
            "SELECT COUNT(*), MIN(revision) FROM usage_dirty",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0, None));
    let revision: i64 = conn
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

/// WAL size beyond which a checkpoint is attempted.
const WAL_CHECKPOINT_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
/// WAL size at which the condition is a durability/throughput *incident*, not
/// routine pressure. Reaching this is logged at error level: it can only
/// happen when a snapshot is pinned for minutes, and it must be visible.
const WAL_HARD_CAP_BYTES: u64 = 512 * 1024 * 1024;
/// Age beyond which an active reader snapshot is reported as the pressure
/// that keeps the WAL from being reclaimed.
const READER_AGE_WARN_MS: u64 = 30_000;
/// First retry delay after a checkpoint that could not complete. Doubles on
/// every consecutive failure up to [`WAL_BACKOFF_MAX`].
const WAL_BACKOFF_BASE: Duration = Duration::from_secs(5);
/// Ceiling for the checkpoint retry ladder. A pinned snapshot may last an
/// hour; retrying it every five seconds buys nothing and costs everything.
const WAL_BACKOFF_MAX: Duration = Duration::from_secs(300);
/// How often the maintenance thread wakes. This is *not* the checkpoint
/// cadence — the backoff ladder owns that.
const MAINTENANCE_TICK: Duration = Duration::from_millis(500);
/// Storage metrics cadence. Debug-level: cheap, but not free.
const METRICS_INTERVAL: Duration = Duration::from_secs(5);
/// Busy timeout for the maintenance connection. Small on purpose: a
/// checkpoint that cannot make progress returns immediately and the thread
/// goes back to sleep. It must never park a lock the writer needs.
const MAINTENANCE_BUSY_TIMEOUT: Duration = Duration::from_millis(100);
/// Retry delay when the maintenance connection itself cannot open. Opening a
/// WAL database can fail transiently while another connection rebuilds the
/// shared-memory index, so the thread retries rather than exits.
const MAINTENANCE_OPEN_RETRY: Duration = Duration::from_secs(5);

fn wal_bytes(db_path: &Path) -> u64 {
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0)
}

/// The retry ladder for a checkpoint that cannot complete.
struct WalBackoff {
    next_attempt: Instant,
    delay: Duration,
}

impl WalBackoff {
    fn new() -> Self {
        Self {
            next_attempt: Instant::now(),
            delay: WAL_BACKOFF_BASE,
        }
    }

    /// A checkpoint attempt would be allowed now.
    fn due(&self) -> bool {
        Instant::now() >= self.next_attempt
    }

    /// Record a failed attempt: the next one is strictly later.
    fn failed(&mut self) -> Duration {
        let delay = self.delay;
        self.next_attempt = Instant::now() + delay;
        self.delay = (self.delay * 2).min(WAL_BACKOFF_MAX);
        delay
    }

    /// Record progress: cadence returns to the base delay. A pass that fully
    /// drains the WAL resets the ladder; one that merely advances keeps the
    /// base cadence so it can advance again soon.
    fn progressed(&mut self, fully_drained: bool) {
        self.next_attempt = Instant::now() + WAL_BACKOFF_BASE;
        self.delay = if fully_drained {
            WAL_BACKOFF_BASE
        } else {
            self.delay.min(WAL_BACKOFF_MAX)
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalCheckpointOutcome {
    BelowThreshold,
    Deferred,
    Advanced { log_frames: i64, backfilled: i64 },
    FullyDrained { log_frames: i64 },
    Failed,
}

/// Bound WAL growth without ever blocking the writer.
///
/// The policy is deliberately small:
///
/// 1. **PASSIVE only.** A checkpoint never waits for a reader. `TRUNCATE` is
///    attempted *after* a PASSIVE pass has fully backfilled the log and no
///    reader snapshot is registered — i.e. only when it cannot wait on
///    anything. The historic implementation chose `TRUNCATE` whenever
///    `active_readers == 0`, and a single untracked snapshot then made every
///    attempt park the writer for the full busy timeout — 20–55 second turns.
/// 2. **Backoff on no progress.** A pinned WAL is not retried every tick.
/// 3. **Hard cap surfaced.** Crossing [`WAL_HARD_CAP_BYTES`] is an error-level
///    signal, because by then some snapshot has been pinned for minutes.
fn maintain_storage_pressure(
    db_path: &Path,
    conn: &Connection,
    reader_ages: &ReaderAges,
    wal_threshold_bytes: u64,
    reader_warn_ms: u64,
    backoff: &mut WalBackoff,
) -> WalCheckpointOutcome {
    let wal_bytes = wal_bytes(db_path);
    if wal_bytes < wal_threshold_bytes {
        return WalCheckpointOutcome::BelowThreshold;
    }
    if !backoff.due() {
        return WalCheckpointOutcome::Deferred;
    }

    let active_readers = reader_ages.active();
    let oldest_reader_ms = reader_ages.oldest_age().map(|age| age.as_millis() as u64);
    if let Some(age_ms) = oldest_reader_ms
        && age_ms >= reader_warn_ms
    {
        warn!(
            oldest_reader_ms = age_ms,
            active_readers, "a long-lived reader snapshot is pinning the WAL"
        );
    }
    if wal_bytes >= WAL_HARD_CAP_BYTES {
        tracing::error!(
            wal_bytes,
            active_readers,
            oldest_reader_ms = oldest_reader_ms.unwrap_or(0),
            "WAL exceeded the hard cap: a reader snapshot has pinned it for minutes"
        );
    }

    let checkpoint = |sql: &str| {
        conn.query_row(sql, [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
    };

    let (busy, log_frames, backfilled) = match checkpoint("PRAGMA wal_checkpoint(PASSIVE)") {
        Ok(result) => result,
        Err(error) => {
            let delay_ms = backoff.failed().as_millis() as u64;
            warn!(%error, wal_bytes, retry_in_ms = delay_ms, "WAL checkpoint failed");
            return WalCheckpointOutcome::Failed;
        }
    };

    if busy != 0 {
        let delay_ms = backoff.failed().as_millis() as u64;
        warn!(
            wal_bytes,
            busy,
            log_frames,
            backfilled,
            active_readers,
            oldest_reader_ms = oldest_reader_ms.unwrap_or(0),
            retry_in_ms = delay_ms,
            "WAL checkpoint could not complete; backing off"
        );
        return WalCheckpointOutcome::Failed;
    }

    let fully_drained = log_frames == backfilled;
    backoff.progressed(fully_drained);
    if !fully_drained {
        info!(
            wal_bytes,
            log_frames, backfilled, active_readers, "WAL checkpoint advanced"
        );
        return WalCheckpointOutcome::Advanced {
            log_frames,
            backfilled,
        };
    }

    // Only now is TRUNCATE safe to attempt: every frame is already in the
    // database, so the call has nothing to wait for. It can still lose a race
    // with a reader; the 100 ms busy timeout makes that a cheap no-op.
    if active_readers == 0
        && let Ok((0, frames, _)) = checkpoint("PRAGMA wal_checkpoint(TRUNCATE)")
    {
        let _ = conn.execute_batch("PRAGMA optimize;");
        info!(wal_bytes, frames, "reclaimed WAL after pressure threshold");
    }
    WalCheckpointOutcome::FullyDrained { log_frames }
}

/// The maintenance thread's lifetime. Signalling stops it; joining guarantees
/// its connection is gone before the next writer generation opens (a stale
/// connection cannot pin the WAL, but it can hold the checkpoint lock for a
/// tick, and "deterministic teardown" is cheaper to reason about than "it
/// would have been fine").
struct MaintenanceThread {
    stop: std::sync::mpsc::Sender<()>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl MaintenanceThread {
    fn spawn(db_path: PathBuf, reader_ages: Arc<ReaderAges>) -> Self {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let join = match std::thread::Builder::new()
            .name("nuo-storage-maintenance".into())
            .spawn(move || run_maintenance(db_path, reader_ages, stop_rx))
        {
            Ok(join) => Some(join),
            Err(error) => {
                warn!(
                    %error,
                    "storage maintenance thread could not start; \
                     the WAL is now bounded only by the retire checkpoint"
                );
                None
            }
        };
        Self {
            stop: stop_tx,
            join,
        }
    }
}

impl Drop for MaintenanceThread {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Open the maintenance connection. Not an engine: this thread runs no
/// migrations and never serves a command. It needs write access only for
/// checkpointing, and a short busy timeout so a lock race is a no-op.
fn open_maintenance_connection(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(MAINTENANCE_BUSY_TIMEOUT)?;
    Ok(conn)
}

fn run_maintenance(
    db_path: PathBuf,
    reader_ages: Arc<ReaderAges>,
    stop: std::sync::mpsc::Receiver<()>,
) {
    let mut conn: Option<Connection> = None;
    let mut next_open = Instant::now();
    let mut backoff = WalBackoff::new();
    let mut last_metrics = Instant::now() - METRICS_INTERVAL;

    while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = stop.recv_timeout(MAINTENANCE_TICK)
    {
        if conn.is_none() {
            if Instant::now() < next_open {
                continue;
            }
            match open_maintenance_connection(&db_path) {
                Ok(opened) => conn = Some(opened),
                Err(error) => {
                    next_open = Instant::now() + MAINTENANCE_OPEN_RETRY;
                    warn!(%error, "storage maintenance could not open its connection; retrying");
                    continue;
                }
            }
        }
        let Some(conn) = conn.as_ref() else { continue };

        if last_metrics.elapsed() >= METRICS_INTERVAL {
            last_metrics = Instant::now();
            log_storage_metrics(&db_path, conn, &reader_ages);
        }
        let _ = maintain_storage_pressure(
            &db_path,
            conn,
            &reader_ages,
            WAL_CHECKPOINT_THRESHOLD_BYTES,
            READER_AGE_WARN_MS,
            &mut backoff,
        );
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
            // The engine is owned as an `Option` so the `RetireEngine`
            // handshake can close it deterministically *before* the ack —
            // a fresh generation must never open while a wedged engine is
            // still live.
            let mut engine = Some(engine);
            let mut foreground = 0u32;
            // WAL pressure is managed by a thread with its own connection, so
            // no checkpoint can ever block the next command. Dropping the
            // guard stops and joins it before this generation exits.
            let mut maintenance = Some(MaintenanceThread::spawn(
                metrics_path.clone(),
                Arc::clone(&reader_ages),
            ));
            loop {
                if let Some(engine) = engine.as_ref()
                    && (rx.is_empty() || foreground >= 16)
                {
                    foreground = 0;
                    match crate::db::usage_projection::project_usage_batch(&engine.conn, 64) {
                        Ok(n) if n > 0 && rx.is_empty() => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => warn!(%error, "usage projection recovery failed"),
                        _ => {}
                    }
                }
                // Derived indexes (causal graph + FTS) are advanced only while
                // no command is waiting. A commit enqueues the session; this
                // drain — and an explicit flush on the read path — does the
                // indexing. Bounded so the next command never waits behind a
                // backlog.
                if rx.is_empty()
                    && let Some(engine) = engine.as_ref()
                {
                    match crate::db::derived::advance_batch(
                        &engine.conn,
                        crate::db::derived::IDLE_BATCH,
                    ) {
                        Ok(n) if n > 0 && rx.is_empty() => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Err(error) => warn!(%error, "derived index drain failed"),
                        _ => {}
                    }
                }
                let Some(command) = rx.blocking_recv() else {
                    break;
                };
                if let PersistenceCommand::RetireEngine { ack } = command {
                    // Stop the maintenance thread first: its connection must not
                    // contend with the retire checkpoint for the checkpoint
                    // lock, and it must not survive into the next generation.
                    drop(maintenance.take());
                    // Copy every WAL frame into the main database while the
                    // engine is still fully functional: the
                    // close-time checkpoint is a heuristic — it can be
                    // skipped, or deferred by close-v2 teardown — and the
                    // fresh generation must never depend on it. A live
                    // checkpoint against an unlinked WAL recovers the frames
                    // deterministically, because the fd-backed read and the
                    // main-database write are both still valid inodes.
                    if let Some(engine) = engine.take() {
                        match engine
                            .conn
                            .query_row("PRAGMA wal_checkpoint(FULL)", [], |row| {
                                Ok((
                                    row.get::<_, i64>(0)?,
                                    row.get::<_, i64>(1)?,
                                    row.get::<_, i64>(2)?,
                                ))
                            }) {
                            Ok((0, frames, copied)) => {
                                tracing::debug!(frames, copied, "retire checkpoint drained the WAL")
                            }
                            Ok((busy, frames, copied)) => warn!(
                                busy,
                                frames, copied, "retire checkpoint could not drain the WAL"
                            ),
                            Err(error) => {
                                warn!(%error, "retire checkpoint failed; frames may be stranded")
                            }
                        }
                        // Clean close: nothing left to checkpoint.
                        drop(engine);
                    }
                    let _ = ack.send(());
                    break;
                }
                foreground += 1;
                let Some(engine) = engine.as_ref() else {
                    break;
                };
                if !command.execute(engine) {
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

    /// The maintenance pass is safe in every state: no WAL, a pressured WAL,
    /// and a registered (over-age) reader snapshot. It must never panic and
    /// must leave the engine usable.
    #[test]
    fn maintenance_is_safe_in_every_wal_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuo.db");
        let engine = DatabaseEngine::open(&path, None).unwrap();
        let reader_ages = ReaderAges::default();
        // A registered snapshot is reported as over-age at any non-negative
        // bound.
        let _id = reader_ages.register();

        let mut backoff = WalBackoff::new();
        let outcome =
            maintain_storage_pressure(&path, &engine.conn, &reader_ages, 0, 0, &mut backoff);
        assert!(matches!(
            outcome,
            WalCheckpointOutcome::FullyDrained { .. } | WalCheckpointOutcome::Advanced { .. }
        ));

        let version: u32 = engine
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_DB_VERSION);
    }

    /// A completed pass schedules the next one; it never spins the writer.
    #[test]
    fn a_completed_pass_enters_backoff_before_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuo.db");
        let engine = DatabaseEngine::open(&path, None).unwrap();
        let reader_ages = ReaderAges::default();
        let mut backoff = WalBackoff::new();

        let first =
            maintain_storage_pressure(&path, &engine.conn, &reader_ages, 0, u64::MAX, &mut backoff);
        assert!(matches!(
            first,
            WalCheckpointOutcome::FullyDrained { .. } | WalCheckpointOutcome::Advanced { .. }
        ));
        let second =
            maintain_storage_pressure(&path, &engine.conn, &reader_ages, 0, u64::MAX, &mut backoff);
        assert_eq!(second, WalCheckpointOutcome::Deferred);
    }

    /// The incident's invariant: a snapshot the reader pool cannot see must not
    /// turn maintenance into a blocking wait, and must not make it choose
    /// TRUNCATE.
    ///
    /// This reproduces the 2026-11-07 shape exactly: `active_readers == 0` while
    /// SQLite reports a pinned WAL. The old policy selected
    /// `wal_checkpoint(TRUNCATE)` on the writer thread with a 5 s busy timeout
    /// and parked every queued write behind it.
    #[test]
    fn a_pinned_snapshot_never_makes_maintenance_wait() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuo.db");
        let engine = DatabaseEngine::open(&path, None).unwrap();
        engine
            .conn
            .execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();

        // A read snapshot opened *after* the first frame and held across the
        // next write. It is deliberately not a `DbReader`, so the pool's age
        // registry reports zero active readers — the exact false negative the
        // incident logs showed.
        let pinned = Connection::open(&path).unwrap();
        pinned.execute_batch("BEGIN DEFERRED").unwrap();
        let _: i64 = pinned
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        engine
            .conn
            .execute_batch("INSERT INTO t VALUES (2);")
            .unwrap();

        let maintenance = open_maintenance_connection(&path).unwrap();
        let reader_ages = ReaderAges::default();
        assert_eq!(reader_ages.active(), 0);
        let mut backoff = WalBackoff::new();

        let started = Instant::now();
        let outcome =
            maintain_storage_pressure(&path, &maintenance, &reader_ages, 0, u64::MAX, &mut backoff);
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "maintenance must not wait on a pinned snapshot; took {elapsed:?}"
        );
        match outcome {
            WalCheckpointOutcome::Advanced {
                log_frames,
                backfilled,
            } => assert!(
                backfilled < log_frames,
                "PASSIVE must stop at the snapshot's mark, not report a drained log"
            ),
            other => panic!("expected a partial PASSIVE checkpoint, got {other:?}"),
        }
        // The next attempt is on the ladder, not immediately.
        assert_eq!(
            maintain_storage_pressure(&path, &maintenance, &reader_ages, 0, u64::MAX, &mut backoff),
            WalCheckpointOutcome::Deferred
        );
    }

    /// Below the threshold the pass is a file-stat and nothing else.
    #[test]
    fn a_small_wal_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuo.db");
        let engine = DatabaseEngine::open(&path, None).unwrap();
        let reader_ages = ReaderAges::default();
        let mut backoff = WalBackoff::new();

        let outcome = maintain_storage_pressure(
            &path,
            &engine.conn,
            &reader_ages,
            u64::MAX,
            u64::MAX,
            &mut backoff,
        );
        assert_eq!(outcome, WalCheckpointOutcome::BelowThreshold);
    }
}
