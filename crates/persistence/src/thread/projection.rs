//! The derived SQLite projection worker (ADR-0236).
//!
//! The journal is the durable truth; this worker is the only consumer that
//! turns committed frames into the queryable SQLite view. It runs on its own
//! OS thread with a bounded queue:
//!
//! - `enqueue` never awaits the async runtime and never blocks the ReAct loop;
//! - a full queue marks the session **desynced** instead of dropping a commit;
//! - the next drained job repairs a desynced session with one full rewrite
//!   read from the journal, so the projection can always catch up;
//! - a cold reader flushes through [`ProjectionState::wait`] before it relies
//!   on SQLite.
//!
//! Ownership is deliberately split: the state owns the channel sender, the
//! worker owns the receiver plus a sender-free [`ProjectionShared`]. Dropping
//! the last session reference therefore closes the channel and lets the worker
//! exit; sharing the whole state would keep the sender alive and leak the
//! worker thread.
//!
//! The worker uses `futures::executor::block_on` for the channel-based
//! `PersistenceHandle::save_session` future. The future only waits on tokio
//! sync primitives (no timer/reactor), and the writer actor owns its own
//! thread, so this never needs a tokio runtime context.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};

use crate::db::{CommitGuard, PersistenceHandle, SaveMode};
use crate::journal::SessionJournal;
use crate::thread::ThreadData;

/// Bound on the in-flight projection queue. A full queue is a *signal* that
/// the derived view is behind, not a reason to stall the writer.
const QUEUE_CAPACITY: usize = 256;

/// Sessions deleted in this process. A projection that was already queued
/// when the delete landed must not recreate the row it was deleting.
fn deleted_sessions() -> &'static Mutex<HashSet<String>> {
    static DELETED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    DELETED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mark a session as deleted for the lifetime of this process.
pub(crate) fn mark_deleted(session_id: &str) {
    if let Ok(mut guard) = deleted_sessions().lock() {
        guard.insert(session_id.to_string());
    }
}

fn is_deleted(session_id: &str) -> bool {
    deleted_sessions()
        .lock()
        .map(|guard| guard.contains(session_id))
        .unwrap_or(false)
}

/// One committed frame awaiting projection.
pub(crate) struct ProjectionJob {
    pub(crate) seq: u64,
    pub(crate) data: ThreadData,
    pub(crate) mode: SaveMode,
    pub(crate) usage: Vec<protocol::RequestUsageRecord>,
    pub(crate) guard: CommitGuard,
}

/// The atomics and watch channel shared with the worker. It intentionally does
/// **not** own the sender (see the module docs).
pub(crate) struct ProjectionShared {
    /// Highest journal seq handed to the projection.
    submitted: AtomicU64,
    /// Highest seq the derived view has actually absorbed.
    projected: tokio::sync::watch::Sender<u64>,
    /// The last projection failed or the queue overflowed. The next job (or a
    /// cold flush) repairs the view with a full journal snapshot.
    desynced: AtomicBool,
}

impl ProjectionShared {
    fn new() -> Arc<Self> {
        let (projected, _receiver) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            submitted: AtomicU64::new(0),
            projected,
            desynced: AtomicBool::new(false),
        })
    }

    fn mark_projected(&self, seq: u64) {
        let _ = self.projected.send_if_modified(|current| {
            if *current < seq {
                *current = seq;
                true
            } else {
                false
            }
        });
    }
}

/// Per-session projection bookkeeping plus the lazily spawned worker.
pub(crate) struct ProjectionState {
    worker: Mutex<Option<SyncSender<ProjectionJob>>>,
    shared: Arc<ProjectionShared>,
}

