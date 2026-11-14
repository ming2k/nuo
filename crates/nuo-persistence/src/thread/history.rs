//! Transcript append/replace, round and interrupt records, retry
//! bookkeeping, fork/lineage queries, and the session tree.
use super::*;

/// Identity of a durable compaction, returned to the caller that already holds
/// the in-memory horizon node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedCompaction {
    /// The checkpoint entry's id, which is also the derived causal node's id.
    pub entry_id: String,
    /// The checkpoint entry's membership sequence.
    pub entry_seq: u64,
}

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
    pub usage_records: &'a [nuo_protocol::RequestUsageRecord],
    pub retry_point: Option<Option<nuo_protocol::RetryPoint>>,
    pub round_interrupt: Option<nuo_protocol::RoundInterrupt>,
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
    /// The durable round-interrupt records (C11): one per round stopped
    /// before completing, newest last. Pure projection state — never part
    /// of the transcript.
    pub async fn round_interrupts(&self) -> Vec<nuo_protocol::RoundInterrupt> {
        self.state.lock().await.data.round_interrupts.clone()
    }

    /// Append one round-interrupt record (C11). Best-effort duplicate guard:
    /// a record for the same round with the same reason is not appended twice.
    pub async fn record_round_interrupt(
        &self,
        record: nuo_protocol::RoundInterrupt,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            let already =
                state.data.round_interrupts.iter().any(|existing| {
                    existing.reason == record.reason && existing.round == record.round
                });
            if already {
                return Ok(());
            }
            state.data.round_interrupts.push(record);
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable request-projection archive: forensic snapshots
    /// of assembled requests, oldest first. Pure projection state — never part
    /// of the transcript, never replayed into a later model request. Read from
    /// the key-addressed `request_projections` table on demand; it is not held
    /// in `ThreadData`.
    pub async fn request_projections(&self) -> Vec<nuo_protocol::RequestProjection> {
        let handle = self.writer.clone();
        let session_id = self.id().await;
        tokio::task::spawn_blocking(move || {
            handle
                .reader()
                .and_then(|reader| {
                    reader.load_request_projections(
                        &session_id,
                        crate::db::MAX_RETAINED_REQUEST_PROJECTIONS,
                    )
                })
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    /// Insert one request-projection record, awaiting the durable
    /// write. Keyed by `(session, round, turn)`, so re-recording a logical
    /// invocation replaces its row rather than duplicating it. Retention is a
    /// bounded ring enforced by the storage layer.
    pub async fn record_request_projection(
        &self,
        record: nuo_protocol::RequestProjection,
    ) -> Result<(), String> {
        let session_id = self.id().await;
        self.writer
            .record_request_projection(session_id, record)
            .await
            .map_err(|error| error.to_string())
    }

    /// Fire-and-forget request-projection insert for the model
    /// request hot path: forensic persistence must never block dispatch. The
    /// caller passes the session id because this path cannot await the store
    /// lock to read it. A dropped write is logged, never surfaced as a round
    /// failure.
    pub fn try_record_request_projection(
        &self,
        session_id: String,
        record: nuo_protocol::RequestProjection,
    ) {
        self.writer
            .try_record_request_projection(session_id, record);
    }

    /// Clear every round-interrupt record (C11). Called when the interrupted
    /// round's outcome is superseded.
    pub async fn clear_round_interrupts(&self) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            if state.data.round_interrupts.is_empty() {
                return Ok(());
            }
            state.data.round_interrupts.clear();
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable `/retry` resume point (C12).
    pub async fn retry_pending(&self) -> Option<nuo_protocol::RetryPoint> {
        self.state.lock().await.data.retry_pending.clone()
    }

    /// Arm the `/retry` resume point (C12). Snapshot semantics: the single
    /// slot is replaced (arming for a newer round retires an older point).
    pub async fn arm_retry_pending(&self, point: nuo_protocol::RetryPoint) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.retry_pending = Some(point);
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Clear the `/retry` resume point (C12). A no-op when nothing is armed.
    pub async fn clear_retry_pending(&self) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            if state.data.retry_pending.is_none() {
                return Ok(());
            }
            state.data.retry_pending = None;
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Replace the durable dialogue with exactly `messages`: the
    /// transcript's entries are rebuilt from scratch and the projection
    /// decision history is cleared — the caller asserts the window is the
    /// whole truth.
    pub async fn replace_messages(&self, messages: Vec<Message>) -> Result<(), String> {
        let (path, data, children, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.transcript = rebuild_transcript_from_messages(&messages);
            state.data.generation = uuid::Uuid::new_v4().to_string();
            let children = admit_subagent_children(&mut state.data, &messages);
            state.projected_cache = Some(messages.clone());
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (
                state.path.clone(),
                state.data.clone(),
                children,
                !empty_unpersisted,
            )
        };
        persist_subagent_children(&self.writer, &self.blob_store, &children);
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable command ledger: acts only (ADR-0140).
    pub async fn commands(&self) -> Vec<nuo_protocol::CommandRecord> {
        self.state.lock().await.data.commands.clone()
    }

    /// Atomically mutate the command ledger in place under the lock and persist.
    pub async fn mutate_commands<F>(&self, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut Vec<nuo_protocol::CommandRecord>),
    {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            f(&mut state.data.commands);
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Incrementally persist new messages appended since the last durable
    /// write (ADR-0048). The prefix is compared against the **projection**;
    /// on divergence the transcript is rebuilt (the caller's window is the
    /// truth).
    pub async fn append_turn(&self, current: &[Message]) -> Result<(), String> {
        let (path, data, children) = {
            let mut state = self.state.lock().await;
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
                        .push(nuo_protocol::TranscriptEntry::from_message(0, message));
                }
                children = admit_subagent_children(&mut state.data, tail);
                state.append_to_projection_cache(tail);
            } else if !wire_eq(current, state.get_or_project_messages()) {
                state.data.transcript = rebuild_transcript_from_messages(current);
                state.data.generation = uuid::Uuid::new_v4().to_string();
                children = admit_subagent_children(&mut state.data, current);
                state.invalidate_projection_cache();
                full_rewrite = true;
            }
            state.data.updated_at = unix_timestamp();
            state.defer_persist = false;

            let data = if full_rewrite {
                state.data.clone()
            } else {
                let mut delta = state.data.clone_metadata_without_history();
                delta.transcript.entries =
                    state.data.transcript.entries[old_entries_len..].to_vec();
                delta
            };
            (state.path.clone(), data, children)
        };
        persist_subagent_children(&self.writer, &self.blob_store, &children);
        self.persist_off_runtime(path, data, self.blob_store.clone())
            .await
    }

    /// Commit everything a finished ReAct turn changed, in **one** lock
    /// acquisition and at most **one** snapshot write. Returns the committed
    /// session revision (D3).
    pub async fn commit_turn(&self, commit: CommitTurn<'_>) -> Result<u64, String> {
        let guard = crate::db::CommitGuard {
            operation_id: commit.operation_id.clone(),
            expected_revision: commit.expected_revision,
        };
        let (path, data, children, usage_upserts) = {
            let mut state = self.state.lock().await;

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
                            .push(nuo_protocol::TranscriptEntry::from_message(0, message));
                    }
                    children = admit_subagent_children(&mut state.data, tail);
                    state.append_to_projection_cache(tail);
                    state.data.updated_at = unix_timestamp();
                }
            } else if !wire_eq(commit.messages, state.get_or_project_messages()) {
                state.data.transcript = rebuild_transcript_from_messages(commit.messages);
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
            let mut usage_upserts: Vec<nuo_protocol::RequestUsageRecord> = Vec::new();
            if !commit.usage_records.is_empty() {
                if commit
                    .usage_records
                    .iter()
                    .any(|record| record.key.session_id != state.data.id)
                {
                    return Err("request usage record belongs to another session".to_string());
                }
                let mut index: std::collections::HashMap<nuo_protocol::RequestUsageKey, usize> =
                    state
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
            let data = if full_rewrite {
                state.data.clone()
            } else {
                let mut delta = state.data.clone_metadata_without_history();
                delta.transcript.entries =
                    state.data.transcript.entries[old_entries_len..].to_vec();
                delta
            };
            (state.path.clone(), data, children, usage_upserts)
        };
        persist_subagent_children(&self.writer, &self.blob_store, &children);
        self.persist_with_usage_guarded(path, data, usage_upserts, guard)
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
        use nuo_protocol::{RequestUsageSource, RequestUsageStatus};
        let (path, data, settled): (PathBuf, ThreadData, Vec<nuo_protocol::RequestUsageRecord>) = {
            let mut state = self.state.lock().await;
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
            (
                state.path.clone(),
                state.data.clone_metadata_without_history(),
                settled,
            )
        };
        let count = settled.len();
        self.persist_with_usage_guarded(path, data, settled, crate::db::CommitGuard::default())
            .await?;
        Ok(count)
    }

    /// Record one request-time projection without touching durable facts.
    ///
    /// Pruning and folding are `RequestView` transforms. They may change the
    /// window for the current request, but they never rewrite history and never
    /// survive a restart (`INV-FOLD-01`).
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
    ) -> Result<nuo_protocol::BranchId, String> {
        let mut thread = self.thread_aggregate().await?;
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
            .any(|branch| branch.name == name)
        {
            // A repeated `/fork` name is not a user error; make it unique.
            name = format!("{name}-{}", uuid::Uuid::new_v4().simple());
        }
        let id = thread
            .fork_from(
                &branch,
                from,
                name,
                nuo_protocol::BranchPolicy::default(),
                unix_timestamp() * 1000,
            )
            .map_err(|error| error.to_string())?;
        thread.state.active_branch = id.clone();
        self.writer
            .save_thread_aggregate(thread.clone())
            .await
            .map_err(|error| error.to_string())?;
        let mut state = self.state.lock().await;
        state.data.active_branch = id.clone();
        state.data.transcript = transcript_from_thread(&thread);
        state.data.generation = uuid::Uuid::new_v4().to_string();
        state.invalidate_projection_cache();
        Ok(id)
    }

    /// Project the live thread facts into the UI tree.
    ///
    /// The tree is a view: it has no durable row and no write path of its own.
    /// Named branches are the thread's actual branch cursors.
    pub async fn tree(&self) -> nuo_protocol::ThreadTree {
        let thread = self.thread().await;
        tree_from_thread(&thread)
    }

    /// Project the durable transcript into the canonical [`nuo_protocol::Thread`]
    /// aggregate.
    ///
    /// This is the migration boundary for code that already holds a
    /// `ThreadSession`: the projection preserves fact identity (`entry.id`), the
    /// parent chain, compaction summaries, and canonical tool observations.
    /// The returned value is a projection, not a second SSOT — writing it back
    /// still goes through this store until the live path moves to
    /// `nuo_protocol::ThreadSession`.
    pub async fn thread(&self) -> nuo_protocol::Thread {
        let state = self.state.lock().await;
        thread_from_session_data(&state.data)
    }

    /// Hydrate the full thread aggregate, including sibling branches.
    pub async fn thread_aggregate(&self) -> Result<nuo_protocol::Thread, String> {
        let id = self.id().await;
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let reader = writer.reader().map_err(|error| error.to_string())?;
            reader
                .load_thread(&id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("thread '{id}' not found"))
        })
        .await
        .map_err(|error| format!("thread hydrate task failed: {error}"))?
    }

    /// All branch cursors for this thread.
    pub async fn branches(&self) -> Result<Vec<nuo_protocol::BranchState>, String> {
        let thread = self.thread_aggregate().await?;
        Ok(thread.state.branches.into_values().collect())
    }

    /// Fork a branch cursor at `from` (default: the active head). Facts are
    /// never copied; the branch shares the immutable prefix.
    pub async fn fork_branch(
        &self,
        name: impl Into<String>,
        from: Option<nuo_protocol::FactId>,
    ) -> Result<nuo_protocol::BranchId, String> {
        let mut thread = self.thread_aggregate().await?;
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
                name,
                nuo_protocol::BranchPolicy::default(),
                unix_timestamp() * 1000,
            )
            .map_err(|error| error.to_string())?;
        self.writer
            .save_thread_aggregate(thread)
            .await
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    /// Switch this store's transcript view to another branch.
    pub async fn switch_branch(&self, branch: nuo_protocol::BranchId) -> Result<(), String> {
        let mut thread = self.thread_aggregate().await?;
        if !thread.state.branches.contains_key(&branch) {
            return Err(format!("unknown branch '{}'", branch.0));
        }
        thread.state.active_branch = branch.clone();
        self.writer
            .save_thread_aggregate(thread.clone())
            .await
            .map_err(|error| error.to_string())?;
        let mut state = self.state.lock().await;
        state.data.active_branch = branch;
        state.data.transcript = transcript_from_thread(&thread);
        state.data.generation = uuid::Uuid::new_v4().to_string();
        state.invalidate_projection_cache();
        Ok(())
    }

    /// Delete a branch cursor. Facts remain while another branch reaches them.
    pub async fn delete_branch(&self, branch: nuo_protocol::BranchId) -> Result<(), String> {
        let mut thread = self.thread_aggregate().await?;
        thread
            .delete_branch(&branch, unix_timestamp() * 1000)
            .map_err(|error| error.to_string())?;
        self.writer
            .save_thread_aggregate(thread.clone())
            .await
            .map_err(|error| error.to_string())?;
        if thread.state.active_branch != self.state.lock().await.data.active_branch {
            self.switch_branch(thread.state.active_branch.clone())
                .await?;
        }
        Ok(())
    }

    /// Replace the in-memory transcript with the projection of `thread` and
    /// persist the aggregate.
    ///
    /// The durable write is still routed through this store during the
    /// migration; the transcript is a projection of the facts, never an
    /// independent source.
    pub async fn commit_thread(&self, thread: &nuo_protocol::Thread) -> Result<u64, String> {
        let (path, data, revision) = {
            let mut state = self.state.lock().await;
            state.data.transcript = transcript_from_thread(thread);
            state.data.active_branch = thread.state.active_branch.clone();
            state.data.generation = uuid::Uuid::new_v4().to_string();
            state.data.round_counter = thread.state.round_counter;
            state.data.updated_at = thread.updated_at_ms / 1000;
            state.invalidate_projection_cache();
            state.defer_persist = false;
            (state.path.clone(), state.data.clone(), thread.revision.0)
        };
        self.persist_off_runtime(path, data, self.blob_store.clone())
            .await?;
        Ok(revision)
    }
}

