//! Content-addressed blob store (C13 foundation).
//!
//! Large, repetitive payloads (long tool outputs, subagent transcripts) are
//! stored once under a SHA-256 hash and referenced by that hash. This reduces
//! duplication across sessions/forks and gives future features (semantic
//! search, sync) a stable content key. Liveness is decided by the durable
//! `blob_refs` ledger in SQLite, not by scanning this directory.
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const BLOB_PREFIX_LEN: usize = 2;

/// Minimum modification-time grace window before an unreferenced blob or
/// `.tmp.*` staging file may be reclaimed by background maintenance
/// (ADR-0255 `[INV-SHARD-04]`).
///
/// `prepare_save` offloads oversized observation bodies to the CAS *before*
/// the content shard transaction writes `thread_blob_refs` (ADR-0248). A
/// 10-minute grace window ensures concurrent maintenance can never sweep a
/// freshly written blob or active staging file mid-commit.
pub const DEFAULT_BLOB_GC_GRACE: Duration = Duration::from_secs(10 * 60);

/// Store for immutable byte blobs keyed by SHA-256.
#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Hash bytes and return the hex digest.
    pub fn hash(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    /// Persist `bytes` and return their content hash. Idempotent: writing the
    /// same bytes twice is a no-op on disk.
    pub fn put(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = Self::hash(bytes);
        let path = self.path(&hash);
        if path.exists() {
            return Ok(hash);
        }
        let Some(parent) = path.parent() else {
            return Err(format!("invalid blob path for hash {hash}"));
        };
        crate::fs_privacy::ensure_private_dir(&self.root).map_err(|e| e.to_string())?;
        crate::fs_privacy::ensure_private_dir(parent).map_err(|e| e.to_string())?;

        // Atomic write via unique temporary file in the same directory, sync to disk,
        // and atomic rename. This guarantees no partial/truncated files can ever exist at `path`.
        use std::io::Write;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_name = format!(".tmp.{}.{}.{}", hash, std::process::id(), nanos);
        let tmp_path = parent.join(tmp_name);

        let write_res = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            crate::fs_privacy::restrict_private_file(&tmp_path);
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp_path, &path)?;
            Ok(())
        })();

        if let Err(e) = write_res {
            let _ = fs::remove_file(&tmp_path);
            if !path.exists() {
                return Err(format!("could not write blob {}: {}", hash, e));
            }
        }

        Ok(hash)
    }

    /// Read a blob by hash. Returns `None` if the blob is missing.
    pub fn get(&self, hash: &str) -> Option<Vec<u8>> {
        let path = self.path(hash);
        fs::read(&path).ok()
    }

    /// True if the blob exists locally. Test-only today; production code reads
    /// blobs directly and treats a miss as absence.
    #[cfg(test)]
    pub fn exists(&self, hash: &str) -> bool {
        self.path(hash).exists()
    }

    /// Resolve the on-disk path for a hash.
    pub fn path(&self, hash: &str) -> PathBuf {
        let prefix = &hash[..BLOB_PREFIX_LEN.min(hash.len())];
        self.root.join(prefix).join(hash)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Delete one blob. Infallible by design: a missing blob is the desired
    /// end state, and an undeletable one must not fail a GC pass.
    fn remove(&self, hash: &str) {
        let _ = fs::remove_file(self.path(hash));
    }

    /// Delete every stored blob not in `live`. Delegates to
    /// [`Self::retain_only_with_grace`] with zero grace window.
    pub fn retain_only(&self, live: &std::collections::HashSet<String>) -> (usize, u64) {
        self.retain_only_with_grace(live, Duration::ZERO)
    }

    /// Delete every stored blob not in `live` whose modification time is older
    /// than `grace` (ADR-0255 `[INV-SHARD-04]`).
    ///
    /// Both unreferenced blobs and `.tmp.*` staging files newer than `grace`
    /// are preserved so concurrent `prepare_save` / `put` writes cannot race
    /// with background garbage collection. Returns
    /// `(reclaimed_count, reclaimed_bytes)`.
    pub fn retain_only_with_grace(
        &self,
        live: &std::collections::HashSet<String>,
        grace: Duration,
    ) -> (usize, u64) {
        let mut reclaimed = 0usize;
        let mut reclaimed_bytes = 0u64;
        let Ok(prefixes) = fs::read_dir(&self.root) else {
            return (0, 0);
        };
        let now = SystemTime::now();
        for prefix_dir in prefixes.flatten() {
            let Ok(blobs) = fs::read_dir(prefix_dir.path()) else {
                continue;
            };
            for blob in blobs.flatten() {
                let hash = blob.file_name().to_string_lossy().into_owned();
                let meta = blob.metadata().ok();
                let is_recent = !grace.is_zero()
                    && meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .is_some_and(|mtime| {
                            now.duration_since(mtime).unwrap_or(Duration::ZERO) < grace
                        });
                if is_recent {
                    continue;
                }
                if hash.starts_with(".tmp.") {
                    let size = meta.map(|m| m.len()).unwrap_or(0);
                    let _ = fs::remove_file(blob.path());
                    reclaimed += 1;
                    reclaimed_bytes += size;
                    continue;
                }
                if live.contains(&hash) {
                    continue;
                }
                let size = meta.map(|m| m.len()).unwrap_or(0);
                self.remove(&hash);
                reclaimed += 1;
                reclaimed_bytes += size;
            }
        }
        (reclaimed, reclaimed_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_is_idempotent_and_get_round_trips() {
        let dir = std::env::temp_dir().join(format!("nuo-blobs-{}", uuid::Uuid::new_v4()));
        let store = BlobStore::new(dir.clone());
        let bytes = b"hello world";
        let hash1 = store.put(bytes).unwrap();
        let hash2 = store.put(bytes).unwrap();
        assert_eq!(hash1, hash2);
        assert!(store.exists(&hash1));
        assert_eq!(store.get(&hash1).unwrap(), bytes.to_vec());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn different_bytes_get_different_hashes() {
        let dir = std::env::temp_dir().join(format!("nuo-blobs-{}", uuid::Uuid::new_v4()));
        let store = BlobStore::new(dir.clone());
        let a = store.put(b"a").unwrap();
        let b = store.put(b"b").unwrap();
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retain_only_keeps_live_and_reclaims_orphans() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("blobs"));
        let kept = store.put(b"referenced").unwrap();
        let shared = store.put(b"referenced twice").unwrap();
        let orphan = store.put(b"unreferenced").unwrap();

        let live: std::collections::HashSet<String> =
            [kept.clone(), shared.clone()].into_iter().collect();
        let (count, bytes) = store.retain_only(&live);
        assert_eq!(count, 1);
        assert!(bytes > 0);
        assert!(store.exists(&kept));
        assert!(store.exists(&shared));
        assert!(!store.exists(&orphan));
    }

    #[test]
    fn retain_only_with_empty_live_set_reclaims_everything() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("blobs"));
        let stray = store.put(b"stray").unwrap();
        let (count, _) = store.retain_only(&std::collections::HashSet::new());
        assert_eq!(count, 1);
        assert!(!store.exists(&stray));
    }

    #[test]
    fn retain_only_on_missing_root_is_a_noop() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("does-not-exist"));
        assert_eq!(store.retain_only(&std::collections::HashSet::new()), (0, 0));
    }

    #[test]
    fn put_atomic_writes_and_retains_cleanly() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("blobs"));
        let bytes = b"atomic blob test content";
        let hash = store.put(bytes).unwrap();
        assert_eq!(store.get(&hash).as_deref(), Some(&bytes[..]));

        // Fake an orphaned temporary file
        let parent = store.path(&hash).parent().unwrap().to_path_buf();
        let orphan_tmp = parent.join(".tmp.orphan.12345.6789");
        fs::write(&orphan_tmp, b"garbage").unwrap();
        assert!(orphan_tmp.exists());

        let live = [hash.clone()].into_iter().collect();
        let (count, bytes_reclaimed) = store.retain_only(&live);
        assert_eq!(count, 1);
        assert_eq!(bytes_reclaimed, 7);
        assert!(!orphan_tmp.exists());
        assert!(store.exists(&hash));
    }

    #[test]
    fn retain_only_with_grace_protects_fresh_blobs_and_staging_files() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("blobs"));
        let fresh_orphan = store.put(b"mid-commit blob").unwrap();
        let parent = store.path(&fresh_orphan).parent().unwrap().to_path_buf();
        let active_tmp = parent.join(".tmp.active.12345.6789");
        fs::write(&active_tmp, b"staging").unwrap();

        // Within the grace window, neither the freshly written blob nor the
        // active staging temp file may be deleted.
        let (count, bytes) =
            store.retain_only_with_grace(&std::collections::HashSet::new(), DEFAULT_BLOB_GC_GRACE);
        assert_eq!((count, bytes), (0, 0));
        assert!(store.exists(&fresh_orphan));
        assert!(active_tmp.exists());

        // With zero grace window, both are reclaimed cleanly.
        let (count, bytes) =
            store.retain_only_with_grace(&std::collections::HashSet::new(), Duration::ZERO);
        assert_eq!(count, 2);
        assert!(bytes > 0);
        assert!(!store.exists(&fresh_orphan));
        assert!(!active_tmp.exists());
    }
}
