//! Authentication and credential resolution for Tencent AI CodeBuddy.

use nuo_host::SecretString;
use nuo_provider::auth::{BoxFuture, CredentialSource, ResolvedAuth};
use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Deserialize, Debug)]
struct StoredAuthFile {
    auth: Option<StoredAuth>,
    account: Option<StoredAccount>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct StoredAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<u64>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct StoredAccount {
    uid: Option<String>,
    nickname: Option<String>,
    uin: Option<String>,
}

/// Dynamic or static credential source for Tencent AI CodeBuddy.
///
/// Discovers credentials in the following order:
/// 1. An explicit API key / access token provided in configuration.
/// 2. `CODEBUDDY_API_KEY`, `TENCENT_COPILOT_API_KEY`, or `TENCENT_API_KEY` environment variables.
/// 3. Standard CodeBuddy CLI authentication token stored on disk:
///    - Linux: `~/.local/share/CodeBuddyExtension/Data/Public/auth/Tencent-Cloud.coding-copilot.info`
///    - macOS: `~/Library/Application Support/CodeBuddyExtension/Data/Public/auth/Tencent-Cloud.coding-copilot.info`
///    - Windows: `%LOCALAPPDATA%/CodeBuddyExtension/Data/Public/auth/Tencent-Cloud.coding-copilot.info`
///    - Override via `CODEBUDDY_AUTH_PATH`
pub struct CodeBuddyCredentialSource {
    pub connection_name: String,
    pub api_key: SecretString,
    cached: Mutex<Option<ResolvedAuth>>,
}

impl fmt::Debug for CodeBuddyCredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodeBuddyCredentialSource")
            .field("connection_name", &self.connection_name)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

impl CodeBuddyCredentialSource {
    pub fn new(connection_name: impl Into<String>, api_key: SecretString) -> Self {
        Self {
            connection_name: connection_name.into(),
            api_key,
            cached: Mutex::new(None),
        }
    }

    /// Search candidate paths on disk for CodeBuddy's stored token.
    pub fn candidate_auth_file_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        if let Ok(custom) = std::env::var("CODEBUDDY_AUTH_PATH") {
            let path = PathBuf::from(custom.trim());
            if !path.as_os_str().is_empty() {
                paths.push(path);
            }
        }

        // Linux / XDG
        if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
            paths.push(
                PathBuf::from(xdg_data)
                    .join("CodeBuddyExtension")
                    .join("Data")
                    .join("Public")
                    .join("auth")
                    .join("Tencent-Cloud.coding-copilot.info"),
            );
        }

        if let Ok(home) = std::env::var("HOME") {
            let home_path = PathBuf::from(home);
            // Linux default
            paths.push(
                home_path
                    .join(".local")
                    .join("share")
                    .join("CodeBuddyExtension")
                    .join("Data")
                    .join("Public")
                    .join("auth")
                    .join("Tencent-Cloud.coding-copilot.info"),
            );
            // macOS default
            paths.push(
                home_path
                    .join("Library")
                    .join("Application Support")
                    .join("CodeBuddyExtension")
                    .join("Data")
                    .join("Public")
                    .join("auth")
                    .join("Tencent-Cloud.coding-copilot.info"),
            );
        }

