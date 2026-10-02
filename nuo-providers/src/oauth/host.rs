//! The host-supplied durable material a provider flow needs.
//!
//! `muta-providers` implements provider *mechanisms*: OAuth flows, token
//! refresh, request signing, transport selection. The durable material those
//! mechanisms read and write — where credentials live, and what this
//! installation is called — belongs to the host (ADR-0300 §1, ADR-0303 §1). This
//! bundle is how it arrives: one clone-cheap handle carrying both ports, so a
//! caller threads one value instead of two and an embedding supplies one thing.
//!
//! [`CredentialHost::file_backed`] is the reference wiring: the shipped file
//! store and the shipped file-backed device identity, pointed at paths the host
//! chose. An embedding with a keychain, a tenant directory, or no durable
//! identity at all constructs the ports itself.

use std::path::PathBuf;
use std::sync::Arc;

use super::device_identity::{DeviceIdentity, FileDeviceIdentity, PerProcessIdentity};
use super::store::{CredentialStore, FileCredentialStore, InMemoryCredentialStore};

/// The credential store and device identity a provider flow runs against.
#[derive(Clone)]
pub struct CredentialHost {
    store: Arc<dyn CredentialStore>,
    device: Arc<dyn DeviceIdentity>,
}

impl CredentialHost {
    pub fn new(store: Arc<dyn CredentialStore>, device: Arc<dyn DeviceIdentity>) -> Self {
        Self { store, device }
    }

    /// The reference wiring: a [`FileCredentialStore`] at `auth_file` and a
    /// [`FileDeviceIdentity`] at `device_file`.
    pub fn file_backed(auth_file: impl Into<PathBuf>, device_file: impl Into<PathBuf>) -> Self {
        Self {
            store: Arc::new(FileCredentialStore::new(auth_file)),
            device: Arc::new(FileDeviceIdentity::new(device_file)),
        }
    }

    /// An embedding that keeps nothing durable: an in-memory credential store
    /// that starts empty, and a per-process device identity.
    ///
    /// Provider flows that need a stored credential fail with the ordinary
    /// "no credential stored" error, which is the truthful outcome for a host
    /// that has not supplied any. A flow that mints one works for the process's
    /// lifetime.
    pub fn none() -> Self {
        Self {
            store: Arc::new(InMemoryCredentialStore::new()),
            device: Arc::new(PerProcessIdentity::new()),
        }
    }

    pub fn store(&self) -> &Arc<dyn CredentialStore> {
        &self.store
    }

    pub fn device(&self) -> &Arc<dyn DeviceIdentity> {
        &self.device
    }
}

impl std::fmt::Debug for CredentialHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialHost").finish_non_exhaustive()
    }
}
