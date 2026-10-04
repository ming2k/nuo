//! Discovery file: how co-process clients find a live session daemon's
//! endpoint.
//!
//! Since ADR-0096 the record that matters is **global**: the unified daemon
//! (`muta`, run by `muta start`) writes one `daemon.json` per user
//! ([`global_discovery_path`], in `$XDG_RUNTIME_DIR/muta/` when a runtime
//! dir exists) once its port is bound, and removes it on clean shutdown.
//! Clients (an attaching `mutx` TUI, the web app, the
//! dashboard) read that record to reach the already-running daemon instead of
//! spawning a second one. The module lives in this crate — not in either
//! binary — so writer and reader share one definition of the record and of
//! the path-resolution rule.
//!
//! The per-project path resolution below is the **legacy** pre-ADR-0096
//! scheme, retained for reading (and cleaning up) old records:
//!
//! - `$XDG_RUNTIME_DIR/muta/serve/<bucket>.json` when a runtime dir exists
//!   ([`paths::Dirs::runtime_dir`]) — ephemeral tmpfs is the right home for a
//!   live process's PID/port; the record vanishes with the login session.
//! - `<data>/muta/projects/<bucket>/serve.json` as the fallback
//!   ([`paths::Dirs::project_dir`]) — the bucket always exists because the
//!   project's sessions already live under it.
//!
//! `<bucket>` is [`paths::project_bucket_name`], the same sha256[..16] hash
//! that names the project's session bucket.

use std::path::{Path, PathBuf};

use nuo_persistence::paths;

/// The discovery record, written once the bound port is known.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Discovery {
    /// The serving process's id (staleness probe for readers).
    pub pid: u32,
    /// OS process creation token paired with `pid`, preventing a stale record
    /// from targeting an unrelated process after PID reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_birth_token: Option<u64>,
    /// The bound TCP port the WebSocket listener serves.
    pub port: u16,
    /// The bearer token clients must present, when auth is active
    /// (`--public`, or loopback with `[daemon] local_auth` — ADR-0105);
    /// `null` for an unauthenticated loopback listener.
    pub token: Option<String>,
    /// The project root the host serves. Empty for the unified daemon
    /// (ADR-0096), which is project-agnostic; retained for the legacy
    /// per-project records.
    pub project_root: String,
    /// Unix seconds at startup.
    pub started_at: u64,
    /// Unix domain socket the control plane also listens on (ADR-0096).
    /// `None` for legacy records and when UDS is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uds_path: Option<PathBuf>,
    /// Native local control endpoint. `uds_path` above is retained only for
    /// reading discovery records written before v0.30.2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
    /// The daemon build's `CARGO_PKG_VERSION` (ADR-0100 rule 4): a client
    /// that reads a record whose version differs from its own refuses with
    /// an actionable both-versions message instead of speaking a wire
    /// protocol it may not share. `None` on records predating the field —
    /// treated as "unknown", which also mismatches, prompting a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The daemon's configured graceful-drain budget, **seconds**
    /// (ADR-0116). `muta stop` reads this so its escalation
    /// tiers wait *the daemon's own budget* before SIGTERM/SIGKILL: a
    /// signal arriving mid-drain escalates the daemon to a forced exit,
    /// so a client that escalates early destroys the graceful drain it
    /// just requested. `None` on records predating the field — clients
    /// fall back to a conservative default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_secs: Option<u64>,
    /// The wire protocol number this daemon speaks (ADR-0134). Lets a
    /// local client refuse a daemon outside its window *before* the
    /// handshake, the same way `version` above always has. `None` on
    /// records predating the field — the client then falls back to the
    /// product-version judgment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
}

impl Discovery {
    pub fn effective_local_endpoint(&self) -> Option<nuo_host::ipc::LocalEndpoint> {
        self.local_endpoint.clone().or_else(|| {
            self.uds_path
                .clone()
                .map(nuo_host::ipc::LocalEndpoint::UnixSocket)
        })
    }
}

/// The global discovery path for the unified daemon (ADR-0096): one record
/// per user, in the instance dir (ADR-0121: `MUTA_HOME`, else
/// `$XDG_RUNTIME_DIR`, else the native data/state fallback).
pub fn global_discovery_path() -> PathBuf {
    paths::get().instance_dir().join("daemon.json")
}

/// The resolved daemon instance directory (ADR-0121). Exposed for
/// diagnostics so a report can name *which* instance — host or sandbox —
/// it probed before listing paths inside it.
pub fn instance_dir() -> PathBuf {
    paths::get().instance_dir()
}

/// The default UDS path the daemon binds (ADR-0096), inside the instance dir.
#[cfg(unix)]
pub fn default_uds_path() -> PathBuf {
    paths::get().instance_dir().join("daemon.sock")
}

/// Native local endpoint for the unified per-user daemon.
pub fn default_local_endpoint() -> Result<nuo_host::ipc::LocalEndpoint, String> {
    let instance_dir = paths::get().instance_dir();
    let instance_key = format!("daemon-{}", paths::project_bucket_name(&instance_dir));
    nuo_host::ipc::endpoint_for_instance(instance_dir.join("daemon.sock"), &instance_key)
        .map_err(|error| format!("could not resolve local daemon endpoint: {error}"))
}

/// The daemon's single-instance lock path (ADR-0101): a companion
/// `daemon.lock` next to the global discovery record. The daemon holds a
/// `flock` on it for its whole lifetime; a second daemon (spawned while the
/// first drains) blocks on it for a bounded wait instead of unlinking a live
/// daemon's UDS socket, which is exactly the clobbering race the pre-0101
/// `bind_uds` "remove stale socket file" step could not distinguish.
pub fn global_lock_path() -> PathBuf {
    paths::get().instance_dir().join("daemon.lock")
}

/// RAII guard over the global discovery record: `Drop` removes the file, so
/// *every* exit path — graceful drain, forced escalation, a panic unwinding
/// through `host::run` — leaves the record deleted. The graceful path
/// removes it explicitly (and earlier: pulling the advertisement is the
/// *first* drain step so no new client discovers a draining daemon), at
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
        let path = tmp.path().join("daemon.json");
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
        let path = tmp.path().join("daemon.json");
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
            },
        )
        .unwrap();
        drop(DiscoveryLease::new(Some(path.clone()), 42, None));
        assert!(!path.exists(), "Drop must remove the record");
    }

    #[test]
    fn drop_does_not_remove_newer_daemon_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.json");
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
            },
        )
        .unwrap();
        // Lease from older daemon PID 42 dropped while path contains PID 99
        drop(DiscoveryLease::new(Some(path.clone()), 42, None));
        assert!(path.exists(), "Drop must NOT remove newer daemon's record");
    }
}

/// Write the unified daemon's global discovery record (ADR-0096). Atomic.
pub fn write_global(record: &Discovery) -> Result<PathBuf, String> {
    let path = global_discovery_path();
    write_to(&path, record)?;
    Ok(path)
}

/// Resolve the discovery-file path for `project_root` against the
/// process-wide [`paths::get`] dirs (see module docs).
pub fn discovery_path(project_root: &Path) -> PathBuf {
    let _ = project_root;
    // The pre-ADR-0096 per-project `serve/<bucket>.json` layout is dead:
    // current clients discover through `daemon.json` only. This function is
    // kept solely because the doc-commented layout is still described in
    // `paths.md`'s legacy table; it resolves nothing new.
    global_discovery_path()
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
