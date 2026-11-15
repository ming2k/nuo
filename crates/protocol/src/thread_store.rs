//! Persistence port for the durable `Thread` aggregate.
//!
//! The port is deliberately small: hydrate a thread, append a delta, execute a
//! command, and garbage-collect unreachable facts/blob bytes. Semantic folding,
//! request projection, and UI rendering are not storage concerns.
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::observation::Trust;
use crate::thread::{
    BlobRef, CommandEnvelope, CommandOutcome, FactId, Thread, ThreadDelta, ThreadError, ThreadId,
    ThreadRevision, WriterLease,
};

/// A durable append acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    /// Highest sequence number the sink durably accepted.
    pub durable_upto: u64,
    /// Revision after the append.
    pub revision: ThreadRevision,
    /// True when the append was an idempotent replay.
    pub replayed: bool,
}

/// Acknowledgement of one command. Replaying the command id returns this value
/// without applying the command a second time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAck {
    pub command_id: String,
    pub outcome: CommandOutcome,
    pub revision: ThreadRevision,
    pub replayed: bool,
}

/// Report from one GC pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    pub facts_removed: usize,
    pub blobs_removed: usize,
}

/// Store errors. These are typed; callers never string-match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadStoreError {
    NotFound(ThreadId),
    AlreadyExists(ThreadId),
    Backend(String),
    WatermarkConflict {
        expected: u64,
        actual: u64,
    },
    RevisionConflict {
        expected: ThreadRevision,
        actual: ThreadRevision,
    },
    ReplayConflict(ThreadId),
    WriterLeaseLost,
    LeaseHeld {
        owner: String,
        expires_at_ms: u64,
    },
    BlobMissing(String),
    BlobTooLarge {
        size: u64,
        max: u64,
    },
    QueueFull,
    TurnActive,
    Forbidden(String),
    UnsupportedPromotion,
    Invalid(ThreadError),
}

impl std::fmt::Display for ThreadStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "thread '{id}' not found"),
            Self::AlreadyExists(id) => write!(f, "thread '{id}' already exists"),
            Self::Backend(message) => write!(f, "thread store backend error: {message}"),
            Self::WatermarkConflict { expected, actual } => {
                write!(f, "watermark conflict: expected {expected}, got {actual}")
            }
            Self::RevisionConflict { expected, actual } => write!(
                f,
                "revision conflict: expected {}, got {}",
                expected.0, actual.0
            ),
            Self::ReplayConflict(id) => {
                write!(f, "replayed delta for '{id}' does not match durable facts")
            }
            Self::WriterLeaseLost => write!(f, "writer lease was lost"),
            Self::LeaseHeld {
                owner,
                expires_at_ms,
            } => write!(f, "thread is leased by '{owner}' until {expires_at_ms}"),
            Self::BlobMissing(hash) => write!(f, "blob '{hash}' is missing"),
            Self::BlobTooLarge { size, max } => {
                write!(f, "blob of {size} bytes exceeds the {max}-byte inline cap")
            }
            Self::QueueFull => write!(f, "input queue is full"),
            Self::TurnActive => write!(f, "a turn is already active on this branch"),
            Self::Forbidden(reason) => write!(f, "forbidden: {reason}"),
            Self::UnsupportedPromotion => {
                write!(f, "branch promotion is unsupported in this version")
            }
            Self::Invalid(error) => write!(f, "invalid thread delta: {error}"),
        }
    }
}

impl std::error::Error for ThreadStoreError {}

impl From<ThreadError> for ThreadStoreError {
    fn from(error: ThreadError) -> Self {
        Self::Invalid(error)
    }
}

