//! Connection/model usage telemetry: a process snapshot flushed to SQLite.
//!
//! Reads (picker ordering, budget checks) never touch SQLite; the model-turn
//! path only mutates the snapshot and marks it dirty. A single debounced
//! worker writes the merged JSON. See ADR-0238.
//!
//! Drives recency ordering in the connection picker and models picker. This is
//! program-generated usage signal, not user preference: it lives under
//! `$XDG_STATE_HOME` in `nuo.db`, and losing it only flattens the
//! sort order — never configuration. Favorites and the default-model pointer
//! belong in `config.toml` and are not stored here.
//!
//! Three maps are persisted:
//! - `connections`: connection id → recency (drives Connections list ordering).
//! - `models`: connection id → (model id → recency) (drives flat Models picker ordering per connection-model pair).
//! - `last_models`: connection id → the model id last activated under it, so a
//!   connection re-opens on the exact model it was left at (not a re-derived
//!   default).

use host::paths;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-entity usage record. Stored as a JSON object keyed by canonical id
/// (connection id or model id).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEntry {
    /// Unix epoch milliseconds of the most recent activation. Milliseconds
    /// (not seconds) so two activations within the same second still order
    /// deterministically rather than colliding.
    pub last_used_ms: u64,
    /// Total times the entity was activated. Kept for future tie-breaking and
    /// "most used" views; not used by the current recency sort.
    pub use_count: u64,
    /// Cumulative financial spend on this entity in nano-USD (ADR-0065).
    #[serde(default)]
    pub spend_nano_usd: u64,
}

/// Helper deserializer to cleanly migrate legacy flat `models` format into
/// the canonical hierarchical `connection_id -> (model_id -> UsageEntry)`.
fn deserialize_models<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, HashMap<String, UsageEntry>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ModelsWire {
        Nested(HashMap<String, HashMap<String, UsageEntry>>),
        Flat(serde::de::IgnoredAny),
    }

    match Option::<ModelsWire>::deserialize(deserializer)? {
        Some(ModelsWire::Nested(nested)) => Ok(nested),
        _ => Ok(HashMap::new()),
    }
}

/// The on-disk usage map. Serialized under SQLite KV `state:connection_usage`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionUsage {
    /// Connection id → recency/count. Drives Connections list ordering.
    #[serde(default)]
    connections: HashMap<String, UsageEntry>,
    /// (Connection id, Model id) → recency/count. Keyed hierarchically:
    /// connection_id → (model_id → UsageEntry). Drives flat Models picker ordering.
    #[serde(default, deserialize_with = "deserialize_models")]
    models: HashMap<String, HashMap<String, UsageEntry>>,
    /// Connection id → the wire model id last activated under it. Restores a
    /// connection's exact model on re-open instead of re-deriving a default.
    #[serde(default)]
    last_models: HashMap<String, String>,
}

/// Process-wide usage cache. Usage is program-written telemetry: the writer
/// process is the only authority, so a memory snapshot can serve every read
/// (picker ordering, budget checks) without touching SQLite. A deferred dirty
/// flag flushes the snapshot on the background thread.
type UsageCache = HashMap<std::path::PathBuf, ConnectionUsage>;

fn usage_cache() -> &'static Mutex<UsageCache> {
    static CACHE: OnceLock<Mutex<UsageCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key() -> std::path::PathBuf {
    paths::get().db_file()
}

/// Set when the in-memory snapshot has spend/activation changes not yet
/// written. The flusher coalesces any number of updates into one write.
static DIRTY: AtomicBool = AtomicBool::new(false);

impl ConnectionUsage {
    /// Load the process snapshot, populating it from SQLite once (with the
    /// one-time legacy state-file migration).
    pub fn load() -> Self {
        let key = cache_key();
        if let Ok(guard) = usage_cache().lock()
            && let Some(cached) = guard.get(&key)
        {
            return cached.clone();
        }
        let loaded = Self::load_store();
        if let Ok(mut guard) = usage_cache().lock() {
            guard.insert(key, loaded.clone());
        }
        loaded
    }

