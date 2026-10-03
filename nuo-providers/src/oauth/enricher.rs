//! Polymorphic post-login and post-refresh credential enrichers (ADR-0267).
//!
//! Eliminates procedural branching by encapsulating provider-specific metadata
//! extraction into strategy enrichers.

use async_trait::async_trait;
use nuo_host::SecretString;
use nuo_model_codec::provider_auth::OAuthConfig;

use super::opencode_device::{fetch_orgs, fetch_user, server_from_config};
use super::token::{
    access_token_expiry_ms, chatgpt_account_id, fetch_google_userinfo, resolve_antigravity_project,
};
use super::{AuthError, TokenResponse, TokenSet};
use crate::registry::qoder::{QoderStoredIdentity, generate_machine_key_hex};

/// Lifecycle hook for post-login and post-refresh provider-specific metadata enrichment.
#[async_trait]
pub trait OAuthTokenEnricher: Send + Sync {
    /// Enrich a newly minted [`TokenSet`] following a successful authorization
    /// flow.
    ///
    /// `previous` is whatever the credential store held for this connection
    /// before the login — `None` on a first login. It exists so an enricher can
    /// preserve durable identity material across a re-login without reaching for
    /// a store of its own: the caller owns the store, the enricher owns the
    /// policy (ADR-0303 §1).
    async fn on_login_success(
        &self,
        client: &crate::http::Http,
        connection_label: &str,
        tokens: &TokenResponse,
        previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError>;

    /// Enrich a refreshed [`TokenSet`], preserving durable identity material.
    async fn on_refresh_success(
        &self,
        client: &crate::http::Http,
        stored: &TokenSet,
        refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError>;

    /// Human-facing error formatting for provider-specific failure codes.
    fn format_login_error(&self, error: &AuthError) -> String {
        error.to_string()
    }
}

/// Standard enricher for generic RFC 6749 / RFC 8628 OAuth providers (e.g. xAI, Copilot).
#[derive(Debug, Clone, Default)]
pub struct StandardOAuthEnricher;

#[async_trait]
impl OAuthTokenEnricher for StandardOAuthEnricher {
    async fn on_login_success(
        &self,
        _client: &crate::http::Http,
        _connection_label: &str,
        _tokens: &TokenResponse,
        _previous: Option<&TokenSet>,
        _token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        Ok(())
    }

    async fn on_refresh_success(
        &self,
        _client: &crate::http::Http,
        _stored: &TokenSet,
        _refreshed: &TokenResponse,
        _token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        Ok(())
    }
}

/// ChatGPT / Codex enricher: extracts and tracks `account_id` from JWT claims.
#[derive(Debug, Clone, Default)]
pub struct ChatGptOAuthEnricher;

#[async_trait]
impl OAuthTokenEnricher for ChatGptOAuthEnricher {
    async fn on_login_success(
        &self,
        _client: &crate::http::Http,
        _connection_label: &str,
        tokens: &TokenResponse,
        _previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        if let Some(acct) = tokens
            .id_token
            .as_ref()
            .map(SecretString::expose_secret)
            .or(Some(tokens.access_token.expose_secret()))
            .and_then(chatgpt_account_id)
        {
            token_set.set_attr("account_id", acct);
        }
        Ok(())
    }

    async fn on_refresh_success(
        &self,
        _client: &crate::http::Http,
        stored: &TokenSet,
        refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        let account_id = refreshed
            .id_token
            .as_ref()
            .map(SecretString::expose_secret)
            .or(Some(refreshed.access_token.expose_secret()))
            .and_then(chatgpt_account_id)
            .or_else(|| stored.get_attr("account_id").map(ToString::to_string));

        if let Some(acct) = account_id {
            token_set.set_attr("account_id", acct);
        }
        Ok(())
    }
}

/// Google Antigravity enricher: discovers Google Cloud project ID and user profile.
#[derive(Debug, Clone, Default)]
pub struct AntigravityOAuthEnricher;

#[async_trait]
impl OAuthTokenEnricher for AntigravityOAuthEnricher {
    async fn on_login_success(
        &self,
        client: &crate::http::Http,
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
        if let Ok(info) =
            fetch_google_userinfo(client, tokens.access_token.expose_secret()).await
        {
            token_set.user_email = info.email;
        }
        Ok(())
    }

    async fn on_refresh_success(
        &self,
        client: &crate::http::Http,
        stored: &TokenSet,
        refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        let mut project_id = stored.get_attr("project_id").map(ToString::to_string);
        let mut account_id = stored.get_attr("account_id").map(ToString::to_string);
        let mut user_email = stored.user_email.clone();

        if project_id.is_none() || account_id.is_none() {
            if let Ok(project) =
                resolve_antigravity_project(client, refreshed.access_token.expose_secret()).await
                && !project.is_empty()
            {
                project_id = Some(project.clone());
                if account_id.is_none() {
                    account_id = Some(project);
                }
            }
        }
        if user_email.is_none()
            && let Ok(info) =
                fetch_google_userinfo(client, refreshed.access_token.expose_secret()).await
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

/// Alibaba Qoder enricher: pins durable machine identity and user ID.
#[derive(Debug, Clone, Default)]
pub struct QoderOAuthEnricher;

#[async_trait]
impl OAuthTokenEnricher for QoderOAuthEnricher {
    async fn on_login_success(
        &self,
        _client: &crate::http::Http,
        _connection_label: &str,
        tokens: &TokenResponse,
        previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        let uid = tokens.qoder_uid.clone().unwrap_or_default();
        // Re-login preserves the machine key minted on first login: Qoder pins
        // its risk signals to that value, so a fresh key would present as a new
        // device. `previous` is the caller's own read of the credential store —
        // the enricher holds no store of its own.
        let machine_key_hex = previous
            .and_then(|t| t.get_json_attr::<QoderStoredIdentity>("qoder"))
            .map(|i| i.machine_key_hex.expose_secret().to_string())
            .unwrap_or_else(generate_machine_key_hex);

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
        _client: &crate::http::Http,
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

/// OpenCode Console enricher: records the account identity and default org.
#[derive(Debug, Clone)]
pub struct OpencodeOAuthEnricher {
    /// Console origin (`…/console`) used for the profile and org lookups.
    server: String,
}

impl OpencodeOAuthEnricher {
    pub fn new(config: &OAuthConfig) -> Self {
        Self {
            server: server_from_config(config),
        }
    }
}

#[async_trait]
impl OAuthTokenEnricher for OpencodeOAuthEnricher {
    async fn on_login_success(
        &self,
        client: &crate::http::Http,
        _connection_label: &str,
        tokens: &TokenResponse,
        _previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        let access = tokens.access_token.expose_secret();
        if let Ok(user) = fetch_user(client, &self.server, access).await {
            if !user.email.is_empty() {
                token_set.user_email = Some(user.email);
            }
            if !user.id.is_empty() {
                token_set.set_attr("account_id", user.id);
            }
        }
        if let Ok(mut orgs) = fetch_orgs(client, &self.server, access).await {
            orgs.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
            // Persist the full membership so a later org switch has data, and
            // pin the same deterministic default `../opencode` picks.
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
        client: &crate::http::Http,
        stored: &TokenSet,
        refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        for key in ["account_id", "org_id", "org_name"] {
            if let Some(value) = stored.get_attr(key) {
                token_set.set_attr(key, value.to_string());
            }
        }
        if let Some(orgs) =
            stored.get_json_attr::<Vec<super::opencode_device::OpencodeOrg>>("opencode_orgs")
        {
            token_set.set_json_attr("opencode_orgs", &orgs);
        }
        token_set.user_email = stored.user_email.clone();
        // Credentials minted before org resolution (or with an empty org list)
        // carry no `org_id`, which silently strands every Console surface on
        // `Workspace selection required`. Refetch once with the fresh token so
        // the healed attribute persists with the rotated credential.
        if token_set.get_attr("org_id").is_none()
            && let Ok(mut orgs) =
                fetch_orgs(client, &self.server, refreshed.access_token.expose_secret()).await
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

/// Resolve the appropriate [`OAuthTokenEnricher`] for an OAuth configuration.
pub fn enricher_for_config(config: &OAuthConfig) -> Box<dyn OAuthTokenEnricher> {
    match config.provider_id.as_ref() {
        "chatgpt" | "openai-subscription" => Box::new(ChatGptOAuthEnricher),
        "google-antigravity" | "antigravity" | "antigravity-cli" => {
            Box::new(AntigravityOAuthEnricher)
        }
        "qoder" | "qoder-cn" => Box::new(QoderOAuthEnricher),
        "opencode" | "opencode-go" => Box::new(OpencodeOAuthEnricher::new(config)),
        _ => {
            if config.token_url.contains("openapi.qoder.sh")
                || config.token_url.contains("openapi.qoder.com.cn")
            {
                Box::new(QoderOAuthEnricher)
            } else if config.token_url.contains("auth.openai.com") {
                Box::new(ChatGptOAuthEnricher)
            } else if config.token_url.contains("oauth2.googleapis.com") {
                Box::new(AntigravityOAuthEnricher)
            } else {
                Box::new(StandardOAuthEnricher)
            }
        }
    }
}

/// Helper to build a complete token set using the resolved enricher.
pub async fn build_token_set_with_enricher(
    client: &crate::http::Http,
    config: &OAuthConfig,
    connection_label: &str,
    tokens: TokenResponse,
    previous: Option<TokenSet>,
    now_ms: i64,
) -> Result<TokenSet, AuthError> {
    let expires_ms = access_token_expiry_ms(
        tokens.access_token.expose_secret(),
        tokens.expires_in,
        now_ms,
    );

    let mut token_set = TokenSet {
        access: tokens.access_token.clone(),
        refresh: tokens.refresh_token.clone().unwrap_or_default(),
        expires_ms,
        id_token: tokens.id_token.clone(),
        token_type: tokens.token_type.clone(),
        scope: tokens.scope.clone(),
        user_email: None,
        attributes: serde_json::Map::new(),
    };

    let enricher = enricher_for_config(config);
    enricher
        .on_login_success(client, connection_label, &tokens, previous.as_ref(), &mut token_set)
        .await?;

    Ok(token_set)
}
