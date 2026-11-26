//! Bounded LRU sparse line-offset cache for large text reads.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// A sparse line checkpoint: byte offset of a line together with the number of
/// complete lines before it. A later read can seek here instead of rescanning
/// from byte zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LineCheckpoint {
    pub(super) offset: u64,
    pub(super) line: usize,
}

pub(super) const LINE_INDEX_INTERVAL_BYTES: u64 = 1024 * 1024;
const MAX_LINE_INDEX_FILES: usize = 16;
const MAX_LINE_INDEX_CHECKPOINTS: usize = 128;

struct CachedLineIndex {
    path: PathBuf,
    version: String,
    checkpoints: Vec<LineCheckpoint>,
}

/// Lexically normalize a cache key path so `./a.rs` and `a.rs` map to one slot.
pub(super) fn normalize_cache_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.push(component.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Per-workspace bounded LRU sparse line index for large text reads.
///
/// The version token is the same fingerprint used for optimistic writes. A
/// changed file invalidates its checkpoints rather than risking a page that
/// starts at the wrong byte. Entries are evicted least-recently-used first.
#[derive(Default)]
pub(crate) struct LineIndexCache {
    entries: Mutex<Vec<CachedLineIndex>>,
}

impl LineIndexCache {
    pub(super) fn load(&self, path: &Path, version: &str) -> Vec<LineCheckpoint> {
        let key = normalize_cache_path(path);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(pos) = entries.iter().position(|entry| entry.path == key) else {
            return Vec::new();
        };
        if entries[pos].version != version {
            return Vec::new();
        }
        let entry = entries.remove(pos);
        let checkpoints = entry.checkpoints.clone();
        entries.push(entry);
        checkpoints
    }

    /// Load the checkpoints for `path` and return the best checkpoint strictly
    /// before `requested` (1-based line number), together with the checkpoint
    /// set so the caller can merge newly observed positions back in.
    pub(super) fn resume(
        &self,
        path: &Path,
        version: &str,
        requested: usize,
    ) -> Option<(u64, usize, Vec<LineCheckpoint>)> {
        let checkpoints = self.load(path, version);
        let checkpoint = checkpoints
            .iter()
            .rev()
            .find(|checkpoint| checkpoint.line < requested)
            .copied()?;
        Some((checkpoint.offset, checkpoint.line, checkpoints))
    }

    pub(super) fn store(
        &self,
        path: &Path,
        version: String,
        checkpoints: &mut Vec<LineCheckpoint>,
    ) {
        if checkpoints.is_empty() {
            return;
        }
        checkpoints.sort_by_key(|checkpoint| checkpoint.offset);
        checkpoints.dedup_by_key(|checkpoint| checkpoint.offset);
        if checkpoints.len() > MAX_LINE_INDEX_CHECKPOINTS {
            let excess = checkpoints.len() - MAX_LINE_INDEX_CHECKPOINTS;
            checkpoints.drain(..excess);
        }
        let key = normalize_cache_path(path);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(pos) = entries.iter().position(|entry| entry.path == key) {
            entries.remove(pos);
        } else if entries.len() >= MAX_LINE_INDEX_FILES {
            entries.remove(0);
        }
        entries.push(CachedLineIndex {
            path: key,
            version,
            checkpoints: checkpoints.clone(),
        });
    }
}
