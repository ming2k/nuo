//! OAuth2 token-endpoint helpers: URL builders, PKCE code exchange, token refresh,
//! JWT claim/expiration inspection, and provider-specific onboarding handlers (Google Antigravity & ChatGPT).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::oauth::pkce::PkceCodes;
use nuo_host::SecretString;
use nuo_model_codec::provider_auth::{ClientAuthMethod, OAuthConfig, PkceMode, TokenRequestFormat};

/// Refresh the access token ahead of expiry so long-running calls don't hit a 401.
pub const ACCESS_TOKEN_REFRESH_SKEW_MS: i64 = 120_000;

/// A successful token response from any grant type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: SecretString,
    #[serde(default)]
    pub refresh_token: Option<SecretString>,
    #[serde(default)]
    pub id_token: Option<SecretString>,
    #[serde(default)]
    pub token_type: Option<String>,
    /// Seconds until the access_token expires.
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
    /// Qoder's device-token/device-exchange responses carry the account's
    /// uid; every other issuer leaves this absent. Consumed by the runtime
    /// to assemble the connection's typed [`nuo_model_codec::QoderRequestIdentity`]
    /// at login time.
    #[serde(default, alias = "uid", alias = "userId")]
    pub qoder_uid: Option<String>,
}

impl TokenResponse {
    pub fn validate(self) -> Result<Self, crate::oauth::AuthError> {
        if self.access_token.expose_secret().trim().is_empty() {
            return Err(crate::oauth::AuthError::Decode(
                "token endpoint returned an empty access_token".to_string(),
            ));
        }
        Ok(self)
    }
}

/// Build the authorize URL for the browser-OAuth flow.
pub fn build_authorize_url(
    cfg: &OAuthConfig,
    pkce: &PkceCodes,
    state: &str,
    nonce: &str,
    redirect_uri: &str,
) -> String {
    let mut params: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", cfg.client_id.as_ref()),
        ("redirect_uri", redirect_uri),
        ("scope", cfg.scope.as_ref()),
        ("state", state),
    ];

    match cfg.pkce_mode {
        PkceMode::S256 => {
            params.push(("code_challenge", pkce.challenge.as_str()));
            params.push(("code_challenge_method", "S256"));
        }
        PkceMode::Plain => {
            params.push(("code_challenge", pkce.verifier.expose_secret()));
            params.push(("code_challenge_method", "plain"));
        }
        PkceMode::Disabled => {}
    }

    if cfg.send_nonce {
        params.push(("nonce", nonce));
    }

    for (k, v) in &cfg.extra_authorize_params {
        params.push((k.as_ref(), v.as_ref()));
    }

    let query = serde_urlencoded(&params);
    format!("{}?{query}", cfg.authorize_url)
}

/// Exchange an authorization code for a token set (browser / manual flow).
pub async fn exchange_code(
    client: &nuo_provider_transport::http::Http,
    cfg: &OAuthConfig,
    code: &str,
    pkce: &PkceCodes,
    redirect_uri: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let mut params: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", cfg.client_id.as_ref()),
    ];

    if cfg.pkce_mode != PkceMode::Disabled {
        params.push(("code_verifier", pkce.verifier.expose_secret()));
    }

    let mut basic_auth: Option<String> = None;
    match cfg.client_auth_method {
        ClientAuthMethod::RequestBody => {
            if let Some(secret) = &cfg.client_secret {
                params.push(("client_secret", secret.as_ref()));
            }
        }
        ClientAuthMethod::BasicHeader => {
            if let Some(secret) = &cfg.client_secret {
                let raw = format!("{}:{}", cfg.client_id, secret);
                basic_auth = Some(format!("Basic {}", BASE64_STD.encode(raw)));
            }
        }
        ClientAuthMethod::None => {
            // Optional fallback: if client_secret is set, send in body
            if let Some(secret) = &cfg.client_secret {
                params.push(("client_secret", secret.as_ref()));
            }
        }
    }

    for (k, v) in &cfg.extra_token_params {
        params.push((k.as_ref(), v.as_ref()));
    }

    execute_token_request(client, cfg, &params, basic_auth.as_deref()).await
}

