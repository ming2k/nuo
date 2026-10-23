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
                "codebuddy" | "codebuddy-intl" => Some(LoginMethod::Device),
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
                "codebuddy" | "codebuddy-intl" => method == LoginMethod::Device,
                "copilot" | "qoder" => method == LoginMethod::Device,
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

/// The broad classification of an authentication mode (ADR-0095).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    /// Static or environment-bound API key.
    ApiKey,
    /// Dynamically supervised OAuth subscription session.
    OAuthSubscription,
}

/// A specific authentication capability supported by a provider (ADR-0095).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AuthCapability {
    /// Direct API key authentication.
    ApiKey {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_env: Option<Cow<'static, str>>,
    },
    /// Managed OAuth subscription authentication.
    OAuthSubscription {
        provider_id: Cow<'static, str>,
        supported_methods: Vec<LoginMethod>,
    },
}

impl AuthCapability {
    /// Construct an API key capability without a default environment variable.
    pub const fn api_key() -> Self {
        Self::ApiKey { default_env: None }
    }

    /// Construct an API key capability with a canonical default environment variable.
    pub const fn api_key_with_env(env: &'static str) -> Self {
        Self::ApiKey {
            default_env: Some(Cow::Borrowed(env)),
        }
    }

    /// Construct an OAuth subscription capability.
    pub fn subscription(provider: impl Into<Cow<'static, str>>, methods: Vec<LoginMethod>) -> Self {
        Self::OAuthSubscription {
            provider_id: provider.into(),
            supported_methods: methods,
        }
    }

    /// The broad classification of this capability.
    pub fn kind(&self) -> AuthKind {
        match self {
            Self::ApiKey { .. } => AuthKind::ApiKey,
            Self::OAuthSubscription { .. } => AuthKind::OAuthSubscription,
        }
    }
}

/// A set of authentication capabilities exposed by a model provider (ADR-0095).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuthCapabilitySet {
    capabilities: Vec<AuthCapability>,
}

impl AuthCapabilitySet {
    /// Create a capability set from a list of capabilities.
    pub fn new(capabilities: Vec<AuthCapability>) -> Self {
        Self { capabilities }
    }

    /// Create a single-capability set.
    pub fn single(capability: AuthCapability) -> Self {
        Self {
            capabilities: vec![capability],
        }
    }

    /// Create a dual-lane capability set (supporting both BYOK and BYOS).
    pub fn dual(cap1: AuthCapability, cap2: AuthCapability) -> Self {
        Self {
            capabilities: vec![cap1, cap2],
        }
    }

    /// Access the underlying capability slice.
    pub fn capabilities(&self) -> &[AuthCapability] {
        &self.capabilities
    }

    /// Whether this provider supports API key authentication (BYOK).
    pub fn supports_api_key(&self) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, AuthCapability::ApiKey { .. }))
    }

    /// Whether this provider supports OAuth subscription authentication (BYOS).
    pub fn supports_subscription(&self) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, AuthCapability::OAuthSubscription { .. }))
    }

    /// Whether this provider supports both BYOK and BYOS concurrently.
    pub fn supports_both(&self) -> bool {
        self.supports_api_key() && self.supports_subscription()
    }

    /// Look up the subscription capability if present.
    pub fn subscription_capability(&self) -> Option<&AuthCapability> {
        self.capabilities
            .iter()
            .find(|c| matches!(c, AuthCapability::OAuthSubscription { .. }))
    }
}

/// Placement of a resolved token on the HTTP request wire (ADR-0095).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "header", rename_all = "kebab-case")]
pub enum HeaderPlacement {
    /// Standard Authorization header: `Authorization: Bearer <token>`
    Bearer,
    /// Custom vendor header: `<name>: <token>` (e.g. `x-api-key`, `api-key`)
    Custom(Cow<'static, str>),
}

/// Symmetrical classification of credential lifecycle (ADR-0095).
///
/// Replaces the category error of `ApiKey` vs `OAuth` with a symmetric
/// lifecycle paradigm: `Static` (passive holding) vs `Negotiated` (active protocol).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum CredentialCapability {
    /// Static / Pre-shared token (user provided, passive holding, no refresh state machine).
    Static {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_env: Option<Cow<'static, str>>,
        header: HeaderPlacement,
    },
    /// Dynamically negotiated token (active lifecycle, protocol-managed refresh).
    Negotiated { protocol: AuthProtocol },
}

impl CredentialCapability {
    /// Convenience constructor for a static Bearer token with an optional default env var.
    pub const fn static_bearer(env: Option<&'static str>) -> Self {
        Self::Static {
            default_env: match env {
                Some(e) => Some(Cow::Borrowed(e)),
                None => None,
            },
            header: HeaderPlacement::Bearer,
        }
    }

