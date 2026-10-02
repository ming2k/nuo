//! User-declared model provider customizations (`model_providers.toml`,
//! ADR-0199, ADR-0201).
//!
//! Stores provider-level model scoping and capability customizations keyed by
//! model provider id (e.g. `deepseek`, `anthropic`, `openai`). Applied across
//! every connection that points at that provider — model semantics belong to
//! the provider, not to a credential binding.

use nuo_contracts::model::ModelScopeConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::fsutil;
use crate::paths;

/// A first-class user-declared model provider surface (ADR-0258).
///
/// Declares the physical transport endpoint, default protocol, catalog discovery,
/// dialect, and client identity preset for a custom LLM service surface (e.g.
/// corporate relay, local vLLM, or self-hosted gateway).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserDeclaredProvider {
    /// Optional human-readable display label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Root API URL (e.g. `https://relay.example.com/v1`, without trailing slash).
    pub root_url: String,
    /// Default wire transport protocol (e.g. `chat-completions`, `responses`, `anthropic-messages`, `google-gemini`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_protocol: Option<nuo_contracts::WireProtocol>,
    /// Default client profile preset for User-Agent / client headers emulation (ADR-0164, ADR-0258).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_profile: Option<nuo_contracts::ClientPreset>,
    /// Optional User-Agent override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Catalog discovery format: `openai`, `anthropic`, `google`, `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<nuo_contracts::RemoteCatalogSource>,
    /// Typed service dialect, inherited independently of model protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<nuo_contracts::ProviderDialect>,
    /// Optional explicit transport endpoints, keyed by model wire protocol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_roots: Vec<(nuo_contracts::WireProtocol, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_root_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<nuo_contracts::provider_surface::ProviderPromptCache>,
    #[serde(default)]
    pub client_profile_sensitive: bool,
}

/// Root map of user-configured model providers in `model_providers.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviders {
    #[serde(default = "schema_version")]
    pub version: u32,
    /// Declarative user-defined provider services (ADR-0258).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, UserDeclaredProvider>,
    /// Scoped model inclusions, exclusions, and capability overrides per provider (ADR-0199, ADR-0201).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_providers: BTreeMap<String, ModelScopeConfig>,
}

fn schema_version() -> u32 {
    1
}

impl Default for ModelProviders {
    fn default() -> Self {
        Self {
            version: schema_version(),
            providers: BTreeMap::new(),
            model_providers: BTreeMap::new(),
        }
    }
}

impl ModelProviders {
    fn path() -> std::path::PathBuf {
        paths::get().model_providers_file()
    }

    pub fn load() -> Self {
        Self::try_load().unwrap_or_else(|error| {
            tracing::warn!(%error, "could not load model_providers.toml");
            Self::default()
        })
    }

    pub fn try_load() -> Result<Self, String> {
        let content = match std::fs::read_to_string(Self::path()) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.to_string()),
        };
        Self::parse(&content)
    }

    /// Decode old fields only at the persistence boundary; runtime sees one schema.
    pub fn parse(content: &str) -> Result<Self, String> {
        let mut value: toml::Value = toml::from_str(content).map_err(|e| e.to_string())?;
        if value.get("version").is_none() {
            if let Some(providers) = value
                .get_mut("providers")
                .and_then(toml::Value::as_table_mut)
            {
                for (_, provider) in providers.iter_mut() {
                    let table = provider.as_table_mut().ok_or("provider must be a table")?;
                    if let Some(format) = table.remove("catalog_format") {
                        let format = format.as_str().ok_or("catalog_format must be a string")?;
                        let catalog = if matches!(format, "none" | "static") {
                            nuo_contracts::RemoteCatalogSource::None
                        } else {
                            nuo_contracts::RemoteCatalogSource::Endpoint(
                                serde_json::from_value(serde_json::Value::String(format.into()))
                                    .map_err(|e| e.to_string())?,
                            )
                        };
                        table.insert(
                            "catalog".into(),
                            toml::Value::try_from(catalog).map_err(|e| e.to_string())?,
                        );
                    }
                    if let Some(root) = table.get("root_url").and_then(toml::Value::as_str) {
                        let root = migrate_endpoint_root(root);
                        table.insert("root_url".into(), toml::Value::String(root));
                    }
                }
            }
            value
                .as_table_mut()
                .ok_or("provider store must be a table")?
                .insert("version".into(), toml::Value::Integer(1));
        }
        let parsed: Self = value.try_into().map_err(|e| e.to_string())?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != schema_version() {
            return Err(format!(
                "unsupported provider schema version {}",
                self.version
            ));
        }
        for (id, provider) in &self.providers {
            if id.is_empty() || id.trim() != id {
                return Err("provider id must be nonempty and trimmed".into());
            }
            if nuo_contracts::model_providers::is_known_model_provider(id) {
                return Err(format!("provider `{id}` collides with a built-in provider"));
            }
            nuo_contracts::ApiRoot::parse(&provider.root_url)?;
            if let Some(cache) = &provider.prompt_cache {
                cache.validate()?;
            }
            if let Some(root) = &provider.catalog_root_url {
                nuo_contracts::ApiRoot::parse(root)?;
            }
            let mut wires = std::collections::HashSet::new();
            for (wire, root) in &provider.protocol_roots {
                if !wires.insert(*wire) {
                    return Err(format!("duplicate protocol root for {wire}"));
                }
                nuo_contracts::ApiRoot::parse(root)?;
            }
            if !provider.dialect.unwrap_or_default().supports(
                provider
                    .default_protocol
                    .unwrap_or(nuo_contracts::WireProtocol::ChatCompletions),
            ) {
                return Err(format!(
                    "provider `{id}` has an incompatible default protocol"
                ));
            }
        }
        Ok(())
    }

    /// Persist atomically.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.validate()?;
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// Look up a user-declared provider definition by id.
    pub fn get_provider(&self, provider_id: &str) -> Option<&UserDeclaredProvider> {
        self.providers.get(provider_id)
    }

    /// Set or update a user-declared provider definition.
    pub fn set_provider(&mut self, provider_id: impl Into<String>, provider: UserDeclaredProvider) {
        self.providers.insert(provider_id.into(), provider);
    }

    /// Remove a user-declared provider definition.
    pub fn remove_provider(&mut self, provider_id: &str) -> Option<UserDeclaredProvider> {
        self.providers.remove(provider_id)
    }

    /// Get the scope config for a model provider.
    pub fn get(&self, provider_id: &str) -> Option<&ModelScopeConfig> {
        self.model_providers.get(provider_id)
    }

    /// Get mutable scope config for a model provider, creating it if absent.
    pub fn get_or_create_mut(&mut self, provider_id: &str) -> &mut ModelScopeConfig {
        self.model_providers
            .entry(provider_id.to_string())
            .or_default()
    }
}

