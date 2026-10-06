//! OpenCode Console subscription OAuth surface (ADR-0027).
//!
//! Owns OpenCode's `OAuthConfig` preset, its JSON device grant, and the
//! Console account/org enrichment; the generic engine in `nuo-oauth` never
//! names OpenCode.

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

pub const OPENCODE_CLIENT_ID: &str = "opencode-cli";
pub const OPENCODE_CONSOLE_URL: &str = "https://opencode.ai/console";

/// OpenCode Console account preset (the OpenCode Go / Zen subscription).
pub fn preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("opencode"),
        client_id: Cow::Borrowed(OPENCODE_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed(OPENCODE_CONSOLE_URL),
        token_url: Cow::Borrowed("https://opencode.ai/console/auth/device/token"),
        device_authorization_url: Cow::Borrowed("https://opencode.ai/console/auth/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(""),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: false,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 0,
        port_mode: PortMode::Dynamic,
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::Disabled,
        token_format: TokenRequestFormat::Json,
        device_flow: DeviceFlowMode::custom("opencode"),
        device_token_url: Cow::Borrowed("https://opencode.ai/console/auth/device/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// OpenCode Console enricher: records the account identity and default org.
#[derive(Debug, Clone)]
pub struct OpencodeOAuthEnricher {
    server: String,
}

impl OpencodeOAuthEnricher {
    pub fn new(config: &OAuthConfig) -> Self {
        Self {
            server: crate::device::server_from_config(config),
        }
    }
}

#[async_trait]
impl OAuthTokenEnricher for OpencodeOAuthEnricher {
    async fn on_login_success(
        &self,
        client: &Http,
        _connection_label: &str,
        tokens: &TokenResponse,
        _previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        let access = tokens.access_token.expose_secret();
        if let Ok(user) = crate::device::fetch_user(client, &self.server, access).await {
            if !user.email.is_empty() {
                token_set.user_email = Some(user.email);
            }
            if !user.id.is_empty() {
                token_set.set_attr("account_id", user.id);
            }
        }
        if let Ok(mut orgs) = crate::device::fetch_orgs(client, &self.server, access).await {
            orgs.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
            token_set.set_json_attr("opencode_orgs", &orgs);
            if let Some(org) = orgs.first() {
                if !org.id.is_empty() {
                    token_set.set_attr("org_id", org.id.clone());
                }
                if !org.name.is_empty() {
                    token_set.set_attr("org_name", org.name.clone());
                }
            }
        }
        Ok(())
    }

    async fn on_refresh_success(
        &self,
        client: &Http,
        stored: &TokenSet,
        refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        for key in ["account_id", "org_id", "org_name"] {
            if let Some(value) = stored.get_attr(key) {
                token_set.set_attr(key, value.to_string());
            }
        }
        if let Some(orgs) = stored.get_json_attr::<Vec<crate::device::OpencodeOrg>>("opencode_orgs") {
            token_set.set_json_attr("opencode_orgs", &orgs);
        }
        token_set.user_email = stored.user_email.clone();
        // Credentials minted before org resolution carry no `org_id`, which
        // strands every Console surface on `Workspace selection required`.
        if token_set.get_attr("org_id").is_none()
            && let Ok(mut orgs) =
                crate::device::fetch_orgs(client, &self.server, refreshed.access_token.expose_secret())
                    .await
        {
            orgs.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
            token_set.set_json_attr("opencode_orgs", &orgs);
            if let Some(org) = orgs.first() {
                if !org.id.is_empty() {
                    token_set.set_attr("org_id", org.id.clone());
                }
                if !org.name.is_empty() {
                    token_set.set_attr("org_name", org.name.clone());
                }
            }
        }
        Ok(())
    }
}

fn project_metadata(tokens: &TokenSet, mut auth: ResolvedAuth) -> ResolvedAuth {
    if let Some(org_id) = tokens.get_attr("org_id") {
        auth = auth.with_extension(nuo_model_codec::OpencodeAuthMetadata {
            org_id: org_id.to_string(),
        });
    }
    auth
}

/// The OpenCode Console subscription surface.
pub struct OpencodeOAuthProvider;

#[async_trait]
impl OAuthProvider for OpencodeOAuthProvider {
    fn ids(&self) -> &'static [&'static str] {
        &["opencode", "opencode-go", "opencode-plan"]
    }

    fn config(&self) -> OAuthConfig {
        preset()
    }

    fn enricher(&self) -> Box<dyn OAuthTokenEnricher> {
        Box::new(OpencodeOAuthEnricher::new(&preset()))
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
                crate::device::poll_device_code(&client, &config, &device).await
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
    vec![Arc::new(OpencodeOAuthProvider)]
}