    /// Convenience constructor for a static custom header token.
    pub const fn static_custom(header_name: &'static str, env: Option<&'static str>) -> Self {
        Self::Static {
            default_env: match env {
                Some(e) => Some(Cow::Borrowed(e)),
                None => None,
            },
            header: HeaderPlacement::Custom(Cow::Borrowed(header_name)),
        }
    }

    /// Convenience constructor for an OAuth2 negotiated protocol.
    pub fn oauth2(provider_id: impl Into<Cow<'static, str>>, flows: Vec<LoginMethod>) -> Self {
        Self::Negotiated {
            protocol: AuthProtocol::OAuth2 {
                provider_id: provider_id.into(),
                flows,
            },
        }
    }

    /// Whether this capability is a statically provided token.
    pub fn is_static(&self) -> bool {
        matches!(self, Self::Static { .. })
    }

    /// Whether this capability is a dynamically negotiated protocol session.
    pub fn is_negotiated(&self) -> bool {
        matches!(self, Self::Negotiated { .. })
    }
}

/// Specific handshake & token issuance protocol (ADR-0095).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "kebab-case")]
pub enum AuthProtocol {
    /// RFC 6749 / RFC 8628 OAuth 2.0 framework.
    OAuth2 {
        provider_id: Cow<'static, str>,
        flows: Vec<LoginMethod>,
    },
    /// Enterprise Cloud IAM / STS federation (AWS SigV4, GCP Workload Identity).
    CloudIam { provider_id: Cow<'static, str> },
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

/// Concrete credential binding held by a connection profile (ADR-0095).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CredentialBinding {
    /// Statically held token string.
    Static {
        secret: nuo_host::SecretString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var: Option<String>,
    },
    /// Dynamically negotiated session managed by the token supervisor.
    Negotiated { session_id: String },
}

impl CredentialBinding {
    /// Construct a static inline secret binding.
    pub fn static_secret(secret: impl Into<nuo_host::SecretString>) -> Self {
        Self::Static {
            secret: secret.into(),
            env_var: None,
        }
    }

    /// Construct a static environment-backed secret binding.
    pub fn static_env(
        env_var: impl Into<String>,
        secret: impl Into<nuo_host::SecretString>,
    ) -> Self {
        Self::Static {
            secret: secret.into(),
            env_var: Some(env_var.into()),
        }
    }

    /// Construct a dynamic negotiated session binding.
    pub fn negotiated(session_id: impl Into<String>) -> Self {
        Self::Negotiated {
            session_id: session_id.into(),
        }
    }
}

/// Financial guardrail protecting against unexpected charges during failover (ADR-0095).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum SpendGuardrail {
    /// Suspend the round and emit an interactive confirmation barrier to the client UI.
    #[default]
    RequireInteractiveConfirm,
    /// Automatic failover permitted up to a session-scoped dollar ceiling.
    AutoWithSessionCap { max_usd: f64 },
}

/// Alias for backwards compatibility with earlier draft terminology.
pub type FailoverSafetyMode = SpendGuardrail;

/// Execution routing policy for dispatching turns to connections (ADR-0095).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RoutingPolicy {
    /// Single connection dispatch (legacy-compatible default).
    Direct { connection: String },
    /// Prioritized dual-lane dispatch: consume zero-marginal-cost FlatRate first,
    /// fail over to metered key when rate-limited or quota is exhausted.
    PriorityFailover {
        /// Primary zero-marginal-cost connection name (typically Entitlement::FlatRate).
        primary: String,
        /// Fallback metered connection name (typically Entitlement::Metered).
        fallback: String,
        /// Guardrail against unexpected financial charges on fallback.
        guardrail: SpendGuardrail,
    },
    /// Backward-compatible alias for PriorityFailover with dual-lane naming.
    DualLaneFailover {
        subscription_connection: String,
        byok_fallback_connection: String,
        safety_mode: SpendGuardrail,
    },
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self::Direct {
            connection: String::new(),
        }
    }
}

impl RoutingPolicy {
    /// Create a direct single-connection policy.
    pub fn direct(connection: impl Into<String>) -> Self {
        Self::Direct {
            connection: connection.into(),
        }
    }

    /// Create a prioritized failover policy (cost-optimized).
    pub fn priority_failover(
        primary: impl Into<String>,
        fallback: impl Into<String>,
        guardrail: SpendGuardrail,
    ) -> Self {
        Self::PriorityFailover {
            primary: primary.into(),
            fallback: fallback.into(),
            guardrail,
        }
    }

