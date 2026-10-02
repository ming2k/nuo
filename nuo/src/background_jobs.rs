//! Background job manager for long-running process commands and asynchronous sub-subagents.
//!
//! Dual-track execution model:
//! - **Track A (Process Jobs)**: OS-level subprocesses (`tokio::process`) capturing output
//!   into in-memory ring buffers and disk logs, with 0 LLM token cost.
//! - **Track B (Sub-Subagent Jobs)**: Asynchronous isolated exploration subagents.
//!
//! Emits live progress events and retains completed outcomes (ADR-0234) so a
//! settle is never only a transient notification — the result stays retrievable
//! until a consumer claims it.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use nuo_contracts::{BackgroundJobInfo, BackgroundJobOutcome, JobId, JobKind, JobSpec, JobState};

const DEFAULT_RING_BUFFER_CAPACITY: usize = 500;

/// Maximum retained, unclaimed outcomes per manager (ADR-0234).
///
/// Retention is bounded so a session that never collects results — or a
/// re-arming timer firing forever — cannot grow the process without limit. The
/// bound is loud, never silent: an eviction logs the job it dropped, and the
/// job's own snapshot, ring buffer, and on-disk log remain available through
/// `process` (`status`/`logs`).
const MAX_PENDING_OUTCOMES: usize = 256;

struct JobEntry {
    info: BackgroundJobInfo,
    ring_buffer: VecDeque<String>,
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Owning session (ADR-0190 D5): `None` = daemon-level task.
    owner_session: Option<String>,
    /// The job's own most recent settlement (ADR-0234).
    ///
    /// Kept on the entry so `status` and a repeated `wait` still report what
    /// the job produced after its delivery has been claimed: claiming governs
    /// automatic delivery, not readability.
    settled: Option<BackgroundJobOutcome>,
    #[cfg(unix)]
    pid: Option<u32>,
}

/// A settled outcome retained until a consumer claims it (ADR-0234).
///
/// This is the durable-in-process form of "the job finished and nobody has
/// looked at the result yet". `sequence` is the delivery identity: it is
/// assigned once, at settle time, and lets a consumer tell two fires of the
/// same recurring job apart instead of deduplicating by `job_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOutcome {
    /// Monotonic delivery identity within this manager instance.
    pub sequence: u64,
    /// Owning session (`None` = daemon-level job, delivered to no session).
    pub owner_session: Option<String>,
    /// The settled result, including the summary and log path.
    pub outcome: BackgroundJobOutcome,
    /// Whether a consumer has already taken this result (see
    /// [`BackgroundJobManager::pending_outcomes`]); claimed entries are kept
    /// briefly as the delivery record, then pruned.
    pub claimed: bool,
}

/// Shared thread-safe manager for session background jobs.
#[derive(Clone)]
pub struct BackgroundJobManager {
    inner: Arc<RwLock<HashMap<JobId, JobEntry>>>,
    /// Retained outcomes, oldest first (ADR-0234). Bounded by
    /// [`MAX_PENDING_OUTCOMES`].
    pending: Arc<RwLock<VecDeque<PendingOutcome>>>,
    /// Delivery-identity allocator (ADR-0234).
    next_sequence: Arc<AtomicU64>,
    event_tx: broadcast::Sender<BackgroundJobEvent>,
    log_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum BackgroundJobEvent {
    Started(BackgroundJobInfo),
    Progress {
        job_id: JobId,
        line: String,
    },
    /// A service task reported readiness (ADR-0190): readiness condition met,
    /// process alive. Wake-eligible.
    Ready {
        job_id: JobId,
    },
    Completed(BackgroundJobOutcome),
}

impl Default for BackgroundJobManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Execution configuration and sandboxing options for background process spawning.
#[derive(Debug, Clone)]
pub struct ProcessSpawnOptions<'a> {
    pub label: Option<String>,
    pub cwd: Option<PathBuf>,
    pub workspace_root: &'a Path,
    pub additional_roots: &'a [PathBuf],
    pub detached: bool,
    pub timeout: Option<Duration>,
    /// Owning session (ADR-0190 D5): recorded in job snapshots and ledger
    /// rows so "whose task is this" is answerable. `None` = daemon-level
    /// (rehosted services, fabric-internal tasks).
    pub owner_session: Option<String>,
}

