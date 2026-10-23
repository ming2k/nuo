//! Tencent CodeBuddy OAuth subscription surface (ADR-0027).
//!
//! Reverse-engineered from `@tencent-ai/codebuddy-code` v2.172.1
//! (dist/codebuddy.js account-scoped auth service + the persisted
//! `Tencent-Cloud.coding-copilot.info` credential shape on 2026-10-23).
//!
//! CodeBuddy's login is **not** a standard RFC 8628 device grant and **not** a
//! loopback PKCE redirect — it is a *browser web-handoff + state polling*
//! protocol (the CLI launches the user's default browser and never receives a
//! local callback):
//!
//! 1. `POST {origin}/v2/plugin/auth/state?platform=CLI` (no bearer) — mints an
//!    anonymous login session and returns `data.authUrl` (the page the user
//!    signs in on) plus `data.state`, the correlation key for step 2.
//! 2. `GET {origin}/v2/plugin/auth/token?state={state}` — polled (with the
//!    same no-identity header block) until the web sign-in completes, then
//!    returns the wrapped pair `data.data.{accessToken, refreshToken,
//!    tokenType, expiresIn, refreshExpiresIn, scope, sessionState,
//!    notBeforePolicy, domain}`.
//! 3. `POST {origin}/v2/plugin/auth/token/refresh` with `X-Refresh-Token` +
//!    `X-Auth-Refresh-Source: plugin` headers — rotates both tokens and
//!    returns the same wrapped shape.
//!
//! Model requests speak `Authorization: Bearer <accessToken>` plus enterprise
//! identity headers (`X-User-Id`, `X-Enterprise-Id`, `X-Id-Source`,
//! `X-Userinfo`, `X-Auth-Method`). See `credential.rs` for the full header
//! projection and the API-key fallback lane.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use nuo_egress::http::Http;
use nuo_host::SecretString;
use nuo_oauth::oauth::AuthError;
use nuo_oauth::oauth::token::TokenResponse;
use nuo_oauth::{
    ClientAuthMethod, DeviceFlowMode, DeviceLogin, OAuthConfig, OAuthLoginPrompt, OAuthProvider,
    OAuthTokenEnricher, PkceMode, PortMode, TokenRequestFormat,
};
use nuo_provider::LoginMethod;
use nuo_provider::credentials::TokenSet;

/// The domestic (Tencent Cloud / copilot) auth origin.
pub const CODEBUDDY_CN_ORIGIN: &str = "https://copilot.tencent.com";
/// The international (codebuddy.ai) auth origin.
pub const CODEBUDDY_INTL_ORIGIN: &str = "https://www.codebuddy.ai";

/// Route base every auth surface path is prefixed with.
pub const PLUGIN_PREFIX: &str = "/v2/plugin";

/// Interval between state/token polls (the CLI's own value is ~1s).
const POLL_INTERVAL_MS: u64 = 1_000;
/// Total login deadline for the web-handoff wait (the CLI's own budget).
const LOGIN_DEADLINE: Duration = Duration::from_secs(300);

/// The wrapped credential payload CodeBuddy returns from state-login and refresh.
///
/// Field names are quoted exactly as the server emits them; the lowercase
/// serde renames align them with the RFC-facing names `TokenResponse` uses.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CodeBuddyAuthEnvelope {
    #[serde(rename = "accessToken")]
    pub access_token: SecretString,
    #[serde(rename = "refreshToken", default)]
    pub refresh_token: Option<SecretString>,
    #[serde(rename = "tokenType", default)]
    pub token_type: Option<String>,
    #[serde(rename = "expiresIn", default)]
    pub expires_in: Option<u64>,
    #[serde(rename = "refreshExpiresIn", default)]
    pub refresh_expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(rename = "sessionState", default)]
    pub session_state: Option<String>,
    #[serde(rename = "notBeforePolicy", default)]
    pub not_before_policy: Option<u64>,
    /// The account line the credential belongs to (e.g. `www.codebuddy.cn`).
    #[serde(default)]
    pub domain: Option<String>,
}

/// The `data: { data: … }` / `data: { authUrl, state, data: … }` wrapper
/// CodeBuddy's auth responses arrive in.
#[derive(Debug, serde::Deserialize)]
#[serde(bound(deserialize = "T: serde::de::DeserializeOwned"))]
struct CodeBuddyEnvelope<T> {
    #[serde(rename = "data")]
    pub data: Option<CodeBuddyData<T>>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(bound(deserialize = "T: serde::de::DeserializeOwned"))]