/// Content-addressed storage for large payload bytes.
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(
        &self,
        bytes: &[u8],
        mime: &str,
        trust: Trust,
    ) -> Result<BlobRef, ThreadStoreError>;

    async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>, ThreadStoreError>;

    async fn delete(&self, hash: &str) -> Result<bool, ThreadStoreError>;

    /// Retain exactly the given hashes; return how many blobs were removed.
    async fn retain_only(&self, hashes: &BTreeSet<String>) -> Result<usize, ThreadStoreError>;
}

/// Durable thread persistence.
#[async_trait]
pub trait ThreadStore: Send + Sync {
    /// Create a new thread. Creating an existing id is an error.
    async fn create(&self, thread: Thread) -> Result<(), ThreadStoreError>;

    /// Load a thread, or `None` when it does not exist.
    async fn hydrate(&self, id: &ThreadId) -> Result<Option<Thread>, ThreadStoreError>;

    /// Load a point-in-time aggregate. A real store may serve this from a
    /// snapshot; the default is a full hydrate, which is always correct.
    async fn snapshot(&self, id: &ThreadId) -> Result<Option<Thread>, ThreadStoreError> {
        self.hydrate(id).await
    }

    /// Append one delta at the expected thread revision.
    ///
    /// Replaying the same delta is idempotent. A stale `expected_revision` is
    /// rejected with [`ThreadStoreError::RevisionConflict`].
    async fn append(
        &self,
        delta: ThreadDelta,
        expected_revision: ThreadRevision,
    ) -> Result<Ack, ThreadStoreError>;

    /// Apply one command exactly once against the named thread. The command id
    /// is the idempotency key.
    async fn execute(
        &self,
        thread: &ThreadId,
        command: CommandEnvelope,
    ) -> Result<CommandAck, ThreadStoreError>;

    /// Acquire the single-writer lease for a thread.
    async fn acquire_lease(
        &self,
        thread: &ThreadId,
        owner: &str,
        ttl: Duration,
    ) -> Result<WriterLease, ThreadStoreError>;

    /// Release a lease only when the caller still holds the current token.
    async fn release_lease(&self, lease: &WriterLease) -> Result<(), ThreadStoreError>;

    /// Mark-and-sweep facts unreachable from every branch, then delete blobs
    /// absent from the remaining facts.
    async fn gc(&self, thread: &ThreadId, grace: Duration) -> Result<GcReport, ThreadStoreError>;

    /// The blob store this thread store composes.
    fn blobs(&self) -> Arc<dyn BlobStore>;
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn preview(bytes: &[u8]) -> Option<String> {
    const PREVIEW_BYTES: usize = 512;
    if bytes.is_empty() {
        return None;
    }
    let end = bytes.len().min(PREVIEW_BYTES);
    let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    Some(text)
}

/// In-memory blob store for tests and the first vertical slice.
#[derive(Default)]
pub struct MemoryBlobStore {
    inner: Mutex<HashMap<String, (BlobRef, Vec<u8>)>>,
}

impl MemoryBlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (BlobRef, Vec<u8>)>> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[async_trait]
impl BlobStore for MemoryBlobStore {
    async fn put(
        &self,
        bytes: &[u8],
        mime: &str,
        trust: Trust,
    ) -> Result<BlobRef, ThreadStoreError> {
        let hash = hash_bytes(bytes);
        let reference = BlobRef {
            hash: hash.clone(),
            size: bytes.len() as u64,
            mime: mime.to_string(),
            preview: preview(bytes),
            trust,
        };
        self.lock()
            .entry(hash)
            .or_insert_with(|| (reference.clone(), bytes.to_vec()));
        Ok(reference)
    }

    async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>, ThreadStoreError> {
        Ok(self.lock().get(hash).map(|(_, bytes)| bytes.clone()))
    }

    async fn delete(&self, hash: &str) -> Result<bool, ThreadStoreError> {
        Ok(self.lock().remove(hash).is_some())
    }

