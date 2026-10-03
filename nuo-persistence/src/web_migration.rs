//! Read-only compatibility bridge from the short-lived web connection model.
//!
//! The legacy file is never used for routing and is never deleted. Known preset
//! selections and credentials are copied into the singleton schema; unsupported
//! custom routes become disabled without destroying their archived data.

use serde::Deserialize;

use crate::paths;

#[derive(Debug, Default, Deserialize)]
struct LegacyStore {
    #[serde(default)]
    search_connections: Vec<LegacyConnection>,
    #[serde(default)]
    reader_connections: Vec<LegacyConnection>,
}

#[derive(Debug, Deserialize)]
struct LegacyConnection {
    id: String,
    #[serde(default)]
    preset_id: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
}

fn load() -> Option<LegacyStore> {
    std::fs::read_to_string(paths::get().web_connections_file())
        .ok()
        .and_then(|content| toml::from_str(&content).ok())
}

pub(crate) fn migrate_config_source(source: &str) -> String {
    let Ok(mut document) = toml::from_str::<toml::Value>(source) else {
        return source.to_string();
    };
    let Some(root) = document.as_table_mut() else {
        return source.to_string();
    };
    let key = if root.contains_key("web") {
        "web"
    } else {
        "websearch"
    };
    let Some(web) = root.get_mut(key).and_then(toml::Value::as_table_mut) else {
        return source.to_string();
    };

    // A canonical provider value is self-contained. Do not even read the
    // archival store on ordinary loads; only an unknown selected id can be a
    // former connection identity that needs translation.
    let needs_search_lookup = selection_needs_legacy(web, "provider", true);
    let needs_reader_lookup = selection_needs_legacy(web, "reader", false);
    let legacy = if needs_search_lookup || needs_reader_lookup {
        load().unwrap_or_default()
    } else {
        LegacyStore::default()
    };

    migrate_selection(web, "provider", &legacy.search_connections, true);
    migrate_selection(web, "reader", &legacy.reader_connections, false);
    toml::to_string(&document).unwrap_or_else(|_| source.to_string())
}

fn selection_needs_legacy(
    web: &toml::map::Map<String, toml::Value>,
    field: &str,
    search: bool,
) -> bool {
    let Some(value) = web.get(field).and_then(toml::Value::as_str) else {
        return false;
    };
    if search {
        nuo_wire::WebSearchProvider::parse_legacy(value).is_err()
    } else {
        nuo_wire::WebReaderProvider::parse_legacy(value).is_err()
    }
}

fn migrate_selection(
    web: &mut toml::map::Map<String, toml::Value>,
    field: &str,
    connections: &[LegacyConnection],
    search: bool,
) {
    let Some(original) = web
        .get(field)
        .and_then(toml::Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let canonical = if search {
        nuo_wire::WebSearchProvider::parse_legacy(&original)
            .ok()
            .map(|provider| provider.id())
    } else {
        nuo_wire::WebReaderProvider::parse_legacy(&original)
            .ok()
            .map(|provider| provider.id())
    };
    if let Some(canonical) = canonical {
        // Re-serialization canonicalizes legacy aliases such as builtin/none.
        web.insert(field.into(), toml::Value::String(canonical.into()));
        return;
    }

    let connection = connections
        .iter()
        .find(|connection| connection.id == original);
    let preset = connection.and_then(|connection| connection.preset_id.as_deref());
    let canonical = if search {
        preset
            .and_then(|value| nuo_wire::WebSearchProvider::parse_legacy(value).ok())
            .map(|provider| provider.id())
    } else {
        preset
            .and_then(|value| nuo_wire::WebReaderProvider::parse_legacy(value).ok())
            .map(|provider| provider.id())
    };
    if let Some(canonical) = canonical {
        web.insert(field.into(), toml::Value::String(canonical.into()));
        if search
            && canonical == "searxng"
            && !web.contains_key("searxng_url")
            && let Some(url) = connection.and_then(|connection| connection.base_url.clone())
        {
            web.insert("searxng_url".into(), toml::Value::String(url));
        }
        tracing::info!(legacy_connection = %original, provider = canonical, "migrated legacy web connection selection");
    } else {
        web.insert(field.into(), toml::Value::String("disabled".into()));
        tracing::warn!(legacy_connection = %original, axis = field, "legacy web connection has no implemented singleton provider; disabled while preserving legacy data");
    }
}

/// `None` means there was no valid legacy source, so the caller must not mark
/// the migration complete. `Some` means the source was consumed, even when it
/// contained no credential that needed copying.
pub(crate) fn migrate_connection_credentials(
    credentials: &mut crate::config::Credentials,
) -> Option<bool> {
    let legacy = load()?;
    let mut changed = false;
    for connection in legacy.search_connections {
        let Some(provider) = connection
            .preset_id
            .as_deref()
            .and_then(|value| nuo_wire::WebSearchProvider::parse_legacy(value).ok())
        else {
            continue;
        };
        let Some(secret) = credentials.connections.get(&connection.id).cloned() else {
            continue;
        };
        if !credentials.web.search.contains_key(provider.id()) {
            credentials.web.search.insert(provider.id().into(), secret);
            changed = true;
        }
    }
    for connection in legacy.reader_connections {
        let Some(provider) = connection
            .preset_id
            .as_deref()
            .and_then(|value| nuo_wire::WebReaderProvider::parse_legacy(value).ok())
        else {
            continue;
        };
        let Some(secret) = credentials.connections.get(&connection.id).cloned() else {
            continue;
        };
        if !credentials.web.reader.contains_key(provider.id()) {
            credentials.web.reader.insert(provider.id().into(), secret);
            changed = true;
        }
    }
    Some(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_connection_maps_to_its_preset() {
        let guard = crate::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir().unwrap();
        crate::paths::set_test_default(Some(crate::paths::Dirs {
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: None,
        }));
        std::fs::create_dir_all(&crate::paths::get().state_dir).unwrap();
        std::fs::write(
            crate::paths::get().web_connections_file(),
            "[[search_connections]]\nid = 'team-search'\npreset_id = 'tavily'\nenabled = true\n",
        )
        .unwrap();
        let legacy = load().unwrap();
        assert_eq!(legacy.search_connections.len(), 1);
        let parsed: toml::Value =
            toml::from_str("[websearch]\nprovider = 'team-search'\nreader = 'builtin'\n").unwrap();
        assert!(parsed.get("websearch").is_some());
        let migrated =
            migrate_config_source("[websearch]\nprovider = 'team-search'\nreader = 'builtin'\n");
        let config: crate::config::Config =
            toml::from_str(&migrated).unwrap_or_else(|error| panic!("{error}: {migrated}"));
        assert_eq!(
            config.web.provider,
            nuo_wire::WebSearchProvider::Tavily,
            "{migrated}"
        );
        crate::paths::set_test_default(None);
        drop(guard);
    }
}
