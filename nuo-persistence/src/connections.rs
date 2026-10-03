//! Connections — the persisted "who I connect to" records.
//!
//! A connection is a **named pipe** to a model provider (ADR-0201): it points
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
//! Stored in `$XDG_STATE_HOME/muta/connections.toml` — a program-managed
//! state file, separate from the user-edited `config.toml`.

use nuo_wire::model_providers::{canonical_provider_id, is_known_model_provider};
use nuo_wire::{ClientIdentity, ConnectionAuth, WireProtocol};
use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::paths;

pub use nuo_wire::model::{
    ConnectionFilterPolicy, DeclaredModel, ModelCapabilityPatch, ModelScopeConfig,
    NamedFilterPolicy,
};

/// One connection: a credentialed pipe to a model provider (ADR-0201, ADR-0258).
///
/// Purified of transport-level concerns (endpoint, wire protocol, discovery) per ADR-0258.
/// A connection binds an authentication mode and identity to a model provider, and
/// optionally applies model scoping/filtering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// Connection name — the connection's identity and primary key (ADR-0201).
    /// Unique (case-insensitive) across the store; referenced by
    /// `config.toml`'s `default_connection` and by every per-connection store.
    pub name: String,
    /// The model provider this connection points at. Must resolve to a
    /// registered provider; an unknown value is rejected at load.
    pub provider: String,
    /// How this connection authenticates. [`ConnectionAuth::ApiKey`] (the default)
    /// resolves the bearer from the connection credential; the OAuth variants
    /// resolve from `auth.toml`.
    #[serde(default)]
    pub auth: ConnectionAuth,
    /// Optional environment variable name holding this connection's credential.
    /// A 12-factor override: when set (and non-empty), it wins over
    /// `credentials.toml`. Declared once per connection, never per route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Client profile specifying User-Agent and client identity headers (Native/muta, OpenCode, ZCode, Claude Code, etc.).
    /// Defaults to [`nuo_wire::ClientProfile::Native`].
    #[serde(default, alias = "client_profile")]
    pub client_identity: ClientIdentity,
    /// Model scope configuration (ADR-0199, ADR-0201): this connection's
    /// include / exclude / override delta over the provider's universe.
    #[serde(default, skip_serializing_if = "ModelScopeConfig::is_empty")]
    pub models: ModelScopeConfig,
    /// Catalog request dimensions this connection overrides, keyed by dimension
    /// name (e.g. Qoder's `scene`). The provider's catalog shape declares the
    /// defaults; an override here selects a different catalog (Qoder's
    /// `assistant` vs `experts`) and caches independently of the default.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub catalog_dimensions: std::collections::BTreeMap<String, String>,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: String::new(),
            auth: ConnectionAuth::ApiKey,
            api_key_env: None,
            client_identity: ClientIdentity::Native,
            models: ModelScopeConfig::default(),
            catalog_dimensions: std::collections::BTreeMap::new(),
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

/// Pre-ADR-0201 wire shape, accepted only when reading an existing file. The
/// loader rewrites it into [`Connection`] and never serializes these fields
/// back — the migration is one-shot and leaves no alias in the schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnection {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    preset_id: Option<String>,
    #[serde(default)]
    auth: ConnectionAuth,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default, alias = "client_profile")]
    client_identity: ClientIdentity,
    #[serde(default)]
    protocol: Option<WireProtocol>,
    #[serde(default)]
    base_url: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    catalog_source: Option<serde_json::Value>,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default, deserialize_with = "deserialize_connection_models")]
    models: ModelScopeConfig,
    #[serde(default)]
    extra_models: Vec<DeclaredModel>,
    #[serde(default)]
    catalog_dimensions: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnections {
    #[serde(default)]
    connections: Vec<RawConnection>,
}

impl RawConnection {
    fn needs_catalog_policy_migration(&self) -> bool {
        self.models.filter.is_none()
    }

    /// Migrate to the current shape, resolving the provider id and folding the
    /// legacy `extra_models` list into `models.include`.
    ///
    /// Before ADR-0203, curated connections persisted the provider's entire
    /// materialized model snapshot in `models`. A missing filter is the
    /// unambiguous legacy marker: migrate that connection to the official
    /// remote-catalog policy and discard the materialized snapshot. Explicit
    /// `extra_models` remain user-owned injections and are restored afterward.
    fn migrate(
        self,
        store: &mut crate::model_providers::ModelProviders,
    ) -> Result<Connection, String> {
        let name = self
            .name
            .or(self.id)
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| "connection has neither `name` nor a legacy `id`".to_string())?;

