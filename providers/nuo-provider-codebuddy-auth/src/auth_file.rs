//! The CodeBuddy CLI's stored credential file and the realm gate.
//!
//! Credentials saved by the CodeBuddy CLI land in one canonical file per OS:
//! `Tencent-Cloud.coding-copilot.info` under `CodeBuddyExtension/Data/Public/auth/`
//! (Linux `~/.local/share`, macOS `~/Library/Application Support`, Windows
//! `%LOCALAPPDATA%`; override with `CODEBUDDY_AUTH_PATH`). The payload carries
//! `auth.{accessToken, refreshToken, tokenType, …}` and `auth.domain`, plus
//! `account.{uid, nickname, uin}`.
//!
//! This crate owns the *parsing* and the *typed projection*
//! ([`CodeBuddyCredentialKind`] → wire placement) and surfaces the minting
//! realm ([`StoredCredentials::domain`]). Whether a credential is acceptable
//! for a given channel is enforced by the channel crate's realm gate
//! ([`realm_gate`]) — [INV-PROV-REALM-01].

use nuo_provider::WirePlacement;
use nuo_provider::auth::{BoxFuture, CredentialSource, ResolvedAuth};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Mutex;

/// The CodeBuddy inference surface distinguishes two credential lanes at the
/// wire level, mirroring the upstream client's `auth.tokenType`:
///
/// - `Bearer` — the CLI login's OIDC access token (`Authorization: Bearer`),
///   what the normal subscription lane yields.
/// - `ApiKey` — a pasted platform key (`X-API-Key`), what the BYOK lane yields.
///
/// This is resolved from the stored CLI file's `auth.tokenType` when the
/// credential comes from there, and defaults to `ApiKey` for anything the
/// user pasted explicitly (matching the CLI's own precedence — explicit API
/// keys annotate themselves but never look like bearer JWTs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodeBuddyCredentialKind {
    /// OIDC bearer from the CLI's account login (`Authorization: Bearer`).
    Bearer,
    /// Pasted platform key (`X-API-Key`).
    #[default]
    ApiKey,
}

impl CodeBuddyCredentialKind {
    /// Map an upstream `auth.tokenType` string onto the lane.
    pub fn from_token_type(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase) {
            Some(v) if v == "bearer" => Self::Bearer,
            Some(v) if v == "apikey" || v == "api_key" || v == "api-key" => Self::ApiKey,
            // The real stored file carries `tokenType: "Bearer"` on CLI logins
            // and the type is absent on ancient formats; treat absence as
            // `ApiKey` (a pasted key), matching the CLI's own behavior.
            _ => Self::ApiKey,
        }
    }

    /// The outgoing header shape this credential kind rides.
    pub fn wire_placement(self) -> WirePlacement {
        match self {
            Self::Bearer => WirePlacement::Bearer,
            Self::ApiKey => WirePlacement::CustomHeader {
                header: std::borrow::Cow::Borrowed("X-API-Key"),
            },
        }
    }
}

#[derive(Deserialize, Debug)]
struct StoredAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "expiresAt")]
    expires_at: Option<u64>,
    /// `Bearer` for CLI logins, `ApiKey` for pasted platform keys.
    #[serde(rename = "tokenType")]
    token_type: Option<String>,
    /// The account line that minted the credential (`www.codebuddy.cn`,
    /// `codebuddy.ai`, …). Empty/absent on ancient token formats.
    #[serde(default)]
    domain: Option<String>,
}

#[derive(Deserialize, Debug)]
struct StoredAuthFile {
    auth: Option<StoredAuth>,
    #[allow(dead_code)]
    account: Option<StoredAccount>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct StoredAccount {
    uid: Option<String>,
    nickname: Option<String>,
    uin: Option<String>,
}

/// A CodeBuddy credential resolved from any lane, with its minting realm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredentials {
    /// The access token (JWT for `Bearer` logins, platform key otherwise).
    pub token: String,
    /// The credential kind deciding the outgoing header shape.
    pub kind: CodeBuddyCredentialKind,
    /// The account display name (nickname, falling back to uid), if recorded.
    pub nickname: Option<String>,
    /// The vendor-declared account line (`auth.domain`), if recorded.
    pub domain: Option<String>,
}

