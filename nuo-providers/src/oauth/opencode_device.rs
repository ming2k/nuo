//! OpenCode Console device-authorization grant.
//!
//! Unlike standard RFC 8628, the OpenCode Console device flow speaks JSON on
//! `https://opencode.ai/console/auth/device/{code,token}`, returns the
//! fully-formed `verification_uri_complete` (the user code is already
//! embedded), and hands back a rotating `refresh_token` from the same poll
//! endpoint. This mirrors opencode's own
//! `packages/opencode/src/account/account.ts`.

use serde::{Deserialize, Serialize};

use nuo_host::SecretString;
use nuo_model_codec::provider_auth::OAuthConfig;

use crate::oauth::token::TokenResponse;

/// The OpenCode Console origin used when a preset does not carry a full URL.
pub const OPENCODE_DEFAULT_SERVER: &str = "https://opencode.ai/console";

/// Response from `POST {server}/auth/device/code`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpencodeDeviceCode {
    pub device_code: SecretString,
    pub user_code: String,
    /// The URL the user opens; OpenCode embeds the user code directly.
    #[serde(default)]
    pub verification_uri_complete: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub interval: Option<u64>,
}

impl OpencodeDeviceCode {
    /// The URL the user should open, resolved against the Console origin when
    /// the server returned a relative path. A leading `/` is resolved against
    /// the origin (matching `new URL(value, server)`), not the server path.
    pub fn user_url(&self, cfg: &OAuthConfig) -> String {
        let value = self.verification_uri_complete.trim();
        if value.starts_with("http://") || value.starts_with("https://") {
            return value.to_string();
        }
        let server = server_from_config(cfg);
        if let Some(rest) = value.strip_prefix('/') {
            return format!("{}/{}", origin_of(&server), rest);
        }
        format!("{}/{}", server.trim_end_matches('/'), value)
    }
}

/// `scheme://host[/path]` → `scheme://host`.
fn origin_of(server: &str) -> &str {
    let after_scheme = server.find("://").map(|i| i + 3).unwrap_or(0);
    match server[after_scheme..].find('/') {
        Some(index) => &server[..after_scheme + index],
        None => server,
    }
}

/// Derive the OpenCode Console origin from a preset whose device endpoints live
/// beneath it (e.g. `…/console/auth/device/code` → `…/console`).
pub fn server_from_config(cfg: &OAuthConfig) -> String {
    cfg.device_authorization_url
        .strip_suffix("/auth/device/code")
        .map(str::to_string)
        .unwrap_or_else(|| OPENCODE_DEFAULT_SERVER.to_string())
}

// Poll-loop bounds (mirror `oauth::device`).
const DEVICE_CODE_DEFAULT_INTERVAL_MS: u64 = 5_000;
const DEVICE_CODE_MIN_INTERVAL_MS: u64 = 1_000;
const DEVICE_CODE_SLOW_DOWN_INCREMENT_MS: u64 = 5_000;
const DEVICE_CODE_DEFAULT_EXPIRES_MS: u64 = 5 * 60 * 1000;
const OAUTH_POLLING_SAFETY_MARGIN_MS: i64 = 3_000;

/// Request a device code. Prints nothing; the caller surfaces the `user_code`
/// and verification URL to the operator.
pub async fn request_device_code(
    client: &crate::http::Http,
    cfg: &OAuthConfig,
) -> Result<OpencodeDeviceCode, crate::oauth::AuthError> {
    let request =
        crate::http::Request::new(netune::Method::POST, cfg.device_authorization_url.as_ref())
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .json(&serde_json::json!({ "client_id": cfg.client_id.as_ref() }));
    let response = client.send(request).await.map_err(|e| {
        crate::oauth::AuthError::Transport(format!("device code request failed: {e}"))
    })?;
    let status = response.status;
    let text = response.body;
    if !status.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    let json: OpencodeDeviceCode = serde_json::from_str(&text).map_err(|e| {
        crate::oauth::AuthError::Decode(format!("device code response parse failed: {e}"))
    })?;
    if json.device_code.is_empty() || json.user_code.is_empty() {
        return Err(crate::oauth::AuthError::Decode(
            "device code response missing device_code / user_code".to_string(),
        ));
    }
    if json.verification_uri_complete.trim().is_empty() {
        return Err(crate::oauth::AuthError::Decode(
            "device code response missing verification_uri_complete".to_string(),
        ));
    }
    Ok(json)
}

