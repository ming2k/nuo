//! Transcript append/replace, round and interrupt records, retry
//! bookkeeping, fork/lineage queries, and the session tree.
use super::*;

/// What the caller guarantees about the durable projection.
///
/// `commit_turn` used to *infer* append-vs-rebuild by deep-comparing the
/// caller's whole window against the durable projection on every commit — an
/// O(history) scan on the turn hot path. The caller already knows: round
/// admission and turn boundaries append, while a context projection rewrites
/// committed messages. It now states which; the store verifies the claim with
/// a single-message tail check instead of scanning history. A failed check
/// degrades to [`Self::Reprojected`], so a wrong claim costs a comparison, not
/// data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitBasis {
    /// `messages` is the durable projection plus appended messages. The store
    /// appends the tail without comparing history.
    Appended,
    /// `messages` may differ anywhere: a projection rewrote committed
    /// messages, a rebuild happened, or the caller cannot prove appending.
    /// The store compares and rebuilds as needed.
    Reprojected,
}

/// One unified turn commit for [`ThreadSession::commit_turn`]. `messages` is
/// the full current window; `basis` says whether its committed prefix is known
/// to be untouched; `round_counter` is committed only when it differs;
/// `usage_records` are upserted only where they changed; `retry_point` arms or
/// clears the retry affordance; `round_interrupt` records an interrupted
/// outcome.
#[derive(Debug, Clone)]
pub struct CommitTurn<'a> {
    pub messages: &'a [Message],
    /// The caller's guarantee about the committed prefix.
    pub basis: CommitBasis,
    pub round_counter: Option<u64>,
    pub usage_records: &'a [protocol::RequestUsageRecord],
    pub retry_point: Option<Option<protocol::RetryPoint>>,
    pub round_interrupt: Option<protocol::RoundInterrupt>,
    /// D3 idempotency identity. Replaying the same key returns the
    /// original commit receipt instead of applying the deltas a second time.
    /// `None` mints a fresh identity for this commit.
    pub operation_id: Option<String>,
    /// D3 revision precondition. When supplied, the writer refuses
    /// the commit if the durable session revision no longer matches — the
    /// check fails closed rather than writing over content the caller did
    /// not see.
    pub expected_revision: Option<u64>,
}

/// Target temporal boundary for a granular session fork (ADR-0058).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkTarget {
    /// Fork at current settled head (latest turn).
    Head,
    /// Fork at conclusion of round R (1-indexed).
    Round(u64),
    /// Fork after turn T of round R (1-indexed round, 0-indexed turn).
    Turn { round: u64, turn: u32 },
}

impl<'a> CommitTurn<'a> {
    pub fn new(messages: &'a [Message], basis: CommitBasis) -> Self {
        Self {
            messages,
            basis,
            round_counter: None,
            usage_records: &[],
            retry_point: None,
            round_interrupt: None,
            operation_id: None,
            expected_revision: None,
        }
    }
}

/// Guarded persist plumbing shared by the accessors in this module: mutate
/// under the lock, then optionally write the snapshot off the async runtime.
/// (Kept as a doc anchor; each accessor inlines the guard so the decision and
/// the persist are atomic under the session lock.)
fn _persist_guard_doc() {}

/// Wire-normalized equality: harness sidecars (timestamps, display content,
/// provenance) are not provider-visible content, so comparisons that decide
/// append-vs-rebuild must ignore them.
///
/// Implemented via zero-copy `semantic_wire_eq` to eliminate repetitive heap
/// allocations and clones on the ReAct turn hot path.
fn wire_eq(a: &[Message], b: &[Message]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.semantic_wire_eq(y))
}

impl ThreadSession {
    fn forensics_dir(&self, session_id: &str) -> std::path::PathBuf {
        super::journal_root(&self.db_path).join(session_id)
    }

    /// The durable round-interrupt records (C11): one per round stopped
    /// before completing, newest last. Pure projection state — never part
    /// of the transcript.
    pub async fn round_interrupts(&self) -> Vec<protocol::RoundInterrupt> {
        self.state.lock().await.data.round_interrupts.clone()
    }

    /// Append one round-interrupt record (C11). Best-effort duplicate guard:
    /// a record for the same round with the same reason is not appended twice.
    pub async fn record_round_interrupt(
        &self,
        record: protocol::RoundInterrupt,
    ) -> Result<(), String> {
        self.mutate_metadata_and_persist(|data| {
            let already = data
                .round_interrupts
                .iter()
                .any(|existing| existing.reason == record.reason && existing.round == record.round);
            if already {
                return false;
            }
            data.round_interrupts.push(record);
            true
        })
        .await
    }

    /// The request-projection archive: a best-effort ring, never the window.
    /// Pure forensic state — no SQLite row, no foreign key, no round failure.
    pub async fn request_projections(&self) -> Vec<protocol::RequestProjection> {
        let dir = self.forensics_dir(&self.id().await);
        tokio::task::spawn_blocking(move || {
            crate::forensics::read(&dir, crate::db::MAX_RETAINED_REQUEST_PROJECTIONS)
        })
        .await
        .unwrap_or_default()
    }

    /// Append one request-projection record to the session's ring and wait
    /// until the one writer thread has attempted it. Cold path (tests,
    /// tooling); the request path uses `try_record_request_projection`.
    pub async fn record_request_projection(
        &self,
        record: protocol::RequestProjection,
    ) -> Result<(), String> {
        let dir = self.forensics_dir(&self.id().await);
        tokio::task::spawn_blocking(move || crate::forensics::record_wait(&dir, &record))
            .await
            .map_err(|error| error.to_string())
    }

    /// Fire-and-forget request-projection append for the model-request path.
    /// The caller passes the session id because this path must not await the
    /// store lock; a dropped write is a dropped forensic record, not a round
    /// failure.
    pub fn try_record_request_projection(
        &self,
        session_id: String,
        record: protocol::RequestProjection,
    ) {
        crate::forensics::record(&self.forensics_dir(&session_id), &record);
    }

    /// Clear every round-interrupt record (C11). Called when the interrupted
    /// round's outcome is superseded.
    pub async fn clear_round_interrupts(&self) -> Result<(), String> {
        self.mutate_metadata_and_persist(|data| {
            if data.round_interrupts.is_empty() {
                return false;
            }
            data.round_interrupts.clear();
            true
        })
        .await
    }

