//! ChatGPT / OpenAI device-authorization grant.
//!
//! Unlike standard RFC 8628, OpenAI's device flow speaks JSON and runs in two
//! steps: the poll endpoint returns an `authorization_code` + `code_verifier`,
//! which are then exchanged at the `/oauth/token` endpoint for the token set.
//! This mirrors opencode's `ChatGPT Pro/Plus (headless)` flow and the official
//! Codex CLI. Because the server hands back the `code_verifier`, no PKCE pair
//! is generated client-side for this flow (unlike the browser flow).

use serde::{Deserialize, Serialize};

use nuo_host::SecretString;

use nuo_oauth::oauth::TokenResponse;
use nuo_model_codec::provider_auth::OAuthConfig;

/// Response from the `deviceauth/usercode` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatGptDeviceCode {
    pub device_auth_id: SecretString,
    pub user_code: String,
    /// Seconds between polls, returned as a JSON *string* by OpenAI.
    #[serde(default)]
    pub interval: Option<String>,
}

/// The verification URL the user opens to enter the code. OpenAI's device flow
/// does not return a URL in the body; it is a fixed path under the issuer
/// (`https://auth.openai.com/codex/device`).
pub fn verification_url(_cfg: &OAuthConfig) -> &str {
    "https://auth.openai.com/codex/device"
}

impl ChatGptDeviceCode {
    /// The URL the user should open to enter the code.
    pub fn user_url(&self, cfg: &OAuthConfig) -> String {
        verification_url(cfg).to_string()
    }

    /// The poll interval in milliseconds, defaulting to 5s when the server
    /// omits or mangles the `interval` field.
    pub fn interval_ms(&self) -> u64 {
        self.interval
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|s| s.max(1) * 1000)
            .unwrap_or(5_000)
    }
}

/// Request a device code from OpenAI's `deviceauth/usercode` endpoint.
pub async fn request_device_code(
    client: &nuo_provider_transport::http::Http,
    cfg: &OAuthConfig,
) -> Result<ChatGptDeviceCode, nuo_oauth::oauth::AuthError> {
    let request =
        nuo_provider_transport::http::Request::new(netune::Method::POST, cfg.device_authorization_url.as_ref())
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header(
                "user-agent",
                cfg.user_agent.as_deref().unwrap_or(nuo_provider_transport::NUO_USER_AGENT),
            )
            .json(&serde_json::json!({ "client_id": cfg.client_id }));
    let response = client.send(request).await.map_err(|e| {
        nuo_oauth::oauth::AuthError::Transport(format!("device code request failed: {e}"))
    })?;
    let status = response.status;
    let text = response.body;
    if !status.is_success() {
        return Err(nuo_oauth::oauth::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    let json: ChatGptDeviceCode = serde_json::from_str(&text).map_err(|e| {
        nuo_oauth::oauth::AuthError::Decode(format!("device code response parse failed: {e}"))
    })?;
    if json.device_auth_id.is_empty() || json.user_code.is_empty() {
        return Err(nuo_oauth::oauth::AuthError::Decode(
            "device code response missing device_auth_id / user_code".to_string(),
        ));
    }
    Ok(json)
}

/// The result of polling the device token endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatGptDeviceToken {
    pub authorization_code: SecretString,
    pub code_verifier: SecretString,
}

/// Poll the `deviceauth/token` endpoint until the user completes authorization
/// or the flow errors out. On success returns the `authorization_code` +
/// `code_verifier` to exchange at the token endpoint. While pending, OpenAI
/// answers `403`/`404`; any other non-2xx is a terminal failure.
pub async fn poll_device_code(
    client: &nuo_provider_transport::http::Http,
    cfg: &OAuthConfig,
    device: &ChatGptDeviceCode,
) -> Result<ChatGptDeviceToken, nuo_oauth::oauth::AuthError> {
    tokio::time::timeout(
        std::time::Duration::from_secs(15 * 60),
        poll_device_code_with(client, cfg, device, sleep_ms),
    )
    .await
    .map_err(|_| nuo_oauth::oauth::AuthError::Timeout)?
}

/// Test-injectable variant of [`poll_device_code`].
pub async fn poll_device_code_with<S, Fut>(
    client: &nuo_provider_transport::http::Http,
    cfg: &OAuthConfig,
    device: &ChatGptDeviceCode,
    sleep: S,
) -> Result<ChatGptDeviceToken, nuo_oauth::oauth::AuthError>
where
    S: Fn(u64) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    let interval_ms = device.interval_ms();
    loop {
        let request =
            nuo_provider_transport::http::Request::new(netune::Method::POST, cfg.device_token_url.as_ref())
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .header(
                    "user-agent",
                    cfg.user_agent.as_deref().unwrap_or(nuo_provider_transport::NUO_USER_AGENT),
                )
                .json(&serde_json::json!({
                    "device_auth_id": device.device_auth_id.expose_secret(),
                    "user_code": device.user_code,
                }));
        let response = client.send(request).await.map_err(|e| {
            nuo_oauth::oauth::AuthError::Transport(format!("device token poll failed: {e}"))
        })?;
        let status = response.status;
        let text = response.body;
        if status.is_success() {
            return serde_json::from_str::<ChatGptDeviceToken>(&text).map_err(|e| {
                nuo_oauth::oauth::AuthError::Decode(format!("device token response parse failed: {e}"))
            });
        }
        if status.as_u16() != 403 && status.as_u16() != 404 {
            return Err(nuo_oauth::oauth::AuthError::TokenEndpoint {
                status: status.as_u16(),
                body: text,
            });
        }
        sleep(interval_ms + OAUTH_POLLING_SAFETY_MARGIN_MS).await;
    }
}

/// Exchange a device `authorization_code` for a token set.
pub async fn exchange_device_code(
    client: &nuo_provider_transport::http::Http,
    cfg: &OAuthConfig,
    token: &ChatGptDeviceToken,
) -> Result<TokenResponse, nuo_oauth::oauth::AuthError> {
    let body = nuo_oauth::oauth::token::percent_encode_form_pairs(&[
        ("grant_type", "authorization_code"),
        ("code", token.authorization_code.expose_secret()),
        ("redirect_uri", cfg.device_redirect_uri.as_ref()),
        ("client_id", cfg.client_id.as_ref()),
        ("code_verifier", token.code_verifier.expose_secret()),
    ]);
    nuo_oauth::oauth::token::post_form(client, cfg.token_url.as_ref(), &body).await
}

const OAUTH_POLLING_SAFETY_MARGIN_MS: u64 = 3_000;

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::preset as chatgpt_preset;

    #[test]
    fn interval_ms_defaults_and_parses() {
        let d = ChatGptDeviceCode {
            device_auth_id: "id".into(),
            user_code: "UC".into(),
            interval: None,
        };
        assert_eq!(d.interval_ms(), 5_000);
        let d = ChatGptDeviceCode {
            interval: Some("7".into()),
            ..d
        };
        assert_eq!(d.interval_ms(), 7_000);
    }

    #[test]
    fn verification_url_points_at_codex_device() {
        assert_eq!(
            verification_url(&chatgpt_preset()),
            "https://auth.openai.com/codex/device"
        );
    }
}