/// One-time conversion for pre-root provider and connection records only.
/// Versioned provider roots are never inferred from their final path segment.
pub(crate) fn migrate_endpoint_root(endpoint: &str) -> String {
    let endpoint = endpoint.trim().trim_end_matches('/');
    for suffix in [
        "/chat/completions",
        "/responses",
        "/messages",
        "/v1internal",
    ] {
        if let Some(root) = endpoint.strip_suffix(suffix) {
            return root.to_string();
        }
    }
    endpoint.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_contracts::model::DeclaredModel;

    #[test]
    fn user_declared_provider_dialects_are_validated_and_preserve_endpoint_overrides() {
        for name in [
            "standard",
            "antigravity",
            "chat-gpt",
            "copilot",
            "deepseek",
            "openrouter",
            "qoder",
        ] {
            let config = format!(
                r#"
                root_url = "https://relay.example/team"
                dialect = "{name}"
                protocol_roots = [["responses", "https://relay.example/team/responses"]]
            "#
            );
            let provider: UserDeclaredProvider = toml::from_str(&config).unwrap();
            assert_eq!(provider.dialect, Some(name.parse().unwrap()));
            let encoded = toml::to_string(&provider).unwrap();
            assert_eq!(provider, toml::from_str(&encoded).unwrap());
        }
        assert!(
            toml::from_str::<UserDeclaredProvider>(
                r#"
            root_url = "https://relay.example"
            dialect = "antigravty"
        "#
            )
            .is_err()
        );
    }

    #[test]
    fn model_providers_roundtrip_toml() {
        let mut providers = ModelProviders::default();
        let config = providers.get_or_create_mut("deepseek");
        config.include.push(DeclaredModel {
            id: "deepseek-v4-preview".to_string(),
            context_window: Some(1_000_000),
            ..DeclaredModel::default()
        });
        config.exclude.push("deepseek-chat-deprecated".to_string());

        let text = toml::to_string_pretty(&providers).unwrap();
        assert!(text.contains("[model_providers.deepseek]"));
        let parsed: ModelProviders = toml::from_str(&text).unwrap();
        assert_eq!(providers, parsed);
        let deepseek = parsed.get("deepseek").unwrap();
        assert_eq!(deepseek.include.len(), 1);
        assert_eq!(deepseek.include[0].id, "deepseek-v4-preview");
        assert_eq!(deepseek.exclude, vec!["deepseek-chat-deprecated"]);
    }

    #[test]
    fn user_declared_provider_roundtrip_toml() {
        let mut store = ModelProviders::default();
        store.set_provider(
            "corp-relay",
            UserDeclaredProvider {
                label: Some("Corporate Relay".to_string()),
                root_url: "https://relay.corp.example/v1".to_string(),
                default_protocol: Some(nuo_contracts::WireProtocol::ChatCompletions),
                client_profile: Some(nuo_contracts::ClientPreset::Cursor),
                user_agent: None,
                catalog: Some(nuo_contracts::RemoteCatalogSource::Endpoint(
                    nuo_contracts::CatalogShape::OpenAi,
                )),
                dialect: Some(nuo_contracts::ProviderDialect::DeepSeek),
                protocol_roots: vec![],
                catalog_root_url: None,
                prompt_cache: None,
                client_profile_sensitive: false,
            },
        );

        let text = toml::to_string_pretty(&store).unwrap();
        assert!(text.contains("[providers.corp-relay]"));
        assert!(text.contains("root_url = \"https://relay.corp.example/v1\""));
        assert!(text.contains("client_profile = \"cursor\""));

        let parsed: ModelProviders = toml::from_str(&text).unwrap();
        assert_eq!(store, parsed);
        let prov = parsed.get_provider("corp-relay").unwrap();
        assert_eq!(prov.root_url, "https://relay.corp.example/v1");
        assert_eq!(
            prov.default_protocol,
            Some(nuo_contracts::WireProtocol::ChatCompletions)
        );
        assert_eq!(
            prov.client_profile,
            Some(nuo_contracts::ClientPreset::Cursor)
        );
        assert_eq!(
            prov.dialect,
            Some(nuo_contracts::ProviderDialect::DeepSeek)
        );
    }
}
