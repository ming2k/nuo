//! Background job manager for long-running process commands and asynchronous sub-subagents.
//!
//! Dual-track execution model:
//! - **Track A (Process Jobs)**: OS-level subprocesses (`tokio::process`) capturing output
//!   into in-memory ring buffers and disk logs, with 0 LLM token cost.
//! - **Track B (Sub-Subagent Jobs)**: Asynchronous isolated exploration subagents.
//!
//! Emits live progress events and retains completed outcomes so a
//! settle is never only a transient notification — the result stays retrievable
//! until a consumer claims it.
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use nuo_protocol::{BackgroundJobInfo, BackgroundJobOutcome, JobId, JobKind, JobSpec, JobState};

const DEFAULT_RING_BUFFER_CAPACITY: usize = 500;

/// Maximum retained, unclaimed outcomes per manager.
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
    /// Owning session (D5): `None` = server-level task.
    owner_session: Option<String>,
    /// The job's own most recent settlement.
    ///
    /// Kept on the entry so `status` and a repeated `wait` still report what
    /// the job produced after its delivery has been claimed: claiming governs
    /// automatic delivery, not readability.
    settled: Option<BackgroundJobOutcome>,
    #[cfg(unix)]
    pid: Option<u32>,
}

/// A settled outcome retained until a consumer claims it.
///
/// This is the durable-in-process form of "the job finished and nobody has
/// looked at the result yet". `sequence` is the delivery identity: it is
/// assigned once, at settle time, and lets a consumer tell two fires of the
/// same recurring job apart instead of deduplicating by `job_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOutcome {
    /// Monotonic delivery identity within this manager instance.
    pub sequence: u64,
    /// Owning session (`None` = server-level job, delivered to no session).
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
    /// Retained outcomes, oldest first. Bounded by
    /// [`MAX_PENDING_OUTCOMES`].
    pending: Arc<RwLock<VecDeque<PendingOutcome>>>,
    /// Delivery-identity allocator.
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
    /// A service task reported readiness: readiness condition met,
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
    /// Owning session (D5): recorded in job snapshots and ledger
    /// rows so "whose task is this" is answerable. `None` = server-level
    /// (rehosted services, fabric-internal tasks).
    pub owner_session: Option<String>,
}

#[cfg(test)]
mod tests;

mod fabric;
mod process;
mod query;
mod service;
mod settle;

pub use self::service::SessionJobService;