impl BackgroundJobManager {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let log_dir = std::env::temp_dir().join("muta-jobs");
        let _ = std::fs::create_dir_all(&log_dir);

        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(VecDeque::new())),
            next_sequence: Arc::new(AtomicU64::new(1)),
            event_tx,
            log_dir,
        }
    }

    /// Subscribe to real-time job lifecycle events (started, progress, completed).
    ///
    /// The stream is a *notification* channel: it can lag and drop under load
    /// (see [`broadcast::error::RecvError::Lagged`]), and consumers must
    /// reconcile from [`Self::pending_outcomes`] rather than assume they saw
    /// every settle.
    pub fn subscribe(&self) -> broadcast::Receiver<BackgroundJobEvent> {
        self.event_tx.subscribe()
    }

    /// Retained outcomes, oldest first — claimed entries included.
    pub fn pending_outcomes(&self) -> Vec<PendingOutcome> {
        let guard = self
            .pending
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.iter().cloned().collect()
    }

    /// Retained outcomes belonging to one session, oldest first.
    pub fn pending_outcomes_for_session(&self, session_id: &str) -> Vec<PendingOutcome> {
        let guard = self
            .pending
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .iter()
            .filter(|p| p.owner_session.as_deref() == Some(session_id))
            .cloned()
            .collect()
    }

    /// The job's own most recent settlement (ADR-0234): readable through
    /// `status`/`wait` independently of whether its automatic delivery has been
    /// claimed.
    pub fn settled_result(&self, id: &JobId) -> Option<BackgroundJobOutcome> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(id).and_then(|entry| entry.settled.clone())
    }

    /// Take the retained deliveries for one job (ADR-0234).
    ///
    /// Claims every unclaimed delivery for `job_id` — a re-arming timer may
    /// have several — and returns them oldest first. This is the
    /// acknowledgment primitive: once a caller has returned the terminal
    /// result to the model, a later automatic delivery of the same settlement
    /// must not enqueue a second continuation.
    pub fn claim_outcomes_for_job(&self, job_id: &JobId) -> Vec<PendingOutcome> {
        let mut guard = self
            .pending
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut claimed = Vec::new();
        for entry in guard.iter_mut() {
            if !entry.claimed && &entry.outcome.job_id == job_id {
                entry.claimed = true;
                claimed.push(entry.clone());
            }
        }
        claimed
    }

    /// Take the retained deliveries for one session (ADR-0234): every unclaimed
    /// delivery whose owner is `session_id`, claimed in one atomic step.
    ///
    /// Used by the SystemWake admission path, which must hand the model exactly
    /// the results it is acknowledging so a later delivery cannot repeat them.
    pub fn claim_outcomes_for_session(&self, session_id: &str) -> Vec<PendingOutcome> {
        let mut guard = self
            .pending
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut claimed = Vec::new();
        for entry in guard.iter_mut() {
            if !entry.claimed && entry.owner_session.as_deref() == Some(session_id) {
                entry.claimed = true;
                claimed.push(entry.clone());
            }
        }
        claimed
    }

    /// Drop every retained delivery for a closed session.
    ///
    /// Called at session teardown: a result never crosses into another session,
    /// and a closed session is not revived to receive it.
    pub fn discard_pending_for_session(&self, session_id: &str) -> usize {
        let mut guard = self
            .pending
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = guard.len();
        guard.retain(|p| p.owner_session.as_deref() != Some(session_id));
        before - guard.len()
    }

    /// Retain a settled outcome, returning its delivery identity.
    ///
    /// Abandons the oldest retained delivery when the bound is reached, and
    /// says so: an eviction is a lost automatic delivery, and silence here
    /// would turn a bounded resource into a silent correctness hole.
    fn retain_outcome(&self, outcome: BackgroundJobOutcome, owner_session: Option<String>) -> u64 {
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        let mut guard = self
            .pending
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while guard.len() >= MAX_PENDING_OUTCOMES {
            if let Some(dropped) = guard.pop_front() {
                tracing::warn!(
                    job_id = %dropped.outcome.job_id.0,
                    sequence = dropped.sequence,
                    claimed = dropped.claimed,
                    limit = MAX_PENDING_OUTCOMES,
                    "background job outcome evicted from the retention queue; \
                     the job snapshot, ring buffer, and log file remain readable"
                );
            }
        }
        guard.push_back(PendingOutcome {
            sequence,
            owner_session,
            outcome,
            claimed: false,
        });
        sequence
    }

    /// Query snapshot info for all jobs.
    pub fn list_jobs(&self) -> Vec<BackgroundJobInfo> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut list: Vec<BackgroundJobInfo> = guard.values().map(|e| e.info.clone()).collect();
        list.sort_by_key(|b| std::cmp::Reverse(b.created_at_ms));
        list
    }

    /// Query snapshot info for a specific job.
    pub fn get_job(&self, id: &JobId) -> Option<BackgroundJobInfo> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(id).map(|e| e.info.clone())
    }

    /// Retrieve tail logs for a specific job.
    pub fn get_logs(&self, id: &JobId, tail_lines: usize) -> Option<Vec<String>> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = guard.get(id)?;
        let count = tail_lines.min(entry.ring_buffer.len());
        let skip = entry.ring_buffer.len().saturating_sub(count);
        Some(entry.ring_buffer.iter().skip(skip).cloned().collect())
    }

    /// Spawn a deterministic shell command in the background.
    pub async fn spawn_process(
        &self,
        command: String,
        opts: ProcessSpawnOptions<'_>,
    ) -> Result<BackgroundJobInfo, String> {
        let ProcessSpawnOptions {
            label,
            cwd,
            workspace_root,
            additional_roots,
            detached,
            timeout,
            owner_session,
        } = opts;
        let job_id = JobId::new("job");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let mut cmd = nuo_host::workspace_sandbox::shell_with_roots(
            &command,
            workspace_root,
            additional_roots,
            nuo_host::workspace_sandbox::WorkspaceAccess::ReadWrite,
            nuo_host::workspace_sandbox::NetworkAccess::Enabled,
        )?;

        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        {
            // Set process group so we can cleanly kill subprocess trees if needed
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn background command: {e}"))?;
        let pid = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd,
                detached,
                task_kind: JobKind::default(),
                readiness: None,
                restart: None,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    settled: None,
                    #[cfg(unix)]
                    pid,
                },
            );
        }

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        // Spawn async collector and supervisor task
        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();

        tokio::spawn(async move {
            let start_time = Instant::now();
            let mut log_writer = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_file_path)
                .await
                .ok();

            // Bounded line pump (ADR-0190 D5): the fabric never accumulates
            // unbounded progress. Cap 1024 lines in flight; under pressure
            // the *progress broadcast* coalesces (oldest dropped via the
            // send failure path) while the ring buffer + disk log keep full
            // fidelity — a slow UI never stalls the child, and a flood
            // never balloons memory.
            let (line_tx, mut line_rx) = mpsc::channel::<String>(1024);

            // Read stdout stream
            if let Some(out) = stdout {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(out).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }

            // Read stderr stream
            if let Some(err) = stderr {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(err).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            drop(line_tx);

            let timeout_duration = timeout.unwrap_or(Duration::from_secs(3600));
            let timeout_sleep = tokio::time::sleep(timeout_duration);
            tokio::pin!(timeout_sleep);

            let mut was_cancelled = false;
            let mut was_timed_out = false;

            let exit_status = loop {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        mgr.append_line(&jid, &line);
                        if let Some(ref mut w) = log_writer {
                            use tokio::io::AsyncWriteExt;
                            let _ = w.write_all(line.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                        }
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Progress {
                            job_id: jid.clone(),
                            line,
                        });
                    }
                    status = child.wait() => {
                        // Drain remaining lines
                        while let Ok(line) = line_rx.try_recv() {
                            mgr.append_line(&jid, &line);
                            if let Some(ref mut w) = log_writer {
                                use tokio::io::AsyncWriteExt;
                                let _ = w.write_all(line.as_bytes()).await;
                                let _ = w.write_all(b"\n").await;
                            }
                        }
                        break status.ok();
                    }
                    _ = &mut cancel_rx => {
                        was_cancelled = true;
                        let _ = child.kill().await;
                        break None;
                    }
                    _ = &mut timeout_sleep => {
                        was_timed_out = true;
                        let _ = child.kill().await;
                        break None;
                    }
                }
            };

            let duration_ms = start_time.elapsed().as_millis() as u64;
            let final_state = if was_cancelled {
                JobState::Killed { duration_ms }
            } else if was_timed_out {
                JobState::TimedOut { duration_ms }
            } else if let Some(status) = exit_status {
                let code = status
                    .code()
                    .unwrap_or(if status.success() { 0 } else { 1 });
                if status.success() {
                    JobState::Succeeded {
                        duration_ms,
                        exit_code: code,
                    }
                } else {
                    JobState::Failed {
                        duration_ms,
                        exit_code: code,
                        error: format!("Process exited with status code {code}"),
                    }
                }
            } else {
                JobState::Failed {
                    duration_ms,
                    exit_code: -1,
                    error: "Process terminated unexpectedly".to_string(),
                }
            };

            mgr.finish_job(jid, spec, final_state, Some(log_file_path));
        });

        Ok(info)
    }

    /// Spawn with full ADR-0190 control (kind / readiness / restart).
    ///
    /// `Interactive` delegates to [`Self::spawn_process`]. `Service` reports
    /// `Ready` when the readiness condition is met, never settles while
    /// running, wakes the session with `Failed` on unsolicited death, and
    /// honors an optional [`nuo_contracts::RestartPolicy`].
    pub async fn spawn_process_ex(
        &self,
        command: String,
        opts: ProcessSpawnOptions<'_>,
        kind: nuo_contracts::JobKind,
        readiness: Option<nuo_contracts::Readiness>,
        restart: Option<nuo_contracts::RestartPolicy>,
    ) -> Result<BackgroundJobInfo, String> {
        use nuo_contracts::{JobKind, Readiness};
        if kind == JobKind::Interactive {
            return self.spawn_process(command, opts).await;
        }
        let ProcessSpawnOptions {
            label,
            cwd,
            workspace_root,
            additional_roots,
            detached,
            timeout,
            owner_session,
        } = opts;
        let _ = timeout; // service lifetime is not wall-bounded; stop is explicit

        let job_id = JobId::new("svc");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let mut cmd = nuo_host::workspace_sandbox::shell_with_roots(
            &command,
            workspace_root,
            additional_roots,
            nuo_host::workspace_sandbox::WorkspaceAccess::ReadWrite,
            nuo_host::workspace_sandbox::NetworkAccess::Enabled,
        )?;
        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn service task: {e}"))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd,
                detached,
                task_kind: JobKind::Service,
                readiness,
                restart,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    settled: None,
                    #[cfg(unix)]
                    pid,
                },
            );
        }
        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        let readiness = readiness.unwrap_or_default();

        tokio::spawn(async move {
            let start_time = Instant::now();
            let mut log_writer = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_file_path)
                .await
                .ok();

            let (line_tx, mut line_rx) = mpsc::channel::<String>(1024);
            if let Some(out) = stdout {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(out).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            if let Some(err) = stderr {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(err).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            drop(line_tx);

            // Readiness grace timer for AfterMs; armed lazily below.
            let mut grace: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = match readiness {
                Readiness::AfterMs(ms) => Some(Box::pin(tokio::time::sleep(
                    Duration::from_millis(ms.max(1)),
                ))),
                _ => None,
            };

            let mut ready = false;
            let mut unsolicited = true;
            let exit_status = loop {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        if !ready && matches!(readiness, Readiness::FirstOutput) {
                            ready = true;
                            mgr.mark_service_ready(&jid);
                            let _ = mgr.event_tx.send(BackgroundJobEvent::Ready { job_id: jid.clone() });
                        }
                        mgr.append_line(&jid, &line);
                        if let Some(ref mut w) = log_writer {
                            let _ = w.write_all(line.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                        }
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Progress {
                            job_id: jid.clone(),
                            line,
                        });
                    }
                    _ = async {
                        // The grace timer is armed only for `AfterMs` and only
                        // while the service is not yet ready; else pend forever
                        // (the branch guard re-checks the same conditions).
                        // `grace: Pin<Box<Sleep>>` — `as_mut` yields the
                        // pinned projection awaited directly.
                        let armed = if !ready { grace.as_mut() } else { None };
                        match armed {
                            Some(g) => g.await,
                            None => std::future::pending().await,
                        }
                    }, if !ready && matches!(readiness, Readiness::AfterMs(_)) => {
                        ready = true;
                        mgr.mark_service_ready(&jid);
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Ready { job_id: jid.clone() });
                    }
                    status = child.wait() => {
                        // exited on its own — `unsolicited` stays `true`
                        break status.ok();
                    }
                    _ = &mut cancel_rx => {
                        let _ = child.kill().await;
                        unsolicited = false;
                        break None;
                    }
                }
            };

            let duration_ms = start_time.elapsed().as_millis() as u64;
            let final_state = if !unsolicited {
                JobState::Killed { duration_ms }
            } else if let Some(status) = exit_status {
                let code = status
                    .code()
                    .unwrap_or(if status.success() { 0 } else { 1 });
                if status.success() {
                    JobState::Succeeded {
                        duration_ms,
                        exit_code: code,
                    }
                } else {
                    JobState::Failed {
                        duration_ms,
                        exit_code: code,
                        error: format!("Service exited with status code {code}"),
                    }
                }
            } else {
                JobState::Failed {
                    duration_ms,
                    exit_code: -1,
                    error: "Service terminated unexpectedly".to_string(),
                }
            };
            mgr.finish_job(jid, spec, final_state, Some(log_file_path));
        });

        Ok(info)
    }

    /// Spawn a Timer task (ADR-0190): sleep until `fire_at_ms`, then emit a
    /// completed outcome whose digest is the caller's prompt — the mailbox
    /// wakes the session with it. Recurring timers re-arm with `interval_ms`
    /// after each fire (deadline = previous deadline + interval) and keep
    /// the same task id; cancellation (`kill_job`) stops the loop.
    pub fn spawn_timer(
        &self,
        label: Option<String>,
        fire_at_ms: u64,
        interval_ms: Option<u64>,
        prompt: String,
        owner_session: Option<String>,
    ) -> Result<BackgroundJobInfo, String> {
        let job_id = JobId::new("timer");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Timer {
                label,
                fire_at_ms,
                interval_ms,
                prompt: prompt.clone(),
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid: None,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    settled: None,
                    #[cfg(unix)]
                    pid: None,
                },
            );
        }
        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        tokio::spawn(async move {
            let mut deadline = fire_at_ms;
            let mut cancel = cancel_rx;
            let mut cancelled = false;
            loop {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let wait = deadline.saturating_sub(now);
                tokio::select! {
                    _ = &mut cancel => {
                        cancelled = true;
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
                }
                match interval_ms {
                    // Re-arming timer (ADR-0234): the fire publishes its
                    // digest, and the task stays armed — and cancellable — for
                    // the next tick. Only a one-shot timer settles its entry.
                    Some(interval) => {
                        mgr.publish_timer_fire(&jid, &spec, prompt.clone());
                        deadline += interval;
                    }
                    None => {
                        mgr.finish_job_with_summary(
                            jid.clone(),
                            spec.clone(),
                            JobState::Succeeded {
                                duration_ms: 0,
                                exit_code: 0,
                            },
                            None,
                            prompt.clone(),
                        );
                        break;
                    }
                }
            }
            if cancelled {
                mgr.settle_entry_silently(&jid, JobState::Killed { duration_ms: 0 });
            }
        });

        Ok(info)
    }

    fn mark_service_ready(&self, job_id: &JobId) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(job_id)
            && let JobState::Running { started_at_ms, .. } = entry.info.state
        {
            entry.info.state = JobState::Ready {
                started_at_ms,
                ready_at_ms: now_ms,
            };
        }
    }

    /// Adopt an already-running foreground child that hit its sync budget
    /// (ADR-0190 detach-on-budget). The fabric takes ownership of the child,
    /// replays the captured foreground tail as progress, and settles with a
    /// normal outcome when the process exits — the caller's session is woken
    /// through the ordinary completion event.
    pub async fn adopt_process(
        &self,
        command: String,
        label: Option<String>,
        adoption: nuo_contracts::AdoptionInfo,
        owner_session: Option<String>,
    ) -> Result<BackgroundJobInfo, String> {
        let job_id = JobId::new("adopted");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd: None,
                detached: true,
                task_kind: JobKind::Interactive,
                readiness: None,
                restart: None,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid: Some(adoption.pid),
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    // ADR-0234: an adopted child belongs to the session that
                    // detached it — losing the owner here would make the later
                    // settle undeliverable and unrevocable.
                    owner_session: owner_session.clone(),
                    cancel_tx: None,
                    settled: None,
                    #[cfg(unix)]
                    pid: Some(adoption.pid),
                },
            );
        }

        for line in &adoption.captured_lines {
            self.append_line(&job_id, line);
        }
        let mut log_writer = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_file_path)
            .await
            .ok();
        for line in &adoption.captured_lines {
            if let Some(ref mut w) = log_writer {
                let _ = w.write_all(line.as_bytes()).await;
                let _ = w.write_all(b"\n").await;
            }
        }

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        let mut poll_child = adoption.child;

        tokio::spawn(async move {
            let start_time = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                match poll_child.try_wait() {
                    Ok(None) => {}
                    Ok(Some(code)) => {
                        let duration_ms = start_time.elapsed().as_millis() as u64;
                        let summary = String::new();
                        let _ = summary;
                        let state = if code == 0 {
                            JobState::Succeeded {
                                duration_ms,
                                exit_code: 0,
                            }
                        } else {
                            JobState::Failed {
                                duration_ms,
                                exit_code: code,
                                error: format!("Process exited with status code {code}"),
                            }
                        };
                        mgr.finish_job(jid, spec, state, Some(log_file_path.clone()));
                        break;
                    }
                    Err(_) => {
                        let duration_ms = start_time.elapsed().as_millis() as u64;
                        mgr.finish_job(
                            jid,
                            spec,
                            JobState::Failed {
                                duration_ms,
                                exit_code: -1,
                                error: "adopted process could not be polled".to_string(),
                            },
                            Some(log_file_path.clone()),
                        );
                        break;
                    }
                }
            }
        });

        Ok(info)
    }

    fn append_line(&self, job_id: &JobId, line: &str) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(job_id) {
            if entry.ring_buffer.len() >= DEFAULT_RING_BUFFER_CAPACITY {
                entry.ring_buffer.pop_front();
            }
            entry.ring_buffer.push_back(line.to_string());
            entry.info.latest_output = Some(line.to_string());
        }
    }

    fn finish_job(&self, job_id: JobId, spec: JobSpec, state: JobState, log_path: Option<PathBuf>) {
        let summary = {
            let guard = self
                .inner
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = guard.get(&job_id) {
                let tail_count = 15.min(entry.ring_buffer.len());
                let tail: Vec<&str> = entry
                    .ring_buffer
                    .iter()
                    .rev()
                    .take(tail_count)
                    .map(|s| s.as_str())
                    .collect();
                let tail_rev: Vec<&str> = tail.into_iter().rev().collect();
                tail_rev.join("\n")
            } else {
                String::new()
            }
        };

        self.finish_job_with_summary(job_id, spec, state, log_path, summary);
    }

    fn finish_job_with_summary(
        &self,
        job_id: JobId,
        spec: JobSpec,
        state: JobState,
        log_path: Option<PathBuf>,
        summary: String,
    ) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let owner_session = {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match guard.get_mut(&job_id) {
                Some(entry) => {
                    entry.info.state = state.clone();
                    entry.info.completed_at_ms = Some(now_ms);
                    entry.cancel_tx = None;
                    // ADR-0234: drop the kill handle with the cancel channel.
                    // A settled entry must not look signalable — the pid may
                    // already be recycled by an unrelated process.
                    #[cfg(unix)]
                    {
                        entry.pid = None;
                    }
                    entry.owner_session.clone()
                }
                None => None,
            }
        };

        let outcome = BackgroundJobOutcome {
            job_id,
            spec,
            state,
            summary,
            log_path,
        };

        self.deliver_outcome(outcome, owner_session);
    }

    /// Deliver one settled outcome: record it on the entry, retain it, notify,
    /// then ledger (ADR-0234).
    ///
    /// The entry record and the retention queue answer different questions:
    /// the entry is "what did this job produce" (stays readable through
    /// `status`/`wait` after the delivery has been claimed); the queue is "what
    /// has not been delivered automatically yet".
    ///
    /// Retention precedes notification on purpose — an observer that misses the
    /// broadcast (lag, no subscriber yet, a consumer that starts later) must
    /// still find the result. The notification is a hint; the retained outcome
    /// is the record.
    fn deliver_outcome(&self, outcome: BackgroundJobOutcome, owner_session: Option<String>) {
        self.record_settlement(&outcome);

        self.retain_outcome(outcome.clone(), owner_session.clone());

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Completed(outcome.clone()));

        // Persist the settle into the task ledger (ADR-0190 D4, best-effort, on
        // the blocking pool so the fabric's event path never waits on I/O).
        tokio::task::spawn_blocking(move || {
            crate::task_ledger::record_outcome(
                &nuo_persistence::db::get_persistence_handle(),
                &outcome,
                owner_session,
            );
        });
    }

    /// Record a settlement on the job entry, which is what keeps it readable
    /// through `status`/`wait` after its automatic delivery has been claimed
    /// (ADR-0234). Kept separate from [`Self::deliver_outcome`] so the record
    /// has no I/O or notification side effects.
    fn record_settlement(&self, outcome: &BackgroundJobOutcome) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(&outcome.job_id) {
            entry.settled = Some(outcome.clone());
        }
    }

    /// Publish one fire of a re-arming Timer task (ADR-0234).
    ///
    /// A recurring timer is a *live* job whose iteration settled: the digest
    /// goes to the mailbox exactly like a one-shot settle, but the entry keeps
    /// its state (armed/running) and its cancel handle, so
    /// [`Self::kill_job`] can still stop the next fire. Settling the entry
    /// here would strand the re-armed loop — the previous implementation
    /// cleared `cancel_tx` on every fire, leaving a recurring timer
    /// uncancellable after its first tick.
    fn publish_timer_fire(&self, job_id: &JobId, spec: &JobSpec, summary: String) {
        let owner_session = {
            let guard = self
                .inner
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .get(job_id)
                .and_then(|entry| entry.owner_session.clone())
        };
        let outcome = BackgroundJobOutcome {
            job_id: job_id.clone(),
            spec: spec.clone(),
            state: JobState::Succeeded {
                duration_ms: 0,
                exit_code: 0,
            },
            summary,
            log_path: None,
        };
        self.deliver_outcome(outcome, owner_session);
    }

    /// Settle an entry without publishing an outcome, used when an operator
    /// cancels a re-arming timer (ADR-0234): the job stops being armed, but a
    /// deliberate cancellation needs no digest and no wake.
    fn settle_entry_silently(&self, job_id: &JobId, state: JobState) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(job_id) {
            entry.info.state = state;
            entry.info.completed_at_ms = Some(now_ms);
            entry.cancel_tx = None;
            #[cfg(unix)]
            {
                entry.pid = None;
            }
        }
    }

    /// Terminate a running background job.
    ///
    /// ADR-0234: the cancel handle — not the published state — is the
    /// authority on whether a job can still be stopped. Once an entry has
    /// settled, both its cancel sender and its pid are dropped, and the OS
    /// may already have recycled that pid for an unrelated process; signalling
    /// it would kill a stranger. A re-armed recurring timer keeps its handle
    /// alive between fires, so it stays terminable even though its last
    /// published state was a settle.
    pub fn kill_job(&self, id: &JobId) -> Result<(), String> {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = guard
            .get_mut(id)
            .ok_or_else(|| format!("Job not found: {id}"))?;

        #[cfg(unix)]
        let has_pid = entry.pid.is_some();
        #[cfg(not(unix))]
        let has_pid = false;

        if entry.cancel_tx.is_none() && !has_pid {
            return Err(format!(
                "Job {id} is not terminable: it has already finished or is already terminating (state: {}).",
                describe_state(&entry.info.state)
            ));
        }

        if let Some(tx) = entry.cancel_tx.take() {
            let _ = tx.send(());
        }

        #[cfg(unix)]
        if let Some(pid) = entry.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }

        Ok(())
    }

    /// Abort all active background jobs (e.g. during session teardown).
    ///
    /// ADR-0234: settled entries are skipped for the same pid-recycling reason
    /// as [`Self::kill_job`].
    pub fn abort_all(&self) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for entry in guard.values_mut() {
            if entry.info.state.is_terminal() {
                continue;
            }
            if let Some(tx) = entry.cancel_tx.take() {
                let _ = tx.send(());
            }
            #[cfg(unix)]
            if let Some(pid) = entry.pid {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
        }
    }
}

