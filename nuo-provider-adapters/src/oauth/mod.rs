//! OAuth2 + PKCE authentication engine & client emulator.
//!
//! muta's OAuth subsystem provides an ultra-flexible, industrial-grade architecture
//! supporting:
//! - Multi-provider presets (Google Antigravity, OpenAI Codex, xAI SuperGrok, GitHub Copilot)
//! - Dynamic client emulation (custom client IDs, secrets, endpoints, headers, PKCE modes, port strategies)
//! - Dynamic loopback port binding (with automatic fallback on busy ports)
//! - Headless / SSH manual code and redirect URL parsing (solving Google OOB deprecation)
//! - Automatic token refreshing with JWT exp inspection, skew margins, single-flight locking, and atomic persistence.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use nuo_provider_transport::oauth::{browser, device, pkce, presets, AuthError};
pub mod chatgpt_device;
pub mod credential_source;
pub mod device_identity;
pub mod host;
pub mod manual;
pub mod opencode_device;
pub use nuo_provider_qoder::oauth as qoder;
pub mod store;
pub mod token;

pub use browser::{CallbackOutcome, CallbackServer};
pub use chatgpt_device::{
    ChatGptDeviceCode, ChatGptDeviceToken, exchange_device_code as exchange_chatgpt_device_code,
    poll_device_code as poll_chatgpt_device_code,
    request_device_code as request_chatgpt_device_code,
    verification_url as chatgpt_verification_url,
};
pub mod enricher;
pub use credential_source::OAuthCredentialSource;
pub use device::{DeviceCodeResponse, poll_device_code, request_device_code};
pub use device_identity::{DeviceIdentity, FileDeviceIdentity, PerProcessIdentity};
pub use host::CredentialHost;
pub use enricher::*;
pub use manual::parse_authorization_response;
pub use nuo_model_codec::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, OAuthConfigBuilder, PkceMode, PortMode,
    TokenRequestFormat,
};
pub use opencode_device::{
    OpencodeDeviceCode, fetch_orgs as fetch_opencode_orgs, fetch_user as fetch_opencode_user,
    poll_device_code as poll_opencode_device_code,
    poll_device_code_with as poll_opencode_device_code_with,
    request_device_code as request_opencode_device_code,
};
pub use pkce::{PkceCodes, new_nonce, new_state};
pub use presets::*;
pub use qoder::QoderApiKeyCredentialSource;
pub use store::{
    CredentialSession, CredentialStore, CredentialStoreError, FileCredentialStore,
    InMemoryCredentialStore, QoderStoredIdentity, TokenSet,
};

/// The persisted Qoder request identity for a connection, when one exists.
///
/// The uid backfill lives in the credential sources (both
/// [`QoderApiKeyCredentialSource`] and the generic
/// [`OAuthCredentialSource`](super::OAuthCredentialSource), which Qoder's
/// `QoderOAuth` connections use). This accessor is a read used by the catalog
/// layer after that resolution has run; a still-empty uid means resolution
/// failed, and the signed fetch fails closed with the upstream's own error.
///
/// `None` when the connection has no stored Qoder identity.
pub fn stored_qoder_request_identity(
    store: &dyn CredentialStore,
    connection_id: &str,
) -> Option<crate::registry::qoder::QoderRequestIdentity> {
    store
        .read(connection_id)
        .ok()??
        .get_json_attr::<crate::registry::qoder::QoderStoredIdentity>("qoder")
        .map(|q| q.to_request_identity())
}
pub use token::{
    ACCESS_TOKEN_REFRESH_SKEW_MS, ANTIGRAVITY_LOAD_CODE_ASSIST_URL, ANTIGRAVITY_ONBOARD_USER_URL,
    ANTIGRAVITY_USER_AGENT, GOOGLE_USERINFO_URL, GoogleUserInfo, TokenResponse,
    access_token_expiry_ms, access_token_is_expiring, build_authorize_url, chatgpt_account_id,
    exchange_code, fetch_google_userinfo, jwt_exp_ms, refresh_access_token,
    resolve_antigravity_project,
};

pub use nuo_model_codec::LoginMethod;
use nuo_host::SecretString;
use std::sync::{Arc, Mutex};

const OAUTH_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const DEVICE_LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// User-facing authorization material produced before an OAuth flow waits for
/// completion. Frontends render this one shape for both PKCE and device-code
/// sessions; a browser flow has no `user_code`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthLoginPrompt {
    pub method: LoginMethod,
    pub url: String,
    pub user_code: Option<String>,
    pub message: String,
}

