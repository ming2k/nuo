//! ChatGPT Plan (Codex subscription) OAuth surface (ADR-0027).
//!
//! Owns the ChatGPT `OAuthConfig` preset, its custom device grant, and the
//! Codex account-id enrichment; the generic engine in `nuo-oauth` never names
//! ChatGPT.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use nuo_model_codec::LoginMethod;
use nuo_model_codec::ResolvedAuth;
use nuo_model_codec::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, PkceMode, PortMode, TokenRequestFormat,
};
use nuo_oauth::{
    DeviceLogin, OAuthLoginPrompt, OAuthProvider, OAuthTokenEnricher, TokenSet,
};
use nuo_provider_transport::http::Http;
use nuo_oauth::oauth::AuthError;
use nuo_oauth::oauth::token::TokenResponse;

/// Extract the ChatGPT account id from a JWT (id_token or access_token).
pub fn chatgpt_account_id(token: &str) -> Option<String> {
    let claims = nuo_oauth::oauth::token::jwt_claims(token)?;
    if let Some(id) = claims.get("chatgpt_account_id").and_then(|v| v.as_str()) {
        return Some(id.to_string());
    }
    claims
        .get("https://api.openai.com/auth")
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub const CHATGPT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// OpenAI / ChatGPT Subscription preset.
pub fn preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("chatgpt"),
        client_id: Cow::Borrowed(CHATGPT_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://auth.openai.com/oauth/authorize"),
        token_url: Cow::Borrowed("https://auth.openai.com/oauth/token"),
        device_authorization_url: Cow::Borrowed(
            "https://auth.openai.com/api/accounts/deviceauth/usercode",
        ),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        ),
        extra_authorize_params: vec![
            (
                Cow::Borrowed("id_token_add_organizations"),
                Cow::Borrowed("true"),
            ),
            (
                Cow::Borrowed("codex_cli_simplified_flow"),
                Cow::Borrowed("true"),
            ),
            (Cow::Borrowed("originator"), Cow::Borrowed("codex_cli_rs")),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: true,
        default_login_method: LoginMethod::Browser,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 1455,
        port_mode: PortMode::Fixed(1455),
        oauth_path: Cow::Borrowed("/auth/callback"),
        redirect_host: Cow::Borrowed("localhost"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::custom("chatgpt"),
        device_token_url: Cow::Borrowed("https://auth.openai.com/api/accounts/deviceauth/token"),
        device_redirect_uri: Cow::Borrowed("https://auth.openai.com/deviceauth/callback"),
    }
}

/// ChatGPT / Codex enricher: extracts and tracks `account_id` from JWT claims.
#[derive(Debug, Clone, Default)]
pub struct ChatGptOAuthEnricher;

#[async_trait]
impl OAuthTokenEnricher for ChatGptOAuthEnricher {
    async fn on_login_success(
        &self,
        _client: &Http,
        _connection_label: &str,
        tokens: &TokenResponse,
        _previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        if let Some(acct) = tokens
            .id_token
            .as_ref()
            .map(nuo_host::SecretString::expose_secret)
            .or(Some(tokens.access_token.expose_secret()))
            .and_then(chatgpt_account_id)
        {
            token_set.set_attr("account_id", acct);
        }
        Ok(())
    }

    async fn on_refresh_success(
        &self,
        _client: &Http,
        stored: &TokenSet,
        refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        let account_id = refreshed
            .id_token
            .as_ref()
            .map(nuo_host::SecretString::expose_secret)
            .or(Some(refreshed.access_token.expose_secret()))
            .and_then(chatgpt_account_id)
            .or_else(|| stored.get_attr("account_id").map(ToString::to_string));

        if let Some(acct) = account_id {
            token_set.set_attr("account_id", acct);
        }
        Ok(())
    }
}

fn project_metadata(tokens: &TokenSet, mut auth: ResolvedAuth) -> ResolvedAuth {
    let account_id = tokens.get_attr("account_id").map(ToString::to_string).or_else(|| {
        tokens
            .id_token
            .as_ref()
            .map(nuo_host::SecretString::expose_secret)
            .or(Some(tokens.access.expose_secret()))
            .and_then(chatgpt_account_id)
    });
    if let Some(acct) = account_id {
        auth = auth.with_extension(nuo_model_codec::ChatGptAuthMetadata { account_id: acct });
    }
    auth
}

/// The ChatGPT subscription surface: custom device grant + account-id metadata.
pub struct ChatGptOAuthProvider;

#[async_trait]
impl OAuthProvider for ChatGptOAuthProvider {
    fn ids(&self) -> &'static [&'static str] {
        &["chatgpt", "chatgpt-plan", "openai-subscription"]
    }

    fn config(&self) -> OAuthConfig {
        preset()
    }

    fn enricher(&self) -> Box<dyn OAuthTokenEnricher> {
        Box::new(ChatGptOAuthEnricher)
    }

    fn begin_device_login(
        &self,
        client: Http,
        config: OAuthConfig,
        _machine_id: String,
    ) -> Option<BoxFuture<'static, Result<DeviceLogin, AuthError>>> {
        Some(Box::pin(async move {
            let device = crate::device::request_device_code(&client, &config).await?;
            let prompt = OAuthLoginPrompt {
                method: LoginMethod::Device,
                url: device.user_url(&config),
                user_code: Some(device.user_code.clone()),
                message: "Open the URL on any device and enter the code to authorize.".to_string(),
            };
            let completion = Box::pin(async move {
                let token = crate::device::poll_device_code(&client, &config, &device).await?;
                crate::device::exchange_device_code(&client, &config, &token).await
            });
            Ok(DeviceLogin { prompt, completion })
        }))
    }

    fn project_metadata(&self, tokens: &TokenSet, auth: ResolvedAuth) -> ResolvedAuth {
        project_metadata(tokens, auth)
    }
}