struct CodeBuddyData<T> {
    /// `authUrl` / `state` sit at the *outer* `data` level for `/auth/state`.
    #[serde(rename = "authUrl", default)]
    pub auth_url: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// The wrapped credential pair sits one level deeper.
    #[serde(default)]
    pub data: Option<T>,
}

/// A begun CodeBuddy web-handoff login: the browser URL plus the poll handle.
struct CodeBuddyWebHandoff {
    client: Http,
    base: String,
    state: String,
}

/// One subscription surface (domestic or international line).
pub struct CodeBuddyOAuthProvider {
    base: &'static str,
    ids: &'static [&'static str],
}

impl CodeBuddyOAuthProvider {
    pub const fn domestic() -> Self {
        Self {
            base: CODEBUDDY_CN_ORIGIN,
            ids: &["codebuddy"],
        }
    }

    pub const fn international() -> Self {
        Self {
            base: CODEBUDDY_INTL_ORIGIN,
            ids: &["codebuddy-intl"],
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

/// CodeBuddy preset — a browser web-handoff surface routed through the
/// vendor-custom device dispatch, not a loopback redirect and not an RFC
/// device grant.
pub fn preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: "codebuddy".into(),
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

/// The domestic (Tencent Cloud) preset: hosted at `copilot.tencent.com`.
pub fn cn_preset() -> OAuthConfig {
    preset()
}

/// The international (codebuddy.ai) preset.
pub fn intl_preset() -> OAuthConfig {
    let mut cfg = preset();
    cfg.provider_id = "codebuddy-intl".into();
    cfg
}

#[async_trait]
impl OAuthProvider for CodeBuddyOAuthProvider {
    fn ids(&self) -> &'static [&'static str] {
        self.ids
    }

    fn config(&self) -> OAuthConfig {
        let mut cfg = preset();
        cfg.provider_id = self.ids[0].into();
        cfg
    }

    /// The CodeBuddy web-handoff flow: `auth/state` → open `authUrl` in the
    /// browser → poll `auth/token?state=` until the user completes sign-in.
    fn begin_device_login(
        &self,
        client: Http,
        _config: OAuthConfig,
        _machine_id: String,
    ) -> Option<BoxFuture<'static, Result<DeviceLogin, AuthError>>> {
        let base = self.base.to_string();
        Some(Box::pin(async move {
            // 1. Mint the login session (no bearer — identity headers suppressed).
            let state_url = format!("{base}{PLUGIN_PREFIX}/auth/state?platform=CLI");
            let request = nuo_egress::http::Request::new(netune::Method::POST, &state_url)
                .header("accept", "application/json")
                .header("X-No-Authorization", "true")
                .header("X-No-User-Id", "true")
                .header("X-No-Enterprise-Id", "true")
                .header("X-No-Department-Info", "true");
            let response = client
                .send(request)
                .await
                .map_err(|e| AuthError::Transport(format!("auth/state failed: {e}")))?;
            let status = response.status.as_u16();
            let text = response.body;
            if !(200..300).contains(&status) {
                return Err(AuthError::TokenEndpoint { status, body: text });
            }
            let wrapped: CodeBuddyEnvelope<serde_json::Value> = serde_json::from_str(&text)
                .map_err(|e| {
                    AuthError::Decode(format!("auth/state parse failed: {e} (body: {text})"))
                })?;
            let data = wrapped
                .data
                .ok_or_else(|| AuthError::Decode("auth/state returned no data".into()))?;
            let state = data
                .state
                .ok_or_else(|| AuthError::Decode("auth/state returned no session state".into()))?;
            let auth_url = data
                .auth_url
                .ok_or_else(|| AuthError::Decode("auth/state returned no authUrl".into()))?;

            // 2. Surface the URL so the harness can open the browser.
            let prompt = OAuthLoginPrompt {
                // Surface as `Device` because the engine dispatches vendor
                // flows through the `DeviceFlowMode::Custom` branch; the user
                // action is still "open this URL in the browser".
                method: LoginMethod::Device,
                url: auth_url.clone(),
                user_code: None,
                message: "Open the URL and sign in to your Tencent CodeBuddy account.".into(),
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
    ) -> Option<BoxFuture<'a, Result<TokenResponse, AuthError>>> {
        let base = self.base.to_string();
        Some(Box::pin(async move {
            let envelope = refresh_at(client, &base, refresh_token).await?;
            envelope.into_token_response()
        }))
    }
}

/// Drive one web-handoff login to completion by polling `auth/token?state=`.
///
/// Polls every [`POLL_INTERVAL_MS`] until the body's wrapped `data.data`
/// carries a usable token pair. An empty `data.data` means *still pending* —
/// the CLI's own semantic while the user has not finished the web sign-in.
async fn complete_web_handoff(handoff: CodeBuddyWebHandoff) -> Result<TokenResponse, AuthError> {
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
        let request = nuo_egress::http::Request::new(netune::Method::GET, &url)
            .header("accept", "application/json")
            .header("X-No-Authorization", "true")
            .header("X-No-User-Id", "true")
            .header("X-No-Enterprise-Id", "true")
            .header("X-No-Department-Info", "true");
        let response = tokio::time::timeout(LoginDeadline::ROUND_BUDGET, client.send(request))
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

/// Named poll-round budgets so the deadline maths above reads.
struct LoginDeadline;
impl LoginDeadline {
    /// Per-request control-plane budget (the 10s `Http::control_plane` shape).
    const ROUND_BUDGET: Duration = Duration::from_secs(10);
}

impl CodeBuddyAuthEnvelope {
    /// Lift the wrapped payload into the RFC `TokenResponse` the engine hands
    /// to the enricher.
    pub fn into_token_response(self) -> Result<TokenResponse, AuthError> {
        if self.access_token.expose_secret().trim().is_empty() {
            return Err(AuthError::Decode(
                "codebuddy auth returned an empty access_token".into(),
            ));
        }
        Ok(TokenResponse {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            id_token: None,
            token_type: self.token_type,
            expires_in: self.expires_in,
            scope: self.scope,
            qoder_uid: None,
        })
    }
}

/// Rotate one CodeBuddy token pair via `POST {base}{PLUGIN_PREFIX}/auth/token/refresh`.
pub async fn refresh_at(
    client: &Http,
    base: &str,
    refresh_token: &str,
) -> Result<CodeBuddyAuthEnvelope, AuthError> {
    let url = format!("{base}{PLUGIN_PREFIX}/auth/token/refresh");
    let request = nuo_egress::http::Request::new(netune::Method::POST, &url)
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

/// The CodeBuddy enricher: pins the durable `codebuddy` attribute — the auth
/// origin this credential is bound to — so a later refresh stays on the line
/// that minted it (mixing domestic with international invalidates the pair).
#[derive(Debug, Clone, Copy)]
pub struct CodeBuddyOAuthEnricher {
    base: &'static str,
}

impl CodeBuddyOAuthEnricher {
    pub const fn new(base: &'static str) -> Self {
        Self { base }
    }
}

const ORIGIN_ATTR: &str = "codebuddy_origin";

#[async_trait]
impl OAuthTokenEnricher for CodeBuddyOAuthEnricher {
    async fn on_login_success(
        &self,
        _client: &Http,
        _connection_label: &str,
        _tokens: &TokenResponse,
        _previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        token_set.set_attr(ORIGIN_ATTR, self.base);
        Ok(())
    }

    async fn on_refresh_success(
        &self,
        _client: &Http,
        _stored: &TokenSet,
        _refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        // The refresh endpoint does not repeat the origin; replay the pinned one.
        token_set.set_attr(ORIGIN_ATTR, self.base);
        Ok(())
    }
}

/// Read the stored auth origin for an existing credential (the line the
/// original login minted), defaulting to the domestic surface.
pub fn stored_origin(stored: &TokenSet, default: &'static str) -> String {
    stored
        .get_attr(ORIGIN_ATTR)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default.to_string())
}

/// The provider surfaces this crate registers.
pub fn providers() -> Vec<Arc<dyn OAuthProvider>> {
    vec![
        Arc::new(CodeBuddyOAuthProvider::domestic()),
        Arc::new(CodeBuddyOAuthProvider::international()),
    ]
}

/// Test-only: drive the wrapped-envelope lift through an offline path.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domestic_surface_routes_against_copilot_tencent_com() {
        let provider = CodeBuddyOAuthProvider::domestic();
        assert_eq!(provider.ids(), &["codebuddy"]);
        assert_eq!(
            provider.route_base(),
            "https://copilot.tencent.com/v2/plugin"
        );
        assert_eq!(provider.origin(), "https://copilot.tencent.com");
    }

    #[test]
    fn international_surface_uses_codebuddy_ai() {
        let provider = CodeBuddyOAuthProvider::international();
        assert_eq!(provider.ids(), &["codebuddy-intl"]);
        assert_eq!(provider.route_base(), "https://www.codebuddy.ai/v2/plugin");
        assert_eq!(provider.origin(), "https://www.codebuddy.ai");
    }

    #[test]
    fn preset_declares_browser_web_handoff_not_rfc_device() {
        let cfg = preset();
        assert_eq!(
            cfg.device_flow,
            DeviceFlowMode::custom("codebuddy-web-handoff")
        );
        // The engine dispatches vendor flows only through the `Device`
        // branch (`DeviceFlowMode::Custom`), so the configured default
        // *is* `Device` — but no loopback server binds and no RFC device
        // code is ever requested; the URL is the vendor's browser URL.
        assert_eq!(cfg.default_login_method, LoginMethod::Device);
        assert!(!cfg.browser_login, "no loopback callback server is bound");
        assert_eq!(cfg.pkce_mode, PkceMode::Disabled);
        assert_eq!(cfg.scope, "openid profile offline_access email");
    }

    #[test]
    fn surfaces_answer_device_method_only() {
        // Mirrors `connection_auth.supports_login_method`: the vendor surface
        // has no device code to poll, so a pure Device request would only
        // work through the custom vendor branch. Drive through the trait.
        let domestic = CodeBuddyOAuthProvider::domestic();
        let cfg = domestic.config();
        assert_eq!(cfg.provider_id, "codebuddy");
        assert!(matches!(cfg.device_flow, DeviceFlowMode::Custom(_)));
    }

    #[test]
    fn wrapped_envelope_parses_the_full_shape() {
        let sample = r#"{
            "code": 0,
            "data": { "data": {
                "accessToken": "eyJtest-access",
                "refreshToken": "eyJtest-refresh",
                "tokenType": "Bearer",
                "expiresIn": 5184000,
                "refreshExpiresIn": 7776000,
                "scope": "openid profile offline_access email",
                "sessionState": "abc",
                "notBeforePolicy": 1724292326,
                "domain": "www.codebuddy.cn"
            }}
        }"#;
        let wrapped: CodeBuddyEnvelope<CodeBuddyAuthEnvelope> =
            serde_json::from_str(sample).expect("parses");
        let inner = wrapped.data.expect("data").data.expect("inner");
        // Mirrors the live `Tencent-Cloud.coding-copilot.info` capture:
        // ~60-day access / ~90-day refresh.
        assert_eq!(inner.expires_in, Some(5_184_000));
        assert_eq!(inner.refresh_expires_in, Some(7_776_000));
        assert_eq!(inner.token_type.as_deref(), Some("Bearer"));
        assert_eq!(
            inner.scope.as_deref(),
            Some("openid profile offline_access email")
        );
        assert_eq!(inner.domain.as_deref(), Some("www.codebuddy.cn"));
        let response = inner.into_token_response().expect("lifts");
        assert_eq!(response.access_token.expose_secret(), "eyJtest-access");
        assert_eq!(
            response
                .refresh_token
                .as_ref()
                .map(|r| r.expose_secret())
                .unwrap_or_default(),
            "eyJtest-refresh"
        );
    }

    #[test]
    fn empty_access_token_is_rejected() {
        let sample = r#"{"accessToken": "", "refreshToken": "rt"}"#;
        let parsed: CodeBuddyAuthEnvelope = serde_json::from_str(sample).expect("parses");
        let result = parsed.into_token_response();
        assert!(result.is_err());
    }

    #[test]
    fn enricher_pins_origin_attribute() {
        let enricher = CodeBuddyOAuthEnricher::new(CODEBUDDY_INTL_ORIGIN);
        let mut token_set = TokenSet {
            access: SecretString::from("at"),
            refresh: String::new().into(),
            expires_ms: i64::MAX,
            id_token: None,
            token_type: None,
            scope: None,
            user_email: None,
            attributes: Default::default(),
        };
        let tokens = TokenResponse {
            access_token: SecretString::from("at"),
            refresh_token: None,
            id_token: None,
            token_type: None,
            expires_in: None,
            scope: None,
            qoder_uid: None,
        };
        // The traits are async; drive through a minimal single-threaded run.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let client = Http::control_plane().unwrap();
        let stored = token_set.clone();
        rt.block_on(async {
            enricher
                .on_login_success(&client, "conn", &tokens, None, &mut token_set)
                .await
                .unwrap();
            enricher
                .on_refresh_success(&client, &stored, &tokens, &mut token_set)
                .await
                .unwrap();
        });
        assert_eq!(
            stored_origin(&token_set, CODEBUDDY_CN_ORIGIN),
            CODEBUDDY_INTL_ORIGIN
        );
    }

    #[test]
    fn stored_origin_falls_back_to_domestic() {
        let token_set = TokenSet {
            access: SecretString::from("at"),
            refresh: String::new().into(),
            expires_ms: i64::MAX,
            id_token: None,
            token_type: None,
            scope: None,
            user_email: None,
            attributes: Default::default(),
        };
        assert_eq!(
            stored_origin(&token_set, CODEBUDDY_CN_ORIGIN),
            CODEBUDDY_CN_ORIGIN
        );
    }
}