/// An initiated OAuth flow. Starting and completing are deliberately separate:
/// the caller must be able to show/open the authorization URL before the flow
/// blocks on a localhost callback or device-token poll.
pub struct OAuthLoginSession {
    prompt: OAuthLoginPrompt,
    client: crate::http::Http,
    flow: OAuthLoginFlow,
}

enum OAuthLoginFlow {
    Browser(BrowserLogin),
    RfcDevice {
        config: OAuthConfig,
        device: DeviceCodeResponse,
    },
    ChatGptDevice {
        config: OAuthConfig,
        device: ChatGptDeviceCode,
    },
    OpencodeDevice {
        config: OAuthConfig,
        device: OpencodeDeviceCode,
    },
    QoderDevice {
        session: crate::oauth::qoder::QoderDeviceSession,
    },
}

impl OAuthLoginSession {
    pub fn prompt(&self) -> &OAuthLoginPrompt {
        &self.prompt
    }

    /// Wait for authorization and exchange the resulting grant for tokens.
    pub async fn complete(self) -> Result<TokenResponse, AuthError> {
        match self.flow {
            OAuthLoginFlow::Browser(login) => login.complete(&self.client).await,
            OAuthLoginFlow::RfcDevice { config, device } => tokio::time::timeout(
                DEVICE_LOGIN_TIMEOUT,
                poll_device_code(&self.client, &config, &device),
            )
            .await
            .map_err(|_| AuthError::Timeout)?,
            OAuthLoginFlow::ChatGptDevice { config, device } => {
                tokio::time::timeout(DEVICE_LOGIN_TIMEOUT, async {
                    let token = poll_chatgpt_device_code(&self.client, &config, &device).await?;
                    exchange_chatgpt_device_code(&self.client, &config, &token).await
                })
                .await
                .map_err(|_| AuthError::Timeout)?
            }
            OAuthLoginFlow::OpencodeDevice { config, device } => tokio::time::timeout(
                DEVICE_LOGIN_TIMEOUT,
                poll_opencode_device_code(&self.client, &config, &device),
            )
            .await
            .map_err(|_| AuthError::Timeout)?,
            OAuthLoginFlow::QoderDevice { session } => {
                tokio::time::timeout(DEVICE_LOGIN_TIMEOUT, async {
                    // Poll → the `dt-` device token. That token IS the
                    // inference credential: qodercli adopts it directly as
                    // the COSY bearer (`refreshStrategy="device-token"`) and
                    // rotates it lazily via /api/v1/deviceToken/refresh with
                    // the `drt-` device refresh token. Exchanging a `dt-`
                    // token against /api/v1/jobToken/exchange as a
                    // `personal_token` fails with HTTP 400 BadRequest — that
                    // endpoint only accepts `pt-` personal-access tokens.
                    crate::oauth::qoder::device_login(&self.client, &session).await
                })
                .await
                .map_err(|_| AuthError::Timeout)?
            }
        }
    }
}

/// Construct a fully enriched [`TokenSet`] from a successful OAuth login response.
pub async fn build_token_set_from_login(
    oauth: &OAuth,
    connection_label: &str,
    tokens: TokenResponse,
    now_ms: i64,
) -> TokenSet {
    let fallback = TokenSet {
        access: tokens.access_token.clone(),
        refresh: tokens.refresh_token.clone().unwrap_or_default(),
        expires_ms: 0,
        id_token: tokens.id_token.clone(),
        token_type: tokens.token_type.clone(),
        scope: tokens.scope.clone(),
        user_email: None,
        attributes: serde_json::Map::new(),
    };
    build_token_set_with_enricher(
        oauth.client(),
        oauth.config(),
        connection_label,
        tokens,
        oauth.store().read(connection_label).ok().flatten(),
        now_ms,
    )
    .await
    .unwrap_or(fallback)
}

/// The high-level OAuth orchestrator.
#[derive(Clone)]
pub struct OAuth {
    config: OAuthConfig,
    client: crate::http::Http,
    /// Where the durable material this flow touches lives (ADR-0300 §1): the
    /// token store every refresh rotates, and the device identity a
    /// device-pinned flow presents.
    host: CredentialHost,
    refresh_in_flight: Arc<RefreshSlot>,
}

type RefreshSlot = Mutex<Option<Arc<tokio::sync::Mutex<Option<TokenSet>>>>>;

