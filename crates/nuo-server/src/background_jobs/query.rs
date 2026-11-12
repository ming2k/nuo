//! Read-only job queries and outcome claiming.

use super::*;

impl BackgroundJobManager {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let log_dir = std::env::temp_dir().join("nuo-jobs");
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

    /// The job's own most recent settlement: readable through
    /// `status`/`wait` independently of whether its automatic delivery has been
    /// claimed.
    pub fn settled_result(&self, id: &JobId) -> Option<BackgroundJobOutcome> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(id).and_then(|entry| entry.settled.clone())
    }

    /// Take the retained deliveries for one job.
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

    /// Take the retained deliveries for one session: every unclaimed
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
    pub(super) fn retain_outcome(
        &self,
        outcome: BackgroundJobOutcome,
        owner_session: Option<String>,
    ) -> u64 {
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
}
