//! Google Antigravity (Cloud Code) OAuth surface (ADR-0027).
//!
//! Owns the Antigravity and Antigravity-CLI `OAuthConfig` presets and the
//! Cloud-Code project/user enrichment; the generic engine in `nuo-oauth` never
//! names Google.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use nuo_model_codec::LoginMethod;
use nuo_model_codec::ResolvedAuth;
use nuo_model_codec::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, PkceMode, PortMode, TokenRequestFormat,
};
use nuo_oauth::{OAuthProvider, OAuthTokenEnricher};
use nuo_provider::credentials::TokenSet;
use nuo_provider_transport::http::Http;
use nuo_oauth::oauth::AuthError;
use nuo_oauth::oauth::token::TokenResponse;

pub const ANTIGRAVITY_LOAD_CODE_ASSIST_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
pub const ANTIGRAVITY_ONBOARD_USER_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:onboardUser";
pub const ANTIGRAVITY_RETRIEVE_QUOTA_SUMMARY_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
pub const ANTIGRAVITY_FETCH_AVAILABLE_MODELS_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
pub const GOOGLE_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v3/userinfo";
/// IDE platform enum the Cloud Code `ClientMetadata` accepts for a Linux x86_64
/// client (ADR-0289).
pub const ANTIGRAVITY_IDE_PLATFORM: &str = "LINUX_AMD64";

/// Google UserInfo response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct GoogleUserInfo {
    pub sub: Option<String>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub name: Option<String>,
    pub picture: Option<String>,
}

/// Fetch Google UserInfo (email, name, sub, picture) using an access token.
pub async fn fetch_google_userinfo(
    client: &nuo_provider_transport::http::Http,
    access_token: &str,
) -> Result<GoogleUserInfo, AuthError> {
    let request = nuo_provider_transport::http::Request::new(netune::Method::GET, GOOGLE_USERINFO_URL)
        .header("authorization", format!("Bearer {access_token}"))
        .header("accept", "application/json");
    let resp = client
        .send(request)
        .await
        .map_err(|e| AuthError::Transport(format!("userinfo request failed: {e}")))?;

    if resp.is_success() {
        serde_json::from_str::<GoogleUserInfo>(&resp.body)
            .map_err(|e| AuthError::Decode(format!("userinfo parse failed: {e}")))
    } else {
        Err(AuthError::TokenEndpoint {
            status: resp.status.as_u16(),
            body: resp.body,
        })
    }
}

/// POST a JSON body with the Antigravity headers.
async fn antigravity_post(
    client: &nuo_provider_transport::http::Http,
    url: &str,
    access_token: &str,
    body: &serde_json::Value,
    what: &str,
) -> Result<nuo_provider_transport::http::Reply, AuthError> {
    let request = nuo_provider_transport::http::Request::new(netune::Method::POST, url)
        .header("authorization", format!("Bearer {access_token}"))
        .header(
            "user-agent",
            nuo_model_codec::client_identity::ANTIGRAVITY_USER_AGENT,
        )
        .header(
            "x-goog-api-client",
            nuo_model_codec::client_identity::ANTIGRAVITY_API_CLIENT_HEADER,
        )
        .json(body);
    client
        .send(request)
        .await
        .map_err(|e| AuthError::Transport(format!("{what} failed: {e}")))
}