/// The provider surfaces this crate registers.
pub fn providers() -> Vec<Arc<dyn OAuthProvider>> {
    vec![Arc::new(ChatGptOAuthProvider)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use nuo_oauth::oauth::pkce::PkceCodes;
    use nuo_oauth::oauth::token::build_authorize_url;

    #[test]
    fn chatgpt_authorize_url_carries_codex_flow_param() {
        let pkce = PkceCodes {
            verifier: "v".into(),
            challenge: "c".to_string(),
        };
        let cfg = preset();
        let url = build_authorize_url(
            &cfg,
            &pkce,
            "ST",
            "N",
            "http://localhost:1455/auth/callback",
        );
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("originator=codex_cli_rs"));
        assert!(url.contains("scope=openid+profile+email+offline_access"));
        assert!(!url.contains("nonce="), "nonce must be absent for ChatGPT");
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
    }

    #[test]
    fn chatgpt_account_id_decoded_from_top_level_claim() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"chatgpt_account_id":"acct-123"}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(chatgpt_account_id(&token), Some("acct-123".to_string()));
    }

    #[test]
    fn project_metadata_reads_account_id_from_jwt() {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "user_id": "user-123",
                "chatgpt_account_id": "org-xyz789"
            },
            "exp": 2_000_000_000
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(payload.to_string().as_bytes());
        let fake_jwt = format!("eyJhbGciOiJub25lIn0.{encoded}.signature");

        let mut tokens = TokenSet {
            access: "some-access".into(),
            refresh: "refresh".into(),
            expires_ms: i64::MAX,
            id_token: Some(fake_jwt.into()),
            token_type: None,
            scope: None,
            user_email: None,
            attributes: serde_json::Map::new(),
        };
        tokens.set_attr("account_id", "org-xyz789");
        let provider = ChatGptOAuthProvider;
        let resolved = provider.project_metadata(&tokens, ResolvedAuth::new(tokens.access.clone()));
        assert_eq!(
            resolved
                .extension::<nuo_model_codec::ChatGptAuthMetadata>()
                .map(|m| m.account_id.as_str()),
            Some("org-xyz789")
        );
    }
}