        let declared = self.provider.or(self.preset_id);
        let mut provider = match declared {
            None => {
                if name.starts_with("custom-") {
                    name.to_lowercase().replace(' ', "-")
                } else {
                    format!("custom-{}", name.to_lowercase().replace(' ', "-"))
                }
            }
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed == "custom" || trimmed == "custom-openai" {
                    if name.starts_with("custom-") {
                        name.to_lowercase().replace(' ', "-")
                    } else {
                        format!("custom-{}", name.to_lowercase().replace(' ', "-"))
                    }
                } else if let Some(canonical) = canonical_provider_id(trimmed) {
                    canonical
                } else {
                    if store.get_provider(trimmed).is_some() {
                        trimmed.to_string()
                    } else {
                        return Err(format!("unknown model provider '{raw}'"));
                    }
                }
            }
        };

        if self.base_url.is_some()
            || store.get_provider(&provider).is_none() && !is_known_model_provider(&provider)
        {
            // Preserve a legacy connection-specific endpoint as its own service;
            // never shadow an immutable built-in provider ID.
            if is_known_model_provider(&provider) {
                provider = format!("custom-{}", name.to_lowercase().replace(' ', "-"));
            }
            let dialect = match &self.auth {
                ConnectionAuth::Subscription { provider } => match provider.as_ref() {
                    "google-antigravity" => nuo_wire::ProviderDialect::Antigravity,
                    "chatgpt" => nuo_wire::ProviderDialect::ChatGpt,
                    "copilot" => nuo_wire::ProviderDialect::Copilot,
                    "qoder" => nuo_wire::ProviderDialect::Qoder,
                    _ => nuo_wire::ProviderDialect::Standard,
                },
                ConnectionAuth::ApiKey => nuo_wire::ProviderDialect::Standard,
            };
            let protocol = self.protocol.unwrap_or(match dialect {
                nuo_wire::ProviderDialect::Antigravity => WireProtocol::GoogleGemini,
                nuo_wire::ProviderDialect::ChatGpt => WireProtocol::Responses,
                _ => WireProtocol::ChatCompletions,
            });
            let root = self
                .base_url
                .as_deref()
                .unwrap_or("http://localhost:8080/v1");
            let definition = crate::model_providers::UserDeclaredProvider {
                label: Some(name.clone()),
                root_url: crate::model_providers::migrate_endpoint_root(root),
                default_protocol: Some(protocol),
                client_profile: None,
                user_agent: self.user_agent,
                catalog: None,
                dialect: Some(dialect),
                protocol_roots: vec![],
                catalog_root_url: None,
                prompt_cache: None,
                client_profile_sensitive: false,
            };
            if let Some(existing) = store.get_provider(&provider) {
                if existing.root_url != definition.root_url
                    || existing.default_protocol != definition.default_protocol
                    || existing.dialect != definition.dialect
                {
                    return Err(format!(
                        "legacy connection `{name}` conflicts with provider `{provider}`"
                    ));
                }
            } else {
                store.set_provider(&provider, definition);
            }
        }

        let mut models = self.models;
        if models.filter.is_none() {
            models.filter = Some(ConnectionFilterPolicy::Named(NamedFilterPolicy::All));
            if is_known_model_provider(&provider) {
                models.include.clear();
            }
        }
        for extra in self.extra_models {
            if !models.include.iter().any(|m| m.id == extra.id) {
                models.include.push(extra);
            }
        }

        let auth = migrate_connection_auth(&provider, self.auth);
        Ok(Connection {
            name,
            provider,
            auth,
            api_key_env: self.api_key_env,
            client_identity: self.client_identity,
            models,
            catalog_dimensions: self.catalog_dimensions,
        })
    }
}

/// ADR-0268: OpenCode Go is an OpenCode Console account surface, authenticated
/// OpenCode Go connects to the zen/go relay using an API key (`OPENCODE_API_KEY`).
/// Connections mistakenly configured with OpenCode Console OAuth are restored
/// to `ApiKey` on load so they authenticate properly.
fn migrate_connection_auth(provider: &str, auth: ConnectionAuth) -> ConnectionAuth {
    if provider == "opencode-go"
        && matches!(&auth, ConnectionAuth::Subscription { provider } if provider == "opencode")
    {
        tracing::info!(
            provider,
            "restoring opencode-go connection to ApiKey"
        );
        ConnectionAuth::ApiKey
    } else {
        auth
    }
}

fn deserialize_connection_models<'de, D>(deserializer: D) -> Result<ModelScopeConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawModels {
        Scope(ModelScopeConfig),
        List(Vec<String>),
    }

    match Option::<RawModels>::deserialize(deserializer)? {
        Some(RawModels::Scope(scope)) => Ok(scope),
        Some(RawModels::List(list)) => Ok(ModelScopeConfig {
            filter: None,
            include: list
                .into_iter()
                .map(|id| DeclaredModel {
                    id,
                    ..Default::default()
                })
                .collect(),
            exclude: Vec::new(),
            overrides: std::collections::BTreeMap::new(),
        }),
        None => Ok(ModelScopeConfig::default()),
    }
}