    async fn retain_only(&self, hashes: &BTreeSet<String>) -> Result<usize, ThreadStoreError> {
        let mut blobs = self.lock();
        let before = blobs.len();
        blobs.retain(|hash, _| hashes.contains(hash));
        Ok(before - blobs.len())
    }
}

#[derive(Default)]
struct MemoryInner {
    threads: HashMap<ThreadId, Thread>,
    receipts: HashMap<(ThreadId, String), CommandAck>,
    leases: HashMap<ThreadId, WriterLease>,
    next_fencing_token: HashMap<ThreadId, u64>,
}

/// In-memory store for tests and the first vertical slice.
#[derive(Default)]
pub struct MemoryThreadStore {
    inner: Mutex<MemoryInner>,
    blobs: Arc<MemoryBlobStore>,
}

impl MemoryThreadStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(MemoryInner::default()),
            blobs: Arc::new(MemoryBlobStore::new()),
        }
    }

    /// Insert a freshly created thread. Test/fixture helper.
    pub fn insert(&self, thread: Thread) {
        self.lock().threads.insert(thread.id.clone(), thread);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryInner> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn check_lease(
        inner: &MemoryInner,
        thread: &ThreadId,
        token: u64,
    ) -> Result<(), ThreadStoreError> {
        let Some(lease) = inner.leases.get(thread) else {
            // No lease exists: local single-process callers may write directly.
            return Ok(());
        };
        if lease.expires_at_ms <= now_ms() {
            return Ok(());
        }
        if lease.fencing_token != token {
            return Err(ThreadStoreError::WriterLeaseLost);
        }
        Ok(())
    }
}

#[async_trait]
impl ThreadStore for MemoryThreadStore {
    async fn create(&self, thread: Thread) -> Result<(), ThreadStoreError> {
        let mut inner = self.lock();
        if inner.threads.contains_key(&thread.id) {
            return Err(ThreadStoreError::AlreadyExists(thread.id));
        }
        inner.threads.insert(thread.id.clone(), thread);
        Ok(())
    }

    async fn hydrate(&self, id: &ThreadId) -> Result<Option<Thread>, ThreadStoreError> {
        Ok(self.lock().threads.get(id).cloned())
    }

    async fn append(
        &self,
        delta: ThreadDelta,
        expected_revision: ThreadRevision,
    ) -> Result<Ack, ThreadStoreError> {
        let mut inner = self.lock();
        let thread = inner
            .threads
            .get_mut(&delta.thread_id)
            .ok_or_else(|| ThreadStoreError::NotFound(delta.thread_id.clone()))?;

        // Replay is checked before the expected revision: a transport retry
        // carries the original revision and must be acknowledged, not treated
        // as a stale command.
        if delta.revision <= thread.revision {
            let replay_ok = delta
                .new_facts
                .iter()
                .all(|fact| thread.graph.get(&fact.id) == Some(fact));
            if replay_ok {
                return Ok(Ack {
                    durable_upto: delta.new_watermark_seq.min(thread.max_seq()),
                    revision: thread.revision,
                    replayed: true,
                });
            }
            return Err(ThreadStoreError::ReplayConflict(delta.thread_id));
        }

        if thread.revision != expected_revision {
            return Err(ThreadStoreError::RevisionConflict {
                expected: expected_revision,
                actual: thread.revision,
            });
        }

        if delta.previous_watermark_seq != thread.max_seq() {
            return Err(ThreadStoreError::WatermarkConflict {
                expected: thread.max_seq(),
                actual: delta.previous_watermark_seq,
            });
        }

        thread.apply_delta(delta)?;
        Ok(Ack {
            durable_upto: thread.max_seq(),
            revision: thread.revision,
            replayed: false,
        })
    }