impl ProjectionState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            worker: Mutex::new(None),
            shared: ProjectionShared::new(),
        })
    }

    /// Record that `seq` was durably appended and is now owned by projection.
    pub(crate) fn submitted(&self, seq: u64) {
        self.shared.submitted.fetch_max(seq, Ordering::AcqRel);
    }

    pub(crate) fn submitted_seq(&self) -> u64 {
        self.shared.submitted.load(Ordering::Acquire)
    }

    pub(crate) fn projected_seq(&self) -> u64 {
        *self.shared.projected.borrow()
    }

    pub(crate) fn is_desynced(&self) -> bool {
        self.shared.desynced.load(Ordering::Acquire)
    }

    pub(crate) fn clear_desynced(&self) {
        self.shared.desynced.store(false, Ordering::Release);
    }

    /// Hand one committed frame to the worker. Never blocks and never awaits.
    pub(crate) fn enqueue(
        self: &Arc<Self>,
        writer: PersistenceHandle,
        journal: Arc<SessionJournal>,
        job: ProjectionJob,
    ) {
        let seq = job.seq;
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if worker.is_none() {
            let (tx, rx) = sync_channel(QUEUE_CAPACITY);
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name("nuo-projection".to_string())
                .spawn(move || run_worker(shared, writer, journal, rx));
            match spawned {
                Ok(_) => *worker = Some(tx),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "could not start the session projection worker; the journal remains authoritative"
                    );
                    // Do not pretend the row was projected: mark the session
                    // desynced so the next worker (or the bootstrap catch-up)
                    // rebuilds from the journal instead of trusting a gap.
                    self.shared.desynced.store(true, Ordering::Release);
                    self.shared.mark_projected(seq);
                    return;
                }
            }
        }
        let Some(tx) = worker.as_ref() else {
            self.shared.desynced.store(true, Ordering::Release);
            self.shared.mark_projected(seq);
            return;
        };
        match tx.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(job)) => {
                // The view is behind. Do not block the turn: mark it and let
                // the worker repair from the journal before it absorbs the
                // next frame (which includes this one).
                self.shared.desynced.store(true, Ordering::Release);
                self.shared.mark_projected(job.seq);
            }
            Err(TrySendError::Disconnected(job)) => {
                *worker = None;
                self.shared.desynced.store(true, Ordering::Release);
                self.shared.mark_projected(job.seq);
            }
        }
    }

    /// Wait until the derived view has absorbed `seq`.
    pub(crate) async fn wait(&self, seq: u64) {
        if seq == 0 {
            return;
        }
        let mut projected = self.shared.projected.subscribe();
        while *projected.borrow() < seq {
            if projected.changed().await.is_err() {
                break;
            }
        }
    }
}

fn run_worker(
    state: Arc<ProjectionShared>,
    writer: PersistenceHandle,
    journal: Arc<SessionJournal>,
    rx: std::sync::mpsc::Receiver<ProjectionJob>,
) {
    let mut skip_through = 0u64;
    while let Ok(job) = rx.recv() {
        if job.seq <= skip_through {
            state.mark_projected(job.seq);
            continue;
        }
        if state.desynced.load(Ordering::Acquire) {
            match repair(&state, &writer, &journal, &job.data.id) {
                Some(durable) => {
                    skip_through = durable.max(skip_through);
                    state.mark_projected(skip_through.max(job.seq));
                }
                None => {
                    // Repair failed; keep the session marked and advance the
                    // barrier so a cold reader cannot hang on an old seq.
                    state.mark_projected(job.seq);
                }
            }
            continue;
        }
        if is_deleted(&job.data.id) {
            state.mark_projected(job.seq);
            continue;
        }
        match futures::executor::block_on(writer.save_session(
            job.data,
            job.mode,
            job.usage,
            job.guard,
            Some(job.seq),
        )) {
            Ok(_) => state.mark_projected(job.seq),
            Err(error) => {
                tracing::warn!(
                    seq = job.seq,
                    %error,
                    "session projection failed; the journal remains authoritative"
                );
                state.desynced.store(true, Ordering::Release);
                state.mark_projected(job.seq);
            }
        }
    }
}

/// Rebuild the derived view from the journal head. Returns the durable seq
/// the rewrite absorbed, or `None` when the journal could not be read.
fn repair(
    state: &ProjectionShared,
    writer: &PersistenceHandle,
    journal: &SessionJournal,
    session_id: &str,
) -> Option<u64> {
    // A queued job may outlive the delete that terminated its session. The
    // repair is a full rewrite, so it must re-check the tombstone here — the
    // ordinary path checked it before the desync branch (ADR-0241).
    if is_deleted(session_id) || journal.is_deleted() {
        state.desynced.store(false, Ordering::Release);
        return Some(journal.durable_seq());
    }
    let durable = journal.durable_seq();
    match journal.load() {
        Ok(Some(full)) => {
            let guard = CommitGuard {
                operation_id: Some(uuid::Uuid::new_v4().to_string()),
                expected_revision: None,
            };
            match futures::executor::block_on(writer.save_session(
                full,
                SaveMode::FullRewrite,
                Vec::new(),
                guard,
                Some(durable),
            )) {
                Ok(_) => {
                    state.desynced.store(false, Ordering::Release);
                    Some(durable)
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "could not repair the derived session projection from its journal"
                    );
                    None
                }
            }
        }
        Ok(None) => {
            state.desynced.store(false, Ordering::Release);
            Some(durable)
        }
        Err(error) => {
            tracing::warn!(%error, "could not read the journal for projection repair");
            None
        }
    }
}
