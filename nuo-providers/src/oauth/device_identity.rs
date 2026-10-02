//! The host's stable identity for this installation.
//!
//! Some provider flows pin their risk signals to a *device*: Qoder's device
//! authorization carries a machine id, and the identity it issues later is bound
//! to that value. The stability requirement is therefore a contract, not an
//! implementation detail — a value that changes between runs makes the provider
//! see a different device.
//!
//! Where that value comes from is the host's business (ADR-0300 §1): a
//! workstation keeps a file, a fleet host might derive it from an instance id, a
//! CI runner has no device at all. The kernel only asks.

use std::path::PathBuf;
use std::sync::OnceLock;

/// A stable identifier for the installation a provider flow is running on.
///
/// Implementations must return the same value for the lifetime of the process
/// and across restarts. `stable_id` is synchronous and may be called from any
/// thread; implementations that read durable state must therefore cache it,
/// which the stability contract makes safe.
pub trait DeviceIdentity: Send + Sync + 'static {
    /// Stable across runs; two different installations must not collide.
    fn stable_id(&self) -> String;
}

/// Reference implementation: a file the host owns, read once per process.
///
/// The file is created with owner-only permissions on first use. A write failure
/// is non-fatal at the port level — the flow still works with a per-process
/// value — but the *cached* value is what every caller in this process sees, so
/// a failure cannot produce two different ids inside one run.
pub struct FileDeviceIdentity {
    path: PathBuf,
    /// Read once. The stability contract is what makes caching correct rather
    /// than merely convenient.
    cached: OnceLock<String>,
}

impl FileDeviceIdentity {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            cached: OnceLock::new(),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl std::fmt::Debug for FileDeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDeviceIdentity")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity for FileDeviceIdentity {
    fn stable_id(&self) -> String {
        self.cached
            .get_or_init(|| {
                if let Ok(existing) = std::fs::read_to_string(&self.path) {
                    let id = existing.trim();
                    if !id.is_empty() {
                        return id.to_string();
                    }
                }
                let fresh = uuid::Uuid::new_v4().to_string();
                // Best-effort: a host whose state directory is read-only still
                // gets a working flow, at the cost of stability across runs. The
                // warning is the operator's only signal, so it is explicit.
                if let Err(error) =
                    nuo_host::fsutil::atomic_write_bytes(&self.path, fresh.as_bytes())
                {
                    tracing::warn!(
                        path = %self.path.display(),
                        %error,
                        "could not persist the device identity; provider flows that pin to a \
                         device will see a new one on the next run"
                    );
                }
                fresh
            })
            .clone()
    }
}

/// An installation with no durable identity: one fresh value per process.
///
/// Correct for an embedding that never runs a device-pinned provider flow, and
/// honest about the consequence: flows that *do* pin to a device will see a new
/// device every run. A host that runs them supplies [`FileDeviceIdentity`] or its
/// own implementation instead.
pub struct PerProcessIdentity {
    id: String,
}

impl PerProcessIdentity {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl Default for PerProcessIdentity {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PerProcessIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerProcessIdentity").finish_non_exhaustive()
    }
}

impl DeviceIdentity for PerProcessIdentity {
    fn stable_id(&self) -> String {
        self.id.clone()
    }
}
