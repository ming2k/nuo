//! GitHub Copilot subscription OAuth surface (ADR-0027).
//!
//! Owns Copilot's `OAuthConfig` preset; the generic engine in `nuo-oauth` never
//! names Copilot.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use nuo_model_codec::LoginMethod;
use nuo_model_codec::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, PkceMode, PortMode, TokenRequestFormat,
};
use nuo_oauth::OAuthProvider;

pub const COPILOT_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";

/// GitHub Copilot subscription preset.
pub fn preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("copilot"),
        client_id: Cow::Borrowed(COPILOT_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://github.com/login/oauth/authorize"),
        token_url: Cow::Borrowed("https://github.com/login/oauth/access_token"),
        device_authorization_url: Cow::Borrowed("https://github.com/login/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("read:user"),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: false,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 42195,
        port_mode: PortMode::Fixed(42195),
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::rfc8628(),
        device_token_url: Cow::Borrowed("https://github.com/login/oauth/access_token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// The Copilot subscription surface: RFC 8628 device grant, no enricher.
pub struct CopilotOAuthProvider;

#[async_trait]
impl OAuthProvider for CopilotOAuthProvider {
    fn ids(&self) -> &'static [&'static str] {
        &["copilot", "github-copilot"]
    }

    fn config(&self) -> OAuthConfig {
        preset()
    }
}

/// The provider surfaces this crate registers.
pub fn providers() -> Vec<Arc<dyn OAuthProvider>> {
    vec![Arc::new(CopilotOAuthProvider)]
}