    /// The durable `/retry` resume point (C12).
    pub async fn retry_pending(&self) -> Option<protocol::RetryPoint> {
        self.state.lock().await.data.retry_pending.clone()
    }

    /// Arm the `/retry` resume point (C12). Snapshot semantics: the single
    /// slot is replaced (arming for a newer round retires an older point).
    pub async fn arm_retry_pending(&self, point: protocol::RetryPoint) -> Result<(), String> {
        self.mutate_metadata_and_persist(|data| {
            data.retry_pending = Some(point);
            true
        })
        .await
    }

    /// Clear the `/retry` resume point (C12). A no-op when nothing is armed.
    pub async fn clear_retry_pending(&self) -> Result<(), String> {
        self.mutate_metadata_and_persist(|data| {
            if data.retry_pending.is_none() {
                return false;
            }
            data.retry_pending = None;
            true
        })
        .await
    }

    /// Replace the durable dialogue with exactly `messages`: the
    /// transcript's entries are rebuilt from scratch and the projection
    /// decision history is cleared — the caller asserts the window is the
    /// whole truth.
    pub async fn replace_messages(&self, messages: Vec<Message>) -> Result<(), String> {
        let (data, children, undo, should_persist) = {
            let mut state = self.state.lock().await;
            let undo = SessionUndo::replacing(&state);
            state.data.transcript = rebuild_transcript_from_messages(&messages);
            // A wholesale rebuild mints new fact ids; the old graph sidecar
            // would contain unreachable ghosts (ADR-0239).
            state.data.graph = None;
            state.data.generation = uuid::Uuid::new_v4().to_string();
            let children = admit_subagent_children(&mut state.data, &messages);
            state.projected_cache = Some(messages.clone());
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.data.clone(), children, undo, !empty_unpersisted)
        };
        persist_subagent_children(&self.writer, &children).await;
        if should_persist {
            self.persist_full_rewrite(data, undo).await?;
        }
        Ok(())
    }

    /// The durable command ledger: acts only (ADR-0140).
    pub async fn commands(&self) -> Vec<protocol::CommandRecord> {
        self.state.lock().await.data.commands.clone()
    }

    /// Atomically mutate the command ledger in place under the lock and persist.
    ///
    /// Append-only pushes (the normal path) write `O(1)` `CommandAct` frames to
    /// the session journal and insert only the new row(s) into `command_acts`
    /// without rewriting the entire ledger (ADR-0249).
    pub async fn mutate_commands<F>(&self, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut Vec<protocol::CommandRecord>),
    {
        enum Action {
            Noop,
            Append {
                thread_id: String,
                start_seq: u64,
                added: Vec<protocol::CommandRecord>,
                journal: Arc<crate::journal::SessionJournal>,
            },
            FullRewrite {
                data: Box<ThreadData>,
                undo: Box<SessionUndo>,
            },
        }

        let action = {
            let mut state = self.state.lock().await;
            let old_len = state.data.commands.len();
            let undo = SessionUndo::replacing(&state);
            f(&mut state.data.commands);
            if Self::should_skip_persist(&state) {
                Action::Noop
            } else if state.data.commands.len() > old_len {
                let added = state.data.commands[old_len..].to_vec();
                state.data.updated_at = unix_timestamp();
                state.defer_persist = false;
                Action::Append {
                    thread_id: state.data.id.clone(),
                    start_seq: old_len as u64,
                    added,
                    journal: Arc::clone(&state.journal),
                }
            } else if state.data.commands.len() == old_len {
                Action::Noop
            } else {
                state.data.updated_at = unix_timestamp();
                state.defer_persist = false;
                Action::FullRewrite {
                    data: Box::new(state.data.clone()),
                    undo: Box::new(undo),
                }
            }
        };

        match action {
            Action::Noop => Ok(()),
            Action::Append {
                thread_id,
                start_seq,
                added,
                journal,
            } => {
                for (offset, record) in added.into_iter().enumerate() {
                    let seq = start_seq + offset as u64;
                    let jseq = journal
                        .append_command_act(seq, &record)
                        .await
                        .map_err(|error| format!("command act journal append failed: {error}"))?;
                    if let Err(error) = self
                        .writer
                        .append_command_act(thread_id.clone(), seq, &record, Some(jseq))
                        .await
                    {
                        tracing::warn!(
                            session = %thread_id,
                            seq,
                            %error,
                            "command act projection failed; journal remains authoritative"
                        );
                    }
                }
                Ok(())
            }
            Action::FullRewrite { data, undo } => self.persist_full_rewrite(*data, *undo).await,
        }
    }

    /// Incrementally persist new messages appended since the last durable
    /// write (ADR-0048). The prefix is compared against the **projection**;
    /// on divergence the transcript is rebuilt (the caller's window is the
    /// truth).
    pub async fn append_turn(&self, current: &[Message]) -> Result<(), String> {
        let (data, mode, children, undo) = {
            let mut state = self.state.lock().await;
            // Built before the first mutation; restored if the frame never
            // becomes durable. A wholesale rebuild makes this an O(history)
            // clone, which is the cost the rebuild itself already pays.
            let undo = SessionUndo::replacing(&state);
            let durable_len = state.get_or_project_messages().len();
            if current.len() <= durable_len && !wire_eq(current, state.get_or_project_messages()) {
                return Ok(());
            }
            let old_entries_len = state.data.transcript.entries.len();
            let mut children = Vec::new();
            let mut full_rewrite = false;
            if current.len() > durable_len
                && wire_eq(&current[..durable_len], state.get_or_project_messages())
            {
                let tail = &current[durable_len..];
                for message in tail {
                    state
                        .data
                        .transcript
                        .push(protocol::TranscriptEntry::from_message(0, message));
                }
                children = admit_subagent_children(&mut state.data, tail);
                state.append_to_projection_cache(tail);
            } else if !wire_eq(current, state.get_or_project_messages()) {
                state.data.transcript = rebuild_transcript_from_messages(current);
                state.data.graph = None;
                state.data.generation = uuid::Uuid::new_v4().to_string();
                children = admit_subagent_children(&mut state.data, current);
                state.invalidate_projection_cache();
                full_rewrite = true;
            }
            state.data.updated_at = unix_timestamp();
            state.defer_persist = false;

            let (data, mode) = if full_rewrite {
                (state.data.clone(), crate::db::SaveMode::FullRewrite)
            } else if old_entries_len == state.data.transcript.entries.len() {
                (
                    state.data.clone_metadata_without_history(),
                    crate::db::SaveMode::MetadataOnly,
                )
            } else {
                let mut delta = state.data.clone_metadata_without_history();
                delta.transcript.entries =
                    state.data.transcript.entries[old_entries_len..].to_vec();
                (delta, crate::db::SaveMode::Delta)
            };
            (data, mode, children, undo)
        };
        persist_subagent_children(&self.writer, &children).await;
        self.persist_with_mode_guarded(
            data,
            mode,
            Vec::new(),
            crate::db::CommitGuard::default(),
            undo,
        )
        .await
        .map(|_| ())
    }

    /// Commit everything a finished ReAct turn changed, in **one** lock
    /// acquisition and at most **one** snapshot write. Returns the committed
    /// session revision (D3).
    pub async fn commit_turn(&self, commit: CommitTurn<'_>) -> Result<u64, String> {
        let guard = crate::db::CommitGuard {
            operation_id: commit.operation_id.clone(),
            expected_revision: commit.expected_revision,
        };
        let (data, mode, children, usage_upserts, undo) = {
            let mut state = self.state.lock().await;
            // A turn reports `Reprojected` when it may have rewritten committed
            // messages, so the undo must be able to restore the old transcript.
            let undo = if commit.basis == CommitBasis::Reprojected {
                SessionUndo::replacing(&state)
            } else {
                SessionUndo::appending(&state)
            };

            // 1. Message-tail delta against the projection.
            //
            // The caller states whether the committed prefix is untouched. An
            // `Appended` claim is verified by one tail comparison — not by
            // deep-comparing the whole window, which made every turn boundary
            // scan the session's history. A failed check falls through to the
            // safe path, so a wrong claim costs a comparison, never data.
            let (durable_len, tail_ok) = {
                let projection = state.get_or_project_messages();
                let len = projection.len();
                let tail_ok = len == 0
                    || (commit.messages.len() >= len
                        && commit.messages[len - 1].semantic_wire_eq(&projection[len - 1]));
                (len, tail_ok)
            };
            let appended = commit.basis == CommitBasis::Appended
                && commit.messages.len() >= durable_len
                && tail_ok;
            let old_entries_len = state.data.transcript.entries.len();
            let mut children = Vec::new();
            let mut full_rewrite = false;
            if appended {
                let tail = &commit.messages[durable_len..];
                if !tail.is_empty() {
                    for message in tail {
                        state
                            .data
                            .transcript
                            .push(protocol::TranscriptEntry::from_message(0, message));
                    }
                    children = admit_subagent_children(&mut state.data, tail);
                    state.append_to_projection_cache(tail);
                    state.data.updated_at = unix_timestamp();
                }
            } else if !wire_eq(commit.messages, state.get_or_project_messages()) {
                state.data.transcript = rebuild_transcript_from_messages(commit.messages);
                state.data.graph = None;
                state.data.generation = uuid::Uuid::new_v4().to_string();
                children = admit_subagent_children(&mut state.data, commit.messages);
                state.invalidate_projection_cache();
                state.data.updated_at = unix_timestamp();
                full_rewrite = true;
            }

            // 2. Round counter.
            if let Some(counter) = commit.round_counter
                && counter != state.data.round_counter
            {
                state.data.round_counter = counter;
                state.data.updated_at = unix_timestamp();
            }

            // 3. Usage records — upsert only the records that changed. The
            // durable ledger lives in its own key-addressed table, so the
            // changed records ride to the save as upserts; the
            // in-memory mirror feeds the token ledger.
            let mut usage_upserts: Vec<protocol::RequestUsageRecord> = Vec::new();
            if !commit.usage_records.is_empty() {
                if commit
                    .usage_records
                    .iter()
                    .any(|record| record.key.session_id != state.data.id)
                {
                    return Err("request usage record belongs to another session".to_string());
                }
                let mut index: std::collections::HashMap<protocol::RequestUsageKey, usize> = state
                    .data
                    .request_usage_records
                    .iter()
                    .enumerate()
                    .map(|(i, record)| (record.key.clone(), i))
                    .collect();
                for record in commit.usage_records {
                    match index.get(&record.key) {
                        Some(&i) => {
                            if state.data.request_usage_records[i] != *record {
                                state.data.request_usage_records[i] = record.clone();
                                usage_upserts.push(record.clone());
                                state.data.updated_at = unix_timestamp();
                            }
                        }
                        None => {
                            index
                                .insert(record.key.clone(), state.data.request_usage_records.len());
                            state.data.request_usage_records.push(record.clone());
                            usage_upserts.push(record.clone());
                            state.data.updated_at = unix_timestamp();
                        }
                    }
                }
            }

            // 4. Retry point (None = untouched, Some(None) = clear, Some(Some(point)) = arm).
            if let Some(target) = commit.retry_point {
                match target {
                    Some(point) => {
                        if state.data.retry_pending.as_ref() != Some(&point) {
                            state.data.retry_pending = Some(point);
                            state.data.updated_at = unix_timestamp();
                        }
                    }
                    None => {
                        if state.data.retry_pending.is_some() {
                            state.data.retry_pending = None;
                            state.data.updated_at = unix_timestamp();
                        }
                    }
                }
            }

            // 5. Round interrupt.
            if let Some(record) = commit.round_interrupt {
                let already = state.data.round_interrupts.iter().any(|existing| {
                    existing.reason == record.reason && existing.round == record.round
                });
                if !already {
                    state.data.round_interrupts.push(record);
                    state.data.updated_at = unix_timestamp();
                }
            }

            state.defer_persist = false;
            let (data, mode) = if full_rewrite {
                (state.data.clone(), crate::db::SaveMode::FullRewrite)
            } else if old_entries_len == state.data.transcript.entries.len() {
                (
                    state.data.clone_metadata_without_history(),
                    crate::db::SaveMode::MetadataOnly,
                )
            } else {
                let mut delta = state.data.clone_metadata_without_history();
                delta.transcript.entries =
                    state.data.transcript.entries[old_entries_len..].to_vec();
                (delta, crate::db::SaveMode::Delta)
            };
            (data, mode, children, usage_upserts, undo)
        };
        persist_subagent_children(&self.writer, &children).await;
        self.persist_with_mode_guarded(data, mode, usage_upserts, guard, undo)
            .await
    }

    /// Durably classify crash residue (D4): every request attempt
    /// still `InFlight` in this session becomes `Abandoned` with its projected
    /// prompt as a lower bound, committed so every reader sees the resolution
    /// instead of a silently filtered unresolved attempt. Returns the number
    /// of attempts reclassified (0 is the steady state).
    ///
    /// Called by crash recovery after it has read the store-side `InFlight`
    /// signal (which this clears). Idempotent: once reclassified in memory, a
    /// second call finds nothing to settle.
    pub async fn settle_abandoned_attempts(&self) -> Result<usize, String> {
        use protocol::{RequestUsageSource, RequestUsageStatus};
        let (data, settled, undo): (ThreadData, Vec<protocol::RequestUsageRecord>, SessionUndo) = {
            let mut state = self.state.lock().await;
            let undo = SessionUndo::appending(&state);
            let mut settled = Vec::new();
            for record in state.data.request_usage_records.iter_mut() {
                if record.status == RequestUsageStatus::InFlight {
                    record.status = RequestUsageStatus::Abandoned;
                    record.source = RequestUsageSource::Estimated;
                    record.prompt_tokens = record.projected_prompt_tokens.max(0);
                    record.total_tokens = record.prompt_tokens;
                    settled.push(record.clone());
                }
            }
            if settled.is_empty() {
                return Ok(0);
            }
            state.data.updated_at = unix_timestamp();
            state.defer_persist = false;
            (state.data.clone_metadata_without_history(), settled, undo)
        };
        let count = settled.len();
        self.persist_with_mode_guarded(
            data,
            crate::db::SaveMode::MetadataOnly,
            settled,
            crate::db::CommitGuard::default(),
            undo,
        )
        .await?;
        Ok(count)
    }

    /// Record one request-time projection without touching durable facts.
    ///
    /// Pruning and folding are `RequestView` transforms. They may change the
    /// window for the current request, but they never rewrite history and never
    /// survive a restart (`INV-FOLDVIEW-01`).
    pub async fn commit_context_projection(
        &self,
        result: ContextProjectionResult,
    ) -> Result<(), String> {
        let mut state = self.state.lock().await;
        state.data.last_projection = Some(result.checkpoint);
        state.invalidate_projection_cache();
        Ok(())
    }

    /// Fork the active branch at a temporal boundary. No facts are copied:
    /// the branch shares the immutable prefix and the store switches its view to
    /// the new cursor.
    pub async fn fork_at_branch(
        &self,
        target: ForkTarget,
        name: impl Into<String>,
    ) -> Result<protocol::BranchId, String> {
        let (data, id, undo) = {
            let mut state = self.state.lock().await;
            let undo = SessionUndo::replacing(&state);
            let mut thread = thread_from_session_data(&state.data);
            let branch = thread.state.active_branch.clone();
            let path = thread.path(&branch).map_err(|error| error.to_string())?;
            if path.is_empty() {
                return Err("Cannot fork an empty thread.".to_string());
            }
            let from = fork_fact_for_target(&path, target)?;
            let mut name = name.into();
            if thread
                .state
                .branches
                .values()
                .any(|existing| existing.name == name)
            {
                // A repeated `/fork` name is not a user error; make it unique.
                name = format!("{name}-{}", uuid::Uuid::new_v4().simple());
            }
            let id = thread
                .fork_from(
                    &branch,
                    from,
                    name,
                    protocol::BranchPolicy::default(),
                    unix_timestamp() * 1000,
                )
                .map_err(|error| error.to_string())?;
            thread.state.active_branch = id.clone();
            state.data.active_branch = id.clone();
            state.data.branches = thread.state.branches.clone();
            state.data.graph = Some(thread.clone());
            state.data.transcript = transcript_from_thread(&thread);
            state.data.generation = uuid::Uuid::new_v4().to_string();
            state.data.updated_at = unix_timestamp();
            state.invalidate_projection_cache();
            (state.data.clone(), id, undo)
        };
        self.persist_full_rewrite(data, undo).await?;
        Ok(id)
    }

    /// Project the durable transcript into the canonical [`protocol::Thread`]
    /// aggregate (a pure derivation; SQLite is not consulted).
    pub async fn thread(&self) -> protocol::Thread {
        let state = self.state.lock().await;
        thread_from_session_data(&state.data)
    }

    /// Project the live thread facts into the UI tree.
    ///
    /// The tree is a view: it has no durable row and no write path of its own.
    /// Named branches are the thread's actual branch cursors.
    pub async fn tree(&self) -> protocol::ThreadTree {
        let thread = self.thread().await;
        tree_from_thread(&thread)
    }

    /// Project the durable transcript into the canonical [`protocol::Thread`]
    /// aggregate.
    ///
    /// The projection preserves fact identity (`entry.id`), the parent chain,
    /// compaction summaries, branch cursors, and canonical tool observations.
    /// It is a pure derivation of the in-memory session state; SQLite is not
    /// consulted (ADR-0239).
    pub async fn thread_aggregate(&self) -> Result<protocol::Thread, String> {
        let state = self.state.lock().await;
        Ok(thread_from_session_data(&state.data))
    }

    /// All branch cursors for this thread.
    pub async fn branches(&self) -> Result<Vec<protocol::BranchState>, String> {
        Ok(self
            .state
            .lock()
            .await
            .data
            .branches
            .values()
            .cloned()
            .collect())
    }

    /// Fork a branch cursor at `from` (default: the active head). Facts are
    /// never copied; the branch shares the immutable prefix.
    pub async fn fork_branch(
        &self,
        name: impl Into<String>,
        from: Option<protocol::FactId>,
    ) -> Result<protocol::BranchId, String> {
        let (data, id, undo) = {
            let mut state = self.state.lock().await;
            let undo = SessionUndo::replacing(&state);
            let mut thread = thread_from_session_data(&state.data);
            let branch = thread.state.active_branch.clone();
            let from = match from {
                Some(from) => from,
                None => thread
                    .state
                    .branch(&branch)
                    .map_err(|error| error.to_string())?
                    .head
                    .clone()
                    .ok_or_else(|| "cannot fork an empty branch".to_string())?,
            };
            let id = thread
                .fork_from(
                    &branch,
                    from,
                    name.into(),
                    protocol::BranchPolicy::default(),
                    unix_timestamp() * 1000,
                )
                .map_err(|error| error.to_string())?;
            state.data.branches = thread.state.branches.clone();
            state.data.graph = Some(thread.clone());
            state.data.updated_at = unix_timestamp();
            (state.data.clone(), id, undo)
        };
        self.persist_full_rewrite(data, undo).await?;
        Ok(id)
    }

    /// Switch this store's transcript view to another branch.
    ///
    /// The switch is one full journal frame: the branch cursor and the
    /// transcript it projects commit atomically, and the derived SQLite view
    /// materialises both from the same snapshot (ADR-0239).
    pub async fn switch_branch(&self, branch: protocol::BranchId) -> Result<(), String> {
        let (data, undo) = {
            let mut state = self.state.lock().await;
            let undo = SessionUndo::replacing(&state);
            let mut thread = thread_from_session_data(&state.data);
            if !thread.state.branches.contains_key(&branch) {
                return Err(format!("unknown branch '{}'", branch.0));
            }
            thread.state.active_branch = branch.clone();
            state.data.active_branch = branch;
            state.data.branches = thread.state.branches.clone();
            state.data.graph = Some(thread.clone());
            state.data.transcript = transcript_from_thread(&thread);
            state.data.generation = uuid::Uuid::new_v4().to_string();
            state.data.updated_at = unix_timestamp();
            state.invalidate_projection_cache();
            (state.data.clone(), undo)
        };
        self.persist_full_rewrite(data, undo).await
    }

    /// Delete a branch cursor. Facts remain while another branch reaches them.
    pub async fn delete_branch(&self, branch: protocol::BranchId) -> Result<(), String> {
        let (data, undo) = {
            let mut state = self.state.lock().await;
            let undo = SessionUndo::replacing(&state);
            let mut thread = thread_from_session_data(&state.data);
            thread
                .delete_branch(&branch, unix_timestamp() * 1000)
                .map_err(|error| error.to_string())?;
            let active = thread.state.active_branch.clone();
            state.data.branches = thread.state.branches.clone();
            state.data.graph = Some(thread.clone());
            if state.data.active_branch != active {
                state.data.active_branch = active;
                state.data.transcript = transcript_from_thread(&thread);
                state.data.generation = uuid::Uuid::new_v4().to_string();
                state.invalidate_projection_cache();
            }
            state.data.updated_at = unix_timestamp();
            (state.data.clone(), undo)
        };
        self.persist_full_rewrite(data, undo).await
    }

    /// Replace the in-memory transcript with `thread`'s projection and persist
    /// the whole session (transcript, cursors, working state) as one frame.
    pub async fn commit_thread(&self, thread: &protocol::Thread) -> Result<u64, String> {
        let (data, revision, undo) = {
            let mut state = self.state.lock().await;
            let undo = SessionUndo::replacing(&state);
            state.data.transcript = transcript_from_thread(thread);
            state.data.active_branch = thread.state.active_branch.clone();
            state.data.branches = thread.state.branches.clone();
            state.data.graph = Some(thread.clone());
            state.data.generation = uuid::Uuid::new_v4().to_string();
            state.data.round_counter = thread.state.round_counter;
            state.data.updated_at = thread.updated_at_ms / 1000;
            state.invalidate_projection_cache();
            state.defer_persist = false;
            (state.data.clone(), thread.revision.0, undo)
        };
        self.persist_full_rewrite(data, undo).await?;
        Ok(revision)
    }
}

