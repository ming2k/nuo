//! The user's per-(instance, model) reasoning choices — a **state** store.
//!
//! This is *not* a cache. `effort` / `thinking` are the user's own per-route
//! settings (set from the model `e` editor); deleting them loses user
//! configuration that no endpoint can re-derive. They therefore live in
//! SQLite under the `state:route_settings` key (mirrored from the legacy
//! `$XDG_STATE_HOME/muta/route_settings.json`), separate from
//! `$XDG_STATE_HOME/muta/remote_catalog.json`, whose contents are program-
//! generated and re-derivable on the next live `GET /models` ("reset caches"
//! must not erase the user's reasoning overrides).
//!
//! ## Migration
//!
//! Releases before this split kept `route_settings` inside the catalog
//! cache. [`RouteSettingsStore::load`] folds any such entries into this store
//! one-shot and idempotently (presence check, not a version flag): the first
//! load after upgrade moves the map, clears it from the cache file, and a
//! marker field (`migrated_from_cache`) keeps later loads from re-reading a
//! cache that has since legitimately grown a fresh (empty) map.
//!
//! See ADR-0014 for the category rules this split follows.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::config::RouteSettings;
use crate::paths;

/// Read the historical `route_settings` map out of a pre-split catalog file.
/// Returns an empty map for a missing file, a post-split file (no such key), or
/// an unparseable file — migration must never fail startup.
///
/// Three historical locations are read, newest name first: the retired state
/// file (`models_discovery.json`, the pre-rename name of the current
/// `remote_catalog.json`), then the retired cache-dir file. Only the
/// `route_settings` key is taken; the catalog payload around it is derivable
/// and intentionally not migrated (ADR-0203 §29).
fn read_legacy_cache_route_settings() -> BTreeMap<String, BTreeMap<String, RouteSettings>> {
    let dirs = paths::get();
    let candidates = [
        dirs.retired_remote_catalog_state_file(),
        dirs.legacy_remote_catalog_cache_file(),
        dirs.remote_catalog_cache_file(),
    ];
    let Some(content) = candidates
        .iter()
        .find_map(|path| fs::read_to_string(path).ok())
    else {
        return BTreeMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return BTreeMap::new();
    };
    serde_json::from_value(
        value
            .get("route_settings")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .unwrap_or_default()
}

/// The persisted shape: the user's route settings plus the migration marker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct RouteSettingsFile {
    /// `connection_id -> model_id -> settings`
    #[serde(alias = "providers")]
    connections: BTreeMap<String, BTreeMap<String, RouteSettings>>,
    /// `true` once the one-shot fold out of the retired catalog cache has run.
    /// Distinguishes "not yet migrated" from "migrated and empty".
    migrated_from_cache: bool,
}

/// The user's per-route reasoning overrides, backed by
/// `$XDG_STATE_HOME/muta/route_settings.json`.
#[derive(Debug, Clone, Default)]
pub struct RouteSettingsStore {
    file: RouteSettingsFile,
}

impl RouteSettingsStore {
    /// Load the store, running the one-shot migration out of the retired
    /// catalog cache when it has not happened yet. Missing or unparseable
    /// file → an empty store.
    pub fn load() -> Self {
        let mut store = Self::read_file();
        if !store.file.migrated_from_cache {
            store.migrate_from_cache();
        }
        store
    }

    fn read_file() -> Self {
        let handle = crate::db::get_persistence_handle();
        if let Ok(reader) = handle.reader() {
            if let Ok(Some(file)) = reader.get_json::<RouteSettingsFile>("state:route_settings") {
                return Self { file };
            }
            let legacy_path = paths::get().state_dir.join("route_settings.json");
            if legacy_path.exists() {
                if let Ok(content) = fs::read_to_string(&legacy_path)
                    && let Ok(file) = serde_json::from_str::<RouteSettingsFile>(&content)
                {
                    let _ = handle.set_json_blocking("state:route_settings", &file);
                    let _ = fs::remove_file(&legacy_path);
                    return Self { file };
                }
                let _ = fs::remove_file(&legacy_path);
            }
        }
        Self::default()
    }

    /// One-shot fold of the pre-split layout.
    fn migrate_from_cache(&mut self) {
        let legacy = read_legacy_cache_route_settings();
        if !legacy.is_empty() {
            for (conn, models) in legacy {
                let target = self.file.connections.entry(conn).or_default();
                for (model, settings) in models {
                    target.entry(model).or_insert(settings);
                }
            }
        }
        self.file.migrated_from_cache = true;
        if let Err(e) = self.save() {
            tracing::warn!("could not persist route settings migration: {e}");
        }
    }

    /// Persist atomically into SQLite (SSOT), through the single writer.
    pub fn save(&self) -> Result<(), String> {
        crate::db::get_persistence_handle()
            .set_json_blocking("state:route_settings", &self.file)
            .map_err(|e| format!("could not save route settings to sqlite: {e}"))
    }

    /// The reasoning override for one route, if set.
    pub fn settings_for(&self, connection_id: &str, model_id: &str) -> Option<&RouteSettings> {
        self.file
            .connections
            .get(connection_id)
            .and_then(|models| models.get(model_id))
    }

    /// Borrow a route's settings mutably, inserting a default entry when
    /// absent, so a caller can set one field without rebuilding the store.
    pub fn settings_for_mut(&mut self, connection_id: &str, model_id: &str) -> &mut RouteSettings {
        self.file
            .connections
            .entry(connection_id.to_string())
            .or_default()
            .entry(model_id.to_string())
            .or_default()
    }

    /// Remove one route's entry (the `e` editor's "back to default" path).
    pub fn remove(&mut self, connection_id: &str, model_id: &str) {
        if let Some(models) = self.file.connections.get_mut(connection_id) {
            models.remove(model_id);
            if models.is_empty() {
                self.file.connections.remove(connection_id);
            }
        }
    }

