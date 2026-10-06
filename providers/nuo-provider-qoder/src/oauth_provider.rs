//! Qoder's OAuth subscription surface for the generic `nuo-oauth` engine (ADR-0027).
//!
//! Keeps Qoder's proprietary pieces — device session, `drt-` token rotation,
//! uid/endpoint repair, and identity projection — inside the Qoder crate so the
//! engine never names a vendor.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use nuo_model_codec::LoginMethod;
use nuo_model_codec::ResolvedAuth;
use nuo_model_codec::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, PkceMode, PortMode, TokenRequestFormat,
};
use nuo_model_codec::SecretString;
use nuo_oauth::{
    DeviceLogin, OAuthLoginPrompt, OAuthProvider, OAuthTokenEnricher, TokenSet,
};
use nuo_provider::credentials::CredentialSession;
use nuo_provider_transport::http::Http;
use nuo_oauth::oauth::AuthError;
use nuo_oauth::oauth::token::TokenResponse;

use crate::identity::QoderStoredIdentity;

pub const QODER_CLIENT_ID: &str = "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb";
pub const QODER_CN_CLIENT_ID: &str = "e93fe488-5778-4c35-a6fc-0f54ed7b3139";

/// Alibaba Qoder international preset.
pub fn qoder_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("qoder"),
        client_id: Cow::Borrowed(QODER_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://qoder.com/device/selectAccounts"),
        token_url: Cow::Borrowed("https://openapi.qoder.sh/api/v1/deviceToken/poll"),
        device_authorization_url: Cow::Borrowed("https://qoder.com/device/selectAccounts"),
        grant_type_device: Cow::Borrowed("qoder_device_flow"),
        scope: Cow::Borrowed("openid"),
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
        send_nonce: true,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::Json,
        device_flow: DeviceFlowMode::custom("qoder"),
        device_token_url: Cow::Borrowed("https://openapi.qoder.sh/api/v1/deviceToken/poll"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Alibaba Qoder CN region preset.
pub fn qoder_cn_preset() -> OAuthConfig {
    let mut cfg = qoder_preset();
    cfg.provider_id = Cow::Borrowed("qoder-cn");
    cfg.client_id = Cow::Borrowed(QODER_CN_CLIENT_ID);
    cfg.authorize_url = Cow::Borrowed("https://qoder.com.cn/device/selectAccounts");
    cfg.token_url = Cow::Borrowed("https://openapi.qoder.com.cn/api/v1/deviceToken/poll");
    cfg.device_authorization_url = Cow::Borrowed("https://qoder.com.cn/device/selectAccounts");
    cfg.device_token_url =
        Cow::Borrowed("https://openapi.qoder.com.cn/api/v1/deviceToken/poll");
    cfg
}

/// Qoder enricher: pins durable machine identity and user ID.
#[derive(Debug, Clone, Default)]
pub struct QoderOAuthEnricher;

#[async_trait]
impl OAuthTokenEnricher for QoderOAuthEnricher {
    async fn on_login_success(
        &self,
        _client: &Http,
        _connection_label: &str,
        tokens: &TokenResponse,
        previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        let uid = tokens.qoder_uid.clone().unwrap_or_default();
        // Re-login preserves the machine key minted on first login: Qoder pins
        // its risk signals to that value, so a fresh key would present as a new
        // device.
        let machine_key_hex = previous
            .and_then(|t| t.get_json_attr::<QoderStoredIdentity>("qoder"))
            .map(|i| i.machine_key_hex.expose_secret().to_string())
            .unwrap_or_else(crate::generate_machine_key_hex);

        token_set.set_json_attr(
            "qoder",
            &QoderStoredIdentity {
                uid,
                machine_key_hex: SecretString::from(machine_key_hex),
                data_policy_agreed: true,
                organization_id: None,
                organization_tags: Vec::new(),
                infer_endpoint: None,
            },
        );
        Ok(())
    }

    async fn on_refresh_success(
        &self,
        _client: &Http,
        stored: &TokenSet,
        _refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        if let Some(stored_identity) = stored.get_json_attr::<QoderStoredIdentity>("qoder") {
            token_set.set_json_attr("qoder", &stored_identity);
        }
        Ok(())
    }

    fn format_login_error(&self, error: &AuthError) -> String {
        match error {
            AuthError::TokenEndpoint { status: 429, .. } => {
                "Qoder rate-limited this login (shown by the web page as \"Parameter invalid\"). \
                 Wait 1–2 minutes without retrying, then start a new login — repeated attempts \
                 extend the cooldown."
                    .to_string()
            }
            AuthError::Timeout => {
                "Login timed out. Qoder authorize links expire within minutes: start a new login \
                 and finish the browser step (open → sign in → approve) in one pass."
                    .to_string()
            }
            other => other.to_string(),
        }
    }
}

#[derive(Clone, Copy)]
enum Region {
    Intl,
    Cn,
}

struct QoderOAuthProvider(Region);

impl QoderOAuthProvider {
    fn is_intl(&self) -> bool {
        matches!(self.0, Region::Intl)
    }
}

#[async_trait]
impl OAuthProvider for QoderOAuthProvider {
    fn ids(&self) -> &'static [&'static str] {
        match self.0 {
            Region::Intl => &["qoder"],
            Region::Cn => &["qoder-cn"],
        }
    }

    fn config(&self) -> OAuthConfig {
        match self.0 {
            Region::Intl => qoder_preset(),
            Region::Cn => qoder_cn_preset(),
        }
    }

    fn enricher(&self) -> Box<dyn OAuthTokenEnricher> {
        Box::new(QoderOAuthEnricher)
    }

    fn begin_device_login(
        &self,
        client: Http,
        config: OAuthConfig,
        machine_id: String,
    ) -> Option<BoxFuture<'static, Result<DeviceLogin, AuthError>>> {
        let authorize_url = config.authorize_url.to_string();
        let client_id = config.client_id.to_string();
        Some(Box::pin(async move {
            let session =
                crate::oauth::QoderDeviceSession::with_authorize_url(&machine_id, &client_id, &authorize_url);
            let prompt = OAuthLoginPrompt {
                method: LoginMethod::Device,
                url: session.user_url(),
                user_code: None,
                message: "Open the URL and approve this device in your Qoder account.".to_string(),
            };
            let completion = Box::pin(async move {
                crate::oauth::device_login(&client, &session).await
            });
            Ok(DeviceLogin { prompt, completion })
        }))
    }

    fn refresh_token<'a>(
        &'a self,
        client: &'a Http,
        _config: &'a OAuthConfig,
        refresh_token: &'a str,
    ) -> Option<BoxFuture<'a, Result<TokenResponse, AuthError>>> {
        Some(Box::pin(async move {
            crate::oauth::refresh_device_token(client, refresh_token).await
        }))
    }

    fn needs_repair(&self, stored: &TokenSet) -> bool {
        // Only the international line performs uid/endpoint repair (matching
        // the pre-ADR-0027 predicate, which gated on `provider == "qoder"`).
        self.is_intl()
            && stored
                .get_json_attr::<QoderStoredIdentity>("qoder")
                .is_some_and(|identity| identity.uid.is_empty() || identity.infer_endpoint.is_none())
    }

    async fn repair(
        &self,
        session: &mut dyn CredentialSession,
        connection_id: &str,
        stored: TokenSet,
    ) -> Result<TokenSet, String> {
        if !self.is_intl() {
            return Ok(stored);
        }
        let Some(mut identity) = stored.get_json_attr::<QoderStoredIdentity>("qoder") else {
            return Ok(stored);
        };
        let mut changed = false;
        if identity.uid.is_empty() {
            if let Ok(client) = Http::control_plane()
                && let Ok(uid) = crate::oauth::fetch_uid(&client, stored.access.expose_secret()).await
                && !uid.is_empty()
            {
                identity.uid = uid;
                changed = true;
            }
        }
        if identity.infer_endpoint.is_none() {
            if let Ok(client) = Http::control_plane()
                && let Ok(endpoint) =
                    crate::elect_infer_endpoint(&client, stored.access.expose_secret()).await
            {
                identity.infer_endpoint = Some(endpoint);
                changed = true;
            }
        }
        if !changed {
            return Ok(stored);
        }
        let mut updated = stored.clone();
        updated.set_json_attr("qoder", &identity);
        session.set(connection_id, updated.clone());
        session.commit().await.map_err(|error| error.to_string())?;
        Ok(updated)
    }

    fn project_metadata(&self, tokens: &TokenSet, mut auth: ResolvedAuth) -> ResolvedAuth {
        if let Some(identity) = tokens.get_json_attr::<QoderStoredIdentity>("qoder") {
            auth = auth.with_extension(identity.to_request_identity());
        }
        auth
    }
}

/// The provider surfaces this crate registers.
pub fn providers() -> Vec<Arc<dyn OAuthProvider>> {
    vec![
        Arc::new(QoderOAuthProvider(Region::Intl)),
        Arc::new(QoderOAuthProvider(Region::Cn)),
    ]
}
