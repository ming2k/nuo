//! The vendor-custom web-handoff login + refresh flows, driven against a
//! realm-selected origin.
//!
//! The origin is **supplied by the channel crate**, never assumed here —
//! [INV-PROV-REALM-01] makes the physical host a per-channel fact, and this
//! engine's only knowledge of a realm is the origin string it is constructed
//! with. Two realms therefore produce two independently constructed engines,
//! and a token minted by one is structurally never carried into a refresh
//! built against the other.

use std::time::Duration;

use futures::future::BoxFuture;
use nuo_provider::LoginMethod;
use nuo_provider::auth::{
    ClientAuthMethod, DeviceFlowMode, DeviceLogin, OAuthConfig, OAuthProvider, PkceMode, PortMode,
    TokenRequestFormat,
};
use nuo_provider::transport::Http;

use crate::envelope::{CodeBuddyAuthEnvelope, CodeBuddyEnvelope};

/// Route base every auth surface path is prefixed with
/// (the CLI's `product.json authentication.attributes.prefixPath`).
pub const PLUGIN_PREFIX: &str = "/v2/plugin";

/// Interval between state/token polls (the CLI's own value is ~1s).
const POLL_INTERVAL_MS: u64 = 1_000;
/// Total login deadline for the web-handoff wait (the CLI's own budget).
const LOGIN_DEADLINE: Duration = Duration::from_secs(300);

/// A begun CodeBuddy web-handoff login: the browser URL plus the poll handle.
struct CodeBuddyWebHandoff {
    client: Http,
    base: String,
    state: String,
}

/// One CodeBuddy subscription surface bound to exactly one auth origin
/// (one account realm, [INV-PROV-REALM-01]).
///
/// The `ids` it answers to (e.g. `["codebuddy-cn"]` / `["codebuddy-intl"]`)
/// and the origin are both owned by the channel crate; this type only
/// instantiates the shared vendor protocol against them.
#[derive(Debug)]
pub struct CodeBuddyRealmSurface {
    base: &'static str,
    ids: &'static [&'static str],
    /// The human-facing sign-in instruction (channel-specific wording: the
    /// domestic line signs in to the Tencent account, the international line
    /// to the CodeBuddy AI account).
    login_message: &'static str,
}

impl CodeBuddyRealmSurface {
    /// Bind the shared web-handoff engine to one realm.
    ///
    /// `base` is the bare auth origin (`https://host`), `ids` are the OAuth
    /// integration ids the engine resolves this surface by, and
    /// `login_message` is the prompt copy shown while the browser is open.
    pub const fn new(
        base: &'static str,
        ids: &'static [&'static str],
        login_message: &'static str,
    ) -> Self {
        Self {
            base,
            ids,
            login_message,
        }
    }

    /// The plugin route base for this surface's auth lane.
    pub fn route_base(&self) -> String {
        format!("{}{PLUGIN_PREFIX}", self.base)
    }

    /// The bare origin this surface's credentials are bound to.
    pub fn origin(&self) -> &'static str {
        self.base
    }
}

/// The OAuth preset shared by every realm of this vendor — a browser
/// web-handoff surface routed through the engine's vendor-custom device
/// dispatch, not a loopback redirect and not an RFC device grant.
pub fn preset(provider_id: &str) -> OAuthConfig {
    OAuthConfig {
        provider_id: provider_id.to_string().into(),
        client_id: "codebuddy-plugin".into(),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: "".into(),
        token_url: "".into(),
        device_authorization_url: "".into(),
        grant_type_device: "".into(),
        scope: "openid profile offline_access email".into(),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        // CodeBuddy never binds a local callback server: the browser opens on
        // the auth origin and the CLI polls by `state`. Keep `browser_login`
        // off so the engine does not try to bind 127.0.0.1.
        browser_login: false,
        // Declared as `Device` to route through the engine's
        // `DeviceFlowMode::Custom` branch, which is the only path dispatching
        // to the vendor surface's `begin_device_login` override (the Qoder
        // pattern). The prompt the user sees names the auth URL to open in
        // their browser — the label mismatch is cosmetic, the flow is not.
        default_login_method: LoginMethod::Device,
        oauth_host: "127.0.0.1".into(),
        oauth_port: 0,
        port_mode: PortMode::Dynamic,
        oauth_path: "/callback".into(),
        redirect_host: "127.0.0.1".into(),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::Disabled,
        token_format: TokenRequestFormat::Json,
        // Declared as a *custom* flow so only this vendor drives step 1-2.
        device_flow: DeviceFlowMode::custom("codebuddy-web-handoff"),
        device_token_url: "".into(),
        device_redirect_uri: "".into(),
    }
}

