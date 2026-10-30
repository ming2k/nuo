//! Connections — the persisted "who I connect to" records.
//!
//! A connection is a **named pipe** to a model provider: it points
//! at exactly one model provider by id, owns one credential, declares the
//! client identity (impersonation / User-Agent) it speaks with, and may narrow
//! or override the provider's model universe. It never defines models itself.
//!
//! The connection's **name is its identity** — the primary key for its
//! credential (`credentials.toml [connections.<name>]`), its OAuth token set
//! (`auth.toml [tokens.<name>]`), its discovery cache, and `config.toml`'s
//! `default_connection`. Names are unique and compared case-insensitively; a
//! duplicate is rejected with a suggested alternative rather than silently
//! disambiguated.
//!
//! Connections deliberately carry **no channels and no model-list state**: the
//! routes (per-model transport/endpoint/effort) are *derived* at runtime from
//! the connection's provider plus the discovery cache, so two connections to
//! the same provider never duplicate or drift a channel set, and the app never
//! persists production data it can re-derive.
//!
//! Stored in `$XDG_STATE_HOME/nuo/connections.toml` — a program-managed
//! state file, separate from the user-edited `config.toml`. [`Connections::save`]
//! and [`Connections::load`] share one schema: there is no separate legacy decode
//! struct, so a field can never be emitted by the writer yet rejected by the
//! reader (the failure mode that silently empties the whole store).
use nuo_wire::model_providers::is_known_model_provider;
use nuo_wire::{ConnectionAuth, ConnectionBudget};
use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::model_providers::ModelProviders;
use crate::paths;

pub use nuo_wire::model::{
    ConnectionFilterPolicy, DeclaredModel, ModelCapabilityPatch, ModelScopeConfig,
    NamedFilterPolicy,
};

/// One connection: a credentialed pipe to a model provider.
///
/// Purified of transport-level concerns (endpoint, wire protocol, discovery).
/// A connection binds an authentication mode and identity to a model provider, and
/// optionally applies model scoping/filtering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// Connection name — the connection's identity and primary key.
    /// Unique (case-insensitive) across the store; referenced by
    /// `config.toml`'s `default_connection` and by every per-connection store.
    pub name: String,
    /// The model provider this connection points at. Must resolve to a
    /// registered provider; an unknown value is rejected at load.
    pub provider: String,
    /// How this connection authenticates. [`ConnectionAuth::Static`] (the default)
    /// resolves the bearer from the connection credential; the negotiated variants
    /// resolve from `auth.toml`.
    #[serde(default)]
    pub auth: ConnectionAuth,
    /// Optional environment variable name holding this connection's credential.
    /// A 12-factor override: when set (and non-empty), it wins over
    /// `credentials.toml`. Declared once per connection, never per route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Legacy client identity field, silently tolerated on load and pruned on save.
    #[serde(
        default,
        skip_serializing,
        alias = "client_identity",
        alias = "client_profile"
    )]
    pub _legacy_client_identity: Option<serde_json::Value>,
    /// Optional HTTP headers to attach to requests for this connection.
    /// Used primarily by custom/proxy endpoints that require specific headers
    /// (e.g. User-Agent, X-Title, or gateway routing keys).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Model scope configuration: this connection's
    /// include / exclude / override delta over the provider's universe.
    #[serde(default, skip_serializing_if = "ModelScopeConfig::is_empty")]
    pub models: ModelScopeConfig,
    /// Catalog request dimensions this connection overrides, keyed by dimension
    /// name (e.g. Qoder's `scene`). The provider's catalog shape declares the
    /// defaults; an override here selects a different catalog (Qoder's
    /// `assistant` vs `experts`) and caches independently of the default.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub catalog_dimensions: std::collections::BTreeMap<String, String>,
    /// Budget policy and circuit-breaker posture for this connection (ADR-0065).
    #[serde(default)]
    pub budget: ConnectionBudget,
    /// Economic entitlement and cost model for this connection (ADR-0095).
    /// FlatRate (subscription/included) vs Metered (pay-as-you-go).
    #[serde(default)]
    pub entitlement: nuo_wire::Entitlement,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: String::new(),
            auth: ConnectionAuth::Static,
            api_key_env: None,
            _legacy_client_identity: None,
            headers: std::collections::BTreeMap::new(),
            models: ModelScopeConfig::default(),
            catalog_dimensions: std::collections::BTreeMap::new(),
            budget: ConnectionBudget::default(),
            entitlement: nuo_wire::Entitlement::Metered,
        }
    }
}