/// Refresh a rotated access token from a refresh_token.
pub async fn refresh_access_token(
    client: &nuo_provider_transport::http::Http,
    cfg: &OAuthConfig,
    refresh_token: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let mut params: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", cfg.client_id.as_ref()),
    ];

    let mut basic_auth: Option<String> = None;
    match cfg.client_auth_method {
        ClientAuthMethod::RequestBody => {
            if let Some(secret) = &cfg.client_secret {
                params.push(("client_secret", secret.as_ref()));
            }
        }
        ClientAuthMethod::BasicHeader => {
            if let Some(secret) = &cfg.client_secret {
                let raw = format!("{}:{}", cfg.client_id, secret);
                basic_auth = Some(format!("Basic {}", BASE64_STD.encode(raw)));
            }
        }
        ClientAuthMethod::None => {
            if let Some(secret) = &cfg.client_secret {
                params.push(("client_secret", secret.as_ref()));
            }
        }
    }

    for (k, v) in &cfg.extra_refresh_params {
        params.push((k.as_ref(), v.as_ref()));
    }

    execute_token_request(client, cfg, &params, basic_auth.as_deref()).await
}

async fn execute_token_request(
    client: &nuo_provider_transport::http::Http,
    cfg: &OAuthConfig,
    params: &[(&str, &str)],
    basic_auth: Option<&str>,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    match cfg.token_format {
        TokenRequestFormat::FormUrlEncoded => {
            let body = serde_urlencoded(params);
            let mut req = nuo_provider_transport::http::Request::new(netune::Method::POST, cfg.token_url.as_ref())
                .header("content-type", "application/x-www-form-urlencoded")
                .header("accept", "application/json")
                .raw_body(body);

            if let Some(ua) = &cfg.user_agent {
                req = req.header("user-agent", ua.as_ref());
            }
            if let Some(auth) = basic_auth {
                req = req.header("authorization", auth);
            }
            for (k, v) in &cfg.extra_headers {
                req = req.header(k.as_ref(), v.as_ref());
            }

            let resp = client.send(req).await.map_err(|e| {
                crate::oauth::AuthError::Transport(format!("token request failed: {e}"))
            })?;
            let status = resp.status;
            let text = resp.body;
            if !status.is_success() {
                return Err(crate::oauth::AuthError::TokenEndpoint {
                    status: status.as_u16(),
                    body: text,
                });
            }
            let parsed = serde_json::from_str::<TokenResponse>(&text).map_err(|e| {
                crate::oauth::AuthError::Decode(format!("token response parse failed: {e}"))
            })?;
            parsed.validate()
        }
        TokenRequestFormat::Json => {
            let mut map = serde_json::Map::new();
            for (k, v) in params {
                map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
            }
            let mut req = nuo_provider_transport::http::Request::new(netune::Method::POST, cfg.token_url.as_ref())
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .json(&serde_json::Value::Object(map));

            if let Some(ua) = &cfg.user_agent {
                req = req.header("user-agent", ua.as_ref());
            }
            if let Some(auth) = basic_auth {
                req = req.header("authorization", auth);
            }
            for (k, v) in &cfg.extra_headers {
                req = req.header(k.as_ref(), v.as_ref());
            }

            let resp = client.send(req).await.map_err(|e| {
                crate::oauth::AuthError::Transport(format!("token request failed: {e}"))
            })?;
            let status = resp.status;
            let text = resp.body;
            if !status.is_success() {
                return Err(crate::oauth::AuthError::TokenEndpoint {
                    status: status.as_u16(),
                    body: text,
                });
            }
            let parsed = serde_json::from_str::<TokenResponse>(&text).map_err(|e| {
                crate::oauth::AuthError::Decode(format!("token response parse failed: {e}"))
            })?;
            parsed.validate()
        }
    }
}

/// A tiny `application/x-www-form-urlencoded` serializer.
fn serde_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(s: &str) -> String {
    percent_encode_form_value(s)
}