fn fork_fact_for_target(
    path: &[&protocol::ThreadFact],
    target: ForkTarget,
) -> Result<protocol::FactId, String> {
    use protocol::FactPayload;
    use tool::Role;
    let head = || {
        path.last()
            .map(|fact| fact.id.clone())
            .ok_or_else(|| "Cannot fork an empty thread.".to_string())
    };
    match target {
        ForkTarget::Head => head(),
        ForkTarget::Round(round) => {
            if round == 0 {
                return Err("Cannot fork at round 0.".to_string());
            }
            let mut seen = 0u64;
            let mut cut = None;
            for fact in path {
                if matches!(
                    &fact.payload,
                    FactPayload::Message { message } if message.role == Role::User
                ) {
                    seen += 1;
                    if seen > round {
                        cut = Some(fact.id.clone());
                        break;
                    }
                }
            }
            match cut {
                Some(next_round) => {
                    let index = path
                        .iter()
                        .position(|fact| fact.id == next_round)
                        .ok_or_else(|| "fork point vanished".to_string())?;
                    path.get(index.saturating_sub(1))
                        .map(|fact| fact.id.clone())
                        .ok_or_else(|| "Cannot fork before the first fact.".to_string())
                }
                None if seen >= round => head(),
                None => Err(format!(
                    "Cannot fork at round {round}: thread only has {seen} round(s)."
                )),
            }
        }
        ForkTarget::Turn { round, turn } => {
            if round == 0 {
                return Err("Cannot fork at round 0.".to_string());
            }
            let mut seen_rounds = 0u64;
            let mut in_round = false;
            let mut assistants = 0u32;
            for fact in path {
                if matches!(
                    &fact.payload,
                    FactPayload::Message { message } if message.role == Role::User
                ) {
                    seen_rounds += 1;
                    in_round = seen_rounds == round;
                    if seen_rounds > round {
                        break;
                    }
                    continue;
                }
                if in_round
                    && matches!(
                        &fact.payload,
                        FactPayload::Message { message } if message.role == Role::Assistant
                    )
                {
                    if assistants == turn {
                        return Ok(fact.id.clone());
                    }
                    assistants += 1;
                }
            }
            if seen_rounds < round {
                return Err(format!(
                    "Cannot fork at round {round}: thread only has {seen_rounds} round(s)."
                ));
            }
            if assistants <= turn {
                return Err(format!(
                    "Cannot fork at round {round} turn {turn}: round {round} has only {assistants} turn(s)."
                ));
            }
            head()
        }
    }
}

