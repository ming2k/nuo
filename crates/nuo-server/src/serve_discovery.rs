//! Discovery file: how co-process clients find a live session server's
//! endpoint.
//!
//! The record that matters is **global**: the unified server (`nuo`) writes
//! one `server.json` per user ([`global_discovery_path`], in the resolved
//! instance dir) once its port is bound, and removes it on clean shutdown.
//! Clients (an attaching `nuo` TUI, the web app, the dashboard) read that
//! record to reach the already-running server instead of spawning a second
//! one. The module lives in this crate while the read-side contract is shared
//! with `nuo-protocol`, so writer and reader agree on the record and the
//! path-resolution rule.
//!
//! The pre-`server.json` per-project layout (`serve/<bucket>.json`) is dead:
//! current clients discover through `server.json` only, so nothing here
//! resolves a per-project path any more.
use std::path::{Path, PathBuf};

use nuo_persistence::paths;

pub use nuo_wire::discovery::Discovery;

/// The global discovery path for the unified server: one record
/// per user, in the instance dir (`NUO_HOME`, else
/// `$XDG_RUNTIME_DIR`, else the native data/state fallback).
pub fn global_discovery_path() -> PathBuf {
    paths::get().instance_dir().join("server.json")
}

/// The resolved server instance directory. Exposed for
/// diagnostics so a report can name *which* instance — host or sandbox —
/// it probed before listing paths inside it.
pub fn instance_dir() -> PathBuf {
    paths::get().instance_dir()
}

/// The default UDS path the server binds, inside the instance dir.
#[cfg(unix)]
pub fn default_uds_path() -> PathBuf {
    paths::get().instance_dir().join("server.sock")
}

/// Native local endpoint for the unified per-user server.
pub fn default_local_endpoint() -> Result<nuo_host::ipc::LocalEndpoint, String> {
    let instance_dir = paths::get().instance_dir();
    let instance_key = format!("server-{}", paths::project_bucket_name(&instance_dir));
    nuo_host::ipc::endpoint_for_instance(instance_dir.join("server.sock"), &instance_key)
        .map_err(|error| format!("could not resolve local server endpoint: {error}"))
}

/// The server's single-instance lock path: a companion
/// `server.lock` next to the global discovery record. The server holds a
/// `flock` on it for its whole lifetime; a second server (spawned while the
/// first drains) blocks on it for a bounded wait instead of unlinking a live
/// server's UDS socket, which is exactly the clobbering race the pre-0101
/// `bind_uds` "remove stale socket file" step could not distinguish.
pub fn global_lock_path() -> PathBuf {
    paths::get().instance_dir().join("server.lock")
}

/// RAII guard over the global discovery record: `Drop` removes the file, so
/// *every* exit path — graceful drain, forced escalation, a panic unwinding
/// through `host::run` — leaves the record deleted. The graceful path
/// removes it explicitly (and earlier: pulling the advertisement is the
/// *first* drain step so no new client discovers a draining server), at
/// which point the guard's own removal is a no-op.
#[must_use = "dropping the lease removes the discovery record"]
pub struct DiscoveryLease {
    path: Option<PathBuf>,
    pid: u32,
    process_birth_token: Option<u64>,
}

impl DiscoveryLease {
    /// Wrap an already-written record. `None` (write failed) still yields a
    /// guard — a no-op one — so callers need no branching.
    pub fn new(path: Option<PathBuf>, pid: u32, process_birth_token: Option<u64>) -> Self {
        Self {
            path,
            pid,
            process_birth_token,
        }
    }

    /// Remove the record now (the explicit early step of a graceful drain).
    pub fn release(&mut self) {
        if let Some(path) = self.path.take() {
            remove_if_matching_process(&path, self.pid, self.process_birth_token);
        }
    }
}