impl Connections {
    fn path() -> std::path::PathBuf {
        paths::get().connections_file()
    }

    /// Read the connections store, returning an empty value when missing or
    /// unparseable. Entries that fail migration — an unknown provider id, or no
    /// name at all — are rejected with an error log rather than reinterpreted
    /// as a different connection kind (ADR-0201 INV-2).
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let raw: RawConnections = match toml::from_str(&content) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse connections.toml; ignoring it",
                );
                return Self::default();
            }
        };
        let mut provider_store = match crate::model_providers::ModelProviders::try_load() {
            Ok(store) => store,
            Err(error) => {
                tracing::error!(%error, "cannot migrate connections with invalid providers");
                return Self::default();
            }
        };
        let original_providers = provider_store.clone();
        let mut connections: Vec<Connection> = Vec::with_capacity(raw.connections.len());
        let mut catalog_policy_migrated = false;
        let mut lossless = true;
        for raw in raw.connections {
            catalog_policy_migrated |= raw.needs_catalog_policy_migration();
            match raw.migrate(&mut provider_store) {
                Ok(conn) => {
                    if connections
                        .iter()
                        .any(|existing| existing.name.eq_ignore_ascii_case(&conn.name))
                    {
                        tracing::error!(
                            name = %conn.name,
                            "duplicate connection name in connections.toml; dropping the later entry",
                        );
                        lossless = false;
                        continue;
                    }
                    connections.push(conn);
                }
                Err(reason) => {
                    lossless = false;
                    tracing::error!(
                        path = %path.display(),
                        reason = %reason,
                        "rejecting invalid connection entry",
                    );
                }
            }
        }
        let migrated = Self { connections };
        if !lossless {
            return migrated;
        }
        let providers_changed = original_providers != provider_store;
        if providers_changed {
            if let Err(error) = provider_store.save() {
                tracing::error!(%error, "could not persist provider migration; connections left unchanged");
                return Self::default();
            }
        }
        let canonical = toml::to_string_pretty(&migrated).unwrap_or_default();
        if (providers_changed || catalog_policy_migrated || canonical != content)
            && let Err(error) = migrated.save()
        {
            tracing::warn!(%error, "could not persist connection migration");
        }
        migrated
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
    /// Names are compared case-insensitively (ADR-0201 INV-3).
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
    /// connection — only to fill the rejection message (ADR-0201 INV-3).
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

    struct PathsSandbox {
        _guard: std::sync::MutexGuard<'static, ()>,
        _tmp: tempfile::TempDir,
    }

    impl Drop for PathsSandbox {
        fn drop(&mut self) {
            paths::set_test_default(None);
        }
    }

    fn sandboxed_paths() -> PathsSandbox {
        let guard = paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        paths::set_test_default(Some(paths::Dirs {
            config_dir: tmp.path().join("config"),
            data_dir: tmp.path().join("data"),
            state_dir: tmp.path().join("state"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: None,
        }));
        PathsSandbox {
            _guard: guard,
            _tmp: tmp,
        }
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
    fn legacy_id_and_preset_id_migrate_on_load() {
        let raw: RawConnections = toml::from_str(
            r#"
[[connections]]
id = "ds"
preset_id = "deepseek"

[[connections.extra_models]]
id = "legacy-preview"
context_window = 500000
"#,
        )
        .unwrap();
        let conn = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate(&mut crate::model_providers::ModelProviders::default())
            .unwrap();
        assert_eq!(conn.name, "ds");
        assert_eq!(conn.provider, "deepseek");
        assert_eq!(conn.models.include.len(), 1);
        assert_eq!(conn.models.include[0].id, "legacy-preview");
        assert_eq!(conn.models.include[0].context_window, Some(500000));
        assert_eq!(
            conn.models.filter,
            Some(ConnectionFilterPolicy::Named(NamedFilterPolicy::All))
        );
    }

    #[test]
    fn opencode_go_subscription_connection_migrates_to_api_key() {
        let raw: RawConnections = toml::from_str(
            r#"
[[connections]]
name = "opencode-go"
provider = "opencode-go"
auth = { type = "subscription", provider = "opencode" }
"#,
        )
        .unwrap();
        let conn = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate(&mut crate::model_providers::ModelProviders::default())
            .unwrap();
        assert_eq!(
            conn.auth,
            ConnectionAuth::ApiKey,
            "opencode-go must load as an ApiKey connection"
        );
    }

    #[test]
    fn other_api_key_connections_are_not_reauthed() {
        let raw: RawConnections = toml::from_str(
            r#"
[[connections]]
name = "openai"
provider = "openai"
auth = "api-key"
"#,
        )
        .unwrap();
        let conn = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate(&mut crate::model_providers::ModelProviders::default())
            .unwrap();
        assert_eq!(conn.auth, ConnectionAuth::ApiKey);
    }

    #[test]
    fn legacy_curated_snapshot_is_not_promoted_to_user_injections() {
        let raw: RawConnections = toml::from_str(
            r#"
[[connections]]
name = "ds"
provider = "deepseek"
models = ["deepseek-v4-flash", "deepseek-v4-flash-0731"]

[[connections.extra_models]]
id = "deepseek-v4-preview"
context_window = 500000
"#,
        )
        .unwrap();
        let conn = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate(&mut crate::model_providers::ModelProviders::default())
            .unwrap();

        assert_eq!(
            conn.models.filter,
            Some(ConnectionFilterPolicy::Named(NamedFilterPolicy::All))
        );
        assert_eq!(conn.models.include.len(), 1);
        assert_eq!(conn.models.include[0].id, "deepseek-v4-preview");
    }

    #[test]
    fn explicit_filter_preserves_sovereign_injections() {
        let raw: RawConnections = toml::from_str(
            r#"
[[connections]]
name = "ds"
provider = "deepseek"

[connections.models]
filter = "all"

[[connections.models.inject]]
id = "deepseek-v4-preview"
"#,
        )
        .unwrap();
        let conn = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate(&mut crate::model_providers::ModelProviders::default())
            .unwrap();

        assert_eq!(conn.models.include.len(), 1);
        assert_eq!(conn.models.include[0].id, "deepseek-v4-preview");
    }

    #[test]
    fn load_persists_catalog_policy_migration() {
        let _sandbox = sandboxed_paths();
        std::fs::create_dir_all(paths::get().state_dir.clone()).unwrap();
        std::fs::write(
            Connections::path(),
            r#"
[[connections]]
name = "ds"
provider = "deepseek"

[[connections.models.inject]]
id = "deepseek-v4-flash-0731"
"#,
        )
        .unwrap();

        let loaded = Connections::load();
        let connection = loaded.get("ds").unwrap();
        assert_eq!(
            connection.models.filter,
            Some(ConnectionFilterPolicy::Named(NamedFilterPolicy::All))
        );
        assert!(connection.models.include.is_empty());

        let canonical = std::fs::read_to_string(Connections::path()).unwrap();
        assert!(canonical.contains("filter = \"all\""));
        assert!(!canonical.contains("deepseek-v4-flash-0731"));
    }

    #[test]
    fn legacy_pure_custom_connection_becomes_the_custom_provider() {
        let raw: RawConnections = toml::from_str(
            r#"
[[connections]]
id = "custom-ollama"
models = ["llama3:latest", "mistral:latest"]
"#,
        )
        .unwrap();
        let conn = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate(&mut crate::model_providers::ModelProviders::default())
            .unwrap();
        assert_eq!(conn.name, "custom-ollama");
        assert_eq!(conn.provider, "custom-ollama");
        assert_eq!(conn.models.include.len(), 2);
        assert_eq!(conn.models.include[0].id, "llama3:latest");
    }

    #[test]
    fn legacy_provider_ids_are_canonicalized() {
        for (legacy, canonical) in [
            ("chatgpt-oauth", "openai-subscription"),
            ("antigravity-oauth", "google-antigravity"),
            ("copilot-oauth", "github-copilot"),
            ("xai-oauth", "xai"),
            ("zai-code", "glm-cn"),
            ("custom-openai", "custom-c"),
        ] {
            let raw: RawConnections = toml::from_str(&format!(
                "[[connections]]\nname = \"c\"\npreset_id = \"{legacy}\"\n"
            ))
            .unwrap();
            let conn = raw
                .connections
                .into_iter()
                .next()
                .unwrap()
                .migrate(&mut crate::model_providers::ModelProviders::default())
                .unwrap();
            assert_eq!(conn.provider, canonical, "{legacy} → {canonical}");
        }
    }

    #[test]
    fn unknown_provider_is_rejected_not_reinterpreted() {
        let raw: RawConnections =
            toml::from_str("[[connections]]\nname = \"c\"\nprovider = \"nope\"\n").unwrap();
        let err = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate(&mut crate::model_providers::ModelProviders::default())
            .unwrap_err();
        assert!(err.contains("unknown model provider 'nope'"), "{err}");
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
}