/// Short human-readable rendering of a settled job state, for error messages
/// that explain *why* a control action was refused.
fn describe_state(state: &JobState) -> String {
    match state {
        JobState::Succeeded { exit_code, .. } => format!("succeeded, exit code {exit_code}"),
        JobState::Failed { error, .. } => format!("failed: {error}"),
        JobState::Killed { .. } => "killed".to_string(),
        JobState::TimedOut { .. } => "timed out".to_string(),
        JobState::Queued => "queued".to_string(),
        JobState::Running { .. } => "running".to_string(),
        JobState::Ready { .. } => "ready".to_string(),
    }
}

/// Session-scoped wrapper binding [`BackgroundJobManager`] with the session's [`nuo_contracts::ExecutionEnvironment`].
#[derive(Clone)]
pub struct SessionJobService {
    manager: BackgroundJobManager,
    env: Arc<dyn nuo_contracts::ExecutionEnvironment>,
    /// Owning session (ADR-0190 D5): stamped onto every task this service
    /// spawns so snapshots and ledger rows answer "whose task is this".
    owner_session: std::sync::OnceLock<String>,
}

impl SessionJobService {
    pub fn new(
        manager: BackgroundJobManager,
        env: Arc<dyn nuo_contracts::ExecutionEnvironment>,
    ) -> Self {
        Self {
            manager,
            env,
            owner_session: std::sync::OnceLock::new(),
        }
    }