impl Drop for DiscoveryLease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;

    #[test]
    fn drop_removes_the_record_and_double_release_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("server.json");
        nuo_host::fsutil::atomic_write_json(
            &path,
            &Discovery {
                pid: 1,
                process_birth_token: None,
                port: 2,
                token: None,
                project_root: String::new(),
                started_at: 3,
                uds_path: None,
                local_endpoint: None,
                version: None,
                grace_secs: None,
                protocol: None,
                image_digest: None,
                image_len: None,
            },
        )
        .unwrap();
        let mut lease = DiscoveryLease::new(Some(path.clone()), 1, None);
        lease.release(); // explicit early removal
        assert!(!path.exists());
        drop(lease); // Drop's own removal must tolerate the missing file
    }

    #[test]
    fn drop_alone_removes_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("server.json");
        nuo_host::fsutil::atomic_write_json(
            &path,
            &Discovery {
                pid: 42,
                process_birth_token: None,
                port: 2,
                token: None,
                project_root: String::new(),
                started_at: 3,
                uds_path: None,
                local_endpoint: None,
                version: None,
                grace_secs: None,
                protocol: None,
                image_digest: None,
                image_len: None,
            },
        )
        .unwrap();
        drop(DiscoveryLease::new(Some(path.clone()), 42, None));
        assert!(!path.exists(), "Drop must remove the record");
    }

    #[test]
    fn drop_does_not_remove_newer_server_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("server.json");
        nuo_host::fsutil::atomic_write_json(
            &path,
            &Discovery {
                pid: 99,
                process_birth_token: None,
                port: 2,
                token: None,
                project_root: String::new(),
                started_at: 3,
                uds_path: None,
                local_endpoint: None,
                version: None,
                grace_secs: None,
                protocol: None,
                image_digest: None,
                image_len: None,
            },
        )
        .unwrap();
        // Lease from older server PID 42 dropped while path contains PID 99
        drop(DiscoveryLease::new(Some(path.clone()), 42, None));
        assert!(path.exists(), "Drop must NOT remove newer server's record");
    }
}

/// Read the global discovery record.
pub fn read() -> Option<Discovery> {
    read_at(&global_discovery_path())
}

pub fn read_at(path: &Path) -> Option<Discovery> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write the unified server's global discovery record. Atomic.
pub fn write_global(record: &Discovery) -> Result<PathBuf, String> {
    let path = global_discovery_path();
    write_to(&path, record)?;
    Ok(path)
}

/// Best-effort removal on clean shutdown (and of stale records by readers).
/// A missing file is not an error.
pub fn remove(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            ?error,
            path = %path.display(),
            "serve discovery: could not remove discovery file"
        );
    }
}

/// Remove `path` only if the record matches both the expected PID and, when
/// known, its process creation token. This remains safe across PID reuse.
pub fn remove_if_matching_process(
    path: &Path,
    expected_pid: u32,
    expected_birth_token: Option<u64>,
) {
    if let Ok(bytes) = std::fs::read(path)
        && let Ok(record) = serde_json::from_slice::<Discovery>(&bytes)
        && (record.pid != expected_pid
            || expected_birth_token
                .is_some_and(|expected| record.process_birth_token != Some(expected)))
    {
        return;
    }
    remove(path);
}

/// Atomic write of one record to an explicit path.
fn write_to(path: &Path, record: &Discovery) -> Result<(), String> {
    nuo_host::fsutil::atomic_write_json(path, record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> Discovery {
        Discovery {
            pid: 4242,
            process_birth_token: None,
            port: 39871,
            token: Some("deadbeef".to_string()),
            project_root: "/home/me/proj".to_string(),
            started_at: 1_755_000_000,
            uds_path: None,
            local_endpoint: None,
            version: Some("0.30.2".to_string()),
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        }
    }

    #[test]
    fn write_then_read_roundtrips_and_creates_serve_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve").join("abc123.json");
        let record = sample_record();
        write_to(&path, &record).unwrap();
        assert!(path.exists(), "record file must exist after write");
        let bytes = std::fs::read(&path).unwrap();
        let parsed: Discovery = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed, record);
    }

    #[test]
    fn remove_deletes_and_tolerates_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        // Missing file: best-effort, must not panic.
        remove(&path);
        write_to(&path, &sample_record()).unwrap();
        remove(&path);
        assert!(!path.exists(), "record file must be gone after remove");
    }

    #[test]
    fn version_roundtrips_and_defaults_to_none_for_legacy_records() {
        // A legacy record without `version` still deserializes (field is
        // optional) and reports "unknown" to version negotiation.
        let legacy = r#"{
            "pid": 1, "port": 7, "token": null, "project_root": "",
            "started_at": 9
        }"#;
        let parsed: Discovery = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.version, None);
        // And a versioned record roundtrips.
        let json = serde_json::to_string(&sample_record()).unwrap();
        let back: Discovery = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version.as_deref(), Some("0.30.2"));
        // `version` is skipped when None so legacy readers see their own shape.
        let none_version = Discovery {
            version: None,
            ..sample_record()
        };
        let json = serde_json::to_string(&none_version).unwrap();
        assert!(!json.contains("version"), "absent field must be omitted");
    }
}
