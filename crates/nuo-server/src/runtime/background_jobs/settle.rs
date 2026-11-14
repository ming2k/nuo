//! Settlement: line capture, outcome delivery, and teardown.

use super::service::describe_state;
use super::*;

impl BackgroundJobManager {
    pub(super) fn append_line(&self, job_id: &JobId, line: &str) {
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

    pub(super) fn finish_job(
        &self,
        job_id: JobId,
        spec: JobSpec,
        state: JobState,
        log_path: Option<PathBuf>,
    ) {
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

    pub(super) fn finish_job_with_summary(
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
                    // Drop the kill handle with the cancel channel.
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
    /// then ledger.
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
    pub(super) fn deliver_outcome(
        &self,
        outcome: BackgroundJobOutcome,
        owner_session: Option<String>,
    ) {
        self.record_settlement(&outcome);

        self.retain_outcome(outcome.clone(), owner_session.clone());

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Completed(outcome.clone()));

        // Persist the settle into the task ledger (D4, best-effort, on
        // the blocking pool so the fabric's event path never waits on I/O).
        tokio::task::spawn_blocking(move || {
            crate::runtime::task_ledger::record_outcome(
                &nuo_persistence::db::get_persistence_handle(),
                &outcome,
                owner_session,
            );
        });
    }

    /// Record a settlement on the job entry, which is what keeps it readable
    /// through `status`/`wait` after its automatic delivery has been claimed
    /// Kept separate from [`Self::deliver_outcome`] so the record
    /// has no I/O or notification side effects.
    pub(super) fn record_settlement(&self, outcome: &BackgroundJobOutcome) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(&outcome.job_id) {
            entry.settled = Some(outcome.clone());
        }
    }

    /// Publish one fire of a re-arming Timer task.
    ///
    /// A recurring timer is a *live* job whose iteration settled: the digest
    /// goes to the mailbox exactly like a one-shot settle, but the entry keeps
    /// its state (armed/running) and its cancel handle, so
    /// [`Self::kill_job`] can still stop the next fire. Settling the entry
    /// here would strand the re-armed loop — the previous implementation
    /// cleared `cancel_tx` on every fire, leaving a recurring timer
    /// uncancellable after its first tick.
    pub(super) fn publish_timer_fire(&self, job_id: &JobId, spec: &JobSpec, summary: String) {
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
    /// cancels a re-arming timer: the job stops being armed, but a
    /// deliberate cancellation needs no digest and no wake.
    pub(super) fn settle_entry_silently(&self, job_id: &JobId, state: JobState) {
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
    /// The cancel handle — not the published state — is the
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
    /// Settled entries are skipped for the same pid-recycling reason
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