fn fork_fact_for_target(
    path: &[&nuo_protocol::ThreadFact],
    target: ForkTarget,
) -> Result<nuo_protocol::FactId, String> {
    use nuo_protocol::{FactPayload, Role};
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
fn tree_from_thread(thread: &nuo_protocol::Thread) -> nuo_protocol::ThreadTree {
    let mut tree = nuo_protocol::ThreadTree::new();
    for fact in thread.all_facts() {
        let kind = match &fact.payload {
            nuo_protocol::FactPayload::Message { message } => {
                nuo_protocol::ThreadEntryKind::Message {
                    message: message.clone(),
                }
            }
            nuo_protocol::FactPayload::Observation {
                call_id,
                tool_name,
                lifecycle,
                ..
            } => nuo_protocol::ThreadEntryKind::Message {
                message: lifecycle.lower_to_message(call_id, tool_name),
            },
            nuo_protocol::FactPayload::Compaction {
                summary,
                first_kept_fact_id,
                tokens_before,
                read_files,
                modified_files,
            } => nuo_protocol::ThreadEntryKind::Compaction {
                summary: summary.clone(),
                first_kept_entry_id: first_kept_fact_id.0.clone(),
                tokens_before: *tokens_before,
                read_files: read_files.clone(),
                modified_files: modified_files.clone(),
            },
            nuo_protocol::FactPayload::Termination { .. }
            | nuo_protocol::FactPayload::SystemNotice { .. } => {
                nuo_protocol::ThreadEntryKind::Custom {
                    custom_type: "notice".to_string(),
                    content: format!("{:?}", fact.payload),
                    display: None,
                }
            }
        };
        tree.entries.insert(
            fact.id.0.clone(),
            nuo_protocol::ThreadEntry {
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
pub(crate) fn transcript_from_thread(thread: &nuo_protocol::Thread) -> nuo_protocol::Transcript {
    use nuo_protocol::{CompactionMeta, EntryPayload, TranscriptEntry};

    let branch = &thread.state.active_branch;
    let mut transcript = nuo_protocol::Transcript::new();
    let Ok(facts) = thread.path(branch) else {
        return transcript;
    };
    for fact in facts {
        let seq = fact.seq.saturating_sub(1);
        match &fact.payload {
            nuo_protocol::FactPayload::Message { message } => {
                let mut entry = TranscriptEntry::from_message(seq, message);
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                transcript.push(entry);
            }
            nuo_protocol::FactPayload::Observation {
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
            nuo_protocol::FactPayload::Compaction {
                summary,
                first_kept_fact_id,
                tokens_before,
                read_files,
                modified_files,
            } => {
                let mut entry = TranscriptEntry::from_message(
                    seq,
                    &Message::new(nuo_protocol::Role::System, summary.clone()),
                );
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                entry.content = Some(summary.clone());
                entry.payload = EntryPayload::Compaction(Box::new(CompactionMeta {
                    first_kept_node_id: first_kept_fact_id.0.clone(),
                    split_parent_node_id: fact.parent_id.as_ref().map(|id| id.0.clone()),
                    folded_through_seq: first_kept_fact_id
                        .0
                        .trim_start_matches("fact_")
                        .parse::<u64>()
                        .unwrap_or(0),
                    tokens_before: *tokens_before,
                    read_files: read_files.clone(),
                    modified_files: modified_files.clone(),
                }));
                transcript.push(entry);
            }
            nuo_protocol::FactPayload::Termination {
                reason,
                partial_output,
                ..
            } => {
                let reason = match reason {
                    nuo_protocol::TerminationReason::UserInterrupt => "Interrupted by user",
                    nuo_protocol::TerminationReason::Timeout => "Execution timed out",
                    nuo_protocol::TerminationReason::FatalError { error } => error.as_str(),
                    nuo_protocol::TerminationReason::Superseded => "Superseded by user message",
                };
                let content = match partial_output {
                    Some(output) => format!("[Execution stopped: {reason}]\n{output}"),
                    None => format!("[Execution stopped: {reason}]"),
                };
                let mut entry = TranscriptEntry::from_message(
                    seq,
                    &Message::new(nuo_protocol::Role::System, content),
                );
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                transcript.push(entry);
            }
            nuo_protocol::FactPayload::SystemNotice {
                source,
                notice_type,
                content,
            } => {
                let mut entry = TranscriptEntry::from_message(
                    seq,
                    &Message::new(
                        nuo_protocol::Role::System,
                        format!("[System Notice from {source} ({notice_type})]: {content}"),
                    ),
                );
                entry.id = fact.id.0.clone();
                entry.created_at_ms = fact.timestamp_ms;
                transcript.push(entry);
            }
        }
    }
    transcript
}

/// Project [`ThreadData`] into the canonical thread aggregate.
///
/// Tool-role transcript entries become `Observation` facts (the canonical tool
/// execution representation); every other message stays a `Message` fact. The
/// projection keeps entry ids as fact ids so a later durable store can adopt
/// them without rewriting parent links.
pub(crate) fn thread_from_session_data(data: &ThreadData) -> nuo_protocol::Thread {
    use nuo_protocol::{
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
            nuo_protocol::EntryPayload::Message(_) => {
                let Some(message) = entry.to_message() else {
                    continue;
                };
                if message.role == nuo_protocol::Role::Tool {
                    let call_id = message.tool_call_id.clone().unwrap_or_default();
                    let tool_name = tool_names
                        .get(&call_id)
                        .cloned()
                        .unwrap_or_else(|| "tool".to_string());
                    let hash = message.content_blob.clone().unwrap_or_else(|| {
                        crate::blobs::BlobStore::hash(message.content.as_bytes())
                    });
                    let preview_length = message.content.len().min(512);
                    FactPayload::Observation {
                        call_id,
                        tool_name,
                        output_ref: BlobRef::new(hash, message.content.len() as u64, "text/plain")
                            .with_preview(message.content[..preview_length].to_string()),
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
            nuo_protocol::EntryPayload::Compaction(meta) => {
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
            nuo_protocol::EntryPayload::State(_) => continue,
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

    if let Some(head) = parent {
        let branch = data.active_branch.clone();
        thread.state.active_branch = branch.clone();
        thread.state.branches.insert(
            branch.clone(),
            nuo_protocol::BranchState {
                id: branch,
                name: data.active_branch.0.clone(),
                head: Some(head),
                forked_from: None,
                compaction_horizon,
                policy: nuo_protocol::BranchPolicy::default(),
                created_at_ms: thread.created_at_ms,
                updated_at_ms: thread.updated_at_ms,
            },
        );
    }
    thread.revision = ThreadRevision(seq.max(data.transcript.entries.len() as u64));
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
            origin: nuo_protocol::ThreadOrigin::Subagent,
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
                matches!(&entry.payload, nuo_protocol::EntryPayload::Message(payload)                     if payload.tool_call_id == call_id)
            })
            && let nuo_protocol::EntryPayload::Message(payload) = &mut entry.payload {
                payload.subagent = Some(nuo_protocol::SubagentRef {
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
fn persist_subagent_children(
    writer: &crate::db::PersistenceHandle,
    blob_store: &BlobStore,
    subagents: &[ThreadData],
) {
    for subagent in subagents {
        if let Err(error) = crate::thread::persist_to(writer, subagent, blob_store) {
            tracing::warn!(%error, subagent = %subagent.id, "could not persist subagent session; nested transcript is dropped");
        }
    }
}

/// Rebuild a transcript from a flat message window (entries get fresh ids;
/// projection history is cleared). The caller must mint a fresh
/// `ThreadData::generation` afterwards: the new transcript shares no rows
/// with the durable one.
pub(crate) fn rebuild_transcript_from_messages(messages: &[Message]) -> nuo_protocol::Transcript {
    let mut transcript = nuo_protocol::Transcript::new();
    for message in messages {
        transcript.push(nuo_protocol::TranscriptEntry::from_message(0, message));
    }
    transcript
}