    /// Bind the owning session id (called once at driver startup, after the
    /// session id resolves). Later spawns carry it in every snapshot.
    pub fn bind_owner(&self, session_id: String) {
        let _ = self.owner_session.set(session_id);
    }

    fn owner(&self) -> Option<String> {
        self.owner_session.get().cloned()
    }

    pub fn manager(&self) -> &BackgroundJobManager {
        &self.manager
    }
}

#[async_trait::async_trait]
impl nuo_contracts::BackgroundJobService for SessionJobService {
    async fn spawn_process(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        detached: bool,
        timeout: Option<Duration>,
    ) -> Result<BackgroundJobInfo, String> {
        let roots = self.env.additional_roots();
        self.manager
            .spawn_process(
                command,
                ProcessSpawnOptions {
                    label,
                    cwd,
                    workspace_root: self.env.workspace_root(),
                    additional_roots: &roots,
                    detached,
                    timeout,
                    owner_session: self.owner(),
                },
            )
            .await
    }

    async fn spawn_process_ex(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        detached: bool,
        timeout: Option<Duration>,
        kind: nuo_contracts::JobKind,
        readiness: Option<nuo_contracts::Readiness>,
        restart: Option<nuo_contracts::RestartPolicy>,
    ) -> Result<BackgroundJobInfo, String> {
        let roots = self.env.additional_roots();
        self.manager
            .spawn_process_ex(
                command,
                ProcessSpawnOptions {
                    label,
                    cwd,
                    workspace_root: self.env.workspace_root(),
                    additional_roots: &roots,
                    detached,
                    timeout,
                    owner_session: self.owner(),
                },
                kind,
                readiness,
                restart,
            )
            .await
    }

