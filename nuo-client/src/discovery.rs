//! Discovery file: how clients find a live session daemon's endpoint.

use std::path::{Path, PathBuf};
use nuo_host::paths;

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
    /// The bearer token clients must present, when auth is active.
    pub token: Option<String>,
    /// The project root the host serves.
    pub project_root: String,
    /// Unix seconds at startup.
    pub started_at: u64,
    /// Unix domain socket the control plane also listens on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uds_path: Option<PathBuf>,
    /// Native local control endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
    /// The daemon build's version string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The daemon's configured graceful-drain budget, seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_secs: Option<u64>,
    /// The wire protocol number this daemon speaks.
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

/// The global discovery path for the unified daemon.
pub fn global_discovery_path() -> PathBuf {
    paths::get().instance_dir().join("daemon.json")
}

/// The resolved daemon instance directory.
pub fn instance_dir() -> PathBuf {
    paths::get().instance_dir()
}

/// The default UDS path the daemon binds, inside the instance dir.
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

/// The daemon's single-instance lock path.
pub fn global_lock_path() -> PathBuf {
    paths::get().instance_dir().join("daemon.lock")
}

/// Read the global discovery record.
pub fn read() -> Option<Discovery> {
    read_at(&global_discovery_path())
}

/// Read a discovery record from an explicit path.
pub fn read_at(path: &Path) -> Option<Discovery> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Atomically write a discovery record.
pub fn write_to(path: &Path, record: &Discovery) -> Result<(), String> {
    nuo_host::fsutil::atomic_write_json(path, record)
        .map_err(|e| format!("could not write discovery file {}: {e}", path.display()))
}

/// Remove a discovery record.
pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
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