        // Windows default
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            paths.push(
                PathBuf::from(local_app_data)
                    .join("CodeBuddyExtension")
                    .join("Data")
                    .join("Public")
                    .join("auth")
                    .join("Tencent-Cloud.coding-copilot.info"),
            );
        }

        paths
    }

    /// Read and parse saved credentials from local disk if available.
    pub fn read_stored_credentials() -> Option<(String, Option<String>)> {
        for path in Self::candidate_auth_file_paths() {
            if path.exists() && path.is_file() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(parsed) = serde_json::from_str::<StoredAuthFile>(&content) {
                        if let Some(auth) = parsed.auth {
                            if let Some(token) = auth.access_token {
                                let trimmed = token.trim();
                                if !trimmed.is_empty() {
                                    let nickname = parsed
                                        .account
                                        .as_ref()
                                        .and_then(|a| a.nickname.clone())
                                        .or_else(|| {
                                            parsed.account.as_ref().and_then(|a| a.uid.clone())
                                        });
                                    return Some((trimmed.to_string(), nickname));
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

impl CredentialSource for CodeBuddyCredentialSource {
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            // 1. Explicit connection API key
            let static_key = self.api_key.expose_secret().trim();
            if !static_key.is_empty() {
                return Ok(ResolvedAuth::new(self.api_key.clone()));
            }

            // 2. Environment variables
            for env_var in [
                "CODEBUDDY_API_KEY",
                "TENCENT_COPILOT_API_KEY",
                "TENCENT_API_KEY",
            ] {
                if let Ok(val) = std::env::var(env_var) {
                    let trimmed = val.trim();
                    if !trimmed.is_empty() {
                        return Ok(ResolvedAuth::new(trimmed));
                    }
                }
            }

            // 3. Cached token
            if let Ok(guard) = self.cached.lock() {
                if let Some(cached) = guard.as_ref() {
                    return Ok(cached.clone());
                }
            }

            // 4. Stored local credentials
            if let Some((token, nickname)) = Self::read_stored_credentials() {
                let mut auth = ResolvedAuth::new(token);
                if let Some(user) = nickname {
                    auth = auth.with_user_email(user);
                }
                if let Ok(mut guard) = self.cached.lock() {
                    *guard = Some(auth.clone());
                }
                return Ok(auth);
            }

            Err(
                "No Tencent CodeBuddy credentials found. Please set `CODEBUDDY_API_KEY` or sign in via `codebuddy` CLI."
                    .to_string(),
            )
        })
    }

    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            if let Ok(mut guard) = self.cached.lock() {
                *guard = None;
            }
            self.resolve_auth().await
        })
    }

    fn is_ready(&self) -> bool {
        let static_key = self.api_key.expose_secret().trim();
        if !static_key.is_empty() {
            return true;
        }
        for env_var in [
            "CODEBUDDY_API_KEY",
            "TENCENT_COPILOT_API_KEY",
            "TENCENT_API_KEY",
        ] {
            if let Ok(val) = std::env::var(env_var) {
                if !val.trim().is_empty() {
                    return true;
                }
            }
        }
        Self::read_stored_credentials().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_key_resolves_immediately() {
        let source = CodeBuddyCredentialSource::new("test", SecretString::from("test-key-123"));
        assert!(source.is_ready());
        let auth = source.resolve_auth().await.expect("resolves auth");
        assert_eq!(auth.token.expose_secret(), "test-key-123");
    }

    #[test]
    fn parse_stored_credentials_format() {
        let sample_json = r#"{
            "account": {
                "uid": "test-uid-123",
                "nickname": "test-user"
            },
            "auth": {
                "accessToken": "eyTestAccessToken123",
                "refreshToken": "eyTestRefreshToken456",
                "tokenType": "Bearer",
                "expiresIn": 7200
            }
        }"#;

        let parsed: StoredAuthFile =
            serde_json::from_str(sample_json).expect("parses stored auth file");
        let auth = parsed.auth.expect("has auth");
        assert_eq!(auth.access_token.as_deref(), Some("eyTestAccessToken123"));
        let account = parsed.account.expect("has account");
        assert_eq!(account.nickname.as_deref(), Some("test-user"));
        assert_eq!(account.uid.as_deref(), Some("test-uid-123"));
    }

    #[tokio::test]
    async fn auto_discovery_from_stored_file_or_env() {
        let source = CodeBuddyCredentialSource::new("test", SecretString::from(""));
        // If credentials exist on this system (as we know they do in ~/.local/share/CodeBuddyExtension),
        // resolve_auth must successfully extract the token.
        if CodeBuddyCredentialSource::read_stored_credentials().is_some() {
            assert!(source.is_ready());
            let auth = source
                .resolve_auth()
                .await
                .expect("resolves auth from disk");
            assert!(!auth.token.expose_secret().is_empty());
        }
    }
}