impl OAuth {
    /// Construct with a provider configuration and the host's durable material.
    #[allow(clippy::expect_used)]
    pub fn new(config: OAuthConfig, host: CredentialHost) -> Self {
        // The owned transport deliberately has no client-wide timeout (a
        // streaming turn must not be cut), so the OAuth handle carries the
        // whole-request deadline for these short control-plane flows.
        let client = crate::http::Http::new(OAUTH_REQUEST_TIMEOUT).expect(
            "OAuth HTTP client configuration is static and valid; client builder must succeed",
        );
        Self::with_client(config, client, host)
    }

    /// Construct with a provider configuration, pre-configured HTTP client, and
    /// the host's durable material.
    pub fn with_client(config: OAuthConfig, client: crate::http::Http, host: CredentialHost) -> Self {
        Self {
            config,
            client,
            host,
            refresh_in_flight: Arc::new(Mutex::new(None)),
        }
    }

    /// The host material this flow was constructed with.
    pub fn host(&self) -> &CredentialHost {
        &self.host
    }

    /// The host's credential store.
    pub fn store(&self) -> &std::sync::Arc<dyn CredentialStore> {
        self.host.store()
    }

    /// Human-facing message for a failed login, applying vendor-specific framing.
    pub fn format_login_error(&self, error: &AuthError) -> String {
        enricher_for_config(&self.config).format_login_error(error)
    }

    /// Convenience constructor for Google Antigravity.
    pub fn google_antigravity(host: CredentialHost) -> Self {
        Self::new(google_antigravity_preset(), host)
    }

    /// Convenience constructor for xAI SuperGrok.
    pub fn xai(host: CredentialHost) -> Self {
        Self::new(xai_preset(), host)
    }

    /// Convenience constructor for ChatGPT/Codex.
    pub fn chatgpt(host: CredentialHost) -> Self {
        Self::new(chatgpt_preset(), host)
    }

    /// Convenience constructor for GitHub Copilot.
    pub fn copilot(host: CredentialHost) -> Self {
        Self::new(copilot_preset(), host)
    }

    /// Convenience constructor for Alibaba Qoder.
    pub fn qoder(host: CredentialHost) -> Self {
        Self::new(qoder_preset(), host)
    }

    /// Convenience constructor for the OpenCode Console account.
    pub fn opencode(host: CredentialHost) -> Self {
        Self::new(opencode_preset(), host)
    }

    /// The provider config this OAuth instance authenticates against.
    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    /// Borrow the HTTP client.
    pub fn client(&self) -> &crate::http::Http {
        &self.client
    }

    /// Run an OAuth login flow and return the resulting token response.
    pub async fn login(&self, method: LoginMethod) -> Result<TokenResponse, AuthError> {
        self.begin_login(method).await?.complete().await
    }

    /// Initiate either generic PKCE/browser login or the configured device
    /// grant and return a common pending-session handle.
    pub async fn begin_login(&self, method: LoginMethod) -> Result<OAuthLoginSession, AuthError> {
        if !self.config.supports_login_method(method) {
            return Err(AuthError::Transport(format!(
                "{} login is not supported for {}",
                match method {
                    LoginMethod::Browser => "browser PKCE",
                    LoginMethod::Device => "device-code",
                },
                self.config.provider_id
            )));
        }
        let (prompt, flow) = match method {
            LoginMethod::Browser => {
                let login = self.begin_browser_login().await?;
                let prompt = OAuthLoginPrompt {
                    method,
                    url: login.url.clone(),
                    user_code: None,
                    message: "Complete authorization in your browser (or open the link below)."
                        .to_string(),
                };
                (prompt, OAuthLoginFlow::Browser(login))
            }
            LoginMethod::Device => match &self.config.device_flow {
                DeviceFlowMode::Rfc8628 => {
                    let device = request_device_code(&self.client, &self.config).await?;
                    let prompt = OAuthLoginPrompt {
                        method,
                        url: device.user_url().to_string(),
                        user_code: Some(device.user_code.clone()),
                        message: "Open the URL on any device and enter the code to authorize."
                            .to_string(),
                    };
                    (
                        prompt,
                        OAuthLoginFlow::RfcDevice {
                            config: self.config.clone(),
                            device,
                        },
                    )
                }
                DeviceFlowMode::Custom(flow) => {
                    match flow.as_ref() {
                        "chatgpt" => {
                            let device =
                                request_chatgpt_device_code(&self.client, &self.config).await?;
                            let prompt = OAuthLoginPrompt {
                                method,
                                url: device.user_url(&self.config),
                                user_code: Some(device.user_code.clone()),
                                message:
                                    "Open the URL on any device and enter the code to authorize."
                                        .to_string(),
                            };
                            (
                                prompt,
                                OAuthLoginFlow::ChatGptDevice {
                                    config: self.config.clone(),
                                    device,
                                },
                            )
                        }
                        "opencode" => {
                            let device = request_opencode_device_code(&self.client, &self.config)
                                .await?;
                            let prompt = OAuthLoginPrompt {
                                method,
                                url: device.user_url(&self.config),
                                user_code: Some(device.user_code.clone()),
                                message:
                                    "Open the URL on any device and enter the code to authorize."
                                        .to_string(),
                            };
                            (
                                prompt,
                                OAuthLoginFlow::OpencodeDevice {
                                    config: self.config.clone(),
                                    device,
                                },
                            )
                        }
                        "qoder" => {
                            let machine_id = self.host.device().stable_id();
                            let session =
                                crate::oauth::qoder::QoderDeviceSession::with_authorize_url(
                                    &machine_id,
                                    self.config.client_id.as_ref(),
                                    self.config.authorize_url.as_ref(),
                                );
                            let prompt = OAuthLoginPrompt {
                                method,
                                url: session.user_url(),
                                user_code: None,
                                message:
                                    "Open the URL and approve this device in your Qoder account."
                                        .to_string(),
                            };
                            (prompt, OAuthLoginFlow::QoderDevice { session })
                        }
                        other => {
                            return Err(AuthError::DeviceCode(format!(
                                "custom device flow '{other}' not found"
                            )));
                        }
                    }
                }
                DeviceFlowMode::Disabled => {
                    unreachable!("support checked above")
                }
            },
        };
        Ok(OAuthLoginSession {
            prompt,
            client: self.client.clone(),
            flow,
        })
    }