/// Project a thread aggregate into the UI tree model.
fn tree_from_thread(thread: &protocol::Thread) -> protocol::ThreadTree {
    let mut tree = protocol::ThreadTree::new();
    for fact in thread.all_facts() {
        let kind = match &fact.payload {
            protocol::FactPayload::Message { message } => protocol::ThreadEntryKind::Message {
                message: message.clone(),
            },
            protocol::FactPayload::Observation {
                call_id,
                tool_name,
                lifecycle,
                ..
            } => protocol::ThreadEntryKind::Message {
                message: lifecycle.lower_to_message(call_id, tool_name),
            },
            protocol::FactPayload::Compaction {
                summary,
                first_kept_fact_id,
                tokens_before,
                read_files,
                modified_files,
                ..
            } => protocol::ThreadEntryKind::Compaction {
                summary: summary.clone(),
                first_kept_entry_id: first_kept_fact_id.0.clone(),
                tokens_before: *tokens_before,
                read_files: read_files.clone(),
                modified_files: modified_files.clone(),
            },
            protocol::FactPayload::Termination { .. }
            | protocol::FactPayload::SystemNotice { .. } => protocol::ThreadEntryKind::Custom {
                custom_type: "notice".to_string(),
                content: format!("{:?}", fact.payload),
                display: None,
            },
        };
        tree.entries.insert(
            fact.id.0.clone(),
            protocol::ThreadEntry {
                id: fact.id.0.clone(),
                parent_id: fact.parent_id.as_ref().map(|id| id.0.clone()),
                timestamp: fact.timestamp_ms,
                kind,
            },
        );
    }
    tree.root_id = tree
        .entries
        .values()
        .find(|entry| entry.parent_id.is_none())
        .map(|entry| entry.id.clone());
    tree.active_leaf_id = thread
        .state
        .active_branch()
        .ok()
        .and_then(|branch| branch.head.as_ref())
        .map(|id| id.0.clone());
    tree.named_branches = thread
        .state
        .branches
        .values()
        .filter_map(|branch| {
            branch
                .head
                .as_ref()
                .map(|head| (branch.name.clone(), head.0.clone()))
        })
        .collect();
    tree
}

