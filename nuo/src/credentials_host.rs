//! The application plane's credential host: where the product keeps OAuth
//! credentials and this installation's device identity.
//!
//! `muta-providers` implements the flows; it does not know where their durable
//! material lives (ADR-0300 §1, ADR-0303 §1). This module is the shipped
//! product's answer, resolved from the path topology (ADR-0013) once per process
//! and shared.
//!
//! One process, one host: the device identity is cached inside its
//! implementation, and every flow the daemon starts must see the same one — a
//! provider that pins risk signals to a device must not observe two devices in
//! one run.

use std::sync::{Arc, OnceLock};

use nuo_persistence::paths;
use nuo_providers::CredentialHost;

static HOST: OnceLock<CredentialHost> = OnceLock::new();

/// The product's credential host.
pub fn host() -> CredentialHost {
    HOST.get_or_init(|| {
        let dirs = paths::get();
        CredentialHost::file_backed(dirs.auth_file(), dirs.state_dir.join("machine_id"))
    })
    .clone()
}

/// The product's credential store, for callers that need only that half.
pub fn store() -> Arc<dyn nuo_providers::CredentialStore> {
    Arc::clone(host().store())
}