    /// Start the browser PKCE flow and return the authorize URL plus callback
    /// state. Prefer [`Self::begin_login`] in application code so both login
    /// families share the same orchestration path.
    pub async fn begin_browser_login(&self) -> Result<BrowserLogin, AuthError> {
        let server = CallbackServer::start_for(&self.config)
            .await
            .map_err(|e| AuthError::Transport(format!("could not bind loopback server: {e}")))?;
        let bound_port = server.bound_port();
        let redirect = self.config.redirect_uri(Some(bound_port));
        let pkce = PkceCodes::generate();
        let state = new_state();
        let nonce = new_nonce();
        let url = build_authorize_url(&self.config, &pkce, &state, &nonce, &redirect);
        tracing::info!(
            url = %url,
            provider = %self.config.provider_id,
            bound_port = bound_port,
            "open this URL to authorize"
        );
        let rx = server.wait_for_code(state.clone());
        Ok(BrowserLogin {
            config: self.config.clone(),
            url,
            state,
            nonce,
            pkce,
            redirect,
            rx,
            _server: server,
        })
    }

    /// Exchange an authorization code obtained manually (e.g. pasted into CLI in headless mode).
    pub async fn exchange_manual_code(
        &self,
        code: &str,
        pkce: &PkceCodes,
        redirect_uri: &str,
    ) -> Result<TokenResponse, AuthError> {
        exchange_code(&self.client, &self.config, code, pkce, redirect_uri).await
    }

    /// Resolve a live access token from a stored [`TokenSet`], refreshing it
    /// if expiring. Concurrency-safe: concurrent callers share one single-flight HTTP call.
    pub async fn resolve_access_token(
        &self,
        stored: TokenSet,
    ) -> Result<(SecretString, TokenSet), AuthError> {
        let now = now_ms();
        if !access_token_is_expiring(
            Some(stored.access.expose_secret()),
            ACCESS_TOKEN_REFRESH_SKEW_MS,
            now,
        ) && stored.expires_ms > now + ACCESS_TOKEN_REFRESH_SKEW_MS
        {
            return Ok((stored.access.clone(), stored));
        }
        self.force_resolve_access_token(stored).await
    }

    /// Force a fresh token exchange with upstream regardless of remaining expiration TTL.
    /// Concurrency-safe: concurrent callers share one single-flight HTTP call.
    pub async fn force_resolve_access_token(
        &self,
        stored: TokenSet,
    ) -> Result<(SecretString, TokenSet), AuthError> {
        if stored.refresh.is_empty() {
            return Err(AuthError::Authorization(
                "the access token expired and no refresh token is available; reconnect this connection"
                    .to_string(),
            ));
        }
        let guard = {
            let mut slot = self
                .refresh_in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(shared) = slot.as_ref() {
                Arc::clone(shared)
            } else {
                let shared = Arc::new(tokio::sync::Mutex::new(None));
                *slot = Some(Arc::clone(&shared));
                shared
            }
        };

        let mut inner = guard.lock().await;
        if let Some(refreshed) = inner.as_ref() {
            return Ok((refreshed.access.clone(), refreshed.clone()));
        }

        let refreshed =
            refresh_access_token(&self.client, &self.config, stored.refresh.expose_secret())
                .await?;
        let now = now_ms();
        let new_refresh = refreshed
            .refresh_token
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| stored.refresh.clone());
        let expires_ms = access_token_expiry_ms(
            refreshed.access_token.expose_secret(),
            refreshed.expires_in,
            now,
        );