    async fn execute(
        &self,
        thread_id: &ThreadId,
        command: CommandEnvelope,
    ) -> Result<CommandAck, ThreadStoreError> {
        let mut inner = self.lock();
        let key = (thread_id.clone(), command.meta.command_id.clone());
        if let Some(receipt) = inner.receipts.get(&key) {
            let mut receipt = receipt.clone();
            receipt.replayed = true;
            return Ok(receipt);
        }
        Self::check_lease(&inner, thread_id, command.meta.fencing_token)?;
        let thread = inner
            .threads
            .get_mut(thread_id)
            .ok_or_else(|| ThreadStoreError::NotFound(thread_id.clone()))?;
        if thread.revision != command.meta.expected_revision {
            return Err(ThreadStoreError::RevisionConflict {
                expected: command.meta.expected_revision,
                actual: thread.revision,
            });
        }
        let outcome = thread.apply_command(command.command, now_ms())?;
        let ack = CommandAck {
            command_id: command.meta.command_id.clone(),
            revision: thread.revision,
            outcome,
            replayed: false,
        };
        inner.receipts.insert(key, ack.clone());
        Ok(ack)
    }

    async fn acquire_lease(
        &self,
        thread: &ThreadId,
        owner: &str,
        ttl: Duration,
    ) -> Result<WriterLease, ThreadStoreError> {
        let mut inner = self.lock();
        if !inner.threads.contains_key(thread) {
            return Err(ThreadStoreError::NotFound(thread.clone()));
        }
        let now = now_ms();
        if let Some(current) = inner.leases.get(thread)
            && current.expires_at_ms > now
            && current.owner_id != owner
        {
            return Err(ThreadStoreError::LeaseHeld {
                owner: current.owner_id.clone(),
                expires_at_ms: current.expires_at_ms,
            });
        }
        let token = inner
            .next_fencing_token
            .entry(thread.clone())
            .and_modify(|token| *token = token.saturating_add(1))
            .or_insert(1);
        let lease = WriterLease {
            thread_id: thread.clone(),
            owner_id: owner.to_string(),
            fencing_token: *token,
            expires_at_ms: now.saturating_add(ttl.as_millis() as u64),
        };
        inner.leases.insert(thread.clone(), lease.clone());
        Ok(lease)
    }

    async fn release_lease(&self, lease: &WriterLease) -> Result<(), ThreadStoreError> {
        let mut inner = self.lock();
        match inner.leases.get(&lease.thread_id) {
            Some(current) if current.fencing_token == lease.fencing_token => {
                inner.leases.remove(&lease.thread_id);
                Ok(())
            }
            Some(_) => Err(ThreadStoreError::WriterLeaseLost),
            None => Ok(()),
        }
    }

    async fn gc(&self, thread: &ThreadId, grace: Duration) -> Result<GcReport, ThreadStoreError> {
        let (facts_removed, live_blobs) = {
            let mut inner = self.lock();
            let thread = inner
                .threads
                .get_mut(thread)
                .ok_or_else(|| ThreadStoreError::NotFound(thread.clone()))?;
            let cutoff = now_ms().saturating_sub(grace.as_millis() as u64);
            let eligible: Vec<FactId> = thread
                .gc_eligible_facts()
                .into_iter()
                .filter(|id| {
                    thread
                        .graph
                        .get(id)
                        .is_some_and(|fact| fact.timestamp_ms <= cutoff)
                })
                .collect();
            let mut facts_removed = 0;
            for id in eligible {
                if thread.graph.remove(&id).is_some() {
                    facts_removed += 1;
                }
            }
            let live_blobs: BTreeSet<String> = thread
                .live_facts()
                .into_iter()
                .filter_map(|id| thread.graph.get(&id))
                .filter_map(|fact| fact.payload.blob_ref())
                .map(|reference| reference.hash.clone())
                .collect();
            (facts_removed, live_blobs)
        };
        let blobs_removed = self.blobs.retain_only(&live_blobs).await?;
        Ok(GcReport {
            facts_removed,
            blobs_removed,
        })
    }