/// Read and parse saved credentials from local disk if available.
///
/// Returns the first readable candidate's token, kind, display name, and
/// minting realm.
pub fn read_stored_credentials() -> Option<StoredCredentials> {
    for path in candidate_auth_file_paths() {
        if path.exists() && path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Some(parsed) = parse_stored(&content) {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

fn parse_stored(content: &str) -> Option<StoredCredentials> {
    let parsed: StoredAuthFile = serde_json::from_str(content).ok()?;
    let auth = parsed.auth?;
    let token = auth.access_token.as_deref()?.trim().to_string();
    if token.is_empty() {
        return None;
    }
    let kind = CodeBuddyCredentialKind::from_token_type(auth.token_type.as_deref());
    let nickname = parsed
        .account
        .as_ref()
        .and_then(|a| a.nickname.clone().or_else(|| a.uid.clone()));
    Some(StoredCredentials {
        token,
        kind,
        nickname,
        domain: auth.domain,
    })
}

/// The canonical CodeBuddy CLI auth-file layout, per OS.
///
/// The filename is vendor-fixed (`Tencent-Cloud.coding-copilot.info`); both
/// realms write the same path with the same shape, which is exactly why the
/// realm cannot be inferred from the file and must come from `auth.domain` /
/// the caller's explicit lane.
const AUTH_FILE_LEAF: &str = "Tencent-Cloud.coding-copilot.info";

#[cfg(target_os = "macos")]
const AUTH_APP_DIR: &str = "Library/Application Support/CodeBuddyExtension/Data/Public/auth";
#[cfg(all(unix, not(target_os = "macos")))]
const AUTH_APP_DIR: &str = ".local/share/CodeBuddyExtension/Data/Public/auth";
#[cfg(target_os = "windows")]
const AUTH_APP_DIR: &str = r"CodeBuddyExtension\Data\Public\auth";

/// Candidate on-disk auth-file paths, newest-relevant first.
pub fn candidate_auth_file_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(custom) = std::env::var("CODEBUDDY_AUTH_PATH") {
        let path = PathBuf::from(custom.trim());
        if !path.as_os_str().is_empty() {
            paths.push(path);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(AUTH_APP_DIR).join(AUTH_FILE_LEAF));
    }

    // Windows default
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        paths.push(
            PathBuf::from(local_app_data)
                .join(AUTH_APP_DIR)
                .join(AUTH_FILE_LEAF),
        );
    }

    paths
}

/// Reject a credential whose minting realm does not match this channel.
///
/// A stored credential whose recorded `auth.domain` plainly belongs to the
/// other account line (or an unrelated host) must never be presented to this
/// channel's origin — the identity provider invalidates the whole refresh
/// family across realms ([INV-PROV-REALM-01]). A token *without* a recorded
/// domain (ancient format, or a freshly pasted platform key the vendor cannot
/// annotate) is **accepted**: a pasted key carries no realm statement, and
/// historical token formats lack the field.
pub fn realm_gate(credentials: StoredCredentials, allowed_domains: &[&str]) -> StoredCredentials {
    if let Some(domain) = credentials.domain.as_deref() {
        let normalized = domain
            .trim()
            .trim_start_matches("www.")
            .to_ascii_lowercase();
        let permitted = allowed_domains.iter().any(|allowed| {
            allowed
                .trim_start_matches("www.")
                .eq_ignore_ascii_case(&normalized)
        });
        assert!(
            permitted,
            "[INV-PROV-REALM-01] credential minted by `{domain}` is not valid for this channel \
             (allowed: {allowed_domains:?}); refusing cross-realm token replay"
        );
    }
    credentials
}

/// Shared byok credential source: static key → env → CLI auth file, projecting
/// the resolved credential's kind onto the wire placement.
///
/// Each channel crate instantiates this with its own realm gate; the field
/// order and the error copy are the vendor's own resolution precedence.
pub struct CodeBuddyByokSource {
    pub connection_name: String,
    pub api_key: nuo_host::SecretString,
    allowed_domains: &'static [&'static str],
    cached: Mutex<Option<ResolvedAuth>>,
}

impl std::fmt::Debug for CodeBuddyByokSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeBuddyByokSource")
            .field("connection_name", &self.connection_name)
            .field("api_key", &"[REDACTED]")
            .field("allowed_domains", &self.allowed_domains)
            .finish()
    }
}

impl CodeBuddyByokSource {
    /// Bind a source to one realm. Pastes are not realm-stamped and always
    /// resolve; only CLI-file discovered credentials pass the gate.
    pub fn new(
        connection_name: impl Into<String>,
        api_key: nuo_host::SecretString,
        allowed_domains: &'static [&'static str],
    ) -> Self {
        Self {
            connection_name: connection_name.into(),
            api_key,
            allowed_domains,
            cached: Mutex::new(None),
        }
    }
}

/// Environment variables carrying a pasted CodeBuddy key / access token, in
/// the CLI's own precedence order.
pub const CREDENTIAL_ENV_VARS: &[&str] = &[
    "CODEBUDDY_API_KEY",
    "TENCENT_COPILOT_API_KEY",
    "TENCENT_API_KEY",
];

#[async_trait::async_trait]
impl CredentialSource for CodeBuddyByokSource {
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            // 1. An explicit connection API key
            let static_key = self.api_key.expose_secret().trim();
            if !static_key.is_empty() {
                return Ok(ResolvedAuth::new(self.api_key.clone()));
            }

