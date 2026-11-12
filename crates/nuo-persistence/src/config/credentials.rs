//! Secret credentials storage, API keys, and web credentials resolution.
use crate::fsutil;
use crate::paths;
use nuo_protocol::{SecretString, WebConfig, WebProviderAxis};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Provider API keys split out of `config.toml` into their own
/// `credentials.toml` (written `rw-------` via [`crate::fsutil`]). This is the
/// **secret** half of provider configuration: `config.toml` holds the
/// *behavior*, `providers.toml` the instance *declarations*, and this file the
/// *keys* — so the other two files can be shared, screenshotted, or
/// version-controlled without leaking credentials.
///
/// Credentials are keyed by **provider instance**, never by route — a route
/// is a derived model path, not a security master. One instance has
/// exactly one API-key credential:
///
/// ```toml
/// [providers]
/// deepseek = "sk-..."
/// ```
///
/// OAuth logins do **not** live here; their access/refresh token sets are
/// runtime state in `auth.toml` (`[tokens.<connection-id>]`), also keyed by
/// exact connection id.
///
/// Resolution precedence is **`api_key_env` env var > credentials.toml**: the
/// instance declares an optional `api_key_env` (a variable *name*) in
/// `providers.toml`; the catalog resolves env-first, then this file. The map
/// is a `BTreeMap` for stable, diff-friendly serialisation. Unknown tables
/// (e.g. the pre-refactor `[builtins.<id>]` / `[user.<id>]` sections) are
/// tolerated and ignored so a not-yet-migrated file keeps loading.
/// Credentials split out of `config.toml` into their own `credentials.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    /// API keys keyed by connection id.
    #[serde(default)]
    pub connections: BTreeMap<String, SecretString>,
    /// Credentials keyed by compiled web provider id, not connection id.
    #[serde(default, skip_serializing_if = "WebCredentials::is_empty")]
    pub web: WebCredentials,
    /// One-shot migration markers. These prevent preserved legacy secrets from
    /// being re-imported after a user deliberately clears their new value.
    #[serde(default, skip_serializing_if = "CredentialMigrations::is_empty")]
    pub migrations: CredentialMigrations,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CredentialMigrations {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub web_connections_v1: bool,
}

impl CredentialMigrations {
    fn is_empty(&self) -> bool {
        !self.web_connections_v1
    }
}

/// Web credentials. Flat fields are accepted only as legacy migration input;
/// serialization emits the search and reader maps.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebCredentials {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub search: BTreeMap<String, SecretString>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reader: BTreeMap<String, SecretString>,
    #[serde(default, skip_serializing)]
    pub exa_api_key: Option<SecretString>,
    #[serde(default, skip_serializing)]
    pub parallel_api_key: Option<SecretString>,
    #[serde(default, skip_serializing)]
    pub tavily_api_key: Option<SecretString>,
    #[serde(default, skip_serializing)]
    pub bocha_api_key: Option<SecretString>,
    #[serde(default, skip_serializing)]
    pub jina_api_key: Option<SecretString>,
}

impl WebCredentials {
    pub fn is_empty(&self) -> bool {
        self.search.is_empty()
            && self.reader.is_empty()
            && self.exa_api_key.is_none()
            && self.parallel_api_key.is_none()
            && self.tavily_api_key.is_none()
            && self.bocha_api_key.is_none()
            && self.jina_api_key.is_none()
    }

    fn normalize_legacy(&mut self) -> bool {
        let mut changed = false;
        for (id, secret) in [
            ("exa", self.exa_api_key.take()),
            ("parallel", self.parallel_api_key.take()),
            ("tavily", self.tavily_api_key.take()),
            ("bocha", self.bocha_api_key.take()),
        ] {
            if let Some(secret) = secret {
                self.search.entry(id.to_string()).or_insert(secret);
                changed = true;
            }
        }
        if let Some(secret) = self.jina_api_key.take() {
            self.reader.entry("jina".to_string()).or_insert(secret);
            changed = true;
        }
        changed
    }
}

impl Credentials {
    fn path() -> PathBuf {
        paths::get().credentials_file()
    }

