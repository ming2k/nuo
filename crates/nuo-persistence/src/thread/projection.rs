//! The derived SQLite projection runtime (ADR-0236).
//!
//! The journal is the durable truth; these workers are the only consumers that
//! turn committed frames into the queryable SQLite view.
//!
//! ## Why a shared pool, not a thread per session
//!
//! A projection is blocking work: it prepares a fact graph, may write CAS
//! blobs, and commits one SQL transaction. The obvious wiring is one OS thread
//! per session with its own queue — and that is what this module did while the
//! session count was small. It does not scale: a hosted fleet of `N` sessions
//! then owns `N` permanently parked threads and `N` channel buffers, for work
//! that is almost always idle.
//!
//! The pool keeps the properties that mattered and drops the ones that did not:
//!
//! - **Per-session FIFO.** Each session has its own queue, and a queue is
//!   appended by the session's commit gate (one writer at a time), so job order
//!   is commit order. Two jobs of one session never run concurrently.
//! - **Fairness.** A lane's ready list holds a session at most once. A busy
//!   session yields its turn after every job, so a burst in one session cannot
//!   starve another session sharing the lane.
//! - **Bounded memory.** Each session queue is capped; the cap is a *signal*,
//!   not a stall: a full queue marks the session desynced and the next job
//!   repairs the view from the journal.
//! - **Automatic healing.** A job that fails marks the session desynced; the
//!   next drained job rebuilds from the journal head rather than trusting a
//!   gap.
//!
//! Enqueueing never awaits and never touches the ReAct loop: it is a lock, a
//! push, and a notify on the caller's thread.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::db::{CommitGuard, PersistenceHandle, SaveMode};
use crate::journal::SessionJournal;
use crate::thread::ThreadData;

/// Bound on one session's in-flight projection queue. A full queue is a
/// *signal* that the derived view is behind, not a reason to stall the writer.
const QUEUE_CAPACITY: usize = 256;

/// Projection lanes (OS threads). The work is SQL plus occasional file I/O;
/// past a handful of lanes the single SQLite writer is the limit, not the CPU.
fn lane_count() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().clamp(2, 8))
        .unwrap_or(2)
}

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

/// One committed frame awaiting projection, plus the doors it needs.
///
/// The job is self-contained on purpose: it carries the writer door, the
/// session's journal (for a repair), and the shared watermarks. A lane thread
/// therefore needs no session registry to process it, which is what lets one
/// fixed pool serve an unbounded number of sessions.
pub(crate) struct ProjectionJob {
    pub(crate) seq: u64,
    pub(crate) data: ThreadData,
    pub(crate) mode: SaveMode,
    pub(crate) usage: Vec<nuo_protocol::RequestUsageRecord>,
    pub(crate) guard: CommitGuard,
    /// The writer door this session projects through.
    pub(crate) writer: PersistenceHandle,
    /// The session's journal, for a repair after a desync.
    pub(crate) journal: Arc<SessionJournal>,
}

impl ProjectionJob {
    fn session_id(&self) -> &str {
        &self.data.id
    }
}

/// The atomics and watch channel shared with the pool. It intentionally does
/// **not** own a queue (see [`ProjectionState`]).
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

/// One session's queued work, in commit order, plus the watermarks the jobs
/// report against.
struct SessionQueue {
    shared: Arc<ProjectionShared>,
    jobs: VecDeque<ProjectionJob>,
}

/// One lane's queues. `ready` and `ready_order` contain the same session ids:
/// the set answers "is this session already waiting?" in O(1), the deque gives
/// round-robin order.
struct LaneInner {
    queues: HashMap<String, SessionQueue>,
    ready: HashSet<String>,
    ready_order: VecDeque<String>,
}

impl LaneInner {
    fn new() -> Self {
        Self {
            queues: HashMap::new(),
            ready: HashSet::new(),
            ready_order: VecDeque::new(),
        }
    }
}

struct Lane {
    inner: Mutex<LaneInner>,
    wake: Condvar,
}

impl Lane {
    /// Append one job. Returns `false` when the session's queue is full, which
    /// the caller turns into a desync signal.
    pub(crate) fn push(&self, shared: Arc<ProjectionShared>, job: ProjectionJob) -> bool {
        let session = job.session_id().to_string();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let queue = inner
            .queues
            .entry(session.clone())
            .or_insert_with(|| SessionQueue {
                shared,
                jobs: VecDeque::new(),
            });
        if queue.jobs.len() >= QUEUE_CAPACITY {
            return false;
        }
        queue.jobs.push_back(job);
        if inner.ready.insert(session.clone()) {
            inner.ready_order.push_back(session);
        }
        drop(inner);
        self.wake.notify_all();
        true
    }