/// Project a durable thread back into the transcript mirror.
///
/// The transcript is a view: entry ids and sequences are derived from fact ids
/// and positive fact sequences, so a reload never invents a different order.
///
/// The mirror carries the branch's **whole conversation** ([`Thread::path`]),
/// plus one `compact` directive per checkpoint so the model-facing
/// [`protocol::Transcript::project`] can still bound the request window
/// (ADR-0246 `[INV-CMP-08]`, `[INV-CMP-09]`). Those two facts are independent:
/// what the thread said, and what the next request may send.
pub(crate) fn transcript_from_thread(thread: &protocol::Thread) -> protocol::Transcript {
    use protocol::{
        CompactionMeta, DirectiveKind, DirectivePayload, EntryPayload, ProjectionDirective,
        TranscriptEntry,
    };

    let branch = &thread.state.active_branch;
    let mut transcript = protocol::Transcript::new();
    let Ok(facts) = thread.path(branch) else {
        return transcript;
    };
    // The fold's seam is resolved from the *kept* fact's position in the
    // mirrored order, not from any stored number: a checkpoint says "everything
    // before `first_kept` was folded", and the mirror is the only thing that can
    // turn that reference into a membership sequence
    // (ADR-0246 `[INV-CMP-09]`).
    // (checkpoint entry index, watermark membership seq or None)
    let mut folds: Vec<(usize, Option<u64>)> = Vec::new();
    for fact in facts {
        let seq = fact.seq.saturating_sub(1);
        match &fact.payload {
            protocol::FactPayload::Message { message } => {
                let mut entry = TranscriptEntry::from_message(seq, message);
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                transcript.push(entry);
            }
            protocol::FactPayload::Observation {
                call_id,
                tool_name,
                lifecycle,
                ..
            } => {
                let message = lifecycle.lower_to_message(call_id, tool_name);
                let mut entry = TranscriptEntry::from_message(seq, &message);
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                transcript.push(entry);
            }
            protocol::FactPayload::Compaction {
                summary,
                first_kept_fact_id,
                tokens_before,
                read_files,
                modified_files,
            } => {
                // ADR-0246 `[INV-CMP-10]`: the checkpoint's *node* is appended at
                // the head, but its meaning is "everything before the kept fact
                // was folded". The mirror keeps the node where it was appended
                // and records the seam twice — as the directive the model window
                // obeys, and as the kept fact's id so reading order can resolve
                // it ([`protocol::Transcript::conversation_messages`]).
                let watermark = transcript
                    .entries
                    .iter()
                    .rposition(|entry| entry.id == first_kept_fact_id.0)
                    .and_then(|kept| kept.checked_sub(1))
                    .map(|folded| transcript.entries[folded].seq);
                let mut entry =
                    TranscriptEntry::from_message(seq, &protocol::checkpoint_message(summary));
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                entry.content = Some(summary.clone());
                // ADR-0160 `[INV-CMP-01]`: a checkpoint is hidden dialogue, not a
                // visible turn the model or the user authored.
                entry.hidden = true;
                entry.payload = EntryPayload::Compaction(Box::new(CompactionMeta {
                    first_kept_node_id: first_kept_fact_id.0.clone(),
                    split_parent_node_id: fact.parent_id.as_ref().map(|id| id.0.clone()),
                    tokens_before: *tokens_before,
                    read_files: read_files.clone(),
                    modified_files: modified_files.clone(),
                }));
                transcript.push(entry);
                folds.push((transcript.entries.len() - 1, watermark));
            }
            protocol::FactPayload::Termination {
                reason,
                partial_output,
                ..
            } => {
                let reason = match reason {
                    protocol::TerminationReason::UserInterrupt => "Interrupted by user",
                    protocol::TerminationReason::Timeout => "Execution timed out",
                    protocol::TerminationReason::FatalError { error } => error.as_str(),
                    protocol::TerminationReason::Superseded => "Superseded by user message",
                };
                let content = match partial_output {
                    Some(output) => format!("[Execution stopped: {reason}]\n{output}"),
                    None => format!("[Execution stopped: {reason}]"),
                };
                let mut entry =
                    TranscriptEntry::from_message(seq, &Message::new(tool::Role::System, content));
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                transcript.push(entry);
            }
            protocol::FactPayload::SystemNotice {
                source,
                notice_type,
                content,
            } => {
                let mut entry = TranscriptEntry::from_message(
                    seq,
                    &Message::new(
                        tool::Role::System,
                        format!("[System Notice from {source} ({notice_type})]: {content}"),
                    ),
                );
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                transcript.push(entry);
            }
        }
    }

    for (checkpoint_index, watermark) in folds {
        let Some(watermark) = watermark else {
            // The kept fact is the oldest row: nothing was folded, so the
            // checkpoint hides nothing. It keeps its appended position.
            continue;
        };
        let Some(checkpoint_seq) = transcript
            .entries
            .get(checkpoint_index)
            .map(|entry| entry.seq)
        else {
            continue;
        };
        transcript.push_directive(ProjectionDirective {
            seq: 0,
            kind: DirectiveKind::Compact,
            up_to_seq: watermark,
            payload: DirectivePayload::Compact { checkpoint_seq },
        });
    }
    transcript
}

