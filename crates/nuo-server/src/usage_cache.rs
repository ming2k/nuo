//! In-memory Connection Usage Cache with TTL.
//!
//! Prevents repeated, redundant remote HTTP queries to provider quota backends
//! when inspecting connection details or navigating between multiple connections.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use nuo_wire::ConnectionUsageState;

pub const DEFAULT_USAGE_CACHE_TTL: Duration = Duration::from_secs(180);

#[derive(Clone, Debug)]
struct CachedUsage {
    state: ConnectionUsageState,
    fetched_at: Instant,
}

#[derive(Clone, Debug)]
pub struct UsageCache {
    entries: Arc<RwLock<HashMap<String, CachedUsage>>>,
    ttl: Duration,
}

impl Default for UsageCache {
    fn default() -> Self {
        Self::new(DEFAULT_USAGE_CACHE_TTL)
    }
}

impl UsageCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            ttl,
        }
    }

    /// Retrieve cached usage state if available and within TTL.
    ///
    /// Transient errors (`ConnectionUsageState::Error`) and in-progress fetches
    /// are not considered valid cache hits.
    pub fn get(&self, connection_id: &str) -> Option<ConnectionUsageState> {
        let guard = self.entries.read().ok()?;
        let entry = guard.get(connection_id)?;
        if entry.fetched_at.elapsed() <= self.ttl {
            match &entry.state {
                ConnectionUsageState::Available(_) | ConnectionUsageState::Unsupported => {
                    Some(entry.state.clone())
                }
                _ => None,
            }
        } else {
            None
        }
    }

    /// Store newly fetched usage state into cache.
    pub fn put(&self, connection_id: impl Into<String>, state: ConnectionUsageState) {
        if matches!(state, ConnectionUsageState::Fetching) {
            return;
        }
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(
                connection_id.into(),
                CachedUsage {
                    state,
                    fetched_at: Instant::now(),
                },
            );
        }
    }

    /// Invalidate a single connection's cached usage.
    pub fn invalidate(&self, connection_id: &str) {
        if let Ok(mut guard) = self.entries.write() {
            guard.remove(connection_id);
        }
    }

    /// Invalidate all cached usages.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.entries.write() {
            guard.clear();
        }
    }
}

/// Global shared usage cache instance for control plane usage queries.
pub fn shared_usage_cache() -> &'static UsageCache {
    static INSTANCE: OnceLock<UsageCache> = OnceLock::new();
    INSTANCE.get_or_init(UsageCache::default)
}
