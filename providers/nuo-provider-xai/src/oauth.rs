//! xAI SuperGrok OAuth surface (ADR-0027).
//!
//! Owns xAI's `OAuthConfig` preset and protocol-standard behaviour; the generic
//! engine in `nuo-oauth` never names xAI.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use nuo_model_codec::LoginMethod;
use nuo_model_codec::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, PkceMode, PortMode, TokenRequestFormat,
};
use nuo_oauth::OAuthProvider;

pub const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// xAI SuperGrok preset.
pub fn preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("xai"),
        client_id: Cow::Borrowed(XAI_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://auth.x.ai/oauth2/authorize"),
        token_url: Cow::Borrowed("https://auth.x.ai/oauth2/token"),
        device_authorization_url: Cow::Borrowed("https://auth.x.ai/oauth2/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("openid profile email offline_access grok-cli:access api:access"),
        extra_authorize_params: vec![
            (Cow::Borrowed("plan"), Cow::Borrowed("generic")),
            (Cow::Borrowed("referrer"), Cow::Borrowed("nuo")),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: true,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 56121,
        port_mode: PortMode::Fixed(56121),
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: true,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::rfc8628(),
        device_token_url: Cow::Borrowed("https://auth.x.ai/oauth2/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// The xAI subscription surface: RFC 8628 device grant, no enricher.
pub struct XaiOAuthProvider;

#[async_trait]
impl OAuthProvider for XaiOAuthProvider {
    fn ids(&self) -> &'static [&'static str] {
        &["xai"]
    }

    fn config(&self) -> OAuthConfig {
        preset()
    }
}

/// The provider surfaces this crate registers.
pub fn providers() -> Vec<Arc<dyn OAuthProvider>> {
    vec![Arc::new(XaiOAuthProvider)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_oauth::oauth::pkce::PkceCodes;
    use nuo_oauth::oauth::token::build_authorize_url;

    #[test]
    fn xai_authorize_url_carries_plan_generic_and_pkce() {
        let pkce = PkceCodes {
            verifier: "v".into(),
            challenge: "c".to_string(),
        };
        let cfg = preset();
        let url = build_authorize_url(&cfg, &pkce, "ST", "N", "http://127.0.0.1:56121/callback");
        assert!(url.starts_with("https://auth.x.ai/oauth2/authorize?"));
        assert!(url.contains("plan=generic"), "plan=generic must be present");
        assert!(url.contains("referrer=nuo"));
        assert!(url.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(url.contains("code_challenge=c"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state=ST"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A56121%2Fcallback"));
    }
}
