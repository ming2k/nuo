//! How a user-defined connection authenticates — the discriminating field that
//! lets a connection declare it resolves its bearer from OAuth (ChatGPT, Copilot,
//! Google Antigravity, xAI) rather than from an API key.
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// How a user-defined connection authenticates.
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
                // CodeBuddy: the web-handoff is a browser flow but carries no
                // loopback callback, so it routes through the engine's
                // `DeviceFlowMode::Custom` branch (the only path that reaches
                // the vendor surface's `begin_device_login` override). The
                // method label is cosmetic; no RFC device code is polled.
                "codebuddy" => Some(LoginMethod::Device),
                "xai" | "copilot" | "qoder" => Some(LoginMethod::Device),
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
                // CodeBuddy routes through the engine's DeviceFlowMode::Custom
                // branch (browser web-handoff, no RFC device code polled) —
                // so both the default and this predicate say Device, keeping
                // the TUI's pre-flight check and the engine's gate aligned.
                "codebuddy" => method == LoginMethod::Device,
                "copilot" | "qoder" => method == LoginMethod::Device,
                _ => true,
            },
            ConnectionAuth::ApiKey => false,
        }
    }
}

/// Which OAuth login flow to run. Carried by the connect-connection request so
/// the TUI picks the method, not the harness.
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

/// Economic entitlement and cost model of a connection (ADR-0095).
///
/// Strictly orthogonal to the authentication protocol. Determines scheduling
/// priority and whether spend circuit breakers apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Entitlement {
    /// Zero marginal cost per token (monthly subscription, free quota, fixed fee).
    /// Prioritized for consumption by cost-optimized schedulers.
    FlatRate,
    /// Positive marginal cost per token (pay-as-you-go, metered in NanoUSD).
    /// Guarded by spend circuit breakers and audit barriers.
    #[default]
    Metered,
}

impl Entitlement {
    /// Whether this entitlement represents a zero-marginal-cost flat rate.
    pub fn is_flat_rate(&self) -> bool {
        matches!(self, Self::FlatRate)
    }

    /// Whether this entitlement represents a metered per-token cost.
    pub fn is_metered(&self) -> bool {
        matches!(self, Self::Metered)
    }
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
    }

    /// The `ConnectionAuth` pre-flight (the TUI's gate before
    /// `AuthorizeOAuth`) must agree with what the vendor's `OAuthConfig`
    /// declares — otherwise the TUI offers a method the engine then rejects,
    /// or blocks the only method the surface actually routes. The engine's
    /// `OAuthConfig::supports_login_method` is:
    ///   Browser -> config.browser_login
    ///   Device  -> !config.device_flow.is_disabled()
    /// CodeBuddy declares `browser_login: false` (no loopback callback) and a
    /// Custom device flow, so both layers must say `Device`.
    #[test]
    fn codebuddy_preflight_and_engine_gate_both_route_device() {
        let provider = "codebuddy";
        let auth = ConnectionAuth::subscription(provider);
        assert_eq!(
            auth.default_login_method(),
            Some(LoginMethod::Device),
            "{provider} default method must be Device (the only branch the engine dispatches"
        );
        assert!(
            auth.supports_login_method(LoginMethod::Device),
            "{provider} must accept Device — otherwise the TUI blocks the only working flow"
        );
        assert!(
            !auth.supports_login_method(LoginMethod::Browser),
            "{provider} must reject Browser — `OAuthConfig.browser_login` is false, so\
             the engine would bounce any Browser request with 'browser PKCE login is not supported'"
        );
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

    #[test]
    fn entitlement_predicates() {
        let flat_rate = Entitlement::FlatRate;
        assert!(flat_rate.is_flat_rate());
        assert!(!flat_rate.is_metered());

        let metered = Entitlement::Metered;
        assert!(metered.is_metered());
        assert!(!metered.is_flat_rate());

        assert_eq!(Entitlement::default(), Entitlement::Metered);
    }
}