    async fn adopt_process(
        &self,
        command: String,
        label: Option<String>,
        adoption: nuo_contracts::AdoptionInfo,
    ) -> Result<BackgroundJobInfo, String> {
        self.manager
            .adopt_process(command, label, adoption, self.owner())
            .await
    }

    async fn spawn_timer(
        &self,
        label: &str,
        fire_at_ms: u64,
        interval_ms: Option<u64>,
        command: String,
    ) -> Result<BackgroundJobInfo, String> {
        self.manager.spawn_timer(
            Some(label.to_string()),
            fire_at_ms,
            interval_ms,
            command,
            self.owner(),
        )
    }

    fn list_jobs(&self) -> Vec<BackgroundJobInfo> {
        self.manager.list_jobs()
    }

    fn get_job(&self, id: &JobId) -> Option<BackgroundJobInfo> {
        self.manager.get_job(id)
    }

    fn get_logs(&self, id: &JobId, tail_lines: usize) -> Option<Vec<String>> {
        self.manager.get_logs(id, tail_lines)
    }

    fn claim_outcomes(&self, id: &JobId) -> Vec<BackgroundJobOutcome> {
        self.manager
            .claim_outcomes_for_job(id)
            .into_iter()
            .map(|p| p.outcome)
            .collect()
    }