/// Project [`ThreadData`] into the canonical thread aggregate.
///
/// Tool-role transcript entries become `Observation` facts (the canonical tool
/// execution representation); every other message stays a `Message` fact. The
/// projection keeps entry ids as fact ids so a later durable store can adopt
/// them without rewriting parent links.
fn linear_thread_from_data(data: &ThreadData) -> protocol::Thread {
    use protocol::{
        BlobRef, FactContext, FactId, FactPayload, ObservationLifecycle, ObservationMetrics,
        Thread, ThreadFact, ThreadId, ThreadRevision, ThreadStatus,
    };

    let mut thread = Thread::new(
        ThreadId::new(data.id.clone()),
        data.created_at.saturating_mul(1000),
    );
    thread.parent_thread_id = data.parent_id.clone().map(ThreadId::new);
    thread.updated_at_ms = data.updated_at.saturating_mul(1000);
    thread.state.round_counter = data.round_counter;
    thread.state.status = ThreadStatus::Idle;

    // Assistant tool-call names indexed by call id, so a tool result can name
    // its tool without a second table.
    let mut tool_names = std::collections::HashMap::new();
    for entry in &data.transcript.entries {
        if let Some(message) = entry.to_message()
            && let Some(calls) = message.tool_calls
        {
            for call in calls {
                tool_names.insert(call.id, call.name);
            }
        }
    }

    let mut parent: Option<FactId> = None;
    let mut compaction_horizon: Option<FactId> = None;
    let mut seq = 0u64;
    for entry in &data.transcript.entries {
        // Transcript memberships are 0-based; fact sequences are positive and
        // monotonic (INV-FACT-01). Shift by one while preserving order.
        let fact_seq = entry.seq.saturating_add(1);
        seq = seq.max(fact_seq);
        let payload = match &entry.payload {
            protocol::EntryPayload::Message(_) => {
                let Some(message) = entry.to_message() else {
                    continue;
                };
                if message.role == tool::Role::Tool {
                    let call_id = message.tool_call_id.clone().unwrap_or_default();
                    let tool_name = tool_names
                        .get(&call_id)
                        .cloned()
                        .unwrap_or_else(|| "tool".to_string());
                    let hash = message.content_blob.clone().unwrap_or_else(|| {
                        crate::blobs::BlobStore::hash(message.content.as_bytes())
                    });
                    FactPayload::Observation {
                        call_id,
                        tool_name,
                        // Cut on a UTF-8 boundary: a raw `[..512]` slice panics
                        // when a multibyte character straddles the limit.
                        output_ref: BlobRef::new(hash, message.content.len() as u64, "text/plain")
                            .with_preview(tool::truncate_utf8(&message.content, 512)),
                        metrics: ObservationMetrics {
                            original_bytes: message.content.len(),
                            original_lines: message.content.lines().count(),
                            original_tokens: message.content.len().div_ceil(4),
                        },
                        lifecycle: ObservationLifecycle::Raw {
                            content: message.content,
                        },
                    }
                } else {
                    FactPayload::Message { message }
                }
            }
            protocol::EntryPayload::Compaction(meta) => {
                compaction_horizon = Some(FactId::from_raw(meta.first_kept_node_id.clone()));
                FactPayload::Compaction {
                    summary: entry.content.clone().unwrap_or_default(),
                    first_kept_fact_id: FactId::from_raw(meta.first_kept_node_id.clone()),
                    tokens_before: entry
                        .content
                        .as_deref()
                        .map(str::len)
                        .unwrap_or(0)
                        .div_ceil(4),
                    read_files: meta.read_files.clone(),
                    modified_files: meta.modified_files.clone(),
                }
            }
            protocol::EntryPayload::State(_) => continue,
        };
        let id = FactId::from_raw(entry.id.clone());
        let fact = ThreadFact::new(
            id.clone(),
            parent.clone(),
            fact_seq,
            entry.created_at_ms,
            payload,
            FactContext::default(),
        );
        // A malformed legacy entry must not make the projection unreadable.
        if thread.graph.insert(fact.clone()).is_err() {
            continue;
        }
        parent = Some(id);
    }

    thread.state.active_branch = data.active_branch.clone();
    if data.branches.is_empty() {
        // Frames written before branch state moved into the journal only name
        // an active branch; reconstruct the single-cursor shape from the
        // linear fact chain (ADR-0239).
        if let Some(head) = parent.clone() {
            let branch = data.active_branch.clone();
            thread.state.branches.insert(
                branch.clone(),
                protocol::BranchState {
                    id: branch,
                    name: data.active_branch.0.clone(),
                    head: Some(head),
                    forked_from: None,
                    compaction_horizon,
                    policy: protocol::BranchPolicy::default(),
                    created_at_ms: thread.created_at_ms,
                    updated_at_ms: thread.updated_at_ms,
                },
            );
        }
    } else {
        thread.state.branches = data.branches.clone();
        if !thread.state.branches.contains_key(&data.active_branch) {
            // A journal from a store that predates branch durability may name
            // an active branch with no cursor. Fall back to the linear head so
            // the aggregate still hydrates rather than becoming unreadable.
            thread.state.active_branch = protocol::BranchId::main();
            if let Some(head) = parent.clone() {
                let branch = protocol::BranchId::main();
                thread.state.branches.insert(
                    branch.clone(),
                    protocol::BranchState {
                        id: branch,
                        name: "main".to_string(),
                        head: Some(head),
                        forked_from: None,
                        compaction_horizon,
                        policy: protocol::BranchPolicy::default(),
                        created_at_ms: thread.created_at_ms,
                        updated_at_ms: thread.updated_at_ms,
                    },
                );
            }
        }
    }
    // The transcript is the active branch's projection, so its tail is the
    // active cursor's head by construction. Deriving it here keeps a normal
    // delta (which never rewrites the branch map) from leaving the cursor at
    // `None` and makes a transcript switch self-consistent (ADR-0239).
    if let Some(head) = parent.as_ref()
        && let Some(branch) = thread.state.branches.get_mut(&thread.state.active_branch)
    {
        branch.head = Some(head.clone());
        branch.updated_at_ms = thread.updated_at_ms;
    }

    thread.revision = ThreadRevision(seq.max(data.transcript.entries.len() as u64));
    thread
}

