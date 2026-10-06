//! OAuth2 token-endpoint helpers: URL builders, PKCE code exchange, token refresh,
//! JWT claim/expiration inspection, and provider-specific onboarding handlers (Google Antigravity & ChatGPT).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::oauth::pkce::PkceCodes;
use nuo_model_codec::provider_auth::{ClientAuthMethod, OAuthConfig, PkceMode, TokenRequestFormat};

/// Refresh the access token ahead of expiry so long-running calls don't hit a 401.
pub const ACCESS_TOKEN_REFRESH_SKEW_MS: i64 = 120_000;

/// Standard Antigravity User-Agent matching the official Antigravity CLI.
pub const ANTIGRAVITY_USER_AGENT: &str = nuo_model_codec::client_identity::ANTIGRAVITY_USER_AGENT;
/// Antigravity Google API client header.
pub const ANTIGRAVITY_API_CLIENT_HEADER: &str =
    nuo_model_codec::client_identity::ANTIGRAVITY_API_CLIENT_HEADER;
/// Endpoint for Antigravity loadCodeAssist account metadata.
pub const ANTIGRAVITY_LOAD_CODE_ASSIST_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
/// Endpoint for Antigravity onboardUser account initialization.
pub const ANTIGRAVITY_ONBOARD_USER_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:onboardUser";
/// Endpoint for Antigravity user quota summary inspection.
pub const ANTIGRAVITY_RETRIEVE_QUOTA_SUMMARY_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
/// Endpoint for Antigravity available-models catalog fetch.
pub const ANTIGRAVITY_FETCH_AVAILABLE_MODELS_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
/// Google UserInfo endpoint.
pub const GOOGLE_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v3/userinfo";

/// IDE platform enum the Cloud Code `ClientMetadata` accepts for a Linux x86_64
/// client. The current `agy` CLI sends only `{"ideType":"ANTIGRAVITY"}` on
/// `loadCodeAssist` (captured live, ADR-0289) — the additional fields below are
/// optional and retained because the backend honours them when present, which
/// keeps the onboarding path working against stricter deployments.
pub const ANTIGRAVITY_IDE_PLATFORM: &str = "LINUX_AMD64";

pub use nuo_provider_transport::oauth::TokenResponse;

/// Google UserInfo response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GoogleUserInfo {
    pub sub: Option<String>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub name: Option<String>,
    pub picture: Option<String>,
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
    client: &crate::http::Http,
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
    client: &crate::http::Http,
    cfg: &OAuthConfig,
    refresh_token: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    // Qoder's device grant is not RFC 8628: the `drt-` device refresh token
    // rotates the `dt-` device token via a JSON POST on the OpenAPI surface.
    if super::presets::is_qoder(cfg) {
        return super::qoder::refresh_device_token(client, refresh_token).await;
    }
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
    client: &crate::http::Http,
    cfg: &OAuthConfig,
    params: &[(&str, &str)],
    basic_auth: Option<&str>,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    match cfg.token_format {
        TokenRequestFormat::FormUrlEncoded => {
            let body = serde_urlencoded(params);
            let mut req = crate::http::Request::new(netune::Method::POST, cfg.token_url.as_ref())
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
            let mut req = crate::http::Request::new(netune::Method::POST, cfg.token_url.as_ref())
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

pub(crate) async fn post_form(
    client: &crate::http::Http,
    url: &str,
    body: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let request = crate::http::Request::new(netune::Method::POST, url)
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
pub(crate) fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// Extract the ChatGPT account id from a JWT (id_token or access_token).
pub fn chatgpt_account_id(token: &str) -> Option<String> {
    let claims = jwt_claims(token)?;
    if let Some(id) = claims.get("chatgpt_account_id").and_then(|v| v.as_str()) {
        return Some(id.to_string());
    }
    claims
        .get("https://api.openai.com/auth")
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Fetch Google UserInfo (email, name, sub, picture) using access token.
pub async fn fetch_google_userinfo(
    client: &crate::http::Http,
    access_token: &str,
) -> Result<GoogleUserInfo, crate::oauth::AuthError> {
    let request = crate::http::Request::new(netune::Method::GET, GOOGLE_USERINFO_URL)
        .header("authorization", format!("Bearer {access_token}"))
        .header("accept", "application/json");
    let resp = client
        .send(request)
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("userinfo request failed: {e}")))?;

    if resp.is_success() {
        let info = serde_json::from_str::<GoogleUserInfo>(&resp.body)
            .map_err(|e| crate::oauth::AuthError::Decode(format!("userinfo parse failed: {e}")))?;
        Ok(info)
    } else {
        Err(crate::oauth::AuthError::TokenEndpoint {
            status: resp.status.as_u16(),
            body: resp.body,
        })
    }
}

/// POST a JSON body with the Antigravity headers.
async fn antigravity_post(
    client: &crate::http::Http,
    url: &str,
    access_token: &str,
    body: &serde_json::Value,
    what: &str,
) -> Result<crate::http::Reply, crate::oauth::AuthError> {
    let request = crate::http::Request::new(netune::Method::POST, url)
        .header("authorization", format!("Bearer {access_token}"))
        .header("user-agent", ANTIGRAVITY_USER_AGENT)
        .header("x-goog-api-client", ANTIGRAVITY_API_CLIENT_HEADER)
        .json(body);
    client
        .send(request)
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("{what} failed: {e}")))
}