            // 2. Environment variables
            for env_var in CREDENTIAL_ENV_VARS {
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

            // 4. Stored local credentials — gated on the realm boundary.
            let stored = read_stored_credentials()
                .map(|c| realm_gate(c, self.allowed_domains))
                .ok_or_else(|| {
                    "No Tencent CodeBuddy credentials found. Please set `CODEBUDDY_API_KEY` or sign in via the CodeBuddy CLI."
                        .to_string()
                })?;

            let mut auth = ResolvedAuth::new(stored.token);
            auth = auth.with_placement(stored.kind.wire_placement());
            if let Some(user) = stored.nickname {
                auth = auth.with_user_email(user);
            }
            if let Ok(mut guard) = self.cached.lock() {
                *guard = Some(auth.clone());
            }
            Ok(auth)
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
        if !self.api_key.expose_secret().trim().is_empty() {
            return true;
        }
        for env_var in CREDENTIAL_ENV_VARS {
            if let Ok(val) = std::env::var(env_var) {
                if !val.trim().is_empty() {
                    return true;
                }
            }
        }
        read_stored_credentials().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stored_credentials_format() {
        let sample_json = r#"{
            "account": { "uid": "test-uid-123", "nickname": "test-user" },
            "auth": {
                "accessToken": "eyTestAccessToken123",
                "refreshToken": "eyTestRefreshToken456",
                "tokenType": "Bearer",
                "expiresIn": 7200,
                "domain": "www.codebuddy.cn"
            }
        }"#;
        let cred = parse_stored(sample_json).expect("parses stored auth file");
        assert_eq!(cred.token, "eyTestAccessToken123");
        assert_eq!(cred.kind, CodeBuddyCredentialKind::Bearer);
        assert_eq!(cred.nickname.as_deref(), Some("test-user"));
        assert_eq!(cred.domain.as_deref(), Some("www.codebuddy.cn"));
    }

    #[test]
    fn token_type_mapping() {
        assert_eq!(
            CodeBuddyCredentialKind::from_token_type(Some("Bearer")),
            CodeBuddyCredentialKind::Bearer
        );
        assert_eq!(
            CodeBuddyCredentialKind::from_token_type(Some("bearer")),
            CodeBuddyCredentialKind::Bearer
        );
        assert_eq!(
            CodeBuddyCredentialKind::from_token_type(Some("Bearer")),
            CodeBuddyCredentialKind::Bearer
        );
        assert_eq!(
            CodeBuddyCredentialKind::from_token_type(Some("ApiKey")),
            CodeBuddyCredentialKind::ApiKey
        );
        assert_eq!(
            CodeBuddyCredentialKind::from_token_type(Some("")),
            CodeBuddyCredentialKind::ApiKey
        );
        assert_eq!(
            CodeBuddyCredentialKind::from_token_type(None),
            CodeBuddyCredentialKind::ApiKey
        );
    }

    #[test]
    fn bearer_kind_places_bearer_marker() {
        // The real CLI file carries `tokenType: "Bearer"` on account logins —
        // the resolved auth must carry the bearer marker so the header layer
        // emits `Authorization: Bearer` rather than `X-API-Key`.
        assert_eq!(
            CodeBuddyCredentialKind::Bearer.wire_placement(),
            WirePlacement::Bearer
        );
    }

    #[test]
    fn apikey_kind_places_custom_header() {
        assert_eq!(
            CodeBuddyCredentialKind::ApiKey.wire_placement(),
            WirePlacement::CustomHeader {
                header: std::borrow::Cow::Borrowed("X-API-Key"),
            }
        );
    }

    #[test]
    fn realm_gate_accepts_matching_domain() {
        let cred = StoredCredentials {
            token: "t".into(),
            kind: CodeBuddyCredentialKind::Bearer,
            nickname: None,
            domain: Some("www.codebuddy.cn".into()),
        };
        let gated = realm_gate(cred, &["copilot.tencent.com", "www.codebuddy.cn"]);
        assert_eq!(gated.domain.as_deref(), Some("www.codebuddy.cn"));
    }

    #[test]
    fn realm_gate_accepts_domainless_credentials() {
        // Pasted keys / ancient formats carry no realm statement — accepted.
        let cred = StoredCredentials {
            token: "t".into(),
            kind: CodeBuddyCredentialKind::ApiKey,
            nickname: None,
            domain: None,
        };
        realm_gate(cred, &["copilot.tencent.com"]);
    }

    #[test]
    #[should_panic(expected = "INV-PROV-REALM-01")]
    fn realm_gate_rejects_cross_realm_tokens() {
        let cred = StoredCredentials {
            token: "t".into(),
            kind: CodeBuddyCredentialKind::Bearer,
            nickname: None,
            domain: Some("codebuddy.ai".into()),
        };
        let _ = realm_gate(cred, &["copilot.tencent.com", "www.codebuddy.cn"]);
    }

    #[tokio::test]
    async fn byok_static_key_resolves_immediately() {
        let source = CodeBuddyByokSource::new(
            "test",
            nuo_host::SecretString::from("test-key-123"),
            &["copilot.tencent.com"],
        );
        assert!(source.is_ready());
        let auth = source.resolve_auth().await.expect("resolves auth");
        assert_eq!(auth.token.expose_secret(), "test-key-123");
    }
}
