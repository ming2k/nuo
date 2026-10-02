//! Per-provider OAuth2 client configuration & dynamic client emulator.
//!
//! muta's OAuth engine provides an ultra-flexible, industrial-grade abstraction
//! capable of emulating any OAuth 2.0 client (Google Antigravity, OpenAI Codex,
//! xAI SuperGrok, GitHub Copilot, or custom enterprise OAuth endpoints).
//!
//! Every client parameter (client_id, client_secret, endpoints, scopes, loopback
//! binding strategies, PKCE modes, headers, custom parameters) can be customized
//! dynamically at runtime or resolved from battle-tested presets.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::LoginMethod;

const fn enabled() -> bool {
    true
}

const fn browser_login_method() -> LoginMethod {
    LoginMethod::Browser
}

/// Which device-authorization flow mode a provider speaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DeviceFlowMode {
    /// Standard RFC 8628: form-urlencoded request + poll, the polled token
    /// endpoint returns access tokens directly.
    #[default]
    Rfc8628,
    /// Vendor-specific custom device authorization strategy.
    Custom(Cow<'static, str>),
    /// Device flow is not supported or disabled.
    Disabled,
}

impl DeviceFlowMode {
    pub const fn rfc8628() -> Self {
        Self::Rfc8628
    }

    pub const fn custom(name: &'static str) -> Self {
        Self::Custom(Cow::Borrowed(name))
    }

    pub const fn disabled() -> Self {
        Self::Disabled
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// Port binding strategy for the local callback listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PortMode {
    /// Fixed port (e.g. 1455 for Codex, 56121 for xAI). Fails if port is occupied.
    Fixed(u16),
    /// Ephemeral / dynamic port assigned by OS (binds port 0).
    #[default]
    Dynamic,
    /// Tries the preferred port first. If occupied (AddrInUse), seamlessly
    /// falls back to a dynamic OS port. Ideal for Google Antigravity & local testing.
    PreferredOrDynamic(u16),
}

/// PKCE (RFC 7636) code challenge method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PkceMode {
    /// Standard SHA-256 code challenge (RFC 7636 S256).
    #[default]
    S256,
    /// Plain code challenge.
    Plain,
    /// PKCE disabled.
    Disabled,
}

/// Client authentication method used during token exchange / refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClientAuthMethod {
    /// Public client / PKCE only (no client_secret required).
    #[default]
    None,
    /// Send `client_id` and `client_secret` in the request body (form-urlencoded or JSON).
    RequestBody,
    /// Send `Authorization: Basic base64(client_id:client_secret)` header.
    BasicHeader,
}

/// Format for token endpoint request payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TokenRequestFormat {
    #[default]
    FormUrlEncoded,
    Json,
}

/// Fully flexible OAuth2 client configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthConfig {
    /// Stable integration id used for endpoint selection and diagnostics.
    pub provider_id: Cow<'static, str>,
    /// Public OAuth client id registered with the provider.
    pub client_id: Cow<'static, str>,
    /// Client secret when required by the OAuth provider (e.g. Google Antigravity).
    pub client_secret: Option<Cow<'static, str>>,
    /// How the client credentials are authenticated with the token endpoint.
    pub client_auth_method: ClientAuthMethod,
    /// Authorization endpoint (consent screen).
    pub authorize_url: Cow<'static, str>,
    /// Token endpoint (code exchange + refresh).
    pub token_url: Cow<'static, str>,
    /// Device-authorization endpoint (request the user_code).
    pub device_authorization_url: Cow<'static, str>,
    /// RFC 8628 `grant_type` value sent during the device poll.
    pub grant_type_device: Cow<'static, str>,
    /// OAuth scopes requested.
    pub scope: Cow<'static, str>,
    /// Extra query params appended to the authorize URL.
    pub extra_authorize_params: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    /// Extra form/json params sent during code exchange.
    pub extra_token_params: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    /// Extra form/json params sent during token refresh.
    pub extra_refresh_params: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    /// Extra HTTP headers sent to token/device endpoints.
    pub extra_headers: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    /// Custom User-Agent header (e.g. "antigravity/cli/1.2.12 (aidev_client;
    /// os_type=windows; arch=amd64; cl=0; auth_method=consumer)").
    pub user_agent: Option<Cow<'static, str>>,
    /// Whether this client registration accepts a localhost browser callback.
    /// Some OAuth applications expose an authorize endpoint but only register
    /// a device flow (GitHub Copilot is the built-in example), so this must be
    /// explicit rather than inferred from `authorize_url`.
    #[serde(default = "enabled")]
    pub browser_login: bool,
    /// The login method frontends should select on ordinary activation.
    /// Alternate supported methods remain available for headless/local use.
    #[serde(default = "browser_login_method")]
    pub default_login_method: LoginMethod,
    /// Loopback callback host to bind (e.g. "127.0.0.1" or "0.0.0.0").
    pub oauth_host: Cow<'static, str>,
    /// Preferred / default loopback port.
    pub oauth_port: u16,
    /// Port allocation mode (Fixed, Dynamic, or PreferredOrDynamic).
    pub port_mode: PortMode,
    /// Loopback callback URL path (e.g. "/oauth/callback" or "/callback").
    pub oauth_path: Cow<'static, str>,
    /// The host string used in the browser `redirect_uri` (e.g. "127.0.0.1" or "localhost").
    pub redirect_host: Cow<'static, str>,
    /// Explicit override for the entire `redirect_uri` (useful for reverse proxy or manual flows).
    pub custom_redirect_uri: Option<Cow<'static, str>>,
    /// Whether to send an OIDC `nonce` in the authorize URL.
    pub send_nonce: bool,
    /// PKCE code challenge mode.
    pub pkce_mode: PkceMode,
    /// Request format for token endpoints.
    pub token_format: TokenRequestFormat,
    /// Which device-authorization flow this provider speaks.
    pub device_flow: DeviceFlowMode,
    /// The token endpoint URL polled during the device flow.
    pub device_token_url: Cow<'static, str>,
    /// The `redirect_uri` sent when exchanging the device authorization_code (ChatGPT).
    pub device_redirect_uri: Cow<'static, str>,
}

