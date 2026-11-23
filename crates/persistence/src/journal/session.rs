//! One session's append-only journal: segment files, snapshots, and recovery
//! (ADR-0236).
//!
//! The journal is the durable **source of truth** for a session. SQLite is a
//! derived projection that is updated after the journal ack; it may lag, and
//! it can be rebuilt from these files.
//!
//! On-disk layout:
//!
//! ```text
//! <journal_root>/<session_id>/
//!   seg-00000000000000000001.log   # frames, rotated at 64 MiB
//!   snap-00000000000000000512.json # full ThreadData at that seq
//! ```
//!
//! Recovery rule: load the newest parsable snapshot, then replay every frame
//! above its seq. A frame that fails structure/CRC/JSON ends the replay — the
//! bytes after it are a torn or corrupt tail, never a commit.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use serde::{Deserialize, Serialize};

use super::frame::{self, HEADER_LEN};
use super::lane::JournalRuntime;
use crate::db::SaveMode;
use crate::thread::ThreadData;

/// Rotate a segment before it reaches this size. Frames are appended, never
/// rewritten in place, so a bounded segment makes recovery and pruning
/// predictable without ever touching a live file.
const SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Number of commits between automatic snapshots. A snapshot bounds replay
/// time; it is a cold-path optimisation, never part of a commit.
const SNAPSHOT_EVERY: u64 = 512;
/// Terminal-deletion marker inside a session directory.
const DELETED_MARKER: &str = "DELETED";

/// A point-in-time full session state written beside the segments.
#[derive(Serialize, Deserialize)]
struct CommitPayload {
    /// Logical operation identity. Replaying the same id returns the original
    /// seq instead of appending a duplicate frame.
    op_id: String,
    data: ThreadData,
    #[serde(default)]
    usage_upserts: Vec<protocol::RequestUsageRecord>,
}

#[derive(Debug, Clone)]
struct SegmentState {
    path: PathBuf,
    len: u64,
}

/// One session's journal. Shared per process: two `ThreadSession` values that
/// name the same session must append through the same seq counter and lane.
pub(crate) struct SessionJournal {
    dir: PathBuf,
    key: String,
    runtime: Arc<JournalRuntime>,
    seq: AtomicU64,
    compacted_upto: AtomicU64,
    append_gate: tokio::sync::Mutex<()>,
    segment: Mutex<SegmentState>,
    last_op: Mutex<Option<(String, u64)>>,
    rescan: AtomicBool,
    deleted: AtomicBool,
    init_error: Option<String>,
}

impl std::fmt::Debug for SessionJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionJournal")
            .field("dir", &self.dir)
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .field("init_error", &self.init_error)
            .finish_non_exhaustive()
    }
}

impl SessionJournal {
    /// Open (or reuse) the journal for `session_id` under `journal_root`.
    ///
    /// The process-wide registry guarantees one writer per session directory,
    /// so seq allocation and tail recovery cannot race between two stores that
    /// name the same session.
    pub(crate) fn shared(journal_root: &Path, session_id: &str) -> Arc<Self> {
        let dir = journal_root.join(session_id);
        static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<SessionJournal>>>> = OnceLock::new();
        let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = registry.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(existing) = guard.get(&dir).and_then(Weak::upgrade) {
            return existing;
        }
        let journal = Arc::new(Self::open(dir.clone(), session_id.to_string()));
        guard.insert(dir, Arc::downgrade(&journal));
        journal
    }

    fn open(dir: PathBuf, key: String) -> Self {
        let runtime = JournalRuntime::global();
        let mut init_error = None;
        if let Err(error) = fs::create_dir_all(&dir) {
            init_error = Some(format!(
                "could not create journal directory {}: {error}",
                dir.display()
            ));
        }

        let mut snapshot_upto = 0u64;
        let mut segments: Vec<(u64, PathBuf)> = Vec::new();
        if init_error.is_none()
            && let Ok(entries) = fs::read_dir(&dir)
        {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(seq) = parse_snapshot_name(&name) {
                    snapshot_upto = snapshot_upto.max(seq);
                } else if let Some(first) = parse_segment_name(&name) {
                    segments.push((first, entry.path()));
                }
            }
        }
        segments.sort_by_key(|(first, _)| *first);

