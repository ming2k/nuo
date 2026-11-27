//! Cross-process advisory file locks with equivalent Unix and Windows
//! semantics.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// Guard for an exclusive process lock. The native lock is released on drop.
#[derive(Debug)]
pub struct ProcessLock {
    file: File,
}

impl ProcessLock {
    /// Try to acquire an exclusive lock without blocking.
    ///
    /// The parent directory is created on demand. Without this, a lock on a
    /// path inside a not-yet-materialised directory (a pristine `NUO_HOME`
    /// whose `instance/` has never been written) failed with `ENOENT` — an
    /// error indistinguishable, to every caller that treats `Err` as "someone
    /// else holds it", from genuine contention. `ensure_server_explicit` reads
    /// it that way and falls into a 6 s blind poll instead of spawning a
    /// server, which is 40x the cost of the work the poll waits for.
    pub fn acquire(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "could not create lock directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| format!("could not open lock file {}: {error}", path.display()))?;

        if let Err(error) = native::try_lock(&file) {
            let holder = Self::probe_holder(path)
                .map(|pid| format!("; held by pid {pid}"))
                .unwrap_or_default();
            return Err(format!(
                "could not acquire advisory lock on {}: {error}{holder} \
                 (another nuo instance may already be running)",
                path.display()
            ));
        }

        // The PID is diagnostic metadata only. Lock ownership comes from the
        // open native file handle and cannot be forged by editing this text.
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = writeln!(file, "{}", std::process::id());
        let _ = file.flush();
        Ok(Self { file })
    }

    pub fn probe_holder(path: &Path) -> Option<u32> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// Probe lock state without taking ownership of an available lock.
    pub fn is_locked(path: &Path) -> bool {
        let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
            return false;
        };
        match native::try_lock(&file) {
            Ok(()) => {
                let _ = native::unlock(&file);
                false
            }
            Err(_) => true,
        }
    }

    /// Acquire a lock within a bounded wait budget.
    pub fn acquire_with_timeout(path: &Path, timeout: Duration) -> Result<Self, String> {
        let poll = Duration::from_millis(50);
        let start = Instant::now();
        loop {
            match Self::acquire(path) {
                Ok(lock) => return Ok(lock),
                Err(_) if start.elapsed() < timeout => std::thread::sleep(poll),
                Err(_) => {
                    return Err(format!(
                        "could not acquire advisory lock on {} within {:.0}s \
                         (another nuo server appears to be running)",
                        path.display(),
                        timeout.as_secs_f32()
                    ));
                }
            }
        }
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = native::unlock(&self.file);
    }
}

#[cfg(unix)]
mod native {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    pub(super) fn try_lock(file: &File) -> io::Result<()> {
        // SAFETY: the descriptor remains owned by `file` for the call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn unlock(file: &File) -> io::Result<()> {
        // SAFETY: the descriptor remains owned by `file` for the call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod native {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    /// A byte far beyond the diagnostic PID text. Windows byte-range locks
    /// block reads through the locked range, so locking from offset zero made
    /// `probe_holder` unreadable while the lock was doing its job. Windows
    /// permits locking beyond EOF; every participant contends on this same
    /// single sentinel byte while the human-readable metadata stays readable.
    const LOCK_BYTE_OFFSET: u32 = u32::MAX;

    fn lock_overlapped() -> OVERLAPPED {
        let mut overlapped = OVERLAPPED::default();
        // Selecting the `Anonymous` offset view is the documented layout used
        // by synchronous LockFileEx/UnlockFileEx calls. Writing a union field
        // is safe; only reading a union field requires `unsafe`.
        overlapped.Anonymous.Anonymous.Offset = LOCK_BYTE_OFFSET;
        overlapped
    }

    pub(super) fn try_lock(file: &File) -> io::Result<()> {
        let mut overlapped = lock_overlapped();
        // SAFETY: handle and OVERLAPPED are valid for this synchronous call.
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn unlock(file: &File) -> io::Result<()> {
        let mut overlapped = lock_overlapped();
        // SAFETY: handle and OVERLAPPED match the range used by `try_lock`.
        let ok = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_excludes_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("process.lock");
        let first = ProcessLock::acquire(&path).unwrap();
        assert!(ProcessLock::is_locked(&path));
        assert!(ProcessLock::acquire(&path).is_err());
        assert_eq!(ProcessLock::probe_holder(&path), Some(std::process::id()));
        drop(first);
        assert!(!ProcessLock::is_locked(&path));
        assert!(ProcessLock::acquire(&path).is_ok());
    }

    /// A lock whose parent directory does not exist yet must be acquirable.
    ///
    /// Regression: `acquire` opened the lock file with `create(true)` but never
    /// created the directory, so a pristine instance dir produced `ENOENT`.
    /// Callers treat any `Err` from `acquire` as "another process holds it",
    /// so the failure silently degraded into a 6 s poll (measured: 6364 ms vs
    /// 150 ms on a warm home).
    #[test]
    fn acquire_creates_a_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance").join("server-spawning.lock");
        assert!(!path.parent().unwrap().exists());

        let lock = ProcessLock::acquire(&path).expect("acquire must create the parent directory");
        assert_eq!(ProcessLock::probe_holder(&path), Some(std::process::id()));
        assert!(ProcessLock::is_locked(&path));
        drop(lock);
    }

    /// Absence of a directory must never look like contention: the error for a
    /// genuinely unreachable path names the directory, not "another nuo
    /// instance may already be running".
    #[test]
    fn a_missing_directory_is_not_reported_as_contention() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the parent directory should be: `create_dir_all`
        // cannot satisfy this, so it is the one remaining failure mode.
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("nested").join("server.lock");

        let error = ProcessLock::acquire(&path).unwrap_err();
        assert!(
            error.contains("could not create lock directory"),
            "expected a directory-creation error, got: {error}"
        );
        assert!(!error.contains("another nuo instance may already be running"));
    }

    #[test]
    fn lock_error_message_does_not_contain_project_baggage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("process.lock");
        let _first = ProcessLock::acquire(&path).unwrap();
        let err = ProcessLock::acquire(&path).unwrap_err();
        assert!(!err.contains("for this project"));
        assert!(err.contains("another nuo instance may already be running"));
    }
}