/// Percent-encode a single form value.
pub fn percent_encode_form_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Serialize `&[(&str,&str)]` into an `application/x-www-form-urlencoded` body.
pub fn percent_encode_form_pairs(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                percent_encode_form_value(k),
                percent_encode_form_value(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub async fn post_form(
    client: &nuo_provider_transport::http::Http,
    url: &str,
    body: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let request = nuo_provider_transport::http::Request::new(netune::Method::POST, url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .raw_body(body.to_string());
    let response = client
        .send(request)
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("token request failed: {e}")))?;
    let status = response.status;
    let text = response.body;
    if !status.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    let parsed = serde_json::from_str::<TokenResponse>(&text).map_err(|e| {
        crate::oauth::AuthError::Decode(format!("token response parse failed: {e}"))
    })?;
    parsed.validate()
}

/// Whether a stored access token is expiring within `skew_ms` of now.
pub fn access_token_is_expiring(access_token: Option<&str>, skew_ms: i64, now_ms: i64) -> bool {
    if let Some(exp_ms) = jwt_exp_ms(access_token.unwrap_or(""))
        && exp_ms <= now_ms + skew_ms.max(0)
    {
        return true;
    }
    false
}

/// Decode the `exp` claim from a JWT access token.
pub fn jwt_exp_ms(token: &str) -> Option<i64> {
    let claims = jwt_claims(token)?;
    let exp = claims.get("exp")?.as_i64()?;
    Some(exp * 1000)
}

/// Resolve an access token's absolute expiration without inventing a TTL.
/// Explicit OAuth metadata wins, JWT `exp` is the fallback, and an opaque
/// token with neither is treated as non-expiring.
pub fn access_token_expiry_ms(access_token: &str, expires_in: Option<u64>, now_ms: i64) -> i64 {
    expires_in
        .and_then(|seconds| {
            i64::try_from(seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(1_000))
                .and_then(|ttl| now_ms.checked_add(ttl))
        })
        .or_else(|| jwt_exp_ms(access_token))
        .unwrap_or(i64::MAX)
}

/// Decode a JWT's payload claims (without signature verification) as JSON.
pub fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim_end_matches('=');
    let mut buf = String::from(trimmed);
    while buf.len() % 4 != 0 {
        buf.push('=');
    }
    URL_SAFE_NO_PAD.decode(buf.trim_end_matches('=')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding_handles_space_and_reserved() {
        let body = serde_urlencoded(&[("k", "a b/c"), ("x", "plain")]);
        assert_eq!(body, "k=a+b%2Fc&x=plain");
    }

    #[test]
    fn jwt_exp_is_decoded_from_access_token() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(jwt_exp_ms(&token), Some(2_000_000_000_000));
    }

    #[test]
    fn jwt_exp_none_for_opaque_token() {
        assert!(jwt_exp_ms("opaque-token-no-dots").is_none());
        assert!(jwt_exp_ms("aaa.bbb").is_none());
    }

    #[test]
    fn token_expiry_never_invents_a_ttl_for_opaque_tokens() {
        assert_eq!(access_token_expiry_ms("opaque", None, 123), i64::MAX);
        assert_eq!(access_token_expiry_ms("opaque", Some(60), 1_000), 61_000);
    }

    #[test]
    fn token_expiry_uses_jwt_exp_when_oauth_ttl_is_absent() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(access_token_expiry_ms(&token, None, 1), 2_000_000_000_000);
    }

    #[test]
    fn is_expiring_true_when_jwt_exp_within_skew() {
        let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{}}}", 2_000_000_000));
        let token = format!("h.{payload}.s");
        assert!(access_token_is_expiring(Some(&token), 0, 2_000_000_000_000));
        assert!(!access_token_is_expiring(
            Some(&token),
            120_000,
            1_999_000_000_000
        ));
    }

    #[test]
    fn is_expiring_false_for_opaque_token() {
        assert!(!access_token_is_expiring(Some("opaque"), 0, 0));
        assert!(!access_token_is_expiring(None, 0, 0));
    }
}