        let mut seq = snapshot_upto;
        let mut segment = SegmentState {
            path: dir.join(segment_name(snapshot_upto.saturating_add(1))),
            len: 0,
        };
        if init_error.is_none()
            && let Some((_first, path)) = segments.last()
        {
            match scan_segment(path) {
                Ok((last, valid_len)) => {
                    let file_len = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
                    if valid_len < file_len {
                        tracing::warn!(
                            path = %path.display(),
                            valid_len,
                            file_len,
                            "truncating a torn journal tail"
                        );
                        if let Ok(file) = OpenOptions::new().write(true).open(path) {
                            let _ = file.set_len(valid_len);
                        }
                    }
                    if last >= snapshot_upto {
                        seq = last;
                        segment = SegmentState {
                            path: path.clone(),
                            len: valid_len,
                        };
                    }
                }
                Err(error) => {
                    init_error = Some(format!(
                        "could not scan journal segment {}: {error}",
                        path.display()
                    ));
                }
            }
        }

        Self {
            dir,
            key,
            runtime,
            seq: AtomicU64::new(seq),
            compacted_upto: AtomicU64::new(snapshot_upto),
            append_gate: tokio::sync::Mutex::new(()),
            segment: Mutex::new(segment),
            last_op: Mutex::new(None),
            rescan: AtomicBool::new(false),
            deleted: AtomicBool::new(false),
            init_error,
        }
    }

    /// Highest seq durably appended (or recovered) so far.
    pub(crate) fn durable_seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Append one commit and wait for its per-file `fdatasync`.
    ///
    /// The payload is the same `ThreadData` granularity the projection uses,
    /// so a frame is self-describing: metadata, a transcript delta, or a full
    /// rewrite.
    pub(crate) async fn append(
        &self,
        operation_id: &str,
        mode: SaveMode,
        data: &ThreadData,
        usage: &[protocol::RequestUsageRecord],
    ) -> Result<u64, String> {
        if let Some(error) = &self.init_error {
            return Err(error.clone());
        }
        {
            let last = self
                .last_op
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some((id, seq)) = last.as_ref()
                && id == operation_id
            {
                return Ok(*seq);
            }
        }
        let payload = serde_json::to_vec(&CommitPayload {
            op_id: operation_id.to_string(),
            data: data.clone(),
            usage_upserts: usage.to_vec(),
        })
        .map_err(|error| format!("could not encode journal commit: {error}"))?;
        let kind = frame::FrameKind::from_save_mode(mode);

        let _gate = self.append_gate.lock().await;
        if self.rescan.swap(false, Ordering::AcqRel)
            && let Err(error) = self.rescan_current_segment()
        {
            self.rescan.store(true, Ordering::Release);
            return Err(error);
        }
        let seq = self.seq.load(Ordering::Acquire).saturating_add(1);
        let bytes = frame::encode(seq, kind, &payload);
        let path = {
            let mut segment = self
                .segment
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if segment.len > 0 && segment.len.saturating_add(bytes.len() as u64) > SEGMENT_MAX_BYTES
            {
                *segment = SegmentState {
                    path: self.dir.join(segment_name(seq)),
                    len: 0,
                };
            }
            segment.len = segment.len.saturating_add(bytes.len() as u64);
            segment.path.clone()
        };

        let ack = self
            .runtime
            .submit(&self.key, path, bytes)
            .await
            .map_err(|error| format!("could not submit journal append: {error}"))?;
        match ack.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.rescan.store(true, Ordering::Release);
                return Err(format!("journal append failed: {error}"));
            }
            Err(_) => {
                self.rescan.store(true, Ordering::Release);
                return Err("journal lane dropped the append".to_string());
            }
        }
        self.seq.store(seq, Ordering::Release);
        *self
            .last_op
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some((operation_id.to_string(), seq));
        Ok(seq)
    }

    /// Recover the full session state from the newest snapshot plus every
    /// complete frame above it. Returns `None` when the journal has no state.
    pub(crate) fn load(&self) -> Result<Option<ThreadData>, String> {
        let snapshots = self.snapshot_files()?;
        let mut state: Option<ThreadData> = None;
        let mut base_seq = 0u64;
        for (seq, path) in snapshots {
            match fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<ThreadData>(&bytes).map_err(|error| error.to_string())
                }) {
                Ok(data) => {
                    state = Some(data);
                    base_seq = seq;
                    break;
                }
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "skipping an unreadable journal snapshot"
                    );
                }
            }
        }

        for (_, path) in self.segment_files()? {
            match read_segment(&path, base_seq, &mut state) {
                Ok(true) => {
                    self.deleted.store(true, Ordering::Release);
                    return Ok(None);
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "stopped journal replay at an unreadable segment"
                    );
                    break;
                }
            }
        }
        Ok(state)
    }

    /// Whether any durable state exists for this session.
    pub(crate) fn has_state(&self) -> bool {
        self.seq.load(Ordering::Acquire) > 0
            || self
                .snapshot_files()
                .map(|files| !files.is_empty())
                .unwrap_or(false)
    }

    /// Whether this journal is terminally deleted.
    pub(crate) fn is_deleted(&self) -> bool {
        self.deleted.load(Ordering::Acquire) || self.dir.join(DELETED_MARKER).exists()
    }

    /// Append a terminal `Deleted` frame, then replace the journal body with a
    /// small marker. The frame makes the deletion durable before the segments
    /// are reclaimed; the marker keeps it terminal across restarts (ADR-0241).
    pub(crate) async fn mark_deleted(&self) -> Result<(), String> {
        let _gate = self.append_gate.lock().await;
        if self.is_deleted() {
            // A crash between the marker rename and body removal leaves
            // segments under a terminal marker. Re-running deletion is the
            // sweep: the marker already decided the outcome, so only the
            // orphaned bodies remain to be reclaimed.
            self.reclaim_body();
            return Ok(());
        }
        let seq = self.seq.load(Ordering::Acquire).saturating_add(1);
        let bytes = frame::encode(seq, frame::FrameKind::Deleted, &[]);
        let path = self
            .segment
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .path
            .clone();
        let ack = self
            .runtime
            .submit(&self.key, path, bytes)
            .await
            .map_err(|error| format!("could not submit journal deletion: {error}"))?;
        match ack.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(format!("journal deletion failed: {error}")),
            Err(_) => return Err("journal lane dropped the deletion".to_string()),
        }
        self.seq.store(seq, Ordering::Release);
        let marker = self.dir.join(DELETED_MARKER);
        let temp = marker.with_extension("tmp");
        fs::write(&temp, b"deleted").map_err(|error| error.to_string())?;
        if let Ok(file) = fs::File::open(&temp) {
            let _ = file.sync_all();
        }
        fs::rename(&temp, &marker).map_err(|error| error.to_string())?;
        // Persist the directory entry so the marker survives a power loss.
        if let Ok(dir) = fs::File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        self.reclaim_body();
        self.deleted.store(true, Ordering::Release);
        Ok(())
    }

    /// Remove the transcript body under a terminal `DELETED` marker. The
    /// marker is the receipt; the segments have no reader once it exists.
    fn reclaim_body(&self) {
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("seg-") || name.starts_with("snap-") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    /// Materialise an existing session (from the derived SQLite view) into a
    /// seq-0 snapshot. Called once per session, on the cold migration path.
    pub(crate) fn import(&self, data: &ThreadData) -> Result<(), String> {
        if self.has_state() {
            return Ok(());
        }
        let target = self.dir.join(snapshot_name(0));
        let temp = target.with_extension("json.tmp");
        let bytes = serde_json::to_vec(data)
            .map_err(|error| format!("could not encode imported session: {error}"))?;
        {
            let mut file = File::create(&temp)
                .map_err(|error| format!("could not create imported session: {error}"))?;
            file.write_all(&bytes)
                .map_err(|error| format!("could not write imported session: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("could not fsync imported session: {error}"))?;
        }
        fs::rename(&temp, &target)
            .map_err(|error| format!("could not publish imported session: {error}"))
    }

    /// True once enough commits have accumulated to justify a snapshot.
    pub(crate) fn should_compact(&self) -> bool {
        self.seq
            .load(Ordering::Acquire)
            .saturating_sub(self.compacted_upto.load(Ordering::Acquire))
            >= SNAPSHOT_EVERY
    }

    /// Write a full snapshot at the current seq and prune fully-covered
    /// segments. Synchronous by design: the caller runs it on a blocking
    /// thread, never on the async runtime.
    #[cfg(test)]
    pub(crate) fn compact(&self, data: &ThreadData) -> Result<u64, String> {
        self.compact_at(self.seq.load(Ordering::Acquire), data)
    }

    /// Write a snapshot **for an exact seq**.
    ///
    /// The caller must have captured `data` at that seq (and verified the
    /// journal did not advance while it did). Replaying frames above `seq`
    /// then cannot duplicate state already folded into the snapshot.
    pub(crate) fn compact_at(&self, seq: u64, data: &ThreadData) -> Result<u64, String> {
        if seq == 0 {
            return Ok(0);
        }
        let target = self.dir.join(snapshot_name(seq));
        let temp = target.with_extension("json.tmp");
        let bytes = serde_json::to_vec(data)
            .map_err(|error| format!("could not encode journal snapshot: {error}"))?;
        {
            let mut file = File::create(&temp)
                .map_err(|error| format!("could not create journal snapshot: {error}"))?;
            file.write_all(&bytes)
                .map_err(|error| format!("could not write journal snapshot: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("could not fsync journal snapshot: {error}"))?;
        }
        fs::rename(&temp, &target)
            .map_err(|error| format!("could not publish journal snapshot: {error}"))?;
        self.compacted_upto.store(seq, Ordering::Release);

        let current = self
            .segment
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .path
            .clone();
        if let Ok(segments) = self.segment_files() {
            for (_, path) in segments {
                if path == current {
                    continue;
                }
                let covered = scan_segment(&path)
                    .map(|(last, _)| last <= seq)
                    .unwrap_or(false);
                if covered {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(seq)
    }

    fn rescan_current_segment(&self) -> Result<(), String> {
        let path = self
            .segment
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .path
            .clone();
        if !path.exists() {
            let mut segment = self
                .segment
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            segment.len = 0;
            return Ok(());
        }
        let (last, valid_len) = scan_segment(&path).map_err(|error| error.to_string())?;
        let mut segment = self
            .segment
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        segment.len = valid_len;
        if last > self.seq.load(Ordering::Acquire) {
            self.seq.store(last, Ordering::Release);
        }
        Ok(())
    }

    fn snapshot_files(&self) -> Result<Vec<(u64, PathBuf)>, String> {
        let mut files = Vec::new();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(files),
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(seq) = parse_snapshot_name(&name) {
                files.push((seq, entry.path()));
            }
        }
        files.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        Ok(files)
    }

    fn segment_files(&self) -> Result<Vec<(u64, PathBuf)>, String> {
        let mut files = Vec::new();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(files),
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(first) = parse_segment_name(&name) {
                files.push((first, entry.path()));
            }
        }
        files.sort_by_key(|(first, _)| *first);
        Ok(files)
    }
}

fn read_segment(
    path: &Path,
    base_seq: u64,
    state: &mut Option<ThreadData>,
) -> Result<bool, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let file_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut offset = 0u64;
    loop {
        if offset == file_len {
            return Ok(false);
        }
        let mut header_bytes = [0u8; HEADER_LEN];
        match reader.read_exact(&mut header_bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(error) => return Err(error.to_string()),
        }
        let header = frame::decode_header(&header_bytes)?;
        let total = (HEADER_LEN as u64).saturating_add(header.payload_len as u64);
        if offset.saturating_add(total) > file_len {
            return Ok(false);
        }
        let mut payload = vec![0u8; header.payload_len];
        reader
            .read_exact(&mut payload)
            .map_err(|error| error.to_string())?;
        let decoded = match frame::verify_payload(header, &payload) {
            Ok(decoded) => decoded,
            Err(error) => {
                // A CRC mismatch is the first invalid frame. Everything after
                // it is unreachable by definition, so replay stops here rather
                // than failing the whole session (ADR-0236 §2).
                tracing::warn!(
                    path = %path.display(),
                    offset,
                    %error,
                    "journal replay stops at an invalid frame"
                );
                return Ok(false);
            }
        };
        offset = offset.saturating_add(total);
        if decoded.kind == frame::FrameKind::Deleted {
            return Ok(true);
        }
        if decoded.seq <= base_seq {
            continue;
        }
        let commit: CommitPayload = match serde_json::from_slice(decoded.payload) {
            Ok(commit) => commit,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    offset,
                    %error,
                    "journal replay stops at an undecodable commit"
                );
                return Ok(false);
            }
        };
        let mode = match decoded.kind {
            frame::FrameKind::Metadata => SaveMode::MetadataOnly,
            frame::FrameKind::Delta => SaveMode::Delta,
            frame::FrameKind::Full => SaveMode::FullRewrite,
            frame::FrameKind::Deleted => return Ok(true),
        };
        match state {
            Some(base) => {
                base.apply_journal_commit(commit.data, mode);
                base.apply_usage_upserts(&commit.usage_upserts);
            }
            None => {
                let mut data = commit.data;
                data.apply_usage_upserts(&commit.usage_upserts);
                *state = Some(data);
            }
        }
    }
}

fn scan_segment(path: &Path) -> io::Result<(u64, u64)> {
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut offset = 0u64;
    let mut last_seq = 0u64;
    let mut header_bytes = [0u8; HEADER_LEN];
    while offset < file_len {
        if file_len.saturating_sub(offset) < HEADER_LEN as u64 {
            break;
        }
        reader.seek(SeekFrom::Start(offset))?;
        if let Err(error) = reader.read_exact(&mut header_bytes) {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(error);
        }
        let header = match frame::decode_header(&header_bytes) {
            Ok(header) => header,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    offset,
                    %error,
                    "journal segment ends at a malformed frame"
                );
                break;
            }
        };
        let total = (HEADER_LEN as u64).saturating_add(header.payload_len as u64);
        if offset.saturating_add(total) > file_len {
            break;
        }
        let mut payload = vec![0u8; header.payload_len];
        reader.seek(SeekFrom::Start(offset + HEADER_LEN as u64))?;
        if let Err(error) = reader.read_exact(&mut payload) {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(error);
        }
        let decoded = match frame::verify_payload(header, &payload) {
            Ok(decoded) => decoded,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    offset,
                    %error,
                    "journal segment ends at a frame that fails its checksum"
                );
                break;
            }
        };
        if decoded.kind != frame::FrameKind::Deleted
            && serde_json::from_slice::<CommitPayload>(decoded.payload).is_err()
        {
            tracing::warn!(
                path = %path.display(),
                offset,
                "journal segment ends at an undecodable commit"
            );
            break;
        }
        offset = offset.saturating_add(total);
        last_seq = header.seq;
    }
    Ok((last_seq, offset))
}