/// Discover or onboard the user's Antigravity `cloudaicompanionProject`.
pub async fn resolve_antigravity_project(
    client: &crate::http::Http,
    access_token: &str,
) -> Result<String, crate::oauth::AuthError> {
    let load_body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY",
            "ideVersion": nuo_model_codec::client_identity::ANTIGRAVITY_VERSION,
            "ideName": nuo_model_codec::client_identity::ANTIGRAVITY_APP_NAME,
            "platform": ANTIGRAVITY_IDE_PLATFORM,
            "pluginType": "GEMINI"
        }
    });

    let resp = antigravity_post(
        client,
        ANTIGRAVITY_LOAD_CODE_ASSIST_URL,
        access_token,
        &load_body,
        "loadCodeAssist",
    )
    .await?;

    if resp.is_success()
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&resp.body)
    {
        if let Some(p) = extract_cloudaicompanion_project(&val) {
            tracing::info!(project = %p, "resolved existing Antigravity cloudaicompanionProject");
            return Ok(p);
        }

        // Project missing; attempt onboardUser with detected tier (defaulting to g1-pro-tier)
        let tier_id = val
            .get("paidTier")
            .or_else(|| val.get("paid_tier"))
            .and_then(|p| p.get("id").or(Some(p)))
            .and_then(|id| id.as_str())
            .or_else(|| {
                val.get("currentTier")
                    .or_else(|| val.get("current_tier"))
                    .and_then(|c| c.get("id").or(Some(c)))
                    .and_then(|id| id.as_str())
            })
            .or_else(|| {
                val.get("allowedTiers")
                    .or_else(|| val.get("allowed_tiers"))
                    .and_then(|a| a.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|t| t.get("id").or(Some(t)))
                    .and_then(|id| id.as_str())
            })
            .unwrap_or("g1-pro-tier");

        let onboard_body = serde_json::json!({
            "tierId": tier_id,
            "metadata": {
                "ideType": "ANTIGRAVITY",
                "ideVersion": nuo_model_codec::client_identity::ANTIGRAVITY_VERSION,
                "ideName": nuo_model_codec::client_identity::ANTIGRAVITY_APP_NAME,
                "platform": ANTIGRAVITY_IDE_PLATFORM,
                "pluginType": "GEMINI"
            }
        });

        let onboard_resp = antigravity_post(
            client,
            ANTIGRAVITY_ONBOARD_USER_URL,
            access_token,
            &onboard_body,
            "onboardUser",
        )
        .await?;

        if onboard_resp.is_success() {
            if let Ok(onboard_val) = serde_json::from_str::<serde_json::Value>(&onboard_resp.body)
                && let Some(p) = extract_cloudaicompanion_project(&onboard_val)
            {
                tracing::info!(project = %p, tier = %tier_id, "onboarded Antigravity cloudaicompanionProject");
                return Ok(p);
            }

            // If onboardUser completed, retry loadCodeAssist to read the freshly provisioned project
            if let Ok(second_resp) = antigravity_post(
                client,
                ANTIGRAVITY_LOAD_CODE_ASSIST_URL,
                access_token,
                &load_body,
                "loadCodeAssist",
            )
            .await
                && second_resp.is_success()
                && let Ok(second_val) = serde_json::from_str::<serde_json::Value>(&second_resp.body)
                && let Some(p) = extract_cloudaicompanion_project(&second_val)
            {
                tracing::info!(project = %p, "resolved newly onboarded Antigravity cloudaicompanionProject");
                return Ok(p);
            }
        }
    }

    Ok(String::new())
}