/// Project `data` into the canonical thread aggregate.
///
/// Ordinary deltas only extend the active transcript; the full fact DAG lives
/// in `data.graph` (written on branch mutations and full rewrites). This
/// merges the active transcript's new facts into that DAG, so sibling branches
/// survive a reload without SQLite (ADR-0239).
pub(crate) fn thread_from_session_data(data: &ThreadData) -> protocol::Thread {
    let linear = linear_thread_from_data(data);
    let Some(graph) = data.graph.as_ref() else {
        return linear;
    };
    let mut thread = graph.clone();
    let mut max_seq = thread.graph.iter().map(|fact| fact.seq).max().unwrap_or(0);
    for fact in linear.graph.facts_in_seq_order() {
        if thread.graph.contains(&fact.id) {
            continue;
        }
        let mut fact = fact.clone();
        max_seq = max_seq.saturating_add(1);
        // A fork's new turn can reuse a sequence already used by the other
        // branch. Renumber above the whole DAG; parent order is preserved.
        fact.seq = max_seq;
        // A missing parent means the fact cannot anchor a path; dropping it is
        // safer than fabricating lineage.
        let _ = thread.graph.insert(fact);
    }
    thread.state.active_branch = data.active_branch.clone();
    thread.state.round_counter = data.round_counter;
    thread.updated_at_ms = data.updated_at.saturating_mul(1000);
    if !data.branches.is_empty() {
        thread.state.branches = data.branches.clone();
    }
    // The transcript is the active branch's projection, so its tail is that
    // branch's head by construction.
    if let Some(active) = linear.state.branches.get(&linear.state.active_branch)
        && let Some(head) = active.head.clone()
        && let Some(branch) = thread.state.branches.get_mut(&data.active_branch)
    {
        branch.head = Some(head);
        branch.updated_at_ms = thread.updated_at_ms;
    }
    thread.revision = protocol::ThreadRevision(thread.graph.len() as u64);
    thread
}