#[async_trait::async_trait]
impl OAuthProvider for CodeBuddyRealmSurface {
    fn ids(&self) -> &'static [&'static str] {
        self.ids
    }

    fn config(&self) -> OAuthConfig {
        preset(self.ids[0])
    }

    /// The CodeBuddy web-handoff flow: `auth/state` → open `authUrl` in the
    /// browser → poll `auth/token?state=` until the user completes sign-in.
    fn begin_device_login(
        &self,
        client: Http,
        _config: OAuthConfig,
        _machine_id: String,
    ) -> Option<BoxFuture<'static, Result<DeviceLogin, nuo_provider::auth::AuthError>>> {
        let base = self.base.to_string();
        let login_message = self.login_message;
        Some(Box::pin(async move {
            // 1. Mint the login session (no bearer — identity headers suppressed).
            let state_url = format!("{base}{PLUGIN_PREFIX}/auth/state?platform=CLI");
            let request = nuo_provider::transport::Request::new(netune::Method::POST, &state_url)
                .header("accept", "application/json")
                .header("X-No-Authorization", "true")
                .header("X-No-User-Id", "true")
                .header("X-No-Enterprise-Id", "true")
                .header("X-No-Department-Info", "true");
            let response = client.send(request).await.map_err(|e| {
                nuo_provider::auth::AuthError::Transport(format!("auth/state failed: {e}"))
            })?;
            let status = response.status.as_u16();
            let text = response.body;
            if !(200..300).contains(&status) {
                return Err(nuo_provider::auth::AuthError::TokenEndpoint { status, body: text });
            }
            let wrapped: CodeBuddyEnvelope<serde_json::Value> = serde_json::from_str(&text)
                .map_err(|e| {
                    nuo_provider::auth::AuthError::Decode(format!(
                        "auth/state parse failed: {e} (body: {text})"
                    ))
                })?;
            let data = wrapped.data.ok_or_else(|| {
                nuo_provider::auth::AuthError::Decode("auth/state returned no data".into())
            })?;
            let state = data.state.ok_or_else(|| {
                nuo_provider::auth::AuthError::Decode("auth/state returned no session state".into())
            })?;
            let auth_url = data.auth_url.ok_or_else(|| {
                nuo_provider::auth::AuthError::Decode("auth/state returned no authUrl".into())
            })?;

            // 2. Surface the URL so the harness can open the browser.
            let prompt = nuo_provider::auth::OAuthLoginPrompt {
                // Surface as `Device` because the engine dispatches vendor
                // flows through the `DeviceFlowMode::Custom` branch; the user
                // action is still "open this URL in the browser".
                method: LoginMethod::Device,
                url: auth_url.clone(),
                user_code: None,
                message: login_message.to_string(),
            };
            let handoff = CodeBuddyWebHandoff {
                client: client.clone(),
                base: base.clone(),
                state,
            };
            let completion = Box::pin(async move { complete_web_handoff(handoff).await });
            Ok(DeviceLogin { prompt, completion })
        }))
    }

    /// CodeBuddy's refresh: a POST with both tokens in headers, returning a
    /// wrapped pair — NOT an RFC `grant_type=refresh_token` form post.
    fn refresh_token<'a>(
        &'a self,
        client: &'a Http,
        _config: &'a OAuthConfig,
        refresh_token: &'a str,
    ) -> Option<
        BoxFuture<'a, Result<nuo_provider::auth::TokenResponse, nuo_provider::auth::AuthError>>,
    > {
        Some(Box::pin(async move {
            let envelope = refresh_at(client, self.base, refresh_token).await?;
            envelope.into_token_response()
        }))
    }
}