impl OAuthConfig {
    /// Create a fluent builder for a new OAuth configuration.
    pub fn builder(provider_id: impl Into<Cow<'static, str>>) -> OAuthConfigBuilder {
        OAuthConfigBuilder::new(provider_id)
    }

    /// Resolve the registered browser redirect_uri (`http://<redirect_host>:<port><path>`).
    /// If an explicit `actual_port` is given (e.g. from dynamic binding), that port is used.
    pub fn redirect_uri(&self, actual_port: Option<u16>) -> String {
        if let Some(custom) = &self.custom_redirect_uri {
            return custom.to_string();
        }
        let port = actual_port.unwrap_or(self.oauth_port);
        format!("http://{}:{}{}", self.redirect_host, port, self.oauth_path)
    }

    /// Whether this client registration supports `method`.
    pub fn supports_login_method(&self, method: LoginMethod) -> bool {
        match method {
            LoginMethod::Browser => self.browser_login,
            LoginMethod::Device => !self.device_flow.is_disabled(),
        }
    }

    /// The configured default when supported, otherwise the first available
    /// method. A malformed configuration with neither flow returns `None`.
    pub fn effective_default_login_method(&self) -> Option<LoginMethod> {
        if self.supports_login_method(self.default_login_method) {
            return Some(self.default_login_method);
        }
        [LoginMethod::Browser, LoginMethod::Device]
            .into_iter()
            .find(|method| self.supports_login_method(*method))
    }

    /// Helper to clone and override client_id.
    pub fn with_client_id(mut self, client_id: impl Into<Cow<'static, str>>) -> Self {
        self.client_id = client_id.into();
        self
    }

    /// Helper to clone and override client_secret.
    pub fn with_client_secret(mut self, client_secret: impl Into<Cow<'static, str>>) -> Self {
        self.client_secret = Some(client_secret.into());
        self
    }

    /// Helper to clone and override redirect_host.
    pub fn with_redirect_host(mut self, host: impl Into<Cow<'static, str>>) -> Self {
        self.redirect_host = host.into();
        self
    }

    /// Helper to clone and override port_mode.
    pub fn with_port_mode(mut self, mode: PortMode) -> Self {
        self.port_mode = mode;
        self
    }

    /// Helper to clone and append extra authorize parameters.
    pub fn with_extra_authorize_param(
        mut self,
        key: impl Into<Cow<'static, str>>,
        val: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.extra_authorize_params.push((key.into(), val.into()));
        self
    }

    /// Helper to clone and append extra headers.
    pub fn with_extra_header(
        mut self,
        key: impl Into<Cow<'static, str>>,
        val: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.extra_headers.push((key.into(), val.into()));
        self
    }
}

/// Fluent builder for [`OAuthConfig`].
#[derive(Debug, Clone)]
pub struct OAuthConfigBuilder {
    cfg: OAuthConfig,
}

impl OAuthConfigBuilder {
    pub fn new(provider_id: impl Into<Cow<'static, str>>) -> Self {
        Self {
            cfg: OAuthConfig {
                provider_id: provider_id.into(),
                client_id: Cow::Borrowed(""),
                client_secret: None,
                client_auth_method: ClientAuthMethod::None,
                authorize_url: Cow::Borrowed(""),
                token_url: Cow::Borrowed(""),
                device_authorization_url: Cow::Borrowed(""),
                grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
                scope: Cow::Borrowed(""),
                extra_authorize_params: Vec::new(),
                extra_token_params: Vec::new(),
                extra_refresh_params: Vec::new(),
                extra_headers: Vec::new(),
                user_agent: None,
                browser_login: true,
                default_login_method: LoginMethod::Browser,
                oauth_host: Cow::Borrowed("127.0.0.1"),
                oauth_port: 0,
                port_mode: PortMode::Dynamic,
                oauth_path: Cow::Borrowed("/callback"),
                redirect_host: Cow::Borrowed("127.0.0.1"),
                custom_redirect_uri: None,
                send_nonce: false,
                pkce_mode: PkceMode::S256,
                token_format: TokenRequestFormat::FormUrlEncoded,
                device_flow: DeviceFlowMode::Rfc8628,
                device_token_url: Cow::Borrowed(""),
                device_redirect_uri: Cow::Borrowed(""),
            },
        }
    }