/// Intercept subagent results in an admission delta: each nested
/// transcript becomes a **subagent session** (its own `sessions` row, facts
/// shared by identity), and the parent's tool entry gains a `SubagentRef`
/// pointer. The in-memory `Message` keeps its `children` for the live view;
/// the persisted entry carries only the pointer. The subagent rows are
/// *returned*, not written here: the caller persists them after releasing the
/// session lock so no blocking I/O runs under the lock.
fn admit_subagent_children(state: &mut ThreadData, candidates: &[Message]) -> Vec<ThreadData> {
    let mut subagents = Vec::new();
    for message in candidates {
        let (Some(children), Some(subagent_meta)) =
            (message.children.as_ref(), message.subagent_meta.as_ref())
        else {
            continue;
        };
        if children.is_empty() {
            continue;
        }
        let subagent_id = uuid::Uuid::new_v4().to_string();
        let mut subagent = ThreadData {
            id: subagent_id.clone(),
            parent_id: Some(state.id.clone()),
            origin: protocol::ThreadOrigin::Subagent,
            workspace: state.workspace.clone(),
            role: state.role.clone(),
            ..Default::default()
        };
        subagent.transcript = rebuild_transcript_from_messages(children);
        subagents.push(subagent);
        // Stamp the pointer on the already-admitted parent entry (the newest
        // tool result matching this subagent result's content).
        let call_id = message.tool_call_id.clone();
        if let Some(entry) = state
            .transcript
            .entries
            .iter_mut()
            .rev()
            .find(|entry| {
                matches!(&entry.payload, protocol::EntryPayload::Message(payload)                     if payload.tool_call_id == call_id)
            })
            && let protocol::EntryPayload::Message(payload) = &mut entry.payload {
                payload.subagent = Some(protocol::SubagentRef {
                    session_id: subagent_id,
                    description: subagent_meta.description.clone(),
                    duration_ms: subagent_meta.duration_ms,
                    toolset_count: Some(subagent_meta.toolset_count),
                });
            }
    }
    subagents
}

/// Persist admitted subagent sessions. Called after the session lock is
/// released; failures leave the parent entry's pointer dangling, which the
/// load path reports rather than silently dropping the run.
async fn persist_subagent_children(
    writer: &crate::db::PersistenceHandle,
    subagents: &[ThreadData],
) {
    // The owner lock is the write lease for the whole store, child journals
    // included. Checking it here matters because this runs before the parent's
    // guarded append: without it a read-only client would write children and
    // then fail the parent, leaving a half-admitted run (ADR-0241).
    if writer.is_lock_contention() {
        tracing::warn!(
            children = subagents.len(),
            "store is owned by another process; skipping subagent journal admission"
        );
        return;
    }
    for subagent in subagents {
        // A subagent is a session like any other: its admission is one full
        // journal frame, projected asynchronously. The parent's turn must not
        // block on a SQLite write just to record a child (ADR-0236/0237).
        let journal = open_journal(writer.db_path(), &subagent.id);
        let operation_id = uuid::Uuid::new_v4().to_string();
        match journal
            .append(
                &operation_id,
                crate::db::SaveMode::FullRewrite,
                subagent,
                &[],
            )
            .await
        {
            Ok(seq) => {
                let projection = ProjectionState::new();
                projection.submitted(seq);
                projection.enqueue(ProjectionJob {
                    seq,
                    data: subagent.clone(),
                    mode: crate::db::SaveMode::FullRewrite,
                    usage: Vec::new(),
                    guard: crate::db::CommitGuard {
                        operation_id: Some(operation_id),
                        expected_revision: None,
                    },
                    writer: writer.clone(),
                    journal: Arc::clone(&journal),
                });
                // Subagent admission is a cold control path: the parent's
                // durable entry already points at the child, so make the
                // derived row visible before the parent's commit returns.
                projection.wait(seq).await;
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    subagent = %subagent.id,
                    "could not admit the subagent session into its journal"
                );
            }
        }
    }
}

/// Rebuild a transcript from a flat message window (entries get fresh ids;
/// projection history is cleared). The caller must mint a fresh
/// `ThreadData::generation` afterwards: the new transcript shares no rows
/// with the durable one.
pub(crate) fn rebuild_transcript_from_messages(messages: &[Message]) -> protocol::Transcript {
    let mut transcript = protocol::Transcript::new();
    for message in messages {
        transcript.push(protocol::TranscriptEntry::from_message(0, message));
    }
    transcript
}
