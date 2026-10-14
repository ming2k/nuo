//! How a user-defined connection authenticates — the discriminating field that
//! lets a connection declare it resolves its bearer from OAuth (ChatGPT, Copilot,
//! Google Antigravity, xAI) rather than from an API key.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// How a user-defined connection authenticates (ADR-0267).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionAuth {
    /// Bearer from `api_key_env` (env first) or inline `api_key`.
    #[default]
    ApiKey,
    /// Subscription/OAuth session managed dynamically by an AuthProviderDriver.
    Subscription {
        /// Stable provider integration id (e.g. "chatgpt", "copilot", "qoder", "xai", "google-antigravity").
        provider: Cow<'static, str>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum WireConnectionAuth {
    ApiKey,
    Subscription { provider: String },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawConnectionAuth {
    Tagged(WireConnectionAuth),
    BareProvider { provider: String },
    StringForm(String),
}

impl Serialize for ConnectionAuth {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::ApiKey => WireConnectionAuth::ApiKey.serialize(serializer),
            Self::Subscription { provider } => WireConnectionAuth::Subscription {
                provider: provider.to_string(),
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ConnectionAuth {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawConnectionAuth::deserialize(deserializer)?;
        match raw {
            RawConnectionAuth::Tagged(WireConnectionAuth::ApiKey) => Ok(Self::ApiKey),
            RawConnectionAuth::Tagged(WireConnectionAuth::Subscription { provider }) => {
                Ok(Self::Subscription {
                    provider: Cow::Owned(provider),
                })
            }
            RawConnectionAuth::BareProvider { provider } => Ok(Self::Subscription {
                provider: Cow::Owned(provider),
            }),
            RawConnectionAuth::StringForm(s) => {
                let lower = s.to_ascii_lowercase();
                match lower.as_str() {
                    "apikey" | "api-key" | "api_key" => Ok(Self::ApiKey),
                    "subscription" => Ok(Self::Subscription {
                        provider: Cow::Borrowed(""),
                    }),
                    _ => Ok(Self::Subscription {
                        provider: Cow::Owned(s),
                    }),
                }
            }
        }
    }
}

impl ConnectionAuth {
    /// Const constructor for static subscription declarations.
    pub const fn subscription_const(provider: &'static str) -> Self {
        Self::Subscription {
            provider: Cow::Borrowed(provider),
        }
    }

    /// Create a subscription authentication variant.
    pub fn subscription(provider: impl Into<Cow<'static, str>>) -> Self {
        Self::Subscription {
            provider: provider.into(),
        }
    }

    /// Whether this variant resolves its bearer from the OAuth token store
    /// rather than from an API key. Covers every subscription/OAuth provider.
    pub fn is_oauth(&self) -> bool {
        matches!(self, ConnectionAuth::Subscription { .. })
    }

    /// Whether this connection is a subscription authentication mode.
    pub fn is_subscription(&self) -> bool {
        matches!(self, ConnectionAuth::Subscription { .. })
    }

    /// Stable OAuth integration id used to select endpoints and protocol
    /// configuration.
    pub fn subscription_provider(&self) -> Option<&str> {
        match self {
            ConnectionAuth::Subscription { provider } => Some(provider.as_ref()),
            ConnectionAuth::ApiKey => None,
        }
    }

    /// Stable OAuth integration id (alias for subscription_provider).
    pub fn oauth_provider_id(&self) -> Option<&str> {
        self.subscription_provider()
    }

    /// The default login flow for this OAuth provider.
    ///
    /// Returns `None` for API-key connections (no OAuth login to run).
    pub fn default_login_method(&self) -> Option<LoginMethod> {
        match self {
            ConnectionAuth::Subscription { provider } => match provider.as_ref() {
                "chatgpt" | "google-antigravity" => Some(LoginMethod::Browser),
                "xai" | "copilot" | "qoder" | "opencode" | "opencode-go" => {
                    Some(LoginMethod::Device)
                }
                _ => Some(LoginMethod::Browser),
            },
            ConnectionAuth::ApiKey => None,
        }
    }

    /// Whether this OAuth provider supports the specified login method.
    pub fn supports_login_method(&self, method: LoginMethod) -> bool {
        match self {
            ConnectionAuth::Subscription { provider } => match provider.as_ref() {
                "chatgpt" => true,
                "google-antigravity" => method == LoginMethod::Browser,
                "xai" => true,
                "copilot" | "qoder" | "opencode" | "opencode-go" => method == LoginMethod::Device,
                _ => true,
            },
            ConnectionAuth::ApiKey => false,
        }
    }
}

/// Which OAuth login flow to run. Carried by [`crate::events::AgentRequest::
/// ConnectConnection`] so the TUI picks the method, not the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    /// RFC 8628 device-code grant — headless / VPS / SSH / Docker. The default:
    /// works anywhere, prints a URL + short code the user enters on any device.
    #[default]
    Device,
    /// Browser loopback OAuth — local desktop. Binds `127.0.0.1:56121` and
    /// opens the authorize URL.
    Browser,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_providers_choose_a_registration_supported_default() {
        assert_eq!(
            ConnectionAuth::subscription("copilot").default_login_method(),
            Some(LoginMethod::Device)
        );
        assert_eq!(
            ConnectionAuth::subscription("qoder").default_login_method(),
            Some(LoginMethod::Device)
        );
        assert_eq!(
            ConnectionAuth::subscription("chatgpt").default_login_method(),
            Some(LoginMethod::Browser)
        );
        assert_eq!(
            ConnectionAuth::subscription("xai").default_login_method(),
            Some(LoginMethod::Device)
        );
        assert_eq!(
            ConnectionAuth::subscription("opencode").default_login_method(),
            Some(LoginMethod::Device)
        );
    }

    #[test]
    fn opencode_is_device_only() {
        let opencode = ConnectionAuth::subscription("opencode");
        assert!(opencode.supports_login_method(LoginMethod::Device));
        assert!(!opencode.supports_login_method(LoginMethod::Browser));
    }

    #[test]
    fn api_key_connections_have_no_login_method() {
        assert_eq!(ConnectionAuth::ApiKey.default_login_method(), None);
    }

    #[test]
    fn serialization_roundtrip() {
        let modern = ConnectionAuth::subscription("qoder");
        let serialized = serde_json::to_string(&modern).unwrap();
        assert_eq!(
            serialized,
            "{\"type\":\"subscription\",\"provider\":\"qoder\"}"
        );

        let parsed: ConnectionAuth = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, modern);

        let api_key = ConnectionAuth::ApiKey;
        let serialized_key = serde_json::to_string(&api_key).unwrap();
        assert_eq!(serialized_key, "{\"type\":\"api-key\"}");
    }
}
