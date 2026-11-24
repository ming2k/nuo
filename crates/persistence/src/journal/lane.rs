//! Group-commit lanes for the session journal (ADR-0237).
//!
//! A lane is a dedicated OS thread that owns a disjoint set of journal files.
//! Appends arrive over a bounded async channel; the lane writes every queued
//! frame for a batch and then issues **one `fdatasync` per touched file**.
//! Sessions are hashed onto lanes, so:
//!
//! - appends to one session are FIFO (a session is pinned to one lane);
//! - a slow fsync on session A never delays session B on another lane;
//! - the async loop only waits for its own ack, and that ack is bounded by
//!   one batch + one fsync, not by unrelated SQLite work.
//!
//! Lane count is the parallel-fsync budget. It tracks the core count (clamped)
//! rather than a small constant: the floor on a commit is the disk, not the
//! CPU, and the hashed assignment means fewer lanes means more session pairs
//! sharing one ack chain.
//!
//! A failed write truncates the file back to the append offset it observed, so
//! a partially written frame can never be mistaken for a commit by the reader.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use tokio::sync::{mpsc, oneshot};

/// Bound on one lane's in-flight queue. Appends back-pressure the caller at
/// the *turn* boundary, never mid-stream.
const LANE_QUEUE: usize = 1024;
/// Maximum frames written before one group fsync. Bounds the ack latency of
/// an append that lands behind a busy lane.
const MAX_BATCH: usize = 256;
/// Cap on cached file descriptors per lane. Sessions are few; the cap only
/// protects against a pathological scan of thousands of journals.
const MAX_CACHED_FILES: usize = 1024;

/// One append handed to a lane.
pub(crate) struct LaneJob {
    pub(crate) path: PathBuf,
    pub(crate) bytes: Vec<u8>,
    pub(crate) ack: oneshot::Sender<io::Result<()>>,
}

/// The process-wide lane pool. Cloning is cheap (`Arc`); the global instance
/// is created on first use.
pub(crate) struct JournalRuntime {
    lanes: Vec<mpsc::Sender<LaneJob>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for JournalRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalRuntime")
            .field("lanes", &self.lanes.len())
            .finish_non_exhaustive()
    }
}

impl JournalRuntime {
    /// The process-wide runtime.
    ///
    /// One lane is one `fsync` chain. Sessions are hashed onto a lane, so a
    /// collision puts two sessions behind the same fsync — the exact
    /// head-of-line blocking the lane model exists to remove. Four lanes was
    /// enough to decouple "a handful" of sessions; on a many-core host it also
    /// capped concurrent `fdatasync` calls at four.
    ///
    /// The ceiling is deliberately finite: a lane is an OS thread with a
    /// 1024-slot queue, and beyond ~16 the disk, not the CPU, is the floor.
    pub(crate) fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<JournalRuntime>> = OnceLock::new();
        GLOBAL.get_or_init(|| Self::spawn(default_lanes())).clone()
    }

    fn spawn(lanes: usize) -> Arc<Self> {
        let lanes = lanes.max(1);
        let mut senders = Vec::with_capacity(lanes);
        let mut handles = Vec::with_capacity(lanes);
        for index in 0..lanes {
            let (tx, rx) = mpsc::channel(LANE_QUEUE);
            senders.push(tx);
            let handle = std::thread::Builder::new()
                .name(format!("nuo-journal-lane-{index}"))
                .spawn(move || run_lane(rx))
                .ok();
            if let Some(handle) = handle {
                handles.push(handle);
            }
        }
        Arc::new(Self {
            lanes: senders,
            handles: Mutex::new(handles),
        })
    }

    /// Submit one frame to the lane that owns `key` (the session id).
    ///
    /// Returns the ack receiver; the caller awaits it. The `send` itself is
    /// async and yields when the lane queue is full, so no OS thread is
    /// blocked by a full journal.
    pub(crate) async fn submit(
        &self,
        key: &str,
        path: PathBuf,
        bytes: Vec<u8>,
    ) -> io::Result<oneshot::Receiver<io::Result<()>>> {
        let index = (crc32c::crc32c(key.as_bytes()) as usize) % self.lanes.len();
        let (ack, rx) = oneshot::channel();
        let job = LaneJob { path, bytes, ack };
        self.lanes[index]
            .send(job)
            .await
            .map_err(|_| io::Error::other("journal lane stopped"))?;
        Ok(rx)
    }
}

impl Drop for JournalRuntime {
    fn drop(&mut self) {
        // Closing the senders lets every lane drain and exit. Only local test
        // runtimes are ever dropped; the global one lives for the process.
        self.lanes.clear();
        let handles = std::mem::take(
            &mut *self
                .handles
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        );
        for handle in handles {
            let _ = handle.join();
        }
    }
}

fn default_lanes() -> usize {
    // One lane per core, floor 2, ceiling 16. `min(4, cores)` left most of a
    // many-core host's fsync parallelism unused and made a collision between
    // two busy sessions likely; the ceiling keeps a pathological core count
    // from spawning more lane threads than the disk can serve.
    std::thread::available_parallelism()
        .map(|count| count.get().clamp(2, 16))
        .unwrap_or(4)
}