/// Drive one web-handoff login to completion by polling `auth/token?state=`.
///
/// Polls every [`POLL_INTERVAL_MS`] until the body's wrapped `data.data`
/// carries a usable token pair. An empty `data.data` means *still pending* —
/// the CLI's own semantic while the user has not finished the web sign-in.
async fn complete_web_handoff(
    handoff: CodeBuddyWebHandoff,
) -> Result<nuo_provider::auth::TokenResponse, nuo_provider::auth::AuthError> {
    use nuo_provider::auth::AuthError;
    let CodeBuddyWebHandoff {
        state,
        client,
        base,
    } = handoff;
    let deadline = tokio::time::Instant::now() + LOGIN_DEADLINE;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(AuthError::Timeout);
        }
        let url = format!("{base}{PLUGIN_PREFIX}/auth/token?state={state}");
        let request = nuo_provider::transport::Request::new(netune::Method::GET, &url)
            .header("accept", "application/json")
            .header("X-No-Authorization", "true")
            .header("X-No-User-Id", "true")
            .header("X-No-Enterprise-Id", "true")
            .header("X-No-Department-Info", "true");
        // Per-request control-plane budget (the 10s `Http::control_plane` shape).
        let response = tokio::time::timeout(Duration::from_secs(10), client.send(request))
            .await
            .map_err(|_| AuthError::Transport("auth/token poll timed out".into()))?
            .map_err(|e| AuthError::Transport(format!("auth/token failed: {e}")))?;
        let status = response.status.as_u16();
        let text = response.body;
        if !(200..300).contains(&status) {
            return Err(AuthError::TokenEndpoint { status, body: text });
        }
        let wrapped: CodeBuddyEnvelope<CodeBuddyAuthEnvelope> = serde_json::from_str(&text)
            .map_err(|e| {
                AuthError::Decode(format!("auth/token parse failed: {e} (body: {text})"))
            })?;
        if let Some(inner) = wrapped.data.and_then(|d| d.data) {
            return inner.into_token_response();
        }
        // Still pending — keep polling.
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

/// Rotate one CodeBuddy token pair via `POST {base}{PLUGIN_PREFIX}/auth/token/refresh`.
pub async fn refresh_at(
    client: &Http,
    base: &str,
    refresh_token: &str,
) -> Result<CodeBuddyAuthEnvelope, nuo_provider::auth::AuthError> {
    use nuo_provider::auth::AuthError;
    let url = format!("{base}{PLUGIN_PREFIX}/auth/token/refresh");
    let request = nuo_provider::transport::Request::new(netune::Method::POST, &url)
        .header("accept", "application/json")
        .header("X-Refresh-Token", refresh_token)
        .header("X-Auth-Refresh-Source", "plugin");
    let response = client
        .send(request)
        .await
        .map_err(|e| AuthError::Transport(format!("auth/token/refresh failed: {e}")))?;
    let status = response.status.as_u16();
    let text = response.body;
    if !(200..300).contains(&status) {
        return Err(AuthError::TokenEndpoint { status, body: text });
    }
    let wrapped: CodeBuddyEnvelope<CodeBuddyAuthEnvelope> =
        serde_json::from_str(&text).map_err(|e| {
            AuthError::Decode(format!(
                "auth/token/refresh parse failed: {e} (body: {text})"
            ))
        })?;
    wrapped
        .data
        .and_then(|d| d.data)
        .ok_or_else(|| AuthError::Decode("auth/token/refresh returned no token pair".into()))
}