fn parse_snapshot_name(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("snap-")?.strip_suffix(".json")?;
    digits.parse().ok()
}

fn parse_segment_name(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("seg-")?.strip_suffix(".log")?;
    digits.parse().ok()
}

fn snapshot_name(seq: u64) -> String {
    format!("snap-{seq:020}.json")
}

fn segment_name(first_seq: u64) -> String {
    format!("seg-{first_seq:020}.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SaveMode;
    use crate::thread::ThreadData;
    use tool::{Message, Role};

    fn journal() -> (Arc<SessionJournal>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let journal = SessionJournal::shared(dir.path(), "session-1");
        (journal, dir)
    }

    fn turn(content: &str) -> ThreadData {
        let mut data = ThreadData {
            id: "session-1".to_string(),
            updated_at: ThreadData::default().updated_at.saturating_add(1),
            ..ThreadData::default()
        };
        data.transcript
            .push(protocol::TranscriptEntry::from_message(
                0,
                &Message::new(Role::User, content.to_string()),
            ));
        data
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_commit_survives_a_fresh_open() {
        let (journal, dir) = journal();
        let first = turn("hello");
        let seq = journal
            .append("op-1", SaveMode::Delta, &first, &[])
            .await
            .unwrap();
        assert_eq!(seq, 1);

        let reopened = SessionJournal::open(dir.path().join("session-1"), "session-1".to_string());
        let loaded = reopened.load().unwrap().expect("state");
        assert_eq!(loaded.transcript.entries.len(), 1);
        assert_eq!(
            loaded.transcript.entries[0]
                .to_message()
                .map(|message| message.content),
            Some("hello".to_string())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn re_running_deletion_reclaims_bodies_left_after_a_crash() {
        let (journal, dir) = journal();
        journal
            .append("op-1", SaveMode::Delta, &turn("hello"), &[])
            .await
            .unwrap();
        let session_dir = dir.path().join("session-1");
        // The crash window: the terminal marker is durable but the body was
        // never reclaimed. A later delete/sweep must finish the job.
        fs::write(session_dir.join(DELETED_MARKER), b"deleted").unwrap();
        assert!(
            fs::read_dir(&session_dir)
                .unwrap()
                .flatten()
                .any(|entry| { entry.file_name().to_string_lossy().starts_with("seg-") })
        );

        journal.mark_deleted().await.unwrap();

        assert!(session_dir.join(DELETED_MARKER).exists());
        assert!(!fs::read_dir(&session_dir).unwrap().flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("seg-") || name.starts_with("snap-")
        }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn consecutive_commits_replay_in_order() {
        let (journal, dir) = journal();
        journal
            .append("op-1", SaveMode::Delta, &turn("one"), &[])
            .await
            .unwrap();
        journal
            .append("op-2", SaveMode::Delta, &turn("two"), &[])
            .await
            .unwrap();
        let loaded = SessionJournal::shared(dir.path(), "session-1")
            .load()
            .unwrap()
            .expect("state");
        let bodies: Vec<String> = loaded
            .transcript
            .entries
            .iter()
            .filter_map(|entry| entry.to_message())
            .map(|message| message.content)
            .collect();
        assert_eq!(bodies, vec!["one".to_string(), "two".to_string()]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replaying_the_same_operation_is_idempotent() {
        let (journal, _dir) = journal();
        let data = turn("once");
        let first = journal
            .append("same-op", SaveMode::Delta, &data, &[])
            .await
            .unwrap();
        let again = journal
            .append("same-op", SaveMode::Delta, &data, &[])
            .await
            .unwrap();
        assert_eq!(first, again);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_snapshot_at_an_exact_seq_does_not_duplicate_later_frames() {
        let (journal, dir) = journal();
        let first = turn("m0");
        journal
            .append("op-0", SaveMode::Delta, &first, &[])
            .await
            .unwrap();
        // Snapshot the state as it stood at seq 1, then append two more
        // frames. Replay must yield the snapshot plus the two new entries,
        // never the snapshot's entry twice.
        journal.compact_at(1, &first).unwrap();
        journal
            .append("op-1", SaveMode::Delta, &turn("m1"), &[])
            .await
            .unwrap();
        journal
            .append("op-2", SaveMode::Delta, &turn("m2"), &[])
            .await
            .unwrap();
        let loaded = SessionJournal::open(dir.path().join("session-1"), "session-1".to_string())
            .load()
            .unwrap()
            .expect("state");
        let bodies: Vec<String> = loaded
            .transcript
            .entries
            .iter()
            .filter_map(|entry| entry.to_message())
            .map(|message| message.content)
            .collect();
        assert_eq!(
            bodies,
            vec!["m0".to_string(), "m1".to_string(), "m2".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_torn_tail_is_ignored_and_the_next_append_recovers() {
        let (journal, dir) = journal();
        journal
            .append("op-1", SaveMode::Delta, &turn("kept"), &[])
            .await
            .unwrap();
        let segment = journal
            .segment
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .path
            .clone();
        let mut file = OpenOptions::new().append(true).open(&segment).unwrap();
        file.write_all(b"NJ01\x01\x00").unwrap();
        drop(file);

        let reopened = SessionJournal::open(dir.path().join("session-1"), "session-1".to_string());
        let loaded = reopened.load().unwrap().expect("state");
        assert_eq!(loaded.transcript.entries.len(), 1);

        reopened
            .append("op-2", SaveMode::Delta, &turn("after"), &[])
            .await
            .unwrap();
        let loaded = reopened.load().unwrap().expect("state");
        assert_eq!(loaded.transcript.entries.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_checksum_corrupt_tail_is_truncated_so_the_next_append_is_reachable() {
        let (journal, dir) = journal();
        journal
            .append("op-1", SaveMode::Delta, &turn("kept"), &[])
            .await
            .unwrap();
        let segment = journal
            .segment
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .path
            .clone();
        let valid_len = fs::metadata(&segment).unwrap().len();
        journal
            .append("op-2", SaveMode::Delta, &turn("corrupt"), &[])
            .await
            .unwrap();

        // Flip one payload byte in the second frame: its header stays
        // structurally complete, so only the checksum can see it.
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&segment)
            .unwrap();
        file.seek(SeekFrom::Start(valid_len + HEADER_LEN as u64))
            .unwrap();
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 0xff;
        file.seek(SeekFrom::Start(valid_len + HEADER_LEN as u64))
            .unwrap();
        file.write_all(&byte).unwrap();
        drop(file);

        let reopened = SessionJournal::open(dir.path().join("session-1"), "session-1".to_string());
        let loaded = reopened.load().unwrap().expect("state");
        assert_eq!(loaded.transcript.entries.len(), 1);
        assert_eq!(
            fs::metadata(&segment).unwrap().len(),
            valid_len,
            "the corrupt frame and everything after it are truncated"
        );

        reopened
            .append("op-3", SaveMode::Delta, &turn("after"), &[])
            .await
            .unwrap();
        let loaded = reopened.load().unwrap().expect("state");
        assert_eq!(loaded.transcript.entries.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_snapshot_bounds_replay_without_losing_the_tail() {
        let (journal, dir) = journal();
        let mut all = ThreadData {
            id: "session-1".to_string(),
            ..ThreadData::default()
        };
        for index in 0..3 {
            let mut delta = ThreadData {
                id: "session-1".to_string(),
                updated_at: ThreadData::default().updated_at.saturating_add(index),
                ..ThreadData::default()
            };
            delta
                .transcript
                .push(protocol::TranscriptEntry::from_message(
                    0,
                    &Message::new(Role::User, format!("m{index}")),
                ));
            journal
                .append(&format!("op-{index}"), SaveMode::Delta, &delta, &[])
                .await
                .unwrap();
            all.apply_journal_commit(delta, SaveMode::Delta);
        }
        journal.compact(&all).unwrap();
        journal
            .append("op-3", SaveMode::Delta, &turn("after-snapshot"), &[])
            .await
            .unwrap();
        let loaded = SessionJournal::shared(dir.path(), "session-1")
            .load()
            .unwrap()
            .expect("state");
        assert_eq!(loaded.transcript.entries.len(), 4);
    }
}