fn run_lane(mut rx: mpsc::Receiver<LaneJob>) {
    let mut files: HashMap<PathBuf, File> = HashMap::new();
    while let Some(first) = rx.blocking_recv() {
        let mut batch = Vec::with_capacity(MAX_BATCH.min(32));
        batch.push(first);
        while batch.len() < MAX_BATCH {
            match rx.try_recv() {
                Ok(job) => batch.push(job),
                Err(_) => break,
            }
        }
        if files.len() > MAX_CACHED_FILES {
            files.clear();
        }
        run_batch(&mut files, batch);
    }
}

fn run_batch(files: &mut HashMap<PathBuf, File>, batch: Vec<LaneJob>) {
    let mut outcomes: Vec<(LaneJob, Option<PathBuf>, Option<String>)> =
        Vec::with_capacity(batch.len());
    let mut failed_paths: HashSet<PathBuf> = HashSet::new();
    let mut touched: BTreeSet<PathBuf> = BTreeSet::new();

    for job in batch {
        if failed_paths.contains(&job.path) {
            outcomes.push((
                job,
                None,
                Some("an earlier append to this journal failed in the same batch".to_string()),
            ));
            continue;
        }
        let outcome = write_one(files, &job.path, &job.bytes);
        match outcome {
            Ok(()) => {
                touched.insert(job.path.clone());
                let path = job.path.clone();
                outcomes.push((job, Some(path), None));
            }
            Err(error) => {
                files.remove(&job.path);
                failed_paths.insert(job.path.clone());
                outcomes.push((job, None, Some(error.to_string())));
            }
        }
    }

    let mut sync_errors: HashMap<PathBuf, String> = HashMap::new();
    for path in &touched {
        let result = match files.get(path) {
            Some(file) => file.sync_data(),
            None => Ok(()),
        };
        if let Err(error) = result {
            sync_errors.insert(path.clone(), error.to_string());
        }
    }

    for (job, path, write_error) in outcomes {
        let result = match (write_error, path) {
            (Some(error), _) => Err(io::Error::other(error)),
            (None, Some(path)) => match sync_errors.get(&path) {
                Some(error) => Err(io::Error::other(error.clone())),
                None => Ok(()),
            },
            (None, None) => Err(io::Error::other("journal append lost its path")),
        };
        let _ = job.ack.send(result);
    }
}

fn write_one(files: &mut HashMap<PathBuf, File>, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let file = handle(files, path)?;
    let start = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if let Err(error) = file.write_all(bytes) {
        // A failed write can leave a partial frame. O_APPEND means every byte
        // went to `start..`; truncating there removes the fragment and keeps
        // the journal's valid tail exactly where the reader last saw it.
        let _ = file.set_len(start);
        return Err(error);
    }
    Ok(())
}

fn handle<'a>(files: &'a mut HashMap<PathBuf, File>, path: &Path) -> io::Result<&'a mut File> {
    if !files.contains_key(path) {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not open journal segment {}: {error}", path.display()),
                )
            })?;
        files.insert(path.to_path_buf(), file);
    }
    files
        .get_mut(path)
        .ok_or_else(|| io::Error::other("journal lane lost its file handle"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn runtime() -> Arc<JournalRuntime> {
        JournalRuntime::spawn(2)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn appends_are_durable_when_the_ack_arrives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seg.log");
        let runtime = runtime();
        let ack = runtime
            .submit("session-a", path.clone(), b"first".to_vec())
            .await
            .unwrap();
        ack.await.unwrap().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_batch_covers_multiple_sessions_on_the_same_lane() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime();
        let mut acks = Vec::new();
        for index in 0..8 {
            let path = dir.path().join(format!("seg-{index}.log"));
            let ack = runtime
                .submit("same-session", path, format!("frame-{index}").into_bytes())
                .await
                .unwrap();
            acks.push(ack);
        }
        for ack in acks {
            ack.await.unwrap().unwrap();
        }
        for index in 0..8 {
            let bytes = std::fs::read(dir.path().join(format!("seg-{index}.log"))).unwrap();
            assert_eq!(bytes, format!("frame-{index}").into_bytes());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn different_lanes_do_not_share_an_ack_chain() {
        // Two keys that hash to different lanes; both acks must complete
        // independently (this is a smoke test for per-lane queues).
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime();
        let mut keys = std::collections::HashSet::new();
        let mut chosen = Vec::new();
        for index in 0..32 {
            let key = format!("session-{index}");
            let lane = (crc32c::crc32c(key.as_bytes()) as usize) % 2;
            if keys.insert(lane) {
                chosen.push((key, dir.path().join(format!("seg-{lane}.log"))));
            }
            if chosen.len() == 2 {
                break;
            }
        }
        assert_eq!(chosen.len(), 2);
        let mut acks = Vec::new();
        for (key, path) in chosen {
            acks.push(runtime.submit(&key, path, b"x".to_vec()).await.unwrap());
        }
        for ack in acks {
            tokio::time::timeout(Duration::from_secs(5), ack)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
}
