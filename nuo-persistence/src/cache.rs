//! A refreshable local file cache — the shared storage mechanic for dynamic
//! catalogs.
//!
//! [`CachedResource`] wraps a cache file path with atomic-write + load +
//! fallback semantics. It does not know *what* it caches (JSON, text, raw
//! bytes) or *when* to refresh (that is the [`DynamicCatalog`](nuo_contracts::DynamicCatalog)
//! trait's job); it only guarantees that:
//!
//! - Writes are atomic (a crash mid-write never leaves a corrupt file).
//! - Reads never panic (missing/corrupt → `None`; the caller falls back).
//! - The last good copy survives a failed refresh (overwrite only on success).
//!
//! This is the concrete utility that remote skill repos use to persist their
//! downloads.

use std::path::PathBuf;

use crate::fsutil;

/// A local file cache backed by an atomic-write file at `path`.
///
/// Created from a cache path (typically resolved via
/// [`paths::Dirs`](crate::paths::Dirs)). Callers fetch from a remote source,
/// validate, then [`store`](Self::store) the payload; later
/// [`load`](Self::load) returns the cached copy. A missing or corrupt file
/// yields `None` — never an error — so the catalog falls back gracefully.
pub struct CachedResource {
    path: PathBuf,
}

impl CachedResource {
    /// Wrap a cache file path. The file may not exist yet (first run).
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The cache file path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Whether the cache file exists on disk.
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Read the raw cached text. `None` when the file is missing or unreadable
    /// — the caller falls back to a compiled-in default.
    pub fn load(&self) -> Option<String> {
        std::fs::read_to_string(&self.path)
            .ok()
            .filter(|s| !s.trim().is_empty())
    }

    /// Read and deserialize JSON from the cache. `None` when missing, empty,
    /// or unparseable (a corrupt download never replaces a good cache because
    /// the caller validates before storing).
    pub fn load_json<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        let text = self.load()?;
        serde_json::from_str(&text).ok()
    }

    /// Atomically write text to the cache. The write is crash-safe (temp file
    /// → rename); a failure leaves the previous file intact.
    pub fn store(&self, text: &str) -> Result<(), String> {
        fsutil::atomic_write_bytes(&self.path, text.as_bytes())
            .map_err(|e| format!("write cache {}: {e}", self.path.display()))
    }

    /// Serialize and atomically write JSON to the cache.
    pub fn store_json<T: serde::Serialize>(&self, value: &T) -> Result<(), String> {
        let text = serde_json::to_string(value).map_err(|e| format!("serialize cache: {e}"))?;
        self.store(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cache_loads_none() {
        let cache = CachedResource::new(PathBuf::from("/nonexistent/muta-test-cache.json"));
        assert!(cache.load().is_none());
        assert!(cache.load_json::<Vec<String>>().is_none());
        assert!(!cache.exists());
    }

    #[test]
    fn store_then_load_roundtrips() {
        let dir = std::env::temp_dir().join(format!(
            "muta-cache-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.json");
        let cache = CachedResource::new(path.clone());

        cache.store(r#"{"hello":"world"}"#).unwrap();
        assert_eq!(cache.load().as_deref(), Some(r#"{"hello":"world"}"#));

        let parsed: serde_json::Value = cache.load_json().unwrap();
        assert_eq!(parsed["hello"], "world");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_cache_loads_none() {
        let dir = std::env::temp_dir().join(format!(
            "muta-cache-corrupt-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("corrupt.json");
        std::fs::write(&path, "not json at all").unwrap();

        let cache = CachedResource::new(path);
        // load() returns the raw text; load_json() rejects it.
        assert!(cache.load().is_some());
        assert!(cache.load_json::<serde_json::Value>().is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_cache_loads_none() {
        let dir = std::env::temp_dir().join(format!(
            "muta-cache-empty-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.json");
        std::fs::write(&path, "   \n  ").unwrap();

        let cache = CachedResource::new(path);
        assert!(
            cache.load().is_none(),
            "whitespace-only cache should be None"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