    /// Block until one job is available, then take it.
    ///
    /// A lane thread lives for the process: returning `None` on an empty queue
    /// would retire the lane the first time it caught up, and every later job
    /// on that lane would be stranded. Waiting on the condvar is what makes an
    /// idle lane cost nothing.
    fn pop_blocking(&self) -> (Arc<ProjectionShared>, ProjectionJob) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        loop {
            if let Some(item) = take_next(&mut inner) {
                return item;
            }
            inner = self
                .wake
                .wait(inner)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

/// The process-wide projection lanes.
struct ProjectionRuntime {
    lanes: Vec<Arc<Lane>>,
}

impl ProjectionRuntime {
    fn global() -> &'static Arc<Self> {
        static GLOBAL: OnceLock<Arc<ProjectionRuntime>> = OnceLock::new();
        GLOBAL.get_or_init(|| {
            let count = lane_count();
            let lanes: Vec<Arc<Lane>> = (0..count)
                .map(|_| {
                    Arc::new(Lane {
                        inner: Mutex::new(LaneInner::new()),
                        wake: Condvar::new(),
                    })
                })
                .collect();
            for (index, lane) in lanes.iter().enumerate() {
                let lane = Arc::clone(lane);
                let spawned = std::thread::Builder::new()
                    .name(format!("nuo-projection-{index}"))
                    .spawn(move || run_lane(lane));
                if let Err(error) = spawned {
                    // A missing lane degrades throughput, never correctness: the
                    // other lanes keep draining, and its own jobs stay queued
                    // until a later process. Log and continue.
                    tracing::warn!(%error, lane = index, "could not start a projection lane");
                }
            }
            tracing::debug!(lanes = count, "projection lane pool started");
            Arc::new(Self { lanes })
        })
    }

    /// Pick a session's lane. A session always hashes to the same lane, which
    /// is what makes its queue the only ordering authority it needs.
    fn lane_for(&self, session_id: &str) -> &Arc<Lane> {
        let index = crc32c::crc32c(session_id.as_bytes()) as usize % self.lanes.len();
        &self.lanes[index]
    }
}

/// Per-session projection bookkeeping.
///
/// This type no longer owns a thread. It owns the session's watermark and
/// desync flag, and routes jobs to the process-wide lane pool.
pub(crate) struct ProjectionState {
    shared: Arc<ProjectionShared>,
}

impl ProjectionState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
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

    /// Hand one committed frame to the pool. Never blocks and never awaits.
    ///
    /// A refused enqueue (queue full) is not a dropped commit: the journal
    /// already holds it, and marking the session desynced makes the next job
    /// rebuild the whole view from that journal.
    pub(crate) fn enqueue(self: &Arc<Self>, job: ProjectionJob) {
        let seq = job.seq;
        let runtime = ProjectionRuntime::global();
        // The queue is the ordering authority: a lane processes one session's
        // jobs strictly in push order.
        if !runtime
            .lane_for(job.session_id())
            .push(Arc::clone(&self.shared), job)
        {
            self.shared.desynced.store(true, Ordering::Release);
            self.shared.mark_projected(seq);
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

/// Take the next ready session and its first job.
///
/// The session goes to the **back** of the ready list when it still has work,
/// so a session that is producing faster than it is being drained cannot
/// monopolise the lane while other sessions wait behind it.
fn take_next(inner: &mut LaneInner) -> Option<(Arc<ProjectionShared>, ProjectionJob)> {
    loop {
        let session = inner.ready_order.pop_front()?;
        inner.ready.remove(&session);
        let Some(queue) = inner.queues.get_mut(&session) else {
            continue;
        };
        let job = queue.jobs.pop_front();
        let shared = Arc::clone(&queue.shared);
        if queue.jobs.is_empty() {
            inner.queues.remove(&session);
        } else if inner.ready.insert(session.clone()) {
            inner.ready_order.push_back(session);
        }
        if let Some(job) = job {
            return Some((shared, job));
        }
    }
}

fn run_lane(lane: Arc<Lane>) {
    loop {
        let (shared, job) = lane.pop_blocking();
        // A lane is a process-wide resource with no supervisor: a panic in one
        // session's projection must cost that projection, not the lane and every
        // session queued behind it. The failed session is marked desynced so its
        // next job rebuilds the view from the journal.
        let seq = job.seq;
        let guard = Arc::clone(&shared);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(shared, job))).is_err()
        {
            guard.desynced.store(true, Ordering::Release);
            guard.mark_projected(seq);
            tracing::error!(
                seq,
                "a projection lane caught a panic; the session will be repaired"
            );
        }
    }
}

fn process(shared: Arc<ProjectionShared>, job: ProjectionJob) {
    let session = job.session_id().to_string();
    let writer = job.writer.clone();
    let journal = Arc::clone(&job.journal);

    if shared.desynced.load(Ordering::Acquire) {
        match repair(&shared, &writer, &journal, &session) {
            Some(durable) => shared.mark_projected(durable.max(job.seq)),
            None => shared.mark_projected(job.seq),
        }
        return;
    }
    if is_deleted(&session) {
        shared.mark_projected(job.seq);
        return;
    }
    match futures::executor::block_on(writer.save_session(
        job.data,
        job.mode,
        job.usage,
        job.guard,
        Some(job.seq),
    )) {
        Ok(_) => shared.mark_projected(job.seq),
        Err(error) => {
            tracing::warn!(
                seq = job.seq,
                session = %session,
                %error,
                "session projection failed; the journal remains authoritative"
            );
            shared.desynced.store(true, Ordering::Release);
            shared.mark_projected(job.seq);
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