    /// Whether any route carries a setting.
    pub fn is_empty(&self) -> bool {
        self.file.connections.iter().all(|(_, m)| m.is_empty())
    }

    /// Drop every route setting for `connection_id` (connection deletion).
    pub fn retain_connection_except(&mut self, connection_id: &str) {
        self.file.connections.remove(connection_id);
    }

    /// Re-key every route setting from `from` to `to` (connection rename).
    ///
    /// These are the user's own per-model effort/thinking choices — state, not
    /// cache — so a rename must carry them. Leaving them under the old name
    /// would silently reset the renamed connection to derived defaults, which
    /// is a user-visible loss of a setting they never touched.
    pub fn rename_connection(&mut self, from: &str, to: &str) {
        if from.eq_ignore_ascii_case(to) {
            return;
        }
        if let Some(settings) = self.file.connections.remove(from) {
            self.file.connections.insert(to.to_string(), settings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(instance: &str, model: &str, effort: &str) -> RouteSettingsStore {
        let mut store = RouteSettingsStore::default();
        store.settings_for_mut(instance, model).effort = Some(effort.to_string());
        store
    }

    /// A rename carries the user's own per-route effort/thinking choices. These
    /// are state, not cache: dropping them would silently reset the renamed
    /// connection to derived defaults — a user-visible loss of a setting they
    /// never touched.
    #[test]
    fn rename_connection_carries_route_settings() {
        let mut store = RouteSettingsStore::default();
        store.settings_for_mut("old", "qfmodel").effort = Some("xhigh".into());
        store.rename_connection("old", "new");

        assert_eq!(
            store
                .settings_for("new", "qfmodel")
                .and_then(|s| s.effort.clone())
                .as_deref(),
            Some("xhigh")
        );
        assert!(
            store.settings_for("old", "qfmodel").is_none(),
            "no settings may remain under the old name"
        );
    }

    /// A case-only rename must not move the entry: the stored key is exact, so
    /// re-keying to a differently-cased string would orphan the settings.
    #[test]
    fn route_case_only_rename_is_a_no_op() {
        let mut store = RouteSettingsStore::default();
        store.settings_for_mut("qod", "qfmodel").effort = Some("low".into());
        store.rename_connection("qod", "QOD");
        assert_eq!(
            store
                .settings_for("qod", "qfmodel")
                .and_then(|s| s.effort.clone())
                .as_deref(),
            Some("low")
        );
        assert!(store.settings_for("QOD", "qfmodel").is_none());
    }

    #[test]
    fn mut_insert_remove_and_empty_semantics() {
        let mut store = RouteSettingsStore::default();
        assert!(store.is_empty());
        store.settings_for_mut("p", "m").effort = Some("high".into());
        assert!(!store.is_empty());
        assert_eq!(
            store.settings_for("p", "m").unwrap().effort.as_deref(),
            Some("high")
        );
        store.remove("p", "m");
        assert!(store.is_empty(), "empty inner map must be dropped");
        assert!(store.settings_for("p", "m").is_none());
    }

    #[test]
    fn round_trips_through_disk() {
        let _guard = crate::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        crate::paths::set_test_default(Some(crate::paths::Dirs {
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: None,
        }));

        let store = store_with("anthropic", "claude-x", "high");
        store.save().unwrap();

        // A fresh load must see the same entry and not re-run the migration
        // into a different (empty) cache.
        let reloaded = RouteSettingsStore::load();
        assert_eq!(
            reloaded
                .settings_for("anthropic", "claude-x")
                .unwrap()
                .effort
                .as_deref(),
            Some("high")
        );

        crate::paths::set_test_default(None);
    }

    #[test]
    fn migration_rescues_user_route_settings_and_leaves_derivable_state_alone() {
        let _guard = crate::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        crate::paths::set_test_default(Some(crate::paths::Dirs {
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: None,
        }));

        // Seed the retired pre-rename state file the way an older release wrote
        // it: a catalog payload plus a `route_settings` key (the typed struct
        // no longer has the field — that is the point). The `route_settings`
        // map is **user data** (reasoning overrides) and must be rescued; the
        // catalog payload around it is derivable and must NOT be migrated
        // (ADR-0203 §29).
        let retired_json = serde_json::json!({
            "connection_models": { "kimi": ["kimi-k2"] },
            "route_settings": {
                "kimi": {
                    "kimi-k2": { "effort": "medium", "thinking": false }
                }
            }
        });
        std::fs::create_dir_all(root.path().join("state")).unwrap();
        std::fs::write(
            crate::paths::get().retired_remote_catalog_state_file(),
            serde_json::to_string_pretty(&retired_json).unwrap(),
        )
        .unwrap();

        let store = RouteSettingsStore::load();
        assert_eq!(
            store.settings_for("kimi", "kimi-k2").unwrap(),
            &RouteSettings {
                effort: Some("medium".into()),
                thinking: Some(false),
                capability_overrides: None,
                prompt_cache: None,
            },
            "the user's reasoning override must be rescued from the retired file"
        );
        // The derivable catalog payload is NOT carried forward: the current
        // catalog file was never written from the retired one.
        let cache_after = crate::config::RemoteCatalogCache::load();
        assert!(
            cache_after.connection_models.is_empty(),
            "derivable catalog state must not be migrated (ADR-0203 §29)"
        );

        // Idempotency: a second load must not re-fold or lose entries.
        let again = RouteSettingsStore::load();
        assert_eq!(
            again
                .settings_for("kimi", "kimi-k2")
                .unwrap()
                .effort
                .as_deref(),
            Some("medium")
        );

        crate::paths::set_test_default(None);
    }
}