/// Retrieve user quota summary from Google Antigravity CodeAssist backend.
pub async fn retrieve_antigravity_quota_summary(
    client: &crate::http::Http,
    access_token: &str,
    project: Option<&str>,
) -> Result<crate::usage::AntigravityQuotaSummaryResponse, crate::oauth::AuthError> {
    let req_body = serde_json::json!({
        "project": project.unwrap_or("")
    });

    let resp = antigravity_post(
        client,
        ANTIGRAVITY_RETRIEVE_QUOTA_SUMMARY_URL,
        access_token,
        &req_body,
        "retrieveUserQuotaSummary",
    )
    .await?;

    if resp.is_success() {
        serde_json::from_str::<crate::usage::AntigravityQuotaSummaryResponse>(&resp.body).map_err(
            |e| {
                crate::oauth::AuthError::Decode(format!(
                    "retrieveUserQuotaSummary parse failed: {e}"
                ))
            },
        )
    } else {
        Err(crate::oauth::AuthError::TokenEndpoint {
            status: resp.status.as_u16(),
            body: resp.body,
        })
    }
}

/// Extract the Antigravity `cloudaicompanionProject` ID / name from any Google CodeAssist JSON response.
pub fn extract_cloudaicompanion_project(val: &serde_json::Value) -> Option<String> {
    let target = val.get("response").unwrap_or(val);
    let project = target
        .get("cloudaicompanionProject")
        .or_else(|| target.get("cloudaicompanion_project"))
        .or_else(|| target.get("project"))
        .or_else(|| target.get("duetProject"))
        .or_else(|| target.get("duet_project"))
        .or(
            if target.is_object()
                && (target.get("id").is_some()
                    || target.get("projectNumber").is_some()
                    || target.get("name").is_some())
            {
                Some(target)
            } else {
                None
            },
        )?;

    if let Some(p) = project.as_str().filter(|p| !p.trim().is_empty()) {
        let trimmed = p.trim();
        return Some(if trimmed.starts_with("projects/") {
            trimmed.to_string()
        } else if trimmed.chars().all(|c| c.is_ascii_digit()) {
            format!("projects/{trimmed}")
        } else {
            trimmed.to_string()
        });
    }

    if let Some(name) = project
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|n| !n.trim().is_empty())
    {
        let trimmed = name.trim();
        return Some(if trimmed.starts_with("projects/") {
            trimmed.to_string()
        } else if trimmed.chars().all(|c| c.is_ascii_digit()) {
            format!("projects/{trimmed}")
        } else {
            trimmed.to_string()
        });
    }

    if let Some(id) = project
        .get("id")
        .and_then(|i| i.as_str())
        .filter(|id| !id.trim().is_empty())
    {
        let trimmed = id.trim();
        return Some(if trimmed.starts_with("projects/") {
            trimmed.to_string()
        } else if trimmed.chars().all(|c| c.is_ascii_digit()) {
            format!("projects/{trimmed}")
        } else {
            trimmed.to_string()
        });
    }

    if let Some(num) = project
        .get("projectNumber")
        .or_else(|| project.get("project_number"))
        .and_then(|n| n.as_str())
        .filter(|n| !n.trim().is_empty())
    {
        return Some(format!("projects/{}", num.trim()));
    }

    if let Some(num) = project
        .get("projectNumber")
        .or_else(|| project.get("project_number"))
        .and_then(|n| n.as_i64())
    {
        return Some(format!("projects/{num}"));
    }

    if let Some(num) = project.as_i64() {
        return Some(format!("projects/{num}"));
    }
    if let Some(id_num) = project.get("id").and_then(|i| i.as_i64()) {
        return Some(format!("projects/{id_num}"));
    }
    None
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
    use crate::oauth::presets::{chatgpt_preset, xai_preset};

    #[test]
    fn xai_authorize_url_carries_plan_generic_and_pkce() {
        let pkce = PkceCodes {
            verifier: "v".into(),
            challenge: "c".to_string(),
        };
        let cfg = xai_preset();
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

    #[test]
    fn chatgpt_authorize_url_carries_codex_flow_param() {
        let pkce = PkceCodes {
            verifier: "v".into(),
            challenge: "c".to_string(),
        };
        let cfg = chatgpt_preset();
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
    fn chatgpt_account_id_decoded_from_top_level_claim() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"chatgpt_account_id":"acct-123"}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(chatgpt_account_id(&token), Some("acct-123".to_string()));
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
