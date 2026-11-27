//! Discovered model lists and fitted capabilities caching.
use super::routes::FittedModelInfo;
use nuo_host::fsutil;
use nuo_host::paths;
use nuo_protocol::RemoteModelMetadata;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Discovered model lists and fitted capabilities, cached under
/// `$XDG_STATE_HOME/nuo/remote_catalog.json`.
///
/// Lives under state (not cache) since the contents — discovered model ids,
/// ETag revalidation metadata, advertised capability fields — are
/// program-generated state the user expects to survive restarts rather than
/// regenerable-from-scratch cache data. A pre-0.43 build wrote this file under
/// `$XDG_CACHE_HOME/nuo/remote_catalog.json`; that legacy copy is read once
/// on first load after upgrade and adopted into the state path (see
/// [`Self::load`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteCatalogCache {
    /// Cached discovered model lists, keyed by connection id:
    /// connection_id -> model ids (in catalog order).
    #[serde(default)]
    pub connection_models: BTreeMap<String, Vec<String>>,
    /// Fitted capability metadata, keyed by connection id then model id.
    #[serde(default)]
    pub fitted_models: BTreeMap<String, BTreeMap<String, FittedModelInfo>>,
    /// Trusted per-(connection, model) capability metadata advertised by the
    /// connection's live `GET /models` (endpoint, thinking, effort tiers …).
    #[serde(default)]
    pub remote_metadata: BTreeMap<String, BTreeMap<String, RemoteModelMetadata>>,
    /// Revalidation metadata for live catalogs, keyed by connection id. The
    /// actual model/capability payload remains in the maps above so older cache
    /// files continue to deserialize without migration.
    #[serde(default)]
    pub model_lists: BTreeMap<String, ModelListCacheState>,
}

/// Freshness and validator state for one connection's remote model catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelListCacheState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default)]
    pub client_version: String,
    /// Stable identity of the catalog request that produced this validator.
    /// An ETag is reusable only for the same source, endpoint, protocol, and
    /// client emulation profile.
    #[serde(default)]
    pub source_identity: String,
    #[serde(default)]
    pub refreshed_at_ms: i64,
    /// Whether the most recent refresh attempt for this connection **failed**.
    /// A failed refresh deliberately keeps the previous payload
    /// (`[INV-CATALOG-03]`), so this is the only record that the retained
    /// availability verdicts were observed before a failure and may already be
    /// out of date. Cleared by any successful refresh.
    #[serde(default)]
    pub refresh_failed: bool,
}

impl RemoteCatalogCache {
    pub fn file_path() -> PathBuf {
        paths::get().remote_catalog_cache_file()
    }

    /// Read `remote_catalog.json`, returning an empty value if missing or unparseable.
    ///
    /// Clean break: no cache-dir migration shim. A file left at
    /// the retired `$XDG_CACHE_HOME/nuo/remote_catalog.json` location is
    /// ignored and re-derived — the catalog is program-generated state, so a
    /// missing read costs one refresh, not correctness.
    pub fn load() -> Self {
        let path = Self::file_path();
        if let Ok(content) = fs::read_to_string(&path) {
            return serde_json::from_str(&content).unwrap_or_default();
        }
        Self::default()
    }

    /// Persist atomically to `$XDG_STATE_HOME/nuo/remote_catalog.json`.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = serde_json::to_vec_pretty(self)?;
        fsutil::atomic_write_bytes(&Self::file_path(), &bytes)?;
        Ok(())
    }

    /// Remove the per-connection records for `connection_id` (used on connection deletion).
    pub fn remove_connection(&mut self, connection_id: &str) {
        self.connection_models.remove(connection_id);
        self.fitted_models.remove(connection_id);
        self.remote_metadata.remove(connection_id);
        self.model_lists.remove(connection_id);
    }

    /// Re-key every cached entry from `from` to `to` (connection rename).
    ///
    /// A rename changes the connection's *name*, not what it fetches:
    /// [`crate::config::RemoteCatalogCache`]'s `model_lists` validator fingerprint is
    /// derived from the fetch attributes (shape, base URL, client profile,
    /// dimensions), never from the name, so the cached ETag stays valid for the
    /// renamed connection and the next sync revalidates instead of refetching.
    /// Discarding the entry here would throw away a live validator and force a
    /// full catalog fetch for no reason — which is why rename carries rather
    /// than deletes.
    pub fn rename_connection(&mut self, from: &str, to: &str) {
        if from.eq_ignore_ascii_case(to) {
            return;
        }
        if let Some(models) = self.connection_models.remove(from) {
            self.connection_models.insert(to.to_string(), models);
        }
        if let Some(fitted) = self.fitted_models.remove(from) {
            self.fitted_models.insert(to.to_string(), fitted);
        }
        if let Some(metadata) = self.remote_metadata.remove(from) {
            self.remote_metadata.insert(to.to_string(), metadata);
        }
        if let Some(state) = self.model_lists.remove(from) {
            self.model_lists.insert(to.to_string(), state);
        }
    }

    /// The trusted per-(connection, model) metadata, if set.
    pub fn remote_metadata_for(
        &self,
        connection_id: &str,
        model_id: &str,
    ) -> Option<&RemoteModelMetadata> {
        self.remote_metadata
            .get(connection_id)
            .and_then(|models| models.get(model_id))
    }

    /// Acquire the cross-process lock on `remote_catalog.json` and load the latest state under the lock.
    pub async fn lock() -> Result<LockedRemoteCatalogCache, String> {
        let path = Self::file_path();
        let lock = tokio::task::spawn_blocking(move || fsutil::FileLock::acquire(&path))
            .await
            .map_err(|error| format!("remote-catalog lock task failed: {error}"))?
            .map_err(|error| format!("could not lock remote-catalog cache: {error}"))?;
        let cache = Self::load();
        Ok(LockedRemoteCatalogCache { cache, _lock: lock })
    }

    /// Execute a transactional mutation on `RemoteCatalogCache` under the cross-process lock and save atomically.
    pub async fn modify<F, R>(f: F) -> Result<R, String>
    where
        F: FnOnce(&mut RemoteCatalogCache) -> R,
    {
        let mut locked = Self::lock().await?;
        let result = f(&mut locked);
        locked.save().map_err(|e| e.to_string())?;
        Ok(result)
    }
}

/// An exclusive cross-process lock over `remote_catalog.json`.
pub struct LockedRemoteCatalogCache {
    cache: RemoteCatalogCache,
    _lock: fsutil::FileLock,
}

impl std::ops::Deref for LockedRemoteCatalogCache {
    type Target = RemoteCatalogCache;
    fn deref(&self) -> &Self::Target {
        &self.cache
    }
}

impl std::ops::DerefMut for LockedRemoteCatalogCache {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cache
    }
}

impl LockedRemoteCatalogCache {
    pub fn cache(&self) -> &RemoteCatalogCache {
        &self.cache
    }

    pub fn cache_mut(&mut self) -> &mut RemoteCatalogCache {
        &mut self.cache
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.cache.save()
    }
}