    /// Create a dual-lane failover policy (legacy constructor).
    pub fn dual_lane(
        subscription: impl Into<String>,
        byok: impl Into<String>,
        safety_mode: SpendGuardrail,
    ) -> Self {
        Self::DualLaneFailover {
            subscription_connection: subscription.into(),
            byok_fallback_connection: byok.into(),
            safety_mode,
        }
    }

    /// The primary connection to attempt first.
    pub fn primary_connection(&self) -> &str {
        match self {
            Self::Direct { connection } => connection.as_str(),
            Self::PriorityFailover { primary, .. } => primary.as_str(),
            Self::DualLaneFailover {
                subscription_connection,
                ..
            } => subscription_connection.as_str(),
        }
    }

    /// The secondary fallback connection if failover is configured.
    pub fn fallback_connection(&self) -> Option<&str> {
        match self {
            Self::Direct { .. } => None,
            Self::PriorityFailover { fallback, .. } => Some(fallback.as_str()),
            Self::DualLaneFailover {
                byok_fallback_connection,
                ..
            } => Some(byok_fallback_connection.as_str()),
        }
    }

    /// Whether this policy is a failover configuration.
    pub fn is_failover(&self) -> bool {
        !matches!(self, Self::Direct { .. })
    }

    /// Alias for is_failover.
    pub fn is_dual_lane(&self) -> bool {
        self.is_failover()
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
        for provider in ["codebuddy", "codebuddy-intl"] {
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
    fn auth_capability_set_queries() {
        let single_key =
            AuthCapabilitySet::single(AuthCapability::api_key_with_env("OPENAI_API_KEY"));
        assert!(single_key.supports_api_key());
        assert!(!single_key.supports_subscription());
        assert!(!single_key.supports_both());

        let dual_lane = AuthCapabilitySet::dual(
            AuthCapability::subscription("xai", vec![LoginMethod::Device, LoginMethod::Browser]),
            AuthCapability::api_key_with_env("XAI_API_KEY"),
        );
        assert!(dual_lane.supports_api_key());
        assert!(dual_lane.supports_subscription());
        assert!(dual_lane.supports_both());
        assert!(dual_lane.subscription_capability().is_some());
    }

    #[test]
    fn routing_policy_serde_and_accessors() {
        let direct = RoutingPolicy::direct("my-default");
        assert_eq!(direct.primary_connection(), "my-default");
        assert_eq!(direct.fallback_connection(), None);
        assert!(!direct.is_dual_lane());

        let dual = RoutingPolicy::dual_lane(
            "chatgpt-pro",
            "openai-paygo",
            SpendGuardrail::AutoWithSessionCap { max_usd: 5.0 },
        );
        assert_eq!(dual.primary_connection(), "chatgpt-pro");
        assert_eq!(dual.fallback_connection(), Some("openai-paygo"));
        assert!(dual.is_dual_lane());

        let serialized = serde_json::to_string(&dual).unwrap();
        let deserialized: RoutingPolicy = serde_json::from_str(&serialized).unwrap();
        assert_eq!(dual, deserialized);
    }

    #[test]
    fn credential_capability_and_entitlement_symmetry() {
        let static_cap = CredentialCapability::static_bearer(Some("ANTHROPIC_API_KEY"));
        assert!(static_cap.is_static());
        assert!(!static_cap.is_negotiated());

        let custom_header = CredentialCapability::static_custom("x-api-key", None);
        assert!(custom_header.is_static());

        let negotiated_cap = CredentialCapability::oauth2("chatgpt", vec![LoginMethod::Device]);
        assert!(!negotiated_cap.is_static());
        assert!(negotiated_cap.is_negotiated());

        let flat_rate = Entitlement::FlatRate;
        assert!(flat_rate.is_flat_rate());
        assert!(!flat_rate.is_metered());

        let metered = Entitlement::Metered;
        assert!(metered.is_metered());
        assert!(!metered.is_flat_rate());

        let priority_failover = RoutingPolicy::priority_failover(
            "claude-pro-sub",
            "anthropic-paygo-key",
            SpendGuardrail::RequireInteractiveConfirm,
        );
        assert_eq!(priority_failover.primary_connection(), "claude-pro-sub");
        assert_eq!(
            priority_failover.fallback_connection(),
            Some("anthropic-paygo-key")
        );
        assert!(priority_failover.is_failover());

        let ser = serde_json::to_string(&priority_failover).unwrap();
        let de: RoutingPolicy = serde_json::from_str(&ser).unwrap();
        assert_eq!(priority_failover, de);
    }
}