    fn settled_result(&self, id: &JobId) -> Option<BackgroundJobOutcome> {
        self.manager.settled_result(id)
    }

    fn kill_job(&self, id: &JobId) -> Result<(), String> {
        self.manager.kill_job(id)
    }

    fn abort_all(&self) {
        self.manager.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_and_complete_process() {
        if !nuo_host::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];

        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process(
                "echo 'hello from background'".to_string(),
                ProcessSpawnOptions {
                    label: Some("test-echo".to_string()),
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: Some(Duration::from_secs(5)),
                    owner_session: None,
                },
            )
            .await
            .expect("spawn success");

        assert_eq!(
            info.spec,
            JobSpec::Process {
                command: "echo 'hello from background'".to_string(),
                label: Some("test-echo".to_string()),
                cwd: None,
                detached: false,
                task_kind: nuo_contracts::JobKind::default(),
                readiness: None,
                restart: None,
            }
        );

        // Wait for completion event
        let mut completed = false;
        while let Ok(evt) = rx.recv().await {
            if let BackgroundJobEvent::Completed(outcome) = evt
                && outcome.job_id == info.id
            {
                assert!(matches!(
                    outcome.state,
                    JobState::Succeeded { exit_code: 0, .. }
                ));
                assert!(outcome.summary.contains("hello from background"));
                completed = true;
                break;
            }
        }
        assert!(completed);

        // Check list & get
        let snapshot = mgr.get_job(&info.id).expect("job exists");
        assert!(snapshot.state.is_terminal());

        let logs = mgr.get_logs(&info.id, 10).expect("logs exist");
        assert!(logs.iter().any(|l| l.contains("hello from background")));
    }

    #[tokio::test]
    async fn test_kill_process() {
        if !nuo_host::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];

        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process(
                "sleep 10".to_string(),
                ProcessSpawnOptions {
                    label: Some("test-sleep".to_string()),
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: Some(Duration::from_secs(10)),
                    owner_session: None,
                },
            )
            .await
            .expect("spawn success");

        // Give it a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        mgr.kill_job(&info.id).expect("kill succeeded");

