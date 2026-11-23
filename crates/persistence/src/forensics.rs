//! Best-effort request-projection archive (ADR-0240).
//!
//! A request projection is forensic evidence, not session state. It has no
//! foreign key, no turn-path acknowledgement, and no place in the derived
//! SQLite view. Each session owns a tiny append-only ring beside its journal:
//!
//! ```text
//! <journal_root>/<session>/forensics.jsonl    # current
//! <journal_root>/<session>/forensics.1.jsonl  # previous
//! ```
//!
//! One shared writer thread drains a bounded queue; it never fsyncs and drops
//! on overflow. A new session can record its first request before any other
//! row exists because nothing here depends on one.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Once, OnceLock};

use protocol::RequestProjection;

/// Rotate a session's ring at this size. Two files are retained, so the
/// archive is bounded at ~2x this value per session.
const RING_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Older file suffix. Only `forensics.1.jsonl` is kept beside the current file.
const ARCHIVE: &str = "forensics.1.jsonl";
const CURRENT: &str = "forensics.jsonl";
const QUEUE_CAPACITY: usize = 4096;
const MAX_BATCH: usize = 256;

/// Append one projection to a session's ring. Never blocks and never fails the
/// turn; a full queue is a dropped forensic record, not a round error.
pub(crate) fn record(dir: &Path, record: &RequestProjection) {
    let Ok(mut line) = serde_json::to_vec(record) else {
        return;
    };
    line.push(b'\n');
    let Some(sender) = sender() else {
        return;
    };
    match sender.try_send(Job {
        path: dir.join(CURRENT),
        line,
        ack: None,
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            tracing::debug!("forensics ring queue is full; dropped one request projection");
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}

/// Enqueue an append and wait until the one writer thread has attempted it.
/// Used by the awaiting API (tests, tooling) that asked to observe its own
/// write. It still does not fsync, and it never writes the file itself, so
/// there is exactly one writer (ADR-0240).
pub(crate) fn record_wait(dir: &Path, record: &RequestProjection) {
    let Ok(mut line) = serde_json::to_vec(record) else {
        return;
    };
    line.push(b'\n');
    let Some(sender) = sender() else {
        return;
    };
    let (ack, wait) = sync_channel(1);
    if sender
        .send(Job {
            path: dir.join(CURRENT),
            line,
            ack: Some(ack),
        })
        .is_err()
    {
        return;
    }
    let _ = wait.recv();
}

struct Job {
    path: PathBuf,
    line: Vec<u8>,
    /// Present when the caller must observe that the attempt finished.
    ack: Option<SyncSender<()>>,
}

/// Read the newest `limit` records, oldest first. Corrupt lines are skipped;
/// the ring is evidence, not a schema.
pub(crate) fn read(dir: &Path, limit: usize) -> Vec<RequestProjection> {
    if limit == 0 {
        return Vec::new();
    }
    let mut raw = Vec::new();
    read_file(&dir.join(ARCHIVE), &mut raw);
    read_file(&dir.join(CURRENT), &mut raw);
    // A retried logical request re-records the same `(round, turn)`; the ring
    // keeps the last capture, matching the retired table's replace semantics.
    let mut index: std::collections::HashMap<(u64, u64), usize> = std::collections::HashMap::new();
    let mut records: Vec<RequestProjection> = Vec::with_capacity(raw.len());
    for record in raw {
        match index.get(&(record.round, record.turn)) {
            Some(&existing) => records[existing] = record,
            None => {
                index.insert((record.round, record.turn), records.len());
                records.push(record);
            }
        }
    }
    if records.len() > limit {
        let drop = records.len() - limit;
        records.drain(0..drop);
    }
    records
}

fn read_file(path: &Path, out: &mut Vec<RequestProjection>) {
    let Ok(file) = File::open(path) else {
        return;
    };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<RequestProjection>(&line) {
            Ok(record) => out.push(record),
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "skipped a corrupt forensic line");
            }
        }
    }
}

fn sender() -> Option<&'static SyncSender<Job>> {
    static SENDER: OnceLock<SyncSender<Job>> = OnceLock::new();
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let (tx, rx) = sync_channel(QUEUE_CAPACITY);
        let _ = std::thread::Builder::new()
            .name("nuo-forensics".to_string())
            .spawn(move || run(rx));
        let _ = SENDER.set(tx);
    });
    SENDER.get()
}

fn run(rx: Receiver<Job>) {
    let mut files: HashMap<PathBuf, File> = HashMap::new();
    while let Ok(first) = rx.recv() {
        let mut batch = Vec::with_capacity(MAX_BATCH.min(32));
        batch.push(first);
        while batch.len() < MAX_BATCH {
            match rx.try_recv() {
                Ok(job) => batch.push(job),
                Err(_) => break,
            }
        }
        for job in batch {
            let Job { path, line, ack } = job;
            if !files.contains_key(&path) {
                match open(&path) {
                    Some(file) => {
                        files.insert(path.clone(), file);
                    }
                    None => {
                        let _ = ack.map(|ack| ack.send(()));
                        continue;
                    }
                }
            }
            let Some(file) = files.get_mut(&path) else {
                let _ = ack.map(|ack| ack.send(()));
                continue;
            };
            if file.write_all(&line).is_err() {
                files.remove(&path);
                let _ = ack.map(|ack| ack.send(()));
                continue;
            }
            let oversized = file
                .metadata()
                .map(|meta| meta.len() > RING_MAX_BYTES)
                .unwrap_or(false);
            if oversized {
                files.remove(&path);
                rotate(&path);
            }
            let _ = ack.map(|ack| ack.send(()));
        }
    }
}

fn open(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return None;
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

fn rotate(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let archive = dir.join(ARCHIVE);
    let _ = fs::remove_file(&archive);
    let _ = fs::rename(path, &archive);
}