    /// Read `credentials.toml` from `$XDG_STATE_HOME/nuo/credentials.toml` (ADR-0032).
    /// If absent, automatically promotes any legacy file from `$XDG_CONFIG_HOME/nuo/credentials.toml`.
    pub fn load() -> Self {
        let path = Self::path();
        let content = if let Ok(c) = fs::read_to_string(&path) {
            c
        } else {
            // Check and auto-promote legacy credentials in config_dir
            let legacy_path = paths::get().legacy_credentials_file();
            if let Ok(c) = fs::read_to_string(&legacy_path) {
                tracing::info!(
                    legacy = %legacy_path.display(),
                    target = %path.display(),
                    "promoting legacy credentials.toml from config directory into state directory (ADR-0032)"
                );
                // Save immediately into modern path
                let _ = fsutil::atomic_write_bytes(&path, c.as_bytes());
                // Remove or clean legacy file to prevent dotfiles accidental git leaks
                let _ = fs::remove_file(&legacy_path);
                c
            } else {
                return Self::default();
            }
        };
        match toml::from_str::<Self>(&content) {
            Ok(mut credentials) => {
                let mut changed = credentials.web.normalize_legacy();
                if !credentials.migrations.web_connections_v1
                    && crate::web_migration::migrate_connection_credentials(&mut credentials)
                        .is_some()
                {
                    credentials.migrations.web_connections_v1 = true;
                    changed = true;
                }
                if changed && let Err(error) = credentials.save() {
                    tracing::warn!(%error, "could not persist migrated web credentials");
                }
                credentials
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse credentials file; ignoring",
                );
                Self::default()
            }
        }
    }

    /// Persist atomically with owner-only permissions (0600) via
    /// [`crate::fsutil::atomic_write_bytes`].
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// The credential for `connection_id`, if set and non-empty.
    pub fn api_key(&self, connection_id: &str) -> Option<&SecretString> {
        self.connections
            .get(connection_id)
            .filter(|k| !k.expose_secret().trim().is_empty())
    }

    pub fn web_credential(
        &self,
        axis: WebProviderAxis,
        provider_id: &str,
    ) -> Option<&SecretString> {
        let credentials = match axis {
            WebProviderAxis::Search => &self.web.search,
            WebProviderAxis::Reader => &self.web.reader,
        };
        credentials
            .get(provider_id)
            .filter(|secret| !secret.expose_secret().trim().is_empty())
    }

    pub fn set_web_credential(
        &mut self,
        axis: WebProviderAxis,
        provider_id: &str,
        secret: Option<SecretString>,
    ) {
        let credentials = match axis {
            WebProviderAxis::Search => &mut self.web.search,
            WebProviderAxis::Reader => &mut self.web.reader,
        };
        match secret {
            Some(secret) if !secret.expose_secret().trim().is_empty() => {
                credentials.insert(provider_id.to_string(), secret);
            }
            _ => {
                credentials.remove(provider_id);
            }
        }
    }

    /// Set (or clear) the credential for `connection_id`.
    pub fn set_api_key(&mut self, connection_id: &str, key: Option<SecretString>) {
        match key {
            Some(key) if !key.expose_secret().trim().is_empty() => {
                self.connections.insert(connection_id.to_string(), key);
            }
            _ => {
                self.connections.remove(connection_id);
            }
        }
    }

    /// Remove the credential for `connection_id`, if any.
    pub fn remove_api_key(&mut self, connection_id: &str) {
        self.connections.remove(connection_id);
    }
}

pub struct ResolvedWebConfig {
    pub runtime: nuo_protocol::WebRuntimeConfig,
    pub search_credential: nuo_protocol::WebCredentialStatus,
    pub reader_credential: nuo_protocol::WebCredentialStatus,
}

pub fn resolve_web_config(config: &WebConfig, credentials: &Credentials) -> ResolvedWebConfig {
    use nuo_protocol::{WebCredentialRequirement as Requirement, WebCredentialStatus as Status};

    fn resolve(
        axis: WebProviderAxis,
        provider_id: &str,
        requirement: Requirement,
        env_name: Option<&str>,
        credentials: &Credentials,
    ) -> (Option<SecretString>, Status) {
        if requirement == Requirement::None {
            return (None, Status::NotRequired);
        }
        if let Some(env_name) = env_name
            && let Ok(value) = std::env::var(env_name)
            && !value.trim().is_empty()
        {
            return (Some(SecretString::new(value)), Status::Environment);
        }
        if let Some(secret) = credentials.web_credential(axis, provider_id) {
            return (Some(secret.clone()), Status::Stored);
        }
        let status = match requirement {
            Requirement::Required => Status::RequiredMissing,
            Requirement::Optional => Status::OptionalMissing,
            Requirement::None => Status::NotRequired,
        };
        (None, status)
    }

    let search = config.provider.capability();
    let reader = config.reader.capability();
    let (search_credential, search_status) = search
        .as_ref()
        .map(|capability| {
            resolve(
                WebProviderAxis::Search,
                &capability.id,
                capability.credential,
                capability.default_env_var.as_deref(),
                credentials,
            )
        })
        .unwrap_or((None, Status::NotRequired));
    let (reader_credential, reader_status) = reader
        .as_ref()
        .map(|capability| {
            resolve(
                WebProviderAxis::Reader,
                &capability.id,
                capability.credential,
                capability.default_env_var.as_deref(),
                credentials,
            )
        })
        .unwrap_or((None, Status::NotRequired));

    ResolvedWebConfig {
        runtime: nuo_protocol::WebRuntimeConfig {
            behavior: config.clone(),
            search_credential,
            reader_credential,
        },
        search_credential: search_status,
        reader_credential: reader_status,
    }
}