impl Connection {
    /// The declared model ids of this connection, in declaration order.
    pub fn declared_models(&self) -> Vec<String> {
        self.models.included_ids()
    }

    /// Look up an explicitly declared/included model on this connection.
    pub fn extra_model(&self, model_id: &str) -> Option<&DeclaredModel> {
        self.models.find_included(model_id)
    }

    /// The declared included model ids, in declaration order.
    pub fn extra_model_ids(&self) -> Vec<String> {
        self.models.included_ids()
    }
}

/// The persisted set of connections (`connections.toml`). Program-managed
/// state, separate from the user-edited `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connections {
    #[serde(default)]
    pub connections: Vec<Connection>,
}

impl Connections {
    fn path() -> std::path::PathBuf {
        paths::get().connections_file()
    }

    /// Read the connections store, returning an empty value when missing or
    /// unparseable. Entries that cannot be served — a blank name, an unknown
    /// provider id, or a duplicate name (INV-2, INV-3) — are dropped with an
    /// error log rather than reinterpreted as a different connection kind.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let parsed: Connections = match toml::from_str(&content) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "could not parse connections.toml; ignoring it",
                );
                return Self::default();
            }
        };
        parsed.pruned(&path)
    }

    /// Drop entries the runtime cannot resolve. Provider resolution is the
    /// loader's only integrity gate; endpoints, channels, and model lists are
    /// derived later, so they are deliberately not validated here.
    fn pruned(mut self, path: &std::path::Path) -> Self {
        let providers = ModelProviders::load();
        let mut kept: Vec<Connection> = Vec::with_capacity(self.connections.len());
        for mut connection in self.connections.drain(..) {
            let name = connection.name.trim().to_string();
            if name.is_empty() {
                tracing::error!(path = %path.display(), "dropping a connection with no name");
                continue;
            }
            if !is_known_model_provider(&connection.provider)
                && providers.get_provider(&connection.provider).is_none()
            {
                tracing::error!(
                    path = %path.display(),
                    name = %name,
                    provider = %connection.provider,
                    "dropping a connection to an unknown model provider",
                );
                continue;
            }
            if kept
                .iter()
                .any(|existing| existing.name.eq_ignore_ascii_case(&name))
            {
                tracing::error!(
                    path = %path.display(),
                    name = %name,
                    "dropping a duplicate connection name",
                );
                continue;
            }
            connection.name = name;
            kept.push(connection);
        }
        self.connections = kept;
        self
    }

    /// Persist atomically. Errors propagate to the caller.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// Look up a connection by name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&Connection> {
        self.connections
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Look up a connection by name (case-insensitive), mutably.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Connection> {
        self.connections
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Whether a connection named `name` already exists.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Remove a connection by name. Returns the removed connection.
    pub fn remove(&mut self, name: &str) -> Option<Connection> {
        let index = self
            .connections
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))?;
        Some(self.connections.remove(index))
    }

    /// The connection names in declaration order.
    pub fn names(&self) -> Vec<String> {
        self.connections.iter().map(|c| c.name.clone()).collect()
    }

    /// Validate a proposed new connection name. Returns the rejection reason
    /// plus a suggested alternative when the name is empty or already taken.
    /// Names are compared case-insensitively (INV-3).
    pub fn check_new_name(&self, name: &str) -> Result<String, String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("a connection name is required".to_string());
        }
        if self.contains(trimmed) {
            let suggestion = self.suggest_name(trimmed);
            return Err(format!(
                "a connection named '{trimmed}' already exists; try '{suggestion}'"
            ));
        }
        Ok(trimmed.to_string())
    }

    /// A free name derived from `name`: the name with a numeric suffix, chosen
    /// to avoid every existing connection. Never used to silently rename a
    /// connection — only to fill the rejection message (INV-3).
    pub fn suggest_name(&self, name: &str) -> String {
        let base = name.trim();
        let base = if base.is_empty() { "connection" } else { base };
        let mut n = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !self.contains(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// The effective default connection: `default_connection` when it names a
    /// live connection, else the first connection, else `None`.
    pub fn effective_default(&self, default_connection: &str) -> Option<&Connection> {
        self.get(default_connection)
            .or_else(|| self.connections.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::reasoning::ReasoningSupport;

    fn sandboxed_paths() -> (paths::TestDirsGuard, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = paths::Dirs {
            config_dir: tmp.path().join("config"),
            data_dir: tmp.path().join("data"),
            state_dir: tmp.path().join("state"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: None,
        };
        (paths::TestDirsGuard::install(dirs), tmp)
    }

    fn deepseek_with_extras() -> Connection {
        Connection {
            name: "deepseek-personal".into(),
            provider: "deepseek".into(),
            models: ModelScopeConfig {
                filter: None,
                include: vec![DeclaredModel {
                    protocol: None,
                    id: "deepseek-v4-pro-preview-0912".into(),
                    context_window: Some(1_000_000),
                    max_output_tokens: Some(8_192),
                    thinking: Some(ReasoningSupport::ReasoningContent),
                    vision: Some(false),
                    tool_call: Some(true),
                }],
                exclude: Vec::new(),
                overrides: std::collections::BTreeMap::new(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn models_scope_roundtrip_through_toml() {
        let mut conn = deepseek_with_extras();
        conn.models.include.push(DeclaredModel {
            id: "bare-id".into(),
            ..Default::default()
        });
        conn.models.exclude.push("deprecated-model".into());
        let store = Connections {
            connections: vec![conn],
        };
        let text = toml::to_string_pretty(&store).unwrap();
        assert!(
            text.contains("[[connections.models.inject]]")
                || text.contains("[[connections.models.include]]")
        );
        assert!(text.contains("deprecated-model"));
        let parsed: Connections = toml::from_str(&text).unwrap();
        assert_eq!(parsed, store);
    }

    #[test]
    fn save_then_load_roundtrip_survives_every_serialized_field() {
        // Regression: `save` and `load` must share one schema. When the reader
        // was a separate legacy struct, every field the writer emitted but the
        // reader rejected — `entitlement` (ADR-0095) did — made the whole file
        // unparseable and silently dropped every connection.
        let _sandbox = sandboxed_paths();

        // Every serializable field carries a non-default value so the reader is
        // exercised on all of them, not just the ones that changed.
        let populated = Connection {
            name: "flat-conn".into(),
            provider: "anthropic".into(),
            auth: ConnectionAuth::Static,
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            _legacy_client_identity: None,
            headers: std::collections::BTreeMap::from([(
                "User-Agent".to_string(),
                "custom-agent/1.0".to_string(),
            )]),
            models: ModelScopeConfig {
                filter: Some(ConnectionFilterPolicy::Named(NamedFilterPolicy::All)),
                include: vec![DeclaredModel {
                    id: "claude-sonnet-4".into(),
                    ..Default::default()
                }],
                exclude: vec!["claude-instant".into()],
                overrides: std::collections::BTreeMap::new(),
            },
            catalog_dimensions: std::collections::BTreeMap::from([(
                "scene".to_string(),
                "assistant".to_string(),
            )]),
            budget: nuo_wire::ConnectionBudget::local(25.0, nuo_wire::BudgetPeriod::Daily),
            entitlement: nuo_wire::Entitlement::FlatRate,
        };
        let store = Connections {
            connections: vec![
                Connection {
                    name: "metered-conn".into(),
                    provider: "deepseek".into(),
                    entitlement: nuo_wire::Entitlement::Metered,
                    ..Default::default()
                },
                populated.clone(),
            ],
        };
        store.save().unwrap();

        let loaded = Connections::load();
        assert_eq!(
            loaded.names(),
            vec!["metered-conn".to_string(), "flat-conn".to_string()],
            "every connection must survive a save/load round trip",
        );
        assert_eq!(
            loaded.get("flat-conn").unwrap(),
            &populated,
            "a fully-populated connection must round-trip unchanged",
        );
    }

    #[test]
    fn load_drops_a_connection_to_an_unknown_provider() {
        let _sandbox = sandboxed_paths();
        Connections {
            connections: vec![
                Connection {
                    name: "bad".into(),
                    provider: "nope".into(),
                    ..Default::default()
                },
                Connection {
                    name: "good".into(),
                    provider: "deepseek".into(),
                    ..Default::default()
                },
            ],
        }
        .save()
        .unwrap();

        assert_eq!(Connections::load().names(), vec!["good".to_string()]);
    }

    #[test]
    fn load_drops_duplicate_names_keeping_the_first() {
        let _sandbox = sandboxed_paths();
        Connections {
            connections: vec![
                Connection {
                    name: "dup".into(),
                    provider: "deepseek".into(),
                    ..Default::default()
                },
                Connection {
                    name: "DUP".into(),
                    provider: "anthropic".into(),
                    ..Default::default()
                },
            ],
        }
        .save()
        .unwrap();

        let loaded = Connections::load();
        assert_eq!(loaded.names(), vec!["dup".to_string()]);
        assert_eq!(loaded.get("dup").unwrap().provider, "deepseek");
    }

    #[test]
    fn names_are_unique_case_insensitively() {
        let store = Connections {
            connections: vec![deepseek_with_extras()],
        };
        assert!(store.contains("DeepSeek-Personal"));
        assert!(store.check_new_name("DeepSeek-Personal").is_err());
        let err = store.check_new_name("deepseek-personal").unwrap_err();
        assert!(err.contains("deepseek-personal-2"), "{err}");
        assert!(store.check_new_name("  ").is_err());
        assert_eq!(store.check_new_name(" other ").unwrap(), "other");
    }

    #[test]
    fn extra_model_lookup_is_exact_and_empty_by_default() {
        let conn = deepseek_with_extras();
        assert_eq!(
            conn.extra_model("deepseek-v4-pro-preview-0912")
                .unwrap()
                .context_window,
            Some(1_000_000)
        );
        // Exact id match — case variants are distinct ids.
        assert!(conn.extra_model("Deepseek-v4-pro-preview-0912").is_none());
        assert!(conn.extra_model("deepseek-v4-flash").is_none());
        assert!(Connection::default().extra_model("anything").is_none());
    }

    #[test]
    fn sanitized_declared_model_rejects_blank_ids() {
        assert!(DeclaredModel::sanitized("  ").is_none());
        let declared = DeclaredModel::sanitized("  deepseek-x  ").unwrap();
        assert_eq!(declared.id, "deepseek-x");
        assert!(declared.context_window.is_none());
    }

    #[test]
    fn connection_budget_round_trips_through_toml() {
        let toml_str = r#"
[[connections]]
name = "gemini-guarded"
provider = "google"
[connections.budget]
mode = "local"
limit = 1.5
period = "session"
"#;
        let connections: Connections = toml::from_str(toml_str).unwrap();
        let conn = &connections.connections[0];
        assert_eq!(conn.name, "gemini-guarded");
        assert!(conn.budget.is_local());
        assert_eq!(
            conn.budget.limit_nano_usd(),
            Some(nuo_wire::pricing::NanoUsd::from_usd_f64(1.5))
        );
        assert_eq!(
            conn.budget.period(),
            Some(nuo_wire::budget::BudgetPeriod::Session)
        );

        // Serialize back and verify round-trip
        let serialized = toml::to_string(&connections).unwrap();
        assert!(serialized.contains("gemini-guarded"));
        assert!(serialized.contains("mode = \"local\""));
        let round_tripped: Connections = toml::from_str(&serialized).unwrap();
        assert_eq!(connections, round_tripped);
    }

    #[test]
    fn legacy_client_identity_is_tolerated_on_load_and_pruned_on_save() {
        let legacy_toml = r#"
[[connections]]
name = "legacy-conn"
provider = "openai"
client_identity = "claude-code"
"#;
        let connections: Connections = toml::from_str(legacy_toml).unwrap();
        assert_eq!(connections.connections.len(), 1);
        let conn = &connections.connections[0];
        assert_eq!(conn.name, "legacy-conn");

        let serialized = toml::to_string(&connections).unwrap();
        assert!(!serialized.contains("client_identity"));
        assert!(!serialized.contains("client_profile"));
    }
}