    /// Drop the cached snapshot (server reload / external writer).
    pub fn invalidate() {
        if let Ok(mut guard) = usage_cache().lock() {
            guard.remove(&cache_key());
        }
    }

    fn load_store() -> Self {
        let handle = crate::db::get_persistence_handle();
        if let Ok(reader) = handle.reader() {
            if let Ok(Some(mut usage)) = reader.get_json::<Self>("state:connection_usage") {
                usage.ensure_legacy_migrated();
                return usage;
            }
            let legacy_path = paths::get().state_dir.join("connection_usage.json");
            if legacy_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&legacy_path)
                    && let Ok(mut usage) = serde_json::from_str::<Self>(&content)
                {
                    usage.ensure_legacy_migrated();
                    let _ = handle.set_json_blocking("state:connection_usage", &usage);
                    let _ = std::fs::remove_file(&legacy_path);
                    let _ = std::fs::remove_file(
                        paths::get().state_dir.join("connection_usage.json.lock"),
                    );
                    return usage;
                }
                let _ = std::fs::remove_file(&legacy_path);
                let _ =
                    std::fs::remove_file(paths::get().state_dir.join("connection_usage.json.lock"));
            }
        }
        Self::default()
    }

    /// If models is empty but last_models and connections exist (legacy state migration),
    /// populate each connection's last active model with that connection's own recency.
    fn ensure_legacy_migrated(&mut self) {
        if self.models.is_empty() && !self.last_models.is_empty() {
            for (conn_id, model) in &self.last_models {
                if let Some(conn_entry) = self.connections.get(conn_id) {
                    self.models
                        .entry(conn_id.clone())
                        .or_default()
                        .insert(model.clone(), *conn_entry);
                }
            }
        }
    }

    /// Record an activation of connection `id`. Bumps `last_used_ms` to now, and
    /// increments `use_count`.
    pub fn record(&mut self, id: &str) {
        let now = now_ms();
        let entry = self.connections.entry(id.to_string()).or_default();
        entry.last_used_ms = entry.last_used_ms.max(now);
        entry.use_count = entry.use_count.saturating_add(1);
    }

    /// Record an activation of `model` under `connection_id`: bumps that
    /// (connection, model) pair's recency/count and pins it as that connection's
    /// last-used model so the connection re-opens on it.
    pub fn record_model(&mut self, connection_id: &str, model: &str) {
        let now = now_ms();
        let entry = self
            .models
            .entry(connection_id.to_string())
            .or_default()
            .entry(model.to_string())
            .or_default();
        entry.last_used_ms = entry.last_used_ms.max(now);
        entry.use_count = entry.use_count.saturating_add(1);
        self.last_models
            .insert(connection_id.to_string(), model.to_string());
    }

    /// The wire model id last activated under `connection_id`, if any. Lets a
    /// connection re-open on the exact model it was left at rather than a
    /// re-derived default.
    pub fn last_model_for(&self, connection_id: &str) -> Option<&str> {
        self.last_models.get(connection_id).map(|m| m.as_str())
    }

    /// Record spend without touching SQLite, and hand the write to the
    /// background flusher. This is the model-turn path: the turn never waits
    /// for a full JSON read-modify-write.
    pub fn record_spend_deferred(connection_id: &str, model_id: &str, nano_usd: u64) {
        let key = cache_key();
        let mut usage = Self::load();
        usage.record_spend(connection_id, model_id, nano_usd);
        if let Ok(mut guard) = usage_cache().lock() {
            guard.insert(key, usage);
        }
        DIRTY.store(true, Ordering::Release);
        ensure_flusher();
    }

    /// Flush the cached snapshot if anything is dirty. Safe to call from
    /// shutdown and tests; a no-op otherwise.
    pub fn flush_if_dirty() -> Result<(), String> {
        if !DIRTY.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let snapshot = Self::load();
        match snapshot.save_merged() {
            Ok(()) => Ok(()),
            Err(error) => {
                DIRTY.store(true, Ordering::Release);
                Err(error)
            }
        }
    }

    /// Persist atomically into SQLite (SSOT).
    pub fn save(&self) -> Result<(), String> {
        self.save_merged()
    }

    fn save_merged(&self) -> Result<(), String> {
        let mut merged = Self::load_store();
        for (id, entry) in &self.connections {
            let disk = merged.connections.entry(id.clone()).or_default();
            disk.last_used_ms = disk.last_used_ms.max(entry.last_used_ms);
            disk.use_count = disk.use_count.max(entry.use_count);
            disk.spend_nano_usd = disk.spend_nano_usd.max(entry.spend_nano_usd);
        }
        for (conn_id, models) in &self.models {
            let disk_models = merged.models.entry(conn_id.clone()).or_default();
            for (model, entry) in models {
                let disk = disk_models.entry(model.clone()).or_default();
                disk.last_used_ms = disk.last_used_ms.max(entry.last_used_ms);
                disk.use_count = disk.use_count.max(entry.use_count);
                disk.spend_nano_usd = disk.spend_nano_usd.max(entry.spend_nano_usd);
            }
        }
        for (connection_id, model) in &self.last_models {
            let in_mem_recency = self.model_recency(connection_id, model);
            let on_disk_model = merged.last_models.get(connection_id);
            let on_disk_recency = on_disk_model
                .map(|m| merged.model_recency(connection_id, m))
                .unwrap_or(0);
            if in_mem_recency >= on_disk_recency {
                merged
                    .last_models
                    .insert(connection_id.clone(), model.clone());
            }
        }
        crate::db::get_persistence_handle()
            .set_json_blocking("state:connection_usage", &merged)
            .map_err(|e| format!("could not persist connection usage to sqlite: {e}"))?;
        if let Ok(mut guard) = usage_cache().lock() {
            guard.insert(cache_key(), merged);
        }
        Ok(())
    }

    /// Re-key a connection's usage telemetry from `from` to `to` (rename).
    ///
    /// Usage recency is keyed by the connection name, so a rename would
    /// otherwise strand the history under the dead name — the renamed
    /// connection would lose its picker ordering and its last-used model, and
    /// the orphan entry would never be read again.
    pub fn rename_connection(&mut self, from: &str, to: &str) {
        if from.eq_ignore_ascii_case(to) {
            return;
        }
        if let Some(entry) = self.connections.remove(from) {
            self.connections.insert(to.to_string(), entry);
        }
        if let Some(models) = self.models.remove(from) {
            self.models.insert(to.to_string(), models);
        }
        if let Some(last) = self.last_models.remove(from) {
            self.last_models.insert(to.to_string(), last);
        }
    }

    /// Remove a connection and its associated models usage and last_model pointer.
    pub fn remove_connection(&mut self, id: &str) {
        self.connections.remove(id);
        self.models.remove(id);
        self.last_models.remove(id);
    }

    /// Remove a specific model under a connection from usage telemetry and any last_model pointers targeting it.
    pub fn remove_model(&mut self, connection_id: &str, model: &str) {
        if let Some(models) = self.models.get_mut(connection_id) {
            models.remove(model);
            if models.is_empty() {
                self.models.remove(connection_id);
            }
        }
        if self
            .last_models
            .get(connection_id)
            .is_some_and(|m| m == model)
        {
            self.last_models.remove(connection_id);
        }
    }

    /// Prune stale connections and connection-model pairs. Returns whether any entry was removed.
    pub fn prune(
        &mut self,
        mut is_valid_connection: impl FnMut(&str) -> bool,
        mut is_valid_pair: impl FnMut(&str, &str) -> bool,
    ) -> bool {
        let mut changed = false;

        let prev_conn_len = self.connections.len();
        self.connections.retain(|id, _| is_valid_connection(id));
        if self.connections.len() != prev_conn_len {
            changed = true;
        }

        let prev_models_len = self.models.len();
        self.models.retain(|conn_id, models| {
            if !is_valid_connection(conn_id) {
                return false;
            }
            let prev_inner_len = models.len();
            models.retain(|model_id, _| is_valid_pair(conn_id, model_id));
            if models.len() != prev_inner_len {
                changed = true;
            }
            !models.is_empty()
        });
        if self.models.len() != prev_models_len {
            changed = true;
        }

        let prev_last_len = self.last_models.len();
        self.last_models
            .retain(|conn_id, m| is_valid_connection(conn_id) && is_valid_pair(conn_id, m));
        if self.last_models.len() != prev_last_len {
            changed = true;
        }

        changed
    }

    /// Record financial spend on a connection and model in nano-USD (ADR-0065).
    pub fn record_spend(&mut self, connection_id: &str, model_id: &str, nano_usd: u64) {
        let now = now_ms();
        let conn_entry = self
            .connections
            .entry(connection_id.to_string())
            .or_default();
        conn_entry.last_used_ms = now;
        conn_entry.spend_nano_usd = conn_entry.spend_nano_usd.saturating_add(nano_usd);

        let model_map = self.models.entry(connection_id.to_string()).or_default();
        let model_entry = model_map.entry(model_id.to_string()).or_default();
        model_entry.last_used_ms = now;
        model_entry.spend_nano_usd = model_entry.spend_nano_usd.saturating_add(nano_usd);
    }

    /// Return cumulative recorded spend for a connection in nano-USD (ADR-0065).
    pub fn connection_spend(&self, connection_id: &str) -> u64 {
        self.connections
            .get(connection_id)
            .map(|e| e.spend_nano_usd)
            .unwrap_or(0)
    }

    /// Reset cumulative spend on a connection to zero (ADR-0065).
    pub fn reset_spend(&mut self, connection_id: &str) {
        if let Some(entry) = self.connections.get_mut(connection_id) {
            entry.spend_nano_usd = 0;
        }
    }

    /// Persist this usage state directly and atomically to SQLite (SSOT).
    pub fn save_exact(&self) -> Result<(), String> {
        crate::db::get_persistence_handle()
            .set_json_blocking("state:connection_usage", self)
            .map_err(|e| format!("could not persist usage store to sqlite: {e}"))?;
        if let Ok(mut guard) = usage_cache().lock() {
            guard.insert(cache_key(), self.clone());
        }
        Ok(())
    }

    /// Recency (epoch ms) of connection `id`, or `0` if never activated.
    pub fn recency_of(&self, id: &str) -> u64 {
        self.connections.get(id).map_or(0, |e| e.last_used_ms)
    }

    /// Recency (epoch ms) of `model` on `connection_id`, or `0` if never activated on that connection.
    pub fn model_recency(&self, connection_id: &str, model: &str) -> u64 {
        self.models
            .get(connection_id)
            .and_then(|m| m.get(model))
            .map_or(0, |e| e.last_used_ms)
    }

    /// Total activation count for connection `id`.
    pub fn count_of(&self, id: &str) -> u64 {
        self.connections.get(id).map_or(0, |e| e.use_count)
    }

    /// Total activation count for `model` on `connection_id`.
    pub fn model_count(&self, connection_id: &str, model: &str) -> u64 {
        self.models
            .get(connection_id)
            .and_then(|m| m.get(model))
            .map_or(0, |e| e.use_count)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Start the debounced flusher once. One OS thread per process, not per
/// session; it exists only to keep the JSON snapshot off the async runtime.
fn ensure_flusher() {
    static START: OnceLock<()> = OnceLock::new();
    START.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("nuo-usage-flush".to_string())
            .spawn(|| {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    if let Err(error) = ConnectionUsage::flush_if_dirty() {
                        tracing::warn!(%error, "could not flush connection usage");
                    }
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_model_isolates_by_connection() {
        let mut usage = ConnectionUsage::default();
        usage.record_model("conn-a", "shared-model");

        assert!(usage.model_recency("conn-a", "shared-model") > 0);
        assert_eq!(usage.model_count("conn-a", "shared-model"), 1);

        // Same model on a different connection MUST NOT be affected.
        assert_eq!(usage.model_recency("conn-b", "shared-model"), 0);
        assert_eq!(usage.model_count("conn-b", "shared-model"), 0);

        // Activating on conn-b records its own timestamp and count.
        usage.record_model("conn-b", "shared-model");
        assert!(usage.model_recency("conn-b", "shared-model") > 0);
        assert_eq!(usage.model_count("conn-b", "shared-model"), 1);
    }

    #[test]
    fn prune_removes_stale_connections_and_pairs() {
        let mut usage = ConnectionUsage::default();
        usage.record("conn-a");
        usage.record("conn-b");
        usage.record_model("conn-a", "model-2");
        usage.record_model("conn-a", "model-1");
        usage.record_model("conn-b", "model-1");

        let changed = usage.prune(
            |conn| conn == "conn-a",
            |conn, model| conn == "conn-a" && model == "model-1",
        );
        assert!(changed);

        assert!(usage.recency_of("conn-a") > 0);
        assert_eq!(usage.recency_of("conn-b"), 0);
        assert!(usage.model_recency("conn-a", "model-1") > 0);
        assert_eq!(usage.model_recency("conn-a", "model-2"), 0);
        assert_eq!(usage.model_recency("conn-b", "model-1"), 0);
        assert_eq!(usage.last_model_for("conn-a"), Some("model-1"));
        assert_eq!(usage.last_model_for("conn-b"), None);
    }

    #[test]
    fn legacy_flat_models_migration() {
        let legacy_json = r#"{
            "connections": {
                "conn-a": { "last_used_ms": 1000, "use_count": 5 },
                "conn-b": { "last_used_ms": 500, "use_count": 2 }
            },
            "models": {
                "shared-model": { "last_used_ms": 1000, "use_count": 7 }
            },
            "last_models": {
                "conn-a": "shared-model",
                "conn-b": "shared-model"
            }
        }"#;

        let mut usage: ConnectionUsage = serde_json::from_str(legacy_json).unwrap();
        usage.ensure_legacy_migrated();

        // conn-a gets its own recency (1000)
        assert_eq!(usage.model_recency("conn-a", "shared-model"), 1000);
        // conn-b gets its own recency (500), NOT conn-a's recency!
        assert_eq!(usage.model_recency("conn-b", "shared-model"), 500);
    }

    /// A rename carries the connection's recency, its per-model recency, and
    /// its last-used model pointer. Losing `last_models` would reset the
    /// renamed connection to a derived default, silently discarding the user's
    /// last choice.
    #[test]
    fn rename_connection_carries_usage_recency_and_last_model() {
        let mut usage = ConnectionUsage::default();
        usage.record("old");
        usage.record_model("old", "qfmodel");

        usage.rename_connection("old", "new");

        assert_eq!(usage.last_model_for("new"), Some("qfmodel"));
        assert_eq!(
            usage.last_model_for("old"),
            None,
            "no entry under the dead name"
        );
        assert!(
            usage.model_recency("new", "qfmodel") > 0,
            "per-model recency follows the rename"
        );
        assert_eq!(usage.model_recency("old", "qfmodel"), 0);
        assert!(
            usage.recency_of("new") > 0,
            "connection recency follows the rename"
        );
    }

    /// A case-only rename is the same connection; re-keying would strand the
    /// entry under a different exact key, so it must be a no-op.
    #[test]
    fn usage_case_only_rename_is_a_no_op() {
        let mut usage = ConnectionUsage::default();
        usage.record("qod");
        let before = usage.recency_of("qod");
        usage.rename_connection("qod", "QOD");
        assert_eq!(
            usage.recency_of("qod"),
            before,
            "the entry stays under its original exact key"
        );
        assert_eq!(usage.recency_of("QOD"), 0, "no second key was created");
    }

    #[test]
    fn test_connection_spend_tracking() {
        let mut usage = ConnectionUsage::default();
        usage.record_spend("gemini-test", "gemini-2.0-flash", 1_500_000); // $0.0015
        assert_eq!(usage.connection_spend("gemini-test"), 1_500_000);

        usage.record_spend("gemini-test", "gemini-2.0-flash", 2_000_000); // +$0.002
        assert_eq!(usage.connection_spend("gemini-test"), 3_500_000);

        usage.reset_spend("gemini-test");
        assert_eq!(usage.connection_spend("gemini-test"), 0);
    }
}