/// Poll the token endpoint until the user completes authorization, the code
/// expires, or a terminal error arrives. Honors RFC 8628 §3.5 semantics:
/// `authorization_pending` keeps polling; `slow_down` bumps the interval.
pub async fn poll_device_code(
    client: &crate::http::Http,
    cfg: &OAuthConfig,
    device: &OpencodeDeviceCode,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    poll_device_code_with(client, cfg, device, sleep_ms, now_ms).await
}

/// Test-injectable variant of [`poll_device_code`] so unit tests can drive the
/// `authorization_pending` / `slow_down` branches without real waits.
pub async fn poll_device_code_with<S, Fut>(
    client: &crate::http::Http,
    cfg: &OAuthConfig,
    device: &OpencodeDeviceCode,
    sleep: S,
    now: impl Fn() -> i64 + Send + Sync,
) -> Result<TokenResponse, crate::oauth::AuthError>
where
    S: Fn(u64) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    let expires_ms =
        positive_seconds_to_ms(device.expires_in, DEVICE_CODE_DEFAULT_EXPIRES_MS) as i64;
    let deadline = now() + expires_ms;
    let mut interval_ms = positive_seconds_to_ms(device.interval, DEVICE_CODE_DEFAULT_INTERVAL_MS)
        .max(DEVICE_CODE_MIN_INTERVAL_MS);

    loop {
        if now() >= deadline {
            return Err(crate::oauth::AuthError::DeviceCode(
                "device authorization timed out".to_string(),
            ));
        }

        // RFC 8628 §3.5 (and opencode's own poll): wait `interval` *before*
        // the first request, then once per poll. Polling immediately would
        // race the user's browser step and invite `slow_down`.
        sleep(min_with_margin(interval_ms, remaining_ms(&deadline, &now))).await;

        let request =
            crate::http::Request::new(netune::Method::POST, cfg.device_token_url.as_ref())
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .json(&serde_json::json!({
                    "grant_type": cfg.grant_type_device.as_ref(),
                    "device_code": device.device_code.expose_secret(),
                    "client_id": cfg.client_id.as_ref(),
                }));
        let response = client.send(request).await.map_err(|e| {
            crate::oauth::AuthError::Transport(format!("device token poll failed: {e}"))
        })?;

        let status = response.status;
        let text = response.body;

        match classify_token_response(status.as_u16(), &text) {
            TokenPollOutcome::Success(tokens) => return Ok(tokens),
            TokenPollOutcome::KeepPolling(interval_bump) => {
                if let Some(bump) = interval_bump {
                    interval_ms += bump;
                }
                continue;
            }
            TokenPollOutcome::Denied => {
                return Err(crate::oauth::AuthError::DeviceCode(
                    "device authorization was denied".to_string(),
                ));
            }
            TokenPollOutcome::Expired => {
                return Err(crate::oauth::AuthError::DeviceCode(
                    "device code expired - please re-run login".to_string(),
                ));
            }
            TokenPollOutcome::Terminal(detail) => {
                return Err(crate::oauth::AuthError::TokenEndpoint {
                    status: status.as_u16(),
                    body: detail,
                });
            }
        }
    }
}

#[derive(Debug)]
enum TokenPollOutcome {
    Success(TokenResponse),
    KeepPolling(Option<u64>),
    Denied,
    Expired,
    Terminal(String),
}

