//! Shared synchronization and concurrency primitives for `nuo-harness`.

use std::sync::{Mutex, MutexGuard};

/// Acquire a `Mutex` guard, recovering from poisoning via `into_inner()`.
///
/// In `nuo-harness`, mutexes guard in-memory registries, hooks, and request
/// states. If a thread panics while holding the lock, recovering the inner
/// data structure avoids cascading panics across the agent and runtime.
#[inline]
pub(crate) fn poison_lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
