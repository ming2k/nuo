//! How a user-defined connection authenticates — the discriminating field that
//! lets a connection declare it resolves its bearer from OAuth (ChatGPT, Copilot,
//! Google Antigravity, xAI) rather than from an API key.
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Canonical authentication lifecycle mechanics (ADR-0098).
///
/// Decoupled from wire placement and commercial entitlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthLifecycle {
    /// Static pre-shared secret (passive storage in credentials.toml or env, zero refresh).
    Static,
    /// Protocol-managed dynamic session (active lease, background/intercepted token refresh).
    Negotiated,
    /// Ambient / network-topological identity (mTLS, local socket, zero token).
    Ambient,
    /// Environment-issued workload identity (Kubernetes SA, cloud IMDS/STS).
    Workload,
}

/// How a resolved credential token is injected into an outbound HTTP request (ADR-0098).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum WirePlacement {
    /// Standard `Authorization: Bearer <token>` header.
    #[default]
    Bearer,
    /// Custom HTTP header (e.g. `X-API-Key: <token>`, `api-key: <token>`).
    CustomHeader { header: Cow<'static, str> },
    /// URL query parameter (e.g. `?key=<token>`).
    QueryParam { param: Cow<'static, str> },
    /// Cryptographic per-request envelope or body signing (e.g. AWS SigV4, Qoder COSY).
    RequestSigner,
}

/// How a user-defined connection authenticates (ADR-0098).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionAuth {
    /// Static pre-shared secret (passive holding, zero refresh).
    #[default]
    Static,
    /// Protocol-managed negotiated session (active lease, background/intercepted token refresh).
    Negotiated {
        /// Stable provider integration id (e.g. "chatgpt", "copilot", "qoder", "xai", "google-antigravity", "codebuddy").
        provider: Cow<'static, str>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum WireConnectionAuth {
    Static,
    Negotiated { provider: String },
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
            Self::Static => WireConnectionAuth::Static.serialize(serializer),
            Self::Negotiated { provider } => WireConnectionAuth::Negotiated {
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
            RawConnectionAuth::Tagged(WireConnectionAuth::Static) => Ok(Self::Static),
            RawConnectionAuth::Tagged(WireConnectionAuth::Negotiated { provider }) => {
                Ok(Self::Negotiated {
                    provider: Cow::Owned(provider),
                })
            }
            RawConnectionAuth::BareProvider { provider } => Ok(Self::Negotiated {
                provider: Cow::Owned(provider),
            }),
            RawConnectionAuth::StringForm(s) => {
                let lower = s.to_ascii_lowercase();
                match lower.as_str() {
                    "static" => Ok(Self::Static),
                    "negotiated" => Ok(Self::Negotiated {
                        provider: Cow::Borrowed(""),
                    }),
                    _ => Ok(Self::Negotiated {
                        provider: Cow::Owned(s),
                    }),
                }
            }
        }
    }
}