        let mut tokens = TokenSet {
            access: refreshed.access_token.clone(),
            refresh: new_refresh,
            expires_ms,
            id_token: refreshed.id_token.clone().or(stored.id_token.clone()),
            token_type: refreshed.token_type.clone().or(stored.token_type.clone()),
            scope: refreshed.scope.clone().or(stored.scope.clone()),
            user_email: stored.user_email.clone(),
            attributes: stored.attributes.clone(),
        };

        let enricher = enricher_for_config(&self.config);
        enricher
            .on_refresh_success(&self.client, &stored, &refreshed, &mut tokens)
            .await?;

        *inner = Some(tokens.clone());
        let mut slot = self
            .refresh_in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *slot = None;
        let access = tokens.access.clone();
        Ok((access, tokens))
    }
}

/// In-flight browser OAuth login session.
pub struct BrowserLogin {
    pub config: OAuthConfig,
    /// Authorize URL the user should open.
    pub url: String,
    /// Expected CSRF state.
    pub state: String,
    /// Generated OIDC nonce.
    pub nonce: String,
    /// Generated PKCE pair.
    pub pkce: PkceCodes,
    /// The exact registered redirect URI used for this session.
    pub redirect: String,
    rx: tokio::sync::oneshot::Receiver<CallbackOutcome>,
    _server: CallbackServer,
}

impl BrowserLogin {
    /// The port bound by the loopback server for this session.
    pub fn bound_port(&self) -> u16 {
        self._server.bound_port()
    }

    /// Manually inject a pasted authorization response (redirect URL or code).
    pub fn inject_manual_input(&self, input: &str) -> Result<(), AuthError> {
        let code = parse_authorization_response(input, Some(&self.state))?;
        if self._server.inject_outcome(CallbackOutcome::Code(code)) {
            Ok(())
        } else {
            Err(AuthError::Cancelled)
        }
    }

    /// Wait for the callback (or manual input) and exchange the authorization code for tokens.
    pub async fn complete(self, client: &crate::http::Http) -> Result<TokenResponse, AuthError> {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5 * 60), self.rx)
            .await
            .map_err(|_| AuthError::Timeout)?
            .map_err(|_| AuthError::Cancelled)?;
        match outcome {
            CallbackOutcome::Code(code) => {
                let tokens =
                    exchange_code(client, &self.config, &code, &self.pkce, &self.redirect).await?;
                if self.config.send_nonce {
                    let Some(id_token) = &tokens.id_token else {
                        return Err(AuthError::Authorization(
                            "id_token missing in token response when nonce was requested"
                                .to_string(),
                        ));
                    };
                    let claims = token::jwt_claims(id_token.expose_secret()).ok_or_else(|| {
                        AuthError::Decode(
                            "could not parse id_token claims for nonce validation".to_string(),
                        )
                    })?;
                    let token_nonce = claims.get("nonce").and_then(|v| v.as_str());
                    if token_nonce != Some(&self.nonce) {
                        return Err(AuthError::Authorization(format!(
                            "OIDC nonce mismatch: expected '{}', got {:?}",
                            self.nonce, token_nonce
                        )));
                    }
                }
                Ok(tokens)
            }
            CallbackOutcome::Failed(msg) => Err(AuthError::Authorization(msg)),
        }
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_refresh_rotation_errors_are_permanent() {
        for code in [
            "invalid_grant",
            "refresh_token_expired",
            "refresh_token_reused",
            "refresh_token_invalidated",
        ] {
            assert!(
                AuthError::TokenEndpoint {
                    status: 400,
                    body: format!(r#"{{"error":{{"code":"{code}"}}}}"#),
                }
                .is_permanent_grant_error(),
                "{code} must clear the unusable exact credential"
            );
        }

        for body in [
            r#"{"error":"unauthorized_client"}"#,
            r#"{"error":{"message":"request mentioned invalid_grant"}}"#,
            "invalid_grant",
        ] {
            assert!(
                !AuthError::TokenEndpoint {
                    status: 400,
                    body: body.to_string(),
                }
                .is_permanent_grant_error(),
                "only an exact structured grant code may delete a credential: {body}"
            );
        }
    }
}