/// Discover or onboard the user's Antigravity `cloudaicompanionProject`.
pub async fn resolve_antigravity_project(
    client: &nuo_provider_transport::http::Http,
    access_token: &str,
) -> Result<String, AuthError> {
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

/// Extract the Antigravity `cloudaicompanionProject` ID / name from any Google
/// CodeAssist JSON response.
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
    if let Some(name) = project.get("name").and_then(|n| n.as_str()).filter(|n| !n.trim().is_empty())
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
    if let Some(id) = project.get("id").and_then(|i| i.as_str()).filter(|id| !id.trim().is_empty()) {
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

pub const GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_ID: &str = concat!(
    "1071006060591-",
    "tmhssin2h21lcre235vtolojh4g403ep",
    ".apps.googleusercontent.com"
);

pub const GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_SECRET: &str =
    concat!("GOCSPX-", "K58FWR486LdLJ1mLB8sXC4z6qDAf");

/// Google Antigravity standalone CLI (`agy`) OAuth client ID.
pub const GOOGLE_ANTIGRAVITY_CLI_CLIENT_ID: &str = concat!(
    "884354919052-",
    "36trc1jjb3tguiac32ov6cod268c5blh",
    ".apps.googleusercontent.com"
);

pub const GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET: &str =
    concat!("GOCSPX-", "9YQWpF7RWDC0QTdj-YxKMwR0ZtsX");

/// Google Antigravity (Cloud Code Companion) preset.
pub fn antigravity_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("google-antigravity"),
        client_id: Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_ID),
        client_secret: Some(Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_SECRET)),
        client_auth_method: ClientAuthMethod::RequestBody,
        authorize_url: Cow::Borrowed("https://accounts.google.com/o/oauth2/v2/auth"),
        token_url: Cow::Borrowed("https://oauth2.googleapis.com/token"),
        device_authorization_url: Cow::Borrowed("https://oauth2.googleapis.com/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(
            "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs https://www.googleapis.com/auth/aicode openid",
        ),
        extra_authorize_params: vec![
            (Cow::Borrowed("access_type"), Cow::Borrowed("offline")),
            (Cow::Borrowed("prompt"), Cow::Borrowed("consent")),
            (
                Cow::Borrowed("include_granted_scopes"),
                Cow::Borrowed("true"),
            ),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: Some(Cow::Borrowed(
            nuo_model_codec::client_identity::ANTIGRAVITY_USER_AGENT,
        )),
        browser_login: true,
        default_login_method: LoginMethod::Browser,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 51121,
        port_mode: PortMode::PreferredOrDynamic(51121),
        oauth_path: Cow::Borrowed("/oauth-callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::disabled(),
        device_token_url: Cow::Borrowed("https://oauth2.googleapis.com/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Google Antigravity Standalone CLI preset.
pub fn antigravity_cli_preset() -> OAuthConfig {
    let mut cfg = antigravity_preset();
    cfg.provider_id = Cow::Borrowed("antigravity-cli");
    cfg.client_id = Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLI_CLIENT_ID);
    cfg.client_secret = Some(Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET));
    cfg
}

/// Google Antigravity enricher: discovers the Cloud project id and user profile.
#[derive(Debug, Clone, Default)]
pub struct AntigravityOAuthEnricher;

#[async_trait]
impl OAuthTokenEnricher for AntigravityOAuthEnricher {
    async fn on_login_success(
        &self,
        client: &Http,
        _connection_label: &str,
        tokens: &TokenResponse,
        _previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        if let Ok(project) =
            resolve_antigravity_project(client, tokens.access_token.expose_secret()).await
            && !project.is_empty()
        {
            token_set.set_attr("project_id", &project);
            token_set.set_attr("account_id", &project);
        }
        if let Ok(info) = fetch_google_userinfo(client, tokens.access_token.expose_secret()).await {
            token_set.user_email = info.email;
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
        let mut project_id = stored.get_attr("project_id").map(ToString::to_string);
        let mut account_id = stored.get_attr("account_id").map(ToString::to_string);
        let mut user_email = stored.user_email.clone();

        if account_id.is_none() && project_id.is_some() {
            account_id = project_id.clone();
        } else if project_id.is_none() && account_id.is_some() {
            project_id = account_id.clone();
        }

        if project_id.is_none() {
            if let Ok(project) =
                resolve_antigravity_project(client, refreshed.access_token.expose_secret()).await
                && !project.is_empty()
            {
                project_id = Some(project.clone());
                account_id = Some(project);
            }
        }
        if user_email.is_none()
            && let Ok(info) = fetch_google_userinfo(client, refreshed.access_token.expose_secret()).await
        {
            user_email = info.email;
        }

        if let Some(acct) = account_id {
            token_set.set_attr("account_id", acct);
        }
        if let Some(proj) = project_id {
            token_set.set_attr("project_id", proj);
        }
        token_set.user_email = user_email;
        Ok(())
    }
}

fn project_metadata(tokens: &TokenSet, mut auth: ResolvedAuth) -> ResolvedAuth {
    if let Some(project) = tokens.get_attr("project_id") {
        auth = auth.with_extension(nuo_model_codec::GoogleAuthMetadata {
            project_id: project.to_string(),
        });
    }
    auth
}

struct AntigravityProvider;

#[async_trait]
impl OAuthProvider for AntigravityProvider {
    fn ids(&self) -> &'static [&'static str] {
        &["google-antigravity", "antigravity"]
    }

    fn config(&self) -> OAuthConfig {
        antigravity_preset()
    }

    fn enricher(&self) -> Box<dyn OAuthTokenEnricher> {
        Box::new(AntigravityOAuthEnricher)
    }

    fn project_metadata(&self, tokens: &TokenSet, auth: ResolvedAuth) -> ResolvedAuth {
        project_metadata(tokens, auth)
    }
}

struct AntigravityCliProvider;

#[async_trait]
impl OAuthProvider for AntigravityCliProvider {
    fn ids(&self) -> &'static [&'static str] {
        &["antigravity-cli"]
    }

    fn config(&self) -> OAuthConfig {
        antigravity_cli_preset()
    }

    fn enricher(&self) -> Box<dyn OAuthTokenEnricher> {
        Box::new(AntigravityOAuthEnricher)
    }

    fn project_metadata(&self, tokens: &TokenSet, auth: ResolvedAuth) -> ResolvedAuth {
        project_metadata(tokens, auth)
    }
}

/// The provider surfaces this crate registers.
pub fn providers() -> Vec<Arc<dyn OAuthProvider>> {
    vec![Arc::new(AntigravityProvider), Arc::new(AntigravityCliProvider)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled Antigravity presets must ship *matched* client-id/secret
    /// pairs (verified live against `oauth2.googleapis.com/token`, ADR-0289).
    #[test]
    fn antigravity_presets_ship_matched_credential_pairs() {
        let main = antigravity_preset();
        assert_eq!(main.client_id, GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_ID);
        assert_eq!(
            main.client_secret.as_deref(),
            Some(GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_SECRET)
        );
        let cli = antigravity_cli_preset();
        assert_eq!(cli.client_id, GOOGLE_ANTIGRAVITY_CLI_CLIENT_ID);
        assert_eq!(
            cli.client_secret.as_deref(),
            Some(GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET)
        );
        assert_ne!(main.client_secret, cli.client_secret);
        assert_ne!(main.client_id, cli.client_id);
    }

    #[test]
    fn antigravity_preset_advertises_cli_brand_user_agent() {
        let cfg = antigravity_preset();
        let ua = cfg.user_agent.expect("antigravity preset declares a User-Agent");
        assert!(ua.starts_with("antigravity/cli/"));
        assert_eq!(ua, nuo_model_codec::client_identity::ANTIGRAVITY_USER_AGENT);
    }

    #[test]
    fn antigravity_scope_requests_aicode() {
        let cfg = antigravity_preset();
        assert!(cfg.scope.contains("https://www.googleapis.com/auth/aicode"));
        assert!(cfg.scope.contains("https://www.googleapis.com/auth/cloud-platform"));
    }
}