fn classify_token_response(status: u16, text: &str) -> TokenPollOutcome {
    if let Ok(tokens) = serde_json::from_str::<TokenResponse>(text)
        && !tokens.access_token.is_empty()
    {
        return TokenPollOutcome::Success(tokens);
    }
    let err: DeviceTokenError = serde_json::from_str(text).unwrap_or_default();
    match err.error.as_deref() {
        Some("authorization_pending") => TokenPollOutcome::KeepPolling(None),
        Some("slow_down") => {
            TokenPollOutcome::KeepPolling(Some(DEVICE_CODE_SLOW_DOWN_INCREMENT_MS))
        }
        Some("access_denied" | "authorization_denied") => TokenPollOutcome::Denied,
        Some("expired_token") => TokenPollOutcome::Expired,
        _ => {
            let detail = err
                .error_description
                .as_deref()
                .or(err.error.as_deref())
                .unwrap_or("")
                .to_string();
            TokenPollOutcome::Terminal(format!("HTTP {status}: {detail}"))
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct DeviceTokenError {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn remaining_ms(deadline: &i64, now: &dyn Fn() -> i64) -> i64 {
    (deadline - now()).max(0)
}

fn positive_seconds_to_ms(value: Option<u64>, default_ms: u64) -> u64 {
    value
        .filter(|s| *s > 0)
        .map(|s| s * 1000)
        .unwrap_or(default_ms)
}

fn min_with_margin(interval_ms: u64, remaining_ms: i64) -> u64 {
    let capped = (remaining_ms as u64).min(interval_ms);
    capped.saturating_add(OAUTH_POLLING_SAFETY_MARGIN_MS as u64)
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// User profile returned by `GET {server}/api/user`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpencodeUser {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub email: String,
}

/// Organization returned by `GET {server}/api/orgs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpencodeOrg {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// Fetch the signed-in user's profile.
pub async fn fetch_user(
    client: &crate::http::Http,
    server: &str,
    access_token: &str,
) -> Result<OpencodeUser, crate::oauth::AuthError> {
    let request = crate::http::Request::new(netune::Method::GET, format!("{server}/api/user"))
        .header("authorization", format!("Bearer {access_token}"))
        .header("accept", "application/json");
    let response = client.send(request).await.map_err(|e| {
        crate::oauth::AuthError::Transport(format!("opencode user request failed: {e}"))
    })?;
    if !response.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: response.status.as_u16(),
            body: response.body,
        });
    }
    serde_json::from_str(&response.body)
        .map_err(|e| crate::oauth::AuthError::Decode(format!("opencode user parse failed: {e}")))
}

/// Fetch the account's organizations.
pub async fn fetch_orgs(
    client: &crate::http::Http,
    server: &str,
    access_token: &str,
) -> Result<Vec<OpencodeOrg>, crate::oauth::AuthError> {
    let request = crate::http::Request::new(netune::Method::GET, format!("{server}/api/orgs"))
        .header("authorization", format!("Bearer {access_token}"))
        .header("accept", "application/json");
    let response = client.send(request).await.map_err(|e| {
        crate::oauth::AuthError::Transport(format!("opencode orgs request failed: {e}"))
    })?;
    if !response.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: response.status.as_u16(),
            body: response.body,
        });
    }
    serde_json::from_str(&response.body)
        .map_err(|e| crate::oauth::AuthError::Decode(format!("opencode orgs parse failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OAuthConfig {
        crate::oauth::presets::opencode_preset()
    }

    #[test]
    fn server_is_derived_from_device_endpoint() {
        assert_eq!(server_from_config(&cfg()), "https://opencode.ai/console");
    }

    #[test]
    fn user_url_accepts_absolute_and_relative_forms() {
        let mut device = OpencodeDeviceCode {
            device_code: "dev".into(),
            user_code: "ABCD-1234".into(),
            verification_uri_complete: "https://opencode.ai/console/device?code=x".into(),
            expires_in: None,
            interval: None,
        };
        assert_eq!(
            device.user_url(&cfg()),
            "https://opencode.ai/console/device?code=x"
        );
        device.verification_uri_complete = "/console/device?code=x".into();
        assert_eq!(
            device.user_url(&cfg()),
            "https://opencode.ai/console/device?code=x"
        );
        device.verification_uri_complete = "device?code=x".into();
        assert_eq!(
            device.user_url(&cfg()),
            "https://opencode.ai/console/device?code=x"
        );
    }

    #[test]
    fn classify_prefers_success_then_pending() {
        match classify_token_response(
            200,
            r#"{"access_token":"tok","refresh_token":"ref","token_type":"Bearer","expires_in":60}"#,
        ) {
            TokenPollOutcome::Success(t) => assert_eq!(t.access_token.expose_secret(), "tok"),
            other => panic!("expected success, got {other:?}"),
        }
        assert!(matches!(
            classify_token_response(400, r#"{"error":"authorization_pending"}"#),
            TokenPollOutcome::KeepPolling(None)
        ));
        assert!(matches!(
            classify_token_response(400, r#"{"error":"slow_down"}"#),
            TokenPollOutcome::KeepPolling(Some(_))
        ));
        assert!(matches!(
            classify_token_response(400, r#"{"error":"access_denied"}"#),
            TokenPollOutcome::Denied
        ));
        assert!(matches!(
            classify_token_response(400, r#"{"error":"expired_token"}"#),
            TokenPollOutcome::Expired
        ));
    }
}
