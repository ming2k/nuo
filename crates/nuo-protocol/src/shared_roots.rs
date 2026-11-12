//! Live-updatable additional-roots handle shared between the assembling
//! bootstrap and the executing tools.
//!
//! Mirrors [`crate::SharedWebConfig`]: one typed `Arc<RwLock<…>>`
//! service the bootstrap provides into the tool context, so a runtime config
//! reload (`/settings reload`) can recompute roots and swap them in without
//! rebuilding the toolset or restarting the session.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Canonicalized additional roots, snapshotable on every operation.
#[derive(Debug, Clone, Default)]
pub struct SharedAdditionalRoots(Arc<RwLock<Vec<PathBuf>>>);

impl SharedAdditionalRoots {
    pub fn new(admitted: Vec<PathBuf>) -> Self {
        Self(Arc::new(RwLock::new(admitted)))
    }

    /// Empty handle: single-root admission.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Snapshot the current admitted set. Cheap clone per call-path use;
    /// locks are never held across await points.
    pub fn snapshot(&self) -> Vec<PathBuf> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Swap in a new canonical admitted set.
    pub fn store(&self, roots: Vec<PathBuf>) {
        let mut admitted = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *admitted = roots;
    }

    /// True when nothing beyond the primary is admitted right now.
    pub fn is_empty(&self) -> bool {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }
}

/// Session-scoped handle for workspace filesystem confinement.
///
/// Confined by default (`true`). When set to `false`, file tools bypass workspace
/// path boundaries and admit any host path.
#[derive(Debug, Clone)]
pub struct SharedConfinement(Arc<std::sync::atomic::AtomicBool>);

impl Default for SharedConfinement {
    fn default() -> Self {
        Self::new(true)
    }
}

impl SharedConfinement {
    pub fn new(confined: bool) -> Self {
        Self(Arc::new(std::sync::atomic::AtomicBool::new(confined)))
    }

    pub fn is_confined(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_confined(&self, confined: bool) {
        self.0.store(confined, std::sync::atomic::Ordering::Relaxed);
    }
}