    fn blobs(&self) -> Arc<dyn BlobStore> {
        self.blobs.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::{
        BranchPolicy, CommandMeta, FactContext, FactPayload, ThreadCommand, ThreadStatus,
    };

    fn payload(text: &str) -> FactPayload {
        FactPayload::SystemNotice {
            source: "test".to_string(),
            notice_type: "note".to_string(),
            content: text.to_string(),
        }
    }

    fn store_with_thread() -> (MemoryThreadStore, Thread) {
        let store = MemoryThreadStore::new();
        let thread = Thread::new(ThreadId::new("thread-1"), 1_000);
        store.insert(thread.clone());
        (store, thread)
    }

    fn meta(revision: u64, id: &str) -> CommandMeta {
        CommandMeta {
            command_id: id.to_string(),
            expected_revision: ThreadRevision(revision),
            fencing_token: 0,
        }
    }

    #[tokio::test]
    async fn append_then_hydrate_round_trips() {
        let (store, mut thread) = store_with_thread();
        thread
            .append(payload("one"), FactContext::default(), 1_001)
            .unwrap();
        let delta = thread.drain_delta(0);
        let ack = store.append(delta, ThreadRevision(0)).await.unwrap();
        assert_eq!(ack.durable_upto, 1);
        assert_eq!(ack.revision, ThreadRevision(1));

        let hydrated = store
            .hydrate(&ThreadId::new("thread-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hydrated.max_seq(), 1);
        assert_eq!(hydrated.revision, ThreadRevision(1));
        assert_eq!(hydrated.state.active_branch, thread.state.active_branch);
    }

    #[tokio::test]
    async fn replay_is_idempotent() {
        let (store, mut thread) = store_with_thread();
        thread
            .append(payload("one"), FactContext::default(), 1_001)
            .unwrap();
        let delta = thread.drain_delta(0);
        store
            .append(delta.clone(), ThreadRevision(0))
            .await
            .unwrap();
        let ack = store.append(delta, ThreadRevision(0)).await.unwrap();
        assert!(ack.replayed);
        assert_eq!(ack.durable_upto, 1);
        assert_eq!(ack.revision, ThreadRevision(1));
    }

    #[tokio::test]
    async fn stale_watermark_is_rejected() {
        let (store, mut thread) = store_with_thread();
        thread
            .append(payload("one"), FactContext::default(), 1_001)
            .unwrap();
        store
            .append(thread.drain_delta(0), ThreadRevision(0))
            .await
            .unwrap();

        thread
            .append(payload("two"), FactContext::default(), 1_002)
            .unwrap();
        let mut delta = thread.drain_delta(0);
        delta.previous_watermark_seq = 0;
        let error = store.append(delta, ThreadRevision(1)).await.unwrap_err();
        assert!(matches!(error, ThreadStoreError::WatermarkConflict { .. }));
    }

    #[tokio::test]
    async fn fork_delta_persists_the_branch_cursor() {
        let (store, mut thread) = store_with_thread();
        let first = thread
            .append(payload("one"), FactContext::default(), 1_001)
            .unwrap();
        store
            .append(thread.drain_delta(0), ThreadRevision(0))
            .await
            .unwrap();

        let branch = thread
            .fork(first, "side", BranchPolicy::default(), 1_002)
            .unwrap();
        store
            .append(thread.drain_delta(1), ThreadRevision(1))
            .await
            .unwrap();

        let hydrated = store
            .hydrate(&ThreadId::new("thread-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hydrated.state.branches.len(), 2);
        assert_eq!(hydrated.state.branch(&branch).unwrap().name, "side");
    }

    #[tokio::test]
    async fn compaction_delta_persists_the_branch_horizon() {
        let (store, mut thread) = store_with_thread();
        let a = thread
            .append(payload("a"), FactContext::default(), 1_001)
            .unwrap();
        let b = thread
            .append(payload("b"), FactContext::default(), 1_002)
            .unwrap();
        store
            .append(thread.drain_delta(0), ThreadRevision(0))
            .await
            .unwrap();

        thread
            .compact(&crate::BranchId::main(), "summary", b.clone(), 100, 1_003)
            .unwrap();
        store
            .append(thread.drain_delta(2), ThreadRevision(2))
            .await
            .unwrap();

        let hydrated = store
            .hydrate(&ThreadId::new("thread-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            hydrated
                .state
                .branch(&crate::BranchId::main())
                .unwrap()
                .compaction_horizon,
            Some(b)
        );
        assert!(
            !hydrated
                .path(&crate::BranchId::main())
                .unwrap()
                .iter()
                .any(|fact| fact.id == a)
        );
    }

    #[tokio::test]
    async fn revision_conflict_is_rejected() {
        let (store, mut thread) = store_with_thread();
        thread
            .append(payload("one"), FactContext::default(), 1_001)
            .unwrap();
        let mut delta = thread.drain_delta(0);
        delta.revision = ThreadRevision(99);
        let error = store.append(delta, ThreadRevision(99)).await.unwrap_err();
        assert!(matches!(error, ThreadStoreError::RevisionConflict { .. }));
    }

    #[tokio::test]
    async fn status_transition_persists() {
        let (store, mut thread) = store_with_thread();
        thread.state.status = ThreadStatus::Idle;
        thread.revision = ThreadRevision(1);
        let mut delta = thread.drain_delta(0);
        delta.revision = ThreadRevision(1);
        store.append(delta, ThreadRevision(0)).await.unwrap();
        let hydrated = store
            .hydrate(&ThreadId::new("thread-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hydrated.state.status, ThreadStatus::Idle);
    }

    #[tokio::test]
    async fn command_replay_returns_the_prior_ack() {
        let (store, _thread) = store_with_thread();
        let fact = crate::ThreadFact::new(
            FactId::from_raw("fact-1"),
            None,
            1,
            1_001,
            payload("one"),
            FactContext::default(),
        );
        let command = CommandEnvelope {
            meta: meta(0, "cmd-1"),
            command: ThreadCommand::AppendFacts { facts: vec![fact] },
        };
        let ack = store
            .execute(&ThreadId::new("thread-1"), command.clone())
            .await
            .unwrap();
        assert!(!ack.replayed);
        let replay = store
            .execute(&ThreadId::new("thread-1"), command)
            .await
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.revision, ack.revision);
    }

    #[tokio::test]
    async fn stale_fencing_token_is_rejected() {
        let (store, mut thread) = store_with_thread();
        let lease = store
            .acquire_lease(
                &ThreadId::new("thread-1"),
                "owner-a",
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        let fact = crate::ThreadFact::new(
            FactId::from_raw("fact-2"),
            None,
            2,
            1_002,
            payload("two"),
            FactContext::default(),
        );
        let command = CommandEnvelope {
            meta: CommandMeta {
                command_id: "cmd-1".to_string(),
                expected_revision: thread.revision,
                fencing_token: lease.fencing_token + 1,
            },
            command: ThreadCommand::AppendFacts { facts: vec![fact] },
        };
        let error = store
            .execute(&ThreadId::new("thread-1"), command)
            .await
            .unwrap_err();
        assert_eq!(error, ThreadStoreError::WriterLeaseLost);
        thread.revision = ThreadRevision(0);
    }

    #[tokio::test]
    async fn blob_store_deduplicates_and_hashes() {
        let store = MemoryBlobStore::new();
        let first = store
            .put(b"same bytes", "text/plain", Trust::Trusted)
            .await
            .unwrap();
        let second = store
            .put(b"same bytes", "text/plain", Trust::Trusted)
            .await
            .unwrap();
        assert_eq!(first.hash, second.hash);
        assert_eq!(
            store.get(&first.hash).await.unwrap().unwrap(),
            b"same bytes"
        );
        assert_eq!(first.preview.as_deref(), Some("same bytes"));
    }
}