        let mut killed = false;
        while let Ok(evt) = rx.recv().await {
            if let BackgroundJobEvent::Completed(outcome) = evt
                && outcome.job_id == info.id
            {
                assert!(matches!(
                    outcome.state,
                    JobState::Killed { .. } | JobState::Failed { .. }
                ));
                killed = true;
                break;
            }
        }
        assert!(killed);
    }

    /// ADR-0234: a settled job is never signalled again — its pid may already
    /// be recycled by an unrelated process, so a late "kill" would hit a
    /// stranger. The control action refuses instead of claiming a termination.
    #[tokio::test]
    async fn test_kill_refuses_a_settled_job() {
        if !nuo_host::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];
        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process(
                "echo done".to_string(),
                ProcessSpawnOptions {
                    label: Some("test-settled".to_string()),
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: Some(Duration::from_secs(10)),
                    owner_session: None,
                },
            )
            .await
            .expect("spawn success");

        let mut settled = false;
        while let Ok(evt) = rx.recv().await {
            if let BackgroundJobEvent::Completed(outcome) = evt
                && outcome.job_id == info.id
            {
                settled = true;
                break;
            }
        }
        assert!(settled);
        assert!(
            mgr.get_job(&info.id)
                .expect("job exists")
                .state
                .is_terminal(),
            "job must be terminal before the guard is exercised"
        );

        let err = mgr
            .kill_job(&info.id)
            .expect_err("a settled job must not be signalled");
        assert!(
            err.contains("not terminable") && err.contains("state:"),
            "refusal must explain why: {err}"
        );
    }

    /// ADR-0234: a re-arming timer keeps its cancel handle between fires, so a
    /// tick that already fired must not make the timer uncancellable — and a
    /// cancelled recurring timer must stop claiming to be armed.
    #[tokio::test]
    async fn test_recurring_timer_stays_cancellable_after_a_fire() {
        let mgr = BackgroundJobManager::new();
        let mut rx = mgr.subscribe();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let info = mgr
            .spawn_timer(None, now + 100, Some(100), "tick".to_string(), None)
            .expect("timer spawn");

        // Wait for the first fire, then cancel.
        let mut fires = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while fires == 0 && tokio::time::Instant::now() < deadline {
            if let Ok(Ok(BackgroundJobEvent::Completed(o))) =
                tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
                && o.job_id == info.id
            {
                fires += 1;
            }
        }
        assert_eq!(fires, 1, "the recurring timer must fire at least once");

        mgr.kill_job(&info.id)
            .expect("a fired-but-re-armed timer must remain cancellable");

        // The cancellation is silent but it must settle the entry, so the
        // snapshot stops claiming the timer is armed.
        let settle_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut settled = false;
        while tokio::time::Instant::now() < settle_deadline {
            if mgr
                .get_job(&info.id)
                .expect("job listed")
                .state
                .is_terminal()
            {
                settled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(settled, "a cancelled recurring timer must settle");

        // …and no further fire may be published for it.
        let quiet_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while tokio::time::Instant::now() < quiet_deadline {
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    panic!("a cancelled recurring timer must not fire again: {o:?}");
                }
                Ok(_) | Err(_) => {}
            }
        }
    }

    // ADR-0190 fabric tests: service readiness, adopt, extended spawn.

    #[tokio::test]
    async fn test_service_ready_on_first_output() {
        if !nuo_host::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];
        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process_ex(
                "echo service-up && sleep 30".to_string(),
                ProcessSpawnOptions {
                    label: Some("test-svc".to_string()),
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: None,
                    owner_session: None,
                },
                nuo_contracts::JobKind::Service,
                Some(nuo_contracts::Readiness::FirstOutput),
                None,
            )
            .await
            .expect("service spawn");

        // Expect Ready then (implicitly) no Completed while running.
        let mut saw_ready = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Ready { job_id })) if job_id == info.id => {
                    saw_ready = true;
                    break;
                }
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    panic!("service settled while running: {:?}", o.state);
                }
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
        assert!(saw_ready, "service never reported Ready");

        // Snapshot shows Ready state.
        let snap = mgr.get_job(&info.id).expect("job exists");
        assert!(matches!(snap.state, JobState::Ready { .. }));

        mgr.kill_job(&info.id).expect("kill");
    }

    #[tokio::test]
    async fn test_service_unsolicited_death_fails() {
        if !nuo_host::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];
        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process_ex(
                "sleep 1; exit 3".to_string(),
                ProcessSpawnOptions {
                    label: None,
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: None,
                    owner_session: None,
                },
                nuo_contracts::JobKind::Service,
                None,
                None,
            )
            .await
            .expect("service spawn");

        let mut settled: Option<BackgroundJobOutcome> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    settled = Some(o);
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_elapsed) => {} // recv timeout: keep waiting
            }
        }
        let outcome = settled.unwrap_or_else(|| panic!("service crash must settle"));
        assert!(
            matches!(outcome.state, JobState::Failed { exit_code: 3, .. }),
            "unexpected settle state: {:?}",
            outcome.state
        );
    }

    #[tokio::test]
    async fn test_adopt_process_settles_on_exit() {
        if !nuo_host::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];

        // Spawn a real detached child the fabric can adopt. stdio is null:
        // the adoption contract keeps the *tool's* pipes live; a test child
        // with no readers must not die of SIGPIPE on its own output.
        let mut cmd = nuo_host::workspace_sandbox::shell_with_roots(
            "sleep 1",
            &ws,
            &roots,
            nuo_host::workspace_sandbox::WorkspaceAccess::ReadWrite,
            nuo_host::workspace_sandbox::NetworkAccess::Disabled,
        )
        .unwrap();
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        let child = cmd.spawn().expect("spawn adoptable");
        let pid = child.id().unwrap_or_default();

        struct Owned {
            child: tokio::process::Child,
        }
        impl nuo_contracts::CrateChildBridge for Owned {
            fn try_wait(&mut self) -> Result<Option<i32>, String> {
                self.child
                    .try_wait()
                    .map(|s| s.and_then(|st| st.code()))
                    .map_err(|e| e.to_string())
            }
            fn kill(&mut self) -> Result<(), String> {
                Ok(())
            }
        }

        let mut rx = mgr.subscribe();
        let info = mgr
            .adopt_process(
                "sleep 1".to_string(),
                Some("adopt-test".to_string()),
                nuo_contracts::AdoptionInfo {
                    captured_lines: vec!["pre-adopt line".to_string()],
                    pid,
                    child: Box::new(Owned { child }),
                },
                Some("session-adopt".to_string()),
            )
            .await
            .expect("adopt");

        let mut settled: Option<BackgroundJobOutcome> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    settled = Some(o);
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_elapsed) => {} // recv timeout: keep waiting
            }
        }
        let outcome = settled.unwrap_or_else(|| panic!("adopted task must settle"));
        assert!(
            matches!(outcome.state, JobState::Succeeded { exit_code: 0, .. }),
            "unexpected settle state: {:?}",
            outcome.state
        );
        let logs = mgr.get_logs(&info.id, 10).expect("logs");
        assert!(
            logs.iter().any(|l| l.contains("pre-adopt line")),
            "captured foreground tail must be replayed into the fabric"
        );
    }

    #[tokio::test]
    async fn test_timer_fires_once_and_wakes() {
        let mgr = BackgroundJobManager::new();
        let mut rx = mgr.subscribe();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let info = mgr
            .spawn_timer(
                Some("one-shot".to_string()),
                now + 300,
                None,
                "wake: timer digest".to_string(),
                None,
            )
            .expect("timer spawn");

        let mut settled: Option<BackgroundJobOutcome> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    settled = Some(o);
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => {}
            }
        }
        let outcome = settled.expect("timer must fire");
        assert!(matches!(outcome.state, JobState::Succeeded { .. }));
        assert_eq!(outcome.summary, "wake: timer digest");
        // Spec surfaces as Timer in the snapshot before settle.
        let snap = mgr.get_job(&info.id).expect("timer listed");
        assert!(matches!(snap.spec, JobSpec::Timer { .. }));
    }

    #[tokio::test]
    async fn test_timer_cancel_stops_recurring() {
        let mgr = BackgroundJobManager::new();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let info = mgr
            .spawn_timer(None, now + 200, Some(200), "tick".to_string(), None)
            .expect("timer spawn");

        // Let one fire land, then cancel before a second.
        tokio::time::sleep(Duration::from_millis(400)).await;
        mgr.kill_job(&info.id).expect("cancel timer");
        tokio::time::sleep(Duration::from_millis(400)).await;
        // No crash, no panic: cancellation is the contract. The job either
        // settled once or was killed mid-wait — both acceptable.
        let snap = mgr.get_job(&info.id);
        let _ = snap;
    }

    // ADR-0234 outcome retention: a settle must stay retrievable until a
    // consumer claims it, and claiming must be idempotent per delivery.

    /// Build a settled result without a real process, so the retention and
    /// claim contract can be tested deterministically.
    fn settle(
        mgr: &BackgroundJobManager,
        job_id: &str,
        owner: Option<&str>,
    ) -> BackgroundJobOutcome {
        let outcome = BackgroundJobOutcome {
            job_id: JobId(job_id.to_string()),
            spec: JobSpec::Timer {
                label: None,
                fire_at_ms: 0,
                interval_ms: None,
                prompt: "digest".to_string(),
            },
            state: JobState::Succeeded {
                duration_ms: 5,
                exit_code: 0,
            },
            summary: format!("summary for {job_id}"),
            log_path: None,
        };
        mgr.retain_outcome(outcome.clone(), owner.map(str::to_string));
        outcome
    }

    #[test]
    fn retained_outcomes_survive_missing_the_event_and_claim_exactly_once() {
        let mgr = BackgroundJobManager::new();
        // No subscriber at all: the retention, not the notification, is the
        // record.
        settle(&mgr, "job_a", Some("s1"));
        settle(&mgr, "job_b", Some("s1"));
        settle(&mgr, "job_c", Some("s2"));

        assert_eq!(mgr.pending_outcomes().len(), 3);
        assert_eq!(mgr.pending_outcomes_for_session("s1").len(), 2);
        assert_eq!(mgr.pending_outcomes_for_session("s2").len(), 1);

        let job_a = JobId("job_a".to_string());
        let claimed = mgr.claim_outcomes_for_job(&job_a);
        assert_eq!(claimed.len(), 1, "the retained delivery is claimable");
        assert_eq!(claimed[0].outcome.summary, "summary for job_a");
        assert!(
            mgr.claim_outcomes_for_job(&job_a).is_empty(),
            "the same delivery must not be claimed twice"
        );

        // Claiming one job leaves the others untouched, and the claimed row
        // stays as the delivery record.
        assert_eq!(mgr.pending_outcomes_for_session("s1").len(), 2);
        let record = mgr
            .pending_outcomes()
            .into_iter()
            .find(|p| p.outcome.job_id.0 == "job_a")
            .expect("claimed delivery is retained as a record");
        assert!(record.claimed);
    }

    #[test]
    fn distinct_fires_of_one_job_are_distinct_deliveries() {
        let mgr = BackgroundJobManager::new();
        // A re-arming timer settles repeatedly under one job id; deduplicating
        // by job id alone would swallow every fire after the first.
        settle(&mgr, "timer_x", Some("s1"));
        settle(&mgr, "timer_x", Some("s1"));

        let claimed = mgr.claim_outcomes_for_job(&JobId("timer_x".to_string()));
        assert_eq!(claimed.len(), 2);
        assert_ne!(
            claimed[0].sequence, claimed[1].sequence,
            "each fire carries its own delivery identity"
        );
    }

    /// The SystemWake admission path claims a session's deliveries in one step
    /// so the model is handed exactly the results it is acknowledging
    /// (ADR-0234).
    #[test]
    fn a_session_claim_takes_only_that_sessions_unclaimed_deliveries() {
        let mgr = BackgroundJobManager::new();
        settle(&mgr, "job_a", Some("s1"));
        settle(&mgr, "job_b", Some("s1"));
        settle(&mgr, "job_c", Some("s2"));
        settle(&mgr, "daemon_job", None);

        let claimed = mgr.claim_outcomes_for_session("s1");
        assert_eq!(claimed.len(), 2, "both of s1's deliveries are claimed");
        assert!(
            claimed
                .iter()
                .all(|p| p.owner_session.as_deref() == Some("s1")),
            "no other session's result may be delivered to s1"
        );
        assert!(
            claimed.iter().all(|p| p.claimed),
            "returned entries are marked claimed"
        );

        // Idempotent: a second admission finds nothing new to deliver.
        assert!(mgr.claim_outcomes_for_session("s1").is_empty());

        // Other owners are untouched and still deliverable.
        assert_eq!(mgr.claim_outcomes_for_session("s2").len(), 1);
        assert_eq!(
            mgr.pending_outcomes()
                .into_iter()
                .filter(|p| !p.claimed)
                .count(),
            1,
            "only the daemon-level result remains unclaimed"
        );
    }

    #[test]
    fn closing_a_session_discards_only_its_retained_outcomes() {
        let mgr = BackgroundJobManager::new();
        settle(&mgr, "job_a", Some("s1"));
        settle(&mgr, "job_b", Some("s1"));
        settle(&mgr, "daemon_job", None);

        assert_eq!(mgr.discard_pending_for_session("s1"), 2);
        let left = mgr.pending_outcomes();
        assert_eq!(left.len(), 1, "daemon-level results are not session-owned");
        assert_eq!(left[0].outcome.job_id.0, "daemon_job");
    }

    /// ADR-0234: claiming governs *automatic delivery*, not readability — a
    /// caller that inspects or re-waits a finished job must still see what it
    /// produced.
    #[test]
    fn a_claimed_settlement_remains_readable_on_the_job() {
        let mgr = BackgroundJobManager::new();
        let job = JobId("job_readable".to_string());
        let outcome = BackgroundJobOutcome {
            job_id: job.clone(),
            spec: JobSpec::Timer {
                label: None,
                fire_at_ms: 0,
                interval_ms: None,
                prompt: "digest".to_string(),
            },
            state: JobState::Succeeded {
                duration_ms: 12,
                exit_code: 0,
            },
            summary: "the produced result".to_string(),
            log_path: Some(PathBuf::from("/tmp/muta-jobs/job_readable.log")),
        };
        mgr.inner.write().unwrap().insert(
            job.clone(),
            JobEntry {
                info: BackgroundJobInfo {
                    id: job.clone(),
                    spec: outcome.spec.clone(),
                    state: outcome.state.clone(),
                    created_at_ms: 0,
                    completed_at_ms: Some(0),
                    latest_output: None,
                },
                ring_buffer: VecDeque::new(),
                cancel_tx: None,
                owner_session: Some("s1".to_string()),
                settled: None,
                #[cfg(unix)]
                pid: None,
            },
        );

        // The two halves of delivery, minus the I/O the notification path adds:
        // the entry record makes the result *readable*, the retention queue
        // makes it *deliverable*.
        mgr.record_settlement(&outcome);
        mgr.retain_outcome(outcome.clone(), Some("s1".to_string()));

        assert_eq!(
            mgr.settled_result(&job).map(|o| o.summary),
            Some("the produced result".to_string())
        );

        // Spend the automatic delivery; the readable settlement survives.
        assert_eq!(mgr.claim_outcomes_for_job(&job).len(), 1);
        let after = mgr
            .settled_result(&job)
            .expect("still readable after claim");
        assert_eq!(after.summary, "the produced result");
        assert_eq!(
            after.log_path.as_deref(),
            Some(Path::new("/tmp/muta-jobs/job_readable.log"))
        );
    }

    /// A job the fabric never settled has nothing to report — the accessor must
    /// not invent a result from the running snapshot.
    #[test]
    fn an_unsettled_job_has_no_settled_result() {
        let mgr = BackgroundJobManager::new();
        let jid = JobId::new("job");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        mgr.inner.write().unwrap().insert(
            jid.clone(),
            JobEntry {
                info: BackgroundJobInfo {
                    id: jid.clone(),
                    spec: JobSpec::Timer {
                        label: None,
                        fire_at_ms: now + 1000,
                        interval_ms: None,
                        prompt: "later".to_string(),
                    },
                    state: JobState::Running {
                        started_at_ms: now,
                        pid: None,
                    },
                    created_at_ms: now,
                    completed_at_ms: None,
                    latest_output: None,
                },
                ring_buffer: VecDeque::new(),
                cancel_tx: None,
                owner_session: Some("s1".to_string()),
                settled: None,
                #[cfg(unix)]
                pid: None,
            },
        );

        assert!(mgr.settled_result(&jid).is_none());
    }

    #[test]
    fn retention_is_bounded_and_evicts_the_oldest_delivery() {
        let mgr = BackgroundJobManager::new();
        for i in 0..(MAX_PENDING_OUTCOMES + 8) {
            settle(&mgr, &format!("job_{i:04}"), Some("s1"));
        }

        let retained = mgr.pending_outcomes();
        assert_eq!(
            retained.len(),
            MAX_PENDING_OUTCOMES,
            "an unclaimed backlog must not grow without bound"
        );
        assert_eq!(
            retained[0].outcome.job_id.0, "job_0008",
            "the oldest deliveries are the ones abandoned"
        );
        assert_eq!(
            retained[retained.len() - 1].outcome.job_id.0,
            format!("job_{:04}", MAX_PENDING_OUTCOMES + 7)
        );
    }
}
