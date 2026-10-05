//! Filesystem durability helpers: atomic-rename writes, fsync, file locks.
//!
//! The functions here implement the **atomic-rename + fsync** durability
//! pattern required for crash-safe single-file updates. POSIX guarantees that
//! `rename(2)` is atomic on the same filesystem, but only `fsync(2)` forces the
//! data and metadata to durable media. Without an additional `fsync` of the
//! parent directory, ext4 in particular can reorder the directory entry update
//! such that a power loss after `rename` leaves neither the old nor the new
//! file reachable.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Owner-only protection applied to every file we write and its leaf parent:
/// Unix uses `0600`/`0700`; Windows uses protected current-user DACLs. Config
/// and session files hold secrets (API keys) and private conversation content,
/// so inherited or ambient permissions must never weaken this boundary.
/// Create the leaf parent directory of `path` (and any missing ancestors),
/// then tighten the leaf to the platform's owner-only policy.
fn create_parent_dir(path: &Path) -> std::io::Result<()> {
    crate::secure_file::create_private_parent(path)
}

/// Atomically claim a unique temporary file beside `destination`.
///
/// The old fixed `<destination>.tmp` name let concurrent writers open or
/// truncate the same file. On Unix that could silently splice writes; Windows
/// surfaced the race as a sharing violation. A process id plus monotonic
/// sequence makes collisions exceptional, while `create_new` is the actual
/// race-free guarantee across threads and processes.
fn create_temporary_file(destination: &Path) -> std::io::Result<(PathBuf, File)> {
    let Some(file_name) = destination.file_name() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "atomic-write destination must name a file",
        ));
    };
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    for _ in 0..1_024 {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = file_name.to_os_string();
        temporary_name.push(format!(".tmp.{}.{sequence}", std::process::id()));
        let temporary = parent.join(temporary_name);
        match crate::secure_file::create_new_private_file(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique atomic-write temporary file",
    ))
}

/// Write `bytes` atomically: serialise to a uniquely claimed same-directory
/// temporary file, `fsync`, rename over `path`, then best-effort `fsync` of
/// `path`'s parent directory.
///
/// The temp file and leaf parent are owner-only from creation (`0600`/`0700`
/// on Unix, protected current-user DACLs on Windows), so secrets never pass
/// through a broadly readable state.
///
/// Returns the original [`std::io::Error`] on any failure. The temporary file
/// is best-effort cleaned up on failure. A crash may leave that uniquely named
/// sibling behind, but no later write will open or trust it.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    create_parent_dir(path)?;
    let (temporary, mut file) = create_temporary_file(path)?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        crate::secure_file::atomic_replace(&temporary, path)?;
        if let Some(parent) = path.parent()
            && let Ok(dir) = File::open(parent)
        {
            // Best-effort: fsync the directory so the rename entry reaches
            // disk. Errors here (filesystems that reject syncing a dir fd)
            // are non-fatal — the data file is already durable.
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Atomically write a pretty-printed JSON value. Convenience wrapper around
/// [`atomic_write_bytes`]. `?Sized` so it accepts slices like `&[String]`.
pub fn atomic_write_json<T: serde::Serialize + ?Sized>(
    path: &Path,
    value: &T,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    atomic_write_bytes(path, &bytes).map_err(|e| e.to_string())
}

/// Guard for a blocking exclusive advisory lock held on a companion
/// `<path>.lock` file. Used to serialise short read-modify-write windows on
/// shared global files (`provider_usage.json`, slash-command history, the
/// per-project embedding index) so two concurrently-running `nuo` instances
/// in the same project — or across projects — never silently lose each other's
/// updates. Dropping the guard closes the fd and releases the lock.
///
/// The lock is held on a **companion** file rather than the data file itself
/// because the data file is rewritten via temp-file + `rename(2)` (see
/// [`atomic_write_bytes`]), which swaps the underlying inode. A `flock` on the
/// data file's old fd would not protect the newly-renamed inode, so concurrent
/// writers could each believe they held the lock. The companion file is
/// opened once and never renamed, so its file-description lock reliably
/// serialises every holder for the lock's lifetime.
///
/// The companion is locked with `flock(2)` on Unix and `LockFileEx` on
/// Windows. There is no successful no-op fallback: callers either hold real
/// cross-process exclusion or receive an error.
pub struct FileLock {
    _lock: crate::lock::ProcessLock,
}

impl FileLock {
    /// Acquire a blocking exclusive lock on `<path>.lock`. Creates the
    /// companion file (and its parent directory) if missing, then blocks
    /// until an exclusive `flock(2)` is obtained.
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        let lock_path = lock_companion(path);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = crate::lock::ProcessLock::acquire_with_timeout(
            &lock_path,
            std::time::Duration::from_secs(30),
        )
        .map_err(std::io::Error::other)?;
        Ok(Self { _lock: lock })
    }
}

/// Companion lock-file path for `path`: `<path>.lock`.
fn lock_companion(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Sample {
        name: &'static str,
        n: u32,
    }

    #[test]
    fn atomic_write_round_trips_and_removes_tmp() {
        let dir = std::env::temp_dir().join(format!("muta-fsutil-{}", uuid::Uuid::new_v4()));
        let path = dir.join("payload.json");
        atomic_write_json(&path, &Sample { name: "ok", n: 7 }).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"name\": \"ok\""));
        assert!(text.contains("\"n\": 7"));
        let entries = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![std::ffi::OsString::from("payload.json")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("muta-fsutil-{}-perm", uuid::Uuid::new_v4()));
        let path = dir.join("secret.json");
        atomic_write_json(&path, &Sample { name: "k", n: 1 }).unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "secret file must be rw-------");
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "secret dir must be rwx------");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = std::env::temp_dir().join(format!("muta-fsutil-{}-2", uuid::Uuid::new_v4()));
        let path = dir.join("payload.json");
        atomic_write_json(&path, &Sample { name: "v1", n: 1 }).unwrap();
        atomic_write_json(&path, &Sample { name: "v2", n: 2 }).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"name\": \"v2\""));
        assert!(!text.contains("v1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_atomic_writers_use_disjoint_temporary_files() {
        let dir =
            std::env::temp_dir().join(format!("muta-fsutil-{}-concurrent", uuid::Uuid::new_v4()));
        let path = dir.join("payload.json");
        let payloads = (0..16)
            .map(|index| format!("writer-{index}:{}", "x".repeat(16_384)))
            .collect::<Vec<_>>();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(payloads.len()));
        let writers = payloads
            .iter()
            .cloned()
            .map(|payload| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    atomic_write_bytes(&path, payload.as_bytes())
                })
            })
            .collect::<Vec<_>>();

        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        let final_payload = std::fs::read_to_string(&path).unwrap();
        assert!(
            payloads.contains(&final_payload),
            "the destination must contain one complete writer payload"
        );
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp."))
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "temporary files leaked: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