    pub fn client_id(mut self, id: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.client_id = id.into();
        self
    }

    pub fn client_secret(mut self, secret: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.client_secret = Some(secret.into());
        self
    }

    pub fn client_auth_method(mut self, method: ClientAuthMethod) -> Self {
        self.cfg.client_auth_method = method;
        self
    }

    pub fn authorize_url(mut self, url: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.authorize_url = url.into();
        self
    }

    pub fn token_url(mut self, url: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.token_url = url.into();
        self
    }

    pub fn device_authorization_url(mut self, url: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.device_authorization_url = url.into();
        self
    }

    pub fn grant_type_device(mut self, gt: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.grant_type_device = gt.into();
        self
    }

    pub fn scope(mut self, scope: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.scope = scope.into();
        self
    }

    pub fn extra_authorize_param(
        mut self,
        k: impl Into<Cow<'static, str>>,
        v: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.cfg.extra_authorize_params.push((k.into(), v.into()));
        self
    }

    pub fn extra_token_param(
        mut self,
        k: impl Into<Cow<'static, str>>,
        v: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.cfg.extra_token_params.push((k.into(), v.into()));
        self
    }

    pub fn extra_refresh_param(
        mut self,
        k: impl Into<Cow<'static, str>>,
        v: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.cfg.extra_refresh_params.push((k.into(), v.into()));
        self
    }

    pub fn extra_header(
        mut self,
        k: impl Into<Cow<'static, str>>,
        v: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.cfg.extra_headers.push((k.into(), v.into()));
        self
    }

    pub fn user_agent(mut self, ua: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.user_agent = Some(ua.into());
        self
    }

    pub fn browser_login(mut self, enabled: bool) -> Self {
        self.cfg.browser_login = enabled;
        self
    }

    pub fn default_login_method(mut self, method: LoginMethod) -> Self {
        self.cfg.default_login_method = method;
        self
    }

    pub fn oauth_host(mut self, host: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.oauth_host = host.into();
        self
    }

    pub fn oauth_port(mut self, port: u16) -> Self {
        self.cfg.oauth_port = port;
        self
    }

    pub fn port_mode(mut self, mode: PortMode) -> Self {
        self.cfg.port_mode = mode;
        self
    }

    pub fn oauth_path(mut self, path: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.oauth_path = path.into();
        self
    }

    pub fn redirect_host(mut self, host: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.redirect_host = host.into();
        self
    }

    pub fn custom_redirect_uri(mut self, uri: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.custom_redirect_uri = Some(uri.into());
        self
    }

    pub fn send_nonce(mut self, send: bool) -> Self {
        self.cfg.send_nonce = send;
        self
    }

    pub fn pkce_mode(mut self, mode: PkceMode) -> Self {
        self.cfg.pkce_mode = mode;
        self
    }

    pub fn token_format(mut self, format: TokenRequestFormat) -> Self {
        self.cfg.token_format = format;
        self
    }

    pub fn device_flow(mut self, flow: DeviceFlowMode) -> Self {
        self.cfg.device_flow = flow;
        self
    }

    pub fn device_token_url(mut self, url: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.device_token_url = url.into();
        self
    }

    pub fn device_redirect_uri(mut self, uri: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.device_redirect_uri = uri.into();
        self
    }

    pub fn build(self) -> OAuthConfig {
        self.cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_config_builder_constructs_valid_config() {
        let cfg = OAuthConfigBuilder::new("custom-provider")
            .client_id("client-123")
            .authorize_url("https://auth.example.com/oauth/authorize")
            .token_url("https://auth.example.com/oauth/token")
            .scope("openid profile")
            .build();

        assert_eq!(cfg.provider_id, "custom-provider");
        assert_eq!(cfg.client_id, "client-123");
        assert_eq!(
            cfg.authorize_url,
            "https://auth.example.com/oauth/authorize"
        );
        assert_eq!(cfg.token_url, "https://auth.example.com/oauth/token");
        assert_eq!(cfg.scope, "openid profile");
        assert_eq!(cfg.device_flow, DeviceFlowMode::Rfc8628);
    }

    #[test]
    fn oauth_config_redirect_uri_computation() {
        let cfg = OAuthConfigBuilder::new("test-provider")
            .redirect_host("127.0.0.1")
            .oauth_path("/callback")
            .build();

        assert_eq!(
            cfg.redirect_uri(Some(56121)),
            "http://127.0.0.1:56121/callback"
        );
    }
}