impl ConnectionAuth {
    /// Const constructor for static pre-shared key declarations.
    pub const fn r#static() -> Self {
        Self::Static
    }

    /// Create an active protocol-negotiated authentication variant.
    pub fn negotiated(provider: impl Into<Cow<'static, str>>) -> Self {
        Self::Negotiated {
            provider: provider.into(),
        }
    }

    /// Const constructor for negotiated declarations.
    pub const fn negotiated_const(provider: &'static str) -> Self {
        Self::Negotiated {
            provider: Cow::Borrowed(provider),
        }
    }

    /// The canonical lifecycle mechanics of this authentication mode (ADR-0098).
    pub fn lifecycle(&self) -> AuthLifecycle {
        match self {
            Self::Static => AuthLifecycle::Static,
            Self::Negotiated { .. } => AuthLifecycle::Negotiated,
        }
    }

    /// Whether this connection resolves via a passive static pre-shared key.
    pub fn is_static(&self) -> bool {
        self.lifecycle() == AuthLifecycle::Static
    }

    /// Whether this connection resolves via an active protocol-negotiated session.
    pub fn is_negotiated(&self) -> bool {
        self.lifecycle() == AuthLifecycle::Negotiated
    }

    /// Stable negotiated provider integration id used to select endpoints and protocol
    /// configuration.
    pub fn negotiated_provider(&self) -> Option<&str> {
        match self {
            ConnectionAuth::Negotiated { provider } => Some(provider.as_ref()),
            ConnectionAuth::Static => None,
        }
    }

    /// The default login flow for this negotiated provider.
    ///
    /// Returns `None` for static connections (no login flow to run).
    pub fn default_login_method(&self) -> Option<LoginMethod> {
        match self {
            ConnectionAuth::Negotiated { provider } => match provider.as_ref() {
                "chatgpt" | "google-antigravity" => Some(LoginMethod::Browser),
                // CodeBuddy: the web-handoff is a browser flow but carries no
                // loopback callback, so it routes through the engine's
                // `DeviceFlowMode::Custom` branch (the only path that reaches
                // the vendor surface's `begin_device_login` override). The
                // method label is cosmetic; no RFC device code is polled.
                "codebuddy" | "codebuddy-intl" => Some(LoginMethod::Device),
                "xai" | "copilot" | "qoder" => Some(LoginMethod::Device),
                _ => Some(LoginMethod::Browser),
            },
            ConnectionAuth::Static => None,
        }
    }

    /// Whether this negotiated provider supports the specified login method.
    pub fn supports_login_method(&self, method: LoginMethod) -> bool {
        match self {
            ConnectionAuth::Negotiated { provider } => match provider.as_ref() {
                "chatgpt" => true,
                "google-antigravity" => method == LoginMethod::Browser,
                "xai" => true,
                // CodeBuddy routes through the engine's DeviceFlowMode::Custom
                // branch (browser web-handoff, no RFC device code polled) —
                // so both the default and this predicate say Device, keeping
                // the TUI's pre-flight check and the engine's gate aligned.
                "codebuddy" | "codebuddy-intl" => method == LoginMethod::Device,
                "copilot" | "qoder" => method == LoginMethod::Device,
                _ => true,
            },
            ConnectionAuth::Static => false,
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
            ConnectionAuth::negotiated("copilot").default_login_method(),
            Some(LoginMethod::Device)
        );
        assert_eq!(
            ConnectionAuth::negotiated("qoder").default_login_method(),
            Some(LoginMethod::Device)
        );
        assert_eq!(
            ConnectionAuth::negotiated("chatgpt").default_login_method(),
            Some(LoginMethod::Browser)
        );
        assert_eq!(
            ConnectionAuth::negotiated("xai").default_login_method(),
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
        for provider in ["codebuddy", "codebuddy-intl"] {
            let auth = ConnectionAuth::negotiated(provider);
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
    }

    #[test]
    fn static_connections_have_no_login_method() {
        assert_eq!(ConnectionAuth::Static.default_login_method(), None);
    }

    #[test]
    fn serialization_roundtrip() {
        let modern = ConnectionAuth::negotiated("qoder");
        let serialized = serde_json::to_string(&modern).unwrap();
        assert_eq!(
            serialized,
            "{\"type\":\"negotiated\",\"provider\":\"qoder\"}"
        );

        let parsed: ConnectionAuth = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, modern);

        let static_auth = ConnectionAuth::Static;
        let serialized_key = serde_json::to_string(&static_auth).unwrap();
        assert_eq!(serialized_key, "{\"type\":\"static\"}");
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

    #[test]
    fn orthogonal_lifecycle_and_placement_invariants() {
        let static_auth = ConnectionAuth::r#static();
        assert_eq!(static_auth.lifecycle(), AuthLifecycle::Static);
        assert!(static_auth.is_static());
        assert!(!static_auth.is_negotiated());

        let neg_auth = ConnectionAuth::negotiated("chatgpt");
        assert_eq!(neg_auth.lifecycle(), AuthLifecycle::Negotiated);
        assert!(neg_auth.is_negotiated());
        assert!(!neg_auth.is_static());
        assert_eq!(neg_auth.negotiated_provider(), Some("chatgpt"));

        // Modern wire forms deserialize cleanly
        let static_parsed: ConnectionAuth = serde_json::from_str("\"static\"").unwrap();
        assert_eq!(static_parsed, ConnectionAuth::Static);

        let neg_parsed: ConnectionAuth =
            serde_json::from_str("{\"type\":\"negotiated\",\"provider\":\"copilot\"}").unwrap();
        assert_eq!(neg_parsed, ConnectionAuth::negotiated("copilot"));

        // WirePlacement default is Bearer
        assert_eq!(WirePlacement::default(), WirePlacement::Bearer);
    }
}
