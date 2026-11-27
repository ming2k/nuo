//! Secret credentials storage, API keys, and web credentials resolution.
use nuo_host::fsutil;
use nuo_host::paths;
use nuo_protocol::{SecretString, WebConfig, WebProviderAxis};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Provider API keys split out of `config.toml` into their own
/// `credentials.toml` (written `rw-------` via [`nuo_host::fsutil`]). This is the
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
}

/// Web credentials keyed by compiled web provider id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebCredentials {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub search: BTreeMap<String, SecretString>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reader: BTreeMap<String, SecretString>,
}

impl WebCredentials {
    pub fn is_empty(&self) -> bool {
        self.search.is_empty() && self.reader.is_empty()
    }
}

impl Credentials {
    fn path() -> PathBuf {
        paths::get().credentials_file()
    }

    /// Read `credentials.toml` from `$XDG_STATE_HOME/nuo/credentials.toml` (ADR-0032).
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<Self>(&content) {
            Ok(credentials) => credentials,
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
    /// [`nuo_host::fsutil::atomic_write_bytes`].
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
